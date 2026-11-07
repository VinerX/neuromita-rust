"""Device inventory and execution probes executed inside the worker process.

This module reports only what the worker can actually observe. A missing
package, provider or adapter is reported as unavailable; it is never replaced
with a guessed device. Presence of files or a driver is not inference
readiness either: ``execute`` runs a real operation on the selected device.
"""

from __future__ import annotations

import ctypes
import importlib
import json
import platform
import re
import struct
import sys
import uuid
from pathlib import Path
from tempfile import TemporaryDirectory
from typing import Any

SCHEMA_VERSION = 1

_VENDORS = {0x10DE: "NVIDIA", 0x1002: "AMD", 0x8086: "INTEL"}
_DXGI_ERROR_NOT_FOUND = 0x887A0002
_DXGI_ADAPTER_FLAG_SOFTWARE = 2

_BACKENDS = {
    "torch_cpu": ("torch", "cpu"),
    "torch_cuda": ("torch", "cuda"),
    "onnx_cpu": ("onnxruntime", "cpu"),
    "onnx_cuda": ("onnxruntime", "cuda"),
    "onnx_directml": ("onnxruntime", "dml"),
    "ctranslate2_cpu": ("ctranslate2", "cpu"),
    "ctranslate2_cuda": ("ctranslate2", "cuda"),
    "remote_api": ("remote_api", "remote"),
}


def _import(name: str) -> Any:
    return importlib.import_module(name)


def _describe_error(error: BaseException) -> str:
    return f"{type(error).__name__}: {error}"


def _normalize_compute_capability(value: Any) -> int | None:
    if value is None:
        return None
    if isinstance(value, (tuple, list)) and len(value) >= 2:
        try:
            return int(value[0]) * 10 + int(value[1])
        except (TypeError, ValueError):
            return None
    if isinstance(value, int):
        return value if value >= 10 else value * 10
    if isinstance(value, float):
        return int(value * 10)
    text = str(value or "").strip().lower()
    match = re.search(r"(\d+)\D*(\d+)?", text)
    if match is None:
        return None
    major = int(match.group(1))
    minor = int(match.group(2) or 0)
    if text.startswith("sm_") and not match.group(2):
        return major
    return major * 10 + minor if major < 10 else major


def _sm75_tensor_core_sku(gpu_name: str) -> bool:
    """Separate RTX/T4 SM 7.5 parts from GTX 16xx/T-series parts."""

    name = str(gpu_name or "").upper()
    if "RTX" in name:
        return True
    return re.search(r"(?:^|\s)T4(?:\s|$)", name) is not None


def evaluate_fp16(vendor: str, compute_capability: Any, gpu_name: str = "") -> dict[str, Any]:
    """Return the FP16 policy used for CUDA execution.

    This is intentionally stricter than `does CUDA implement FP16`: half mode
    is allowed only where Tensor-Core-class execution is expected, and unknown
    hardware fails closed to FP32.
    """

    normalized_vendor = str(vendor or "CPU").strip().upper()
    cc = _normalize_compute_capability(compute_capability)
    name = str(gpu_name or "").strip()
    if normalized_vendor != "NVIDIA":
        return {"allowed": False, "reason": "non_nvidia"}
    if cc is None:
        return {"allowed": False, "reason": "unknown_compute_capability"}
    if cc in {70, 72}:
        return {"allowed": True, "reason": "tensor_core_sm"}
    if cc == 75:
        if _sm75_tensor_core_sku(name):
            return {"allowed": True, "reason": "sm75_tensor_core_sku"}
        return {"allowed": False, "reason": "sm75_without_tensor_core_evidence"}
    if cc >= 80:
        return {"allowed": True, "reason": "tensor_core_sm"}
    return {"allowed": False, "reason": "compute_capability_below_tensor_core_policy"}


class _Luid(ctypes.Structure):
    _fields_ = (("LowPart", ctypes.c_uint32), ("HighPart", ctypes.c_int32))

    def as_hex(self) -> str:
        return struct.pack("<Ii", int(self.LowPart), int(self.HighPart)).hex()


class _AdapterDesc1(ctypes.Structure):
    _fields_ = (
        ("Description", ctypes.c_wchar * 128),
        ("VendorId", ctypes.c_uint32),
        ("DeviceId", ctypes.c_uint32),
        ("SubSysId", ctypes.c_uint32),
        ("Revision", ctypes.c_uint32),
        ("DedicatedVideoMemory", ctypes.c_size_t),
        ("DedicatedSystemMemory", ctypes.c_size_t),
        ("SharedSystemMemory", ctypes.c_size_t),
        ("AdapterLuid", _Luid),
        ("Flags", ctypes.c_uint32),
    )


def _com_method(pointer: Any, index: int, restype: Any, *argtypes: Any) -> Any:
    vtable = ctypes.cast(pointer, ctypes.POINTER(ctypes.POINTER(ctypes.c_void_p))).contents
    return ctypes.WINFUNCTYPE(restype, ctypes.c_void_p, *argtypes)(vtable[index])


def _release(pointer: Any) -> None:
    if pointer:
        _com_method(pointer, 2, ctypes.c_ulong)(pointer)


def _dxgi_adapters() -> list[dict[str, Any]]:
    """Enumerate real DXGI adapters without any third-party package."""

    if sys.platform != "win32":
        return []
    dxgi = ctypes.WinDLL("dxgi.dll")
    create_factory = dxgi.CreateDXGIFactory1
    create_factory.argtypes = (ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p))
    create_factory.restype = ctypes.c_long
    raw_uuid = uuid.UUID("770aae78-f26f-4dba-a829-253c83d1b387").bytes_le
    factory = ctypes.c_void_p()
    code = int(create_factory(raw_uuid, ctypes.byref(factory)))
    if code < 0 or not factory:
        raise OSError(f"CreateDXGIFactory1 failed: 0x{code & 0xFFFFFFFF:08X}")
    adapters: list[dict[str, Any]] = []
    try:
        enum_adapters = _com_method(
            factory, 12, ctypes.c_long, ctypes.c_uint32, ctypes.POINTER(ctypes.c_void_p)
        )
        index = 0
        while True:
            adapter = ctypes.c_void_p()
            hr = int(enum_adapters(factory, index, ctypes.byref(adapter)))
            if (hr & 0xFFFFFFFF) == _DXGI_ERROR_NOT_FOUND:
                break
            if hr < 0:
                raise OSError(f"EnumAdapters1 failed: 0x{hr & 0xFFFFFFFF:08X}")
            try:
                desc = _AdapterDesc1()
                get_desc = _com_method(adapter, 10, ctypes.c_long, ctypes.POINTER(_AdapterDesc1))
                if int(get_desc(adapter, ctypes.byref(desc))) >= 0 and not (
                    int(desc.Flags) & _DXGI_ADAPTER_FLAG_SOFTWARE
                ):
                    vendor_id = int(desc.VendorId)
                    adapters.append(
                        {
                            "index": index,
                            "name": str(desc.Description).strip(),
                            "vendor": _VENDORS.get(vendor_id, "UNKNOWN"),
                            "vendor_id": f"{vendor_id:04x}",
                            "dedicated_vram_bytes": int(desc.DedicatedVideoMemory),
                            "luid": desc.AdapterLuid.as_hex(),
                        }
                    )
            finally:
                _release(adapter)
            index += 1
    finally:
        _release(factory)
    return adapters


def _cuda_driver() -> dict[str, Any]:
    """Enumerate CUDA ordinals through the driver API, independent of torch."""

    library = "nvcuda.dll" if sys.platform == "win32" else "libcuda.so.1"
    try:
        cuda = ctypes.CDLL(library)
    except OSError as error:
        return {"available": False, "devices": [], "detail": _describe_error(error)}

    def bind(name: str, argtypes: Any, restype: Any = ctypes.c_int) -> Any:
        function = getattr(cuda, name)
        function.argtypes = argtypes
        function.restype = restype
        return function

    try:
        cu_init = bind("cuInit", (ctypes.c_uint,))
        cu_count = bind("cuDeviceGetCount", (ctypes.POINTER(ctypes.c_int),))
        cu_get = bind("cuDeviceGet", (ctypes.POINTER(ctypes.c_int), ctypes.c_int))
        cu_attr = bind(
            "cuDeviceGetAttribute", (ctypes.POINTER(ctypes.c_int), ctypes.c_int, ctypes.c_int)
        )
        cu_name = bind("cuDeviceGetName", (ctypes.c_void_p, ctypes.c_int, ctypes.c_int))
        cu_driver = bind("cuDriverGetVersion", (ctypes.POINTER(ctypes.c_int),))
    except (AttributeError, OSError) as error:
        return {"available": False, "devices": [], "detail": _describe_error(error)}
    try:
        cu_luid = bind(
            "cuDeviceGetLuid",
            (ctypes.c_void_p, ctypes.POINTER(ctypes.c_uint), ctypes.c_int),
        )
    except (AttributeError, OSError):
        cu_luid = None
    code = int(cu_init(0))
    if code != 0:
        return {"available": False, "devices": [], "detail": f"cuInit failed with code {code}"}
    version = ctypes.c_int()
    driver_version = ""
    if int(cu_driver(ctypes.byref(version))) == 0:
        raw = int(version.value)
        driver_version = f"{raw // 1000}.{(raw % 1000) // 10}"
    count = ctypes.c_int()
    if int(cu_count(ctypes.byref(count))) != 0:
        return {"available": False, "devices": [], "detail": "cuDeviceGetCount failed"}
    devices: list[dict[str, Any]] = []
    for ordinal in range(max(0, int(count.value))):
        device = ctypes.c_int()
        if int(cu_get(ctypes.byref(device), ordinal)) != 0:
            continue
        major = ctypes.c_int()
        minor = ctypes.c_int()
        name = ctypes.create_string_buffer(256)
        mask = ctypes.c_uint()
        luid = ctypes.create_string_buffer(8)
        cu_attr(ctypes.byref(major), 75, device.value)
        cu_attr(ctypes.byref(minor), 76, device.value)
        cu_name(name, len(name), device.value)
        luid_hex = ""
        if cu_luid is not None and int(cu_luid(luid, ctypes.byref(mask), device.value)) == 0:
            luid_hex = bytes(luid.raw).hex()
        devices.append(
            {
                "ordinal": ordinal,
                "name": name.value.decode("utf-8", errors="replace"),
                "compute_capability": f"{int(major.value)}.{int(minor.value)}",
                "luid": luid_hex,
                "driver_version": driver_version,
            }
        )
    return {"available": bool(devices), "devices": devices, "driver_version": driver_version}


def _cpu_device(backend: str) -> dict[str, Any]:
    return {
        "backend": backend,
        "kind": "cpu",
        "id": "cpu",
        "name": platform.processor() or "CPU",
        "compute_capability": "",
        "memory_bytes": 0,
        "driver_version": "",
        "fp16": False,
        "fp16_reason": "cpu_backend",
    }


def _cuda_device(
    backend: str,
    ordinal: int,
    name: str,
    compute_capability: Any,
    memory_bytes: int,
    driver_version: str,
) -> dict[str, Any]:
    decision = evaluate_fp16("NVIDIA", compute_capability, name)
    if isinstance(compute_capability, (tuple, list)) and len(compute_capability) >= 2:
        capability = f"{int(compute_capability[0])}.{int(compute_capability[1])}"
    else:
        capability = str(compute_capability or "")
    return {
        "backend": backend,
        "kind": "cuda",
        "id": f"cuda:{ordinal}",
        "name": name,
        "compute_capability": capability,
        "memory_bytes": int(memory_bytes),
        "driver_version": driver_version,
        "fp16": bool(decision["allowed"]),
        "fp16_reason": str(decision["reason"]),
    }


def _probe_torch(cuda: dict[str, Any], devices: list[dict[str, Any]]) -> dict[str, Any]:
    try:
        torch = _import("torch")
        if not callable(torch.tensor) or torch.float32 is None:
            raise ValueError("invalid torch module")
    except Exception as error:
        return {
            "kind": "torch",
            "available": False,
            "version": "",
            "providers": [],
            "detail": _describe_error(error),
        }
    devices.append(_cpu_device("torch"))
    detail = ""
    try:
        available = bool(torch.cuda.is_available())
    except Exception as error:
        available = False
        detail = _describe_error(error)
    if available:
        try:
            for ordinal in range(int(torch.cuda.device_count())):
                name = str(torch.cuda.get_device_name(ordinal))
                major, minor = torch.cuda.get_device_capability(ordinal)
                properties = torch.cuda.get_device_properties(ordinal)
                devices.append(
                    _cuda_device(
                        "torch",
                        ordinal,
                        name,
                        (int(major), int(minor)),
                        int(getattr(properties, "total_memory", 0)),
                        str(cuda.get("driver_version") or ""),
                    )
                )
        except Exception as error:
            detail = _describe_error(error)
    elif not detail:
        detail = "torch was built without a usable CUDA runtime or no CUDA device is visible"
    return {
        "kind": "torch",
        "available": True,
        "version": str(getattr(torch, "__version__", "")),
        "providers": [],
        "detail": detail,
    }


def _probe_onnx(
    adapters: list[dict[str, Any]],
    cuda: dict[str, Any],
    devices: list[dict[str, Any]],
) -> dict[str, Any]:
    try:
        runtime = _import("onnxruntime")
        if not callable(runtime.InferenceSession) or not callable(runtime.SessionOptions):
            raise ValueError("invalid onnxruntime module")
    except Exception as error:
        return {
            "kind": "onnxruntime",
            "available": False,
            "version": "",
            "providers": [],
            "detail": _describe_error(error),
        }
    try:
        providers = [str(item) for item in runtime.get_available_providers()]
    except Exception as error:
        return {
            "kind": "onnxruntime",
            "available": False,
            "version": str(getattr(runtime, "__version__", "")),
            "providers": [],
            "detail": _describe_error(error),
        }
    if "CPUExecutionProvider" in providers:
        devices.append(_cpu_device("onnxruntime"))
    if "CUDAExecutionProvider" in providers:
        for record in cuda.get("devices") or []:
            devices.append(
                _cuda_device(
                    "onnxruntime",
                    int(record["ordinal"]),
                    str(record.get("name") or ""),
                    record.get("compute_capability"),
                    0,
                    str(record.get("driver_version") or ""),
                )
            )
    if "DmlExecutionProvider" in providers:
        for adapter in adapters:
            devices.append(
                {
                    "backend": "onnxruntime",
                    "kind": "dml",
                    "id": f"dml:{adapter['index']}",
                    "name": str(adapter.get("name") or "DirectML adapter"),
                    "compute_capability": "",
                    "memory_bytes": int(adapter.get("dedicated_vram_bytes") or 0),
                    "driver_version": "",
                    "fp16": False,
                    "fp16_reason": "directml_not_in_fp16_policy",
                    "luid": adapter["luid"],
                }
            )
    return {
        "kind": "onnxruntime",
        "available": True,
        "version": str(getattr(runtime, "__version__", "")),
        "providers": providers,
        "detail": "" if adapters or "DmlExecutionProvider" not in providers else "DirectML adapters could not be enumerated",
    }


def _probe_ctranslate2(devices: list[dict[str, Any]]) -> dict[str, Any]:
    try:
        runtime = _import("ctranslate2")
        if not callable(runtime.Translator):
            raise ValueError("invalid ctranslate2 module")
        cpu_types = sorted(runtime.get_supported_compute_types("cpu"))
        cpu = _cpu_device("ctranslate2")
        cpu["compute_types"] = cpu_types
        devices.append(cpu)
        detail = ""
        try:
            for ordinal in range(runtime.get_cuda_device_count()):
                compute_types = sorted(runtime.get_supported_compute_types("cuda", ordinal))
                # CUDA_VISIBLE_DEVICES can remap ordinals relative to the driver API.
                devices.append({
                    "backend": "ctranslate2", "kind": "cuda", "id": f"cuda:{ordinal}",
                    "name": "", "compute_capability": "", "memory_bytes": 0,
                    "driver_version": "", "compute_types": compute_types,
                    "fp16": "float16" in compute_types,
                    "fp16_reason": "ctranslate2_supported_compute_types",
                })
        except Exception as error:
            detail = _describe_error(error)
        return {"kind": "ctranslate2", "available": True,
                "version": str(getattr(runtime, "__version__", "")),
                "providers": [], "detail": detail}
    except Exception as error:
        return {"kind": "ctranslate2", "available": False, "version": "",
                "providers": [], "detail": _describe_error(error)}


def probe() -> dict[str, Any]:
    adapters: list[dict[str, Any]] = []
    adapter_error = ""
    try:
        adapters = _dxgi_adapters()
    except Exception as error:
        adapter_error = _describe_error(error)
    try:
        cuda = _cuda_driver()
    except Exception as error:
        cuda = {"available": False, "devices": [], "detail": _describe_error(error)}
    devices: list[dict[str, Any]] = []
    backends = [_probe_torch(cuda, devices), _probe_onnx(adapters, cuda, devices),
                _probe_ctranslate2(devices)]
    result: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "platform": sys.platform,
        "python": platform.python_version(),
        "adapters": adapters,
        "backends": backends,
        "devices": devices,
    }
    if adapter_error:
        result["adapter_error"] = adapter_error
    if cuda.get("detail"):
        result["cuda_error"] = str(cuda["detail"])
    return result


def _device_ordinal(device: str, prefix: str) -> int:
    text = str(device or "")
    match = re.fullmatch(re.escape(prefix) + r":(0|[1-9]\d*)", text)
    if match is None:
        raise ValueError(f"invalid {prefix} device: {text!r}")
    return int(match.group(1))


def verify_runtime(runtime: Any) -> dict[str, str]:
    """Validate a runtime profile against real worker introspection."""

    if not isinstance(runtime, dict):
        raise ValueError("runtime profile must be an object")
    backend = runtime.get("backend")
    device = runtime.get("device")
    precision = runtime.get("precision")
    if not isinstance(backend, str) or backend not in _BACKENDS:
        raise ValueError(f"unsupported runtime backend: {backend!r}")
    if not isinstance(device, str) or not device:
        raise ValueError("runtime device is required")
    if precision not in ("fp32", "fp16"):
        raise ValueError(f"unsupported precision: {precision!r}")
    family, kind = _BACKENDS[backend]
    if family == "remote_api":
        if device != "remote" or precision != "fp32":
            raise ValueError("remote_api requires device remote and fp32 (wire placeholder; no local precision claim)")
        return {"backend": backend, "device": device, "precision": precision}
    if kind == "cpu" and device != "cpu":
        raise ValueError(f"{backend} requires device cpu")
    if kind in ("cuda", "dml"):
        _device_ordinal(device, kind)
    if precision == "fp16" and backend not in ("torch_cuda", "ctranslate2_cuda"):
        raise ValueError(f"FP16 execution is unsupported for {backend}")
    observed = probe()
    entry = next((item for item in observed["backends"] if item["kind"] == family), None)
    if entry is None or not entry.get("available"):
        detail = "" if entry is None else str(entry.get("detail") or "")
        raise RuntimeError(f"{family} backend is unavailable: {detail}".strip())
    record = next(
        (
            item
            for item in observed["devices"]
            if item["backend"] == family and item["kind"] == kind and item["id"] == device
        ),
        None,
    )
    if record is None:
        raise RuntimeError(f"device {device} is not reported by the {family} backend")
    if family == "ctranslate2":
        compute_type = "float16" if precision == "fp16" else "float32"
        if compute_type not in record.get("compute_types", []):
            raise RuntimeError(f"CTranslate2 does not support {compute_type} on {device}")
    if precision == "fp16" and not record.get("fp16"):
        reason = str(record.get("fp16_reason") or "")
        raise RuntimeError(f"FP16 is not allowed on {device}: {reason}".strip())
    return {"backend": backend, "device": device, "precision": precision}


def _execute_torch(backend: str, device: str, precision: str) -> dict[str, Any]:
    torch = _import("torch")
    if precision == "fp16" and backend != "torch_cuda":
        raise ValueError("FP16 execution is supported only for torch_cuda")
    dtype = torch.float16 if precision == "fp16" else torch.float32
    if backend == "torch_cpu":
        if device != "cpu":
            raise ValueError(f"torch_cpu requires the cpu device, got {device!r}")
        left = torch.tensor([1.0, 2.0], device="cpu", dtype=dtype)
        if str(left.device) != "cpu" or left.dtype != dtype:
            raise RuntimeError("torch allocated a tensor on an unexpected device or precision")
        total = float((left + 1.0).sum().item())
        target = "cpu"
        name = "CPU"
    else:
        ordinal = _device_ordinal(device, "cuda")
        if not bool(torch.cuda.is_available()):
            raise RuntimeError("CUDA is not available in this worker")
        count = int(torch.cuda.device_count())
        if ordinal >= count:
            raise RuntimeError(f"{device} is not present; the worker reports {count} CUDA devices")
        with torch.cuda.device(ordinal):
            left = torch.tensor([1.0, 2.0], device=device, dtype=dtype)
            if str(left.device) != device or left.dtype != dtype:
                raise RuntimeError("torch allocated a tensor on an unexpected device or precision")
            right = left + 1.0
            torch.cuda.synchronize(ordinal)
            total = float(right.sum().item())
        target = device
        name = str(torch.cuda.get_device_name(ordinal))
    if total != 5.0:
        raise RuntimeError(f"torch execution on {target} returned an invalid result")
    return {
        "verified": True,
        "detail": f"torch {torch.__version__} executed {precision} on {target} ({name})",
    }


def _minimal_onnx_model() -> bytes:
    try:
        import onnx
        from onnx import TensorProto, helper
    except Exception as error:
        raise RuntimeError(
            f"onnx package is required for an ONNX execution probe: {_describe_error(error)}"
        ) from error
    node = helper.make_node("Add", ["left", "right"], ["sum"], name="probe")
    graph = helper.make_graph(
        [node],
        "neuromita-probe",
        [helper.make_tensor_value_info(name, TensorProto.FLOAT, [2])
         for name in ("left", "right")],
        [helper.make_tensor_value_info("sum", TensorProto.FLOAT, [2])],
    )
    model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 13)])
    model.ir_version = 7
    return model.SerializeToString()


def _execute_onnx(backend: str, device: str, precision: str) -> dict[str, Any]:
    if precision != "fp32":
        raise ValueError("ONNX execution precision is defined by the model; select fp32")
    runtime = _import("onnxruntime")
    providers = {str(item) for item in runtime.get_available_providers()}
    if backend == "onnx_cpu":
        if device != "cpu":
            raise ValueError(f"onnx_cpu requires the cpu device, got {device!r}")
        if "CPUExecutionProvider" not in providers:
            raise RuntimeError("CPUExecutionProvider is not available")
        provider_list: list[Any] = ["CPUExecutionProvider"]
        target = "cpu"
    elif backend == "onnx_cuda":
        if "CUDAExecutionProvider" not in providers:
            raise RuntimeError("CUDAExecutionProvider is not available")
        ordinal = _device_ordinal(device, "cuda")
        provider_list = [("CUDAExecutionProvider", {"device_id": ordinal}), "CPUExecutionProvider"]
        target = device
    else:
        if "DmlExecutionProvider" not in providers:
            raise RuntimeError("DmlExecutionProvider is not available")
        ordinal = _device_ordinal(device, "dml")
        provider_list = [("DmlExecutionProvider", {"device_id": ordinal}), "CPUExecutionProvider"]
        target = device
    numpy = _import("numpy")
    selected_provider = (provider_list[0] if isinstance(provider_list[0], str)
                         else provider_list[0][0])
    with TemporaryDirectory(prefix="neuromita-onnx-probe-") as directory:
        options = runtime.SessionOptions()
        options.graph_optimization_level = runtime.GraphOptimizationLevel.ORT_DISABLE_ALL
        options.execution_mode = runtime.ExecutionMode.ORT_SEQUENTIAL
        options.enable_mem_pattern = False
        options.enable_profiling = True
        options.profile_file_prefix = str(Path(directory) / "profile")
        if backend != "onnx_cpu":
            options.add_session_config_entry("session.disable_cpu_ep_fallback", "1")
        session = runtime.InferenceSession(_minimal_onnx_model(), sess_options=options,
                                           providers=[provider_list[0]])
        session.disable_fallback()
        if session.get_providers()[0] != selected_provider:
            raise RuntimeError(f"ONNX fell back from {selected_provider}")
        try:
            output = session.run(None, {
                "left": numpy.array([1.0, 2.0], dtype=numpy.float32),
                "right": numpy.array([3.0, 4.0], dtype=numpy.float32),
            })
        finally:
            profile_path = session.end_profiling()
        events = json.loads(Path(profile_path).read_text(encoding="utf-8"))
        placements = [event.get("args", {}).get("provider") for event in events
                      if event.get("cat") == "Node" and event.get("args", {}).get("provider")]
        if not placements or any(provider != selected_provider for provider in placements):
            raise RuntimeError(f"ONNX did not execute exclusively on {selected_provider}")
    values = [float(item) for item in output[0]]
    if values != [4.0, 6.0]:
        raise RuntimeError(f"ONNX execution on {target} returned an invalid result: {values}")
    return {
        "verified": True,
        "detail": f"onnxruntime {getattr(runtime, '__version__', '?')} executed on {target} via {provider_list[0] if isinstance(provider_list[0], str) else provider_list[0][0]}",
    }


def _execute_ctranslate2(device: str, precision: str, model_path: Any, tokens: Any) -> dict[str, Any]:
    if not isinstance(model_path, str) or not model_path or not Path(model_path).is_dir():
        raise ValueError("CTranslate2 execution requires a local Translator model_path directory")
    if (not isinstance(tokens, list) or not tokens
            or not all(isinstance(token, str) and token for token in tokens)):
        raise ValueError("CTranslate2 execution requires nonempty source tokens for that model")
    runtime = _import("ctranslate2")
    kind = "cpu" if device == "cpu" else "cuda"
    ordinal = 0 if kind == "cpu" else _device_ordinal(device, "cuda")
    compute_type = "float16" if precision == "fp16" else "float32"
    model = runtime.Translator(model_path, device=kind, device_index=ordinal,
                               compute_type=compute_type)
    try:
        if (model.device != kind or model.device_index != [ordinal]
                or model.compute_type != compute_type):
            raise RuntimeError("CTranslate2 actual device or compute type differs from selection")
        output = model.translate_batch([tokens], max_decoding_length=8)
        if len(output) != 1 or not output[0].hypotheses:
            raise RuntimeError("CTranslate2 execution returned no hypotheses")
        return {"verified": True, "detail": f"ctranslate2 Translator executed {precision} on {device}"}
    finally:
        model.unload_model()


def execute(backend: str, device: str, precision: str, *,
            model_path: Any = None, tokens: Any = None) -> dict[str, Any]:
    """Run a real operation on the selected device."""

    verify_runtime({"backend": backend, "device": device, "precision": precision})
    if backend == "remote_api":
        raise ValueError("remote_api has no local execution probe; use the plugin runtime contract")
    if backend in ("ctranslate2_cpu", "ctranslate2_cuda"):
        return _execute_ctranslate2(device, precision, model_path, tokens)
    if backend in ("torch_cpu", "torch_cuda"):
        return _execute_torch(backend, device, precision)
    return _execute_onnx(backend, device, precision)
