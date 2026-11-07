"""Hardware observations, fail-closed execution, and optional plugin runtime contracts."""

import contextlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

from neuromita_ai_sdk import hardware
from neuromita_ai_sdk.host import PluginHost


ROOT = Path(__file__).resolve().parents[1]
CPU = {"backend": "torch_cpu", "device": "cpu", "precision": "fp32"}
REMOTE = {"backend": "remote_api", "device": "remote", "precision": "fp32"}


class Tensor:
    def __init__(self, values, device="cpu", dtype="float32"):
        self.values, self.device, self.dtype = values, device, dtype

    def __add__(self, scalar):
        return Tensor([value + scalar for value in self.values], self.device, self.dtype)

    def sum(self):
        return Tensor([sum(self.values)], self.device, self.dtype)

    def item(self):
        return self.values[0]


def torch_module(cuda=False):
    return SimpleNamespace(
        __version__="fixture", tensor=Mock(side_effect=Tensor),
        float32="float32", float16="float16",
        cuda=SimpleNamespace(
            is_available=lambda: cuda, device_count=lambda: int(cuda),
            get_device_name=lambda index: "RTX fixture",
            get_device_capability=lambda index: (8, 0),
            get_device_properties=lambda index: SimpleNamespace(total_memory=1024),
            device=lambda index: contextlib.nullcontext(), synchronize=Mock(),
        ),
    )


class HardwareTests(unittest.TestCase):
    def setUp(self):
        self.modules = {}
        def load(name):
            if name not in self.modules:
                raise ModuleNotFoundError(name)
            return self.modules[name]
        self.enterContext(patch.object(hardware, "_import", side_effect=load))
        self.enterContext(patch.object(hardware, "_dxgi_adapters", return_value=[]))
        self.enterContext(patch.object(hardware, "_cuda_driver", return_value={"devices": []}))

    def test_absent_packages_never_create_devices_or_readiness(self):
        inventory = hardware.probe()
        self.assertEqual(inventory["schema_version"], 1)
        self.assertEqual(inventory["devices"], [])
        self.assertEqual(len(inventory["backends"]), 3)
        self.assertTrue(all(not item["available"] for item in inventory["backends"]))
        for backend, device in (("torch_cpu", "cpu"), ("torch_cuda", "cuda:0"),
                                ("onnx_directml", "dml:0"), ("ctranslate2_cpu", "cpu")):
            with self.subTest(backend=backend), self.assertRaises(RuntimeError):
                hardware.execute(backend, device, "fp32")

    def test_invalid_modules_and_driver_errors_are_inventory_errors(self):
        self.modules.update({name: SimpleNamespace() for name in ("torch", "onnxruntime", "ctranslate2")})
        with patch.object(hardware, "_dxgi_adapters", side_effect=OSError("DXGI unavailable")), \
                patch.object(hardware, "_cuda_driver", side_effect=OSError("driver unavailable")):
            inventory = hardware.probe()
        self.assertEqual(inventory["devices"], [])
        self.assertIn("DXGI unavailable", inventory["adapter_error"])
        self.assertIn("driver unavailable", inventory["cuda_error"])
        self.assertTrue(all(not item["available"] for item in inventory["backends"]))

    def test_malformed_profiles_and_unsupported_precisions(self):
        for profile in (None, [], {}, {**CPU, "backend": []}, {**CPU, "device": "cuda:0"},
                        {**CPU, "precision": "int8"}, {**CPU, "precision": "fp16"},
                        {**CPU, "backend": "torch_cuda", "device": "cuda:00"},
                        {**REMOTE, "device": "cpu"}, {**REMOTE, "precision": "fp16"},
                        {**CPU, "backend": "onnx_cuda", "device": "cuda:0", "precision": "fp16"}):
            with self.subTest(profile=profile), self.assertRaises(ValueError):
                hardware.verify_runtime(profile)

    def test_remote_profile_is_explicit_and_not_hardware(self):
        with patch.object(hardware, "probe", side_effect=AssertionError("local probe")):
            self.assertEqual(hardware.verify_runtime(REMOTE), REMOTE)
            with self.assertRaisesRegex(ValueError, "no local execution probe"):
                hardware.execute(**REMOTE)

    def test_torch_cpu_executes_correct_sum_on_explicit_cpu(self):
        torch = self.modules["torch"] = torch_module()
        self.assertTrue(hardware.execute(**CPU)["verified"])
        torch.tensor.assert_called_once_with([1.0, 2.0], device="cpu", dtype="float32")
        with self.assertRaises(RuntimeError):
            hardware.execute("torch_cuda", "cuda:0", "fp32")

    def test_torch_wrong_result_device_precision_and_operation_failure(self):
        torch = self.modules["torch"] = torch_module()
        for tensor in (Tensor([10, 20]), Tensor([1, 2], "cuda:0"), Tensor([1, 2], dtype="float16")):
            torch.tensor.side_effect = None
            torch.tensor.return_value = tensor
            with self.subTest(tensor=tensor), self.assertRaises(RuntimeError):
                hardware.execute(**CPU)
        torch.tensor.side_effect = RuntimeError("allocation failed")
        with self.assertRaisesRegex(RuntimeError, "allocation failed"):
            hardware.execute(**CPU)

    def test_torch_cuda_precision_policy_and_ordinal(self):
        torch = self.modules["torch"] = torch_module(cuda=True)
        self.assertTrue(hardware.execute("torch_cuda", "cuda:0", "fp16")["verified"])
        torch.cuda.synchronize.assert_called_once_with(0)
        with self.assertRaises(RuntimeError):
            hardware.execute("torch_cuda", "cuda:1", "fp32")
        torch.cuda.get_device_capability = lambda index: (7, 5)
        torch.cuda.get_device_name = lambda index: "GTX 1650"
        with self.assertRaisesRegex(RuntimeError, "FP16 is not allowed"):
            hardware.execute("torch_cuda", "cuda:0", "fp16")

    def onnx_module(self, provider="CPUExecutionProvider", execution_provider=None, output=None):
        execution_provider = execution_provider or provider
        options = SimpleNamespace(add_session_config_entry=Mock())
        session = SimpleNamespace(
            disable_fallback=Mock(), get_providers=lambda: [provider],
            run=Mock(return_value=[[4.0, 6.0]] if output is None else output),
        )
        def end_profiling():
            path = Path(options.profile_file_prefix + ".json")
            path.write_text(json.dumps([{"cat": "Node", "args": {"provider": execution_provider}}]))
            return str(path)
        session.end_profiling = end_profiling
        runtime = SimpleNamespace(
            __version__="fixture", get_available_providers=lambda: [provider],
            SessionOptions=lambda: options, InferenceSession=Mock(return_value=session),
            ExecutionMode=SimpleNamespace(ORT_SEQUENTIAL=0),
            GraphOptimizationLevel=SimpleNamespace(ORT_DISABLE_ALL=0),
        )
        self.modules["onnxruntime"] = runtime
        self.modules["numpy"] = SimpleNamespace(array=lambda values, dtype: values, float32="float32")
        self.enterContext(patch.object(hardware, "_minimal_onnx_model", return_value=b"model-fixture"))
        return runtime, session, options

    def test_onnx_real_inputs_no_folding_and_provider_execution_evidence(self):
        runtime, session, options = self.onnx_module()
        self.assertTrue(hardware.execute("onnx_cpu", "cpu", "fp32")["verified"])
        session.run.assert_called_once_with(None, {"left": [1.0, 2.0], "right": [3.0, 4.0]})
        self.assertFalse(options.enable_mem_pattern)
        self.assertTrue(options.enable_profiling)
        self.assertEqual(runtime.InferenceSession.call_args.kwargs["providers"], ["CPUExecutionProvider"])
        session.disable_fallback.assert_called_once()

    def test_onnx_wrong_output_failed_operation_and_absent_package(self):
        _, session, _ = self.onnx_module(output=[[0, 0]])
        with self.assertRaisesRegex(RuntimeError, "invalid result"):
            hardware.execute("onnx_cpu", "cpu", "fp32")
        session.run.side_effect = RuntimeError("kernel failed")
        with self.assertRaisesRegex(RuntimeError, "kernel failed"):
            hardware.execute("onnx_cpu", "cpu", "fp32")
        self.modules.pop("onnxruntime")
        with self.assertRaises(RuntimeError):
            hardware.execute("onnx_cpu", "cpu", "fp32")

    def test_directml_preserves_dxgi_identity_and_unfiltered_ordinal(self):
        runtime, _, options = self.onnx_module("DmlExecutionProvider")
        adapter = {"index": 3, "name": "Actual AMD adapter", "vendor": "AMD",
                   "dedicated_vram_bytes": 4096, "luid": "0102030405060708"}
        with patch.object(hardware, "_dxgi_adapters", return_value=[adapter]):
            device = hardware.probe()["devices"][0]
            self.assertEqual(device["id"], "dml:3")
            self.assertEqual(device["name"], adapter["name"])
            self.assertEqual(device["luid"], adapter["luid"])
            self.assertTrue(hardware.execute("onnx_directml", "dml:3", "fp32")["verified"])
            with self.assertRaises(RuntimeError):
                hardware.execute("onnx_directml", "dml:0", "fp32")
        self.assertEqual(runtime.InferenceSession.call_args.kwargs["providers"],
                         [("DmlExecutionProvider", {"device_id": 3})])
        options.add_session_config_entry.assert_called_with("session.disable_cpu_ep_fallback", "1")

    def test_onnx_provider_list_does_not_prove_gpu_execution(self):
        _, session, _ = self.onnx_module("CUDAExecutionProvider", "CPUExecutionProvider")
        cuda = {"devices": [{"ordinal": 0, "name": "RTX fixture", "compute_capability": "8.0"}]}
        with patch.object(hardware, "_cuda_driver", return_value=cuda):
            with self.assertRaisesRegex(RuntimeError, "exclusively"):
                hardware.execute("onnx_cuda", "cuda:0", "fp32")
            session.get_providers = lambda: ["CPUExecutionProvider"]
            with self.assertRaisesRegex(RuntimeError, "fell back"):
                hardware.execute("onnx_cuda", "cuda:0", "fp32")

    def ct2_module(self, cuda=0):
        model = SimpleNamespace(device="cpu", device_index=[0], compute_type="float32",
                                translate_batch=Mock(return_value=[SimpleNamespace(hypotheses=[["hello"]])]),
                                unload_model=Mock())
        runtime = SimpleNamespace(
            __version__="fixture", get_cuda_device_count=lambda: cuda,
            get_supported_compute_types=lambda kind, index=0: {"float32", "float16"} if kind == "cuda" else {"float32", "int8"},
            Translator=Mock(return_value=model),
        )
        self.modules["ctranslate2"] = runtime
        return runtime, model

    def test_ctranslate2_inventory_and_real_model_execution_contract(self):
        runtime, model = self.ct2_module()
        self.assertEqual(hardware.verify_runtime({**CPU, "backend": "ctranslate2_cpu"})["device"], "cpu")
        with tempfile.TemporaryDirectory() as directory:
            self.assertTrue(hardware.execute("ctranslate2_cpu", "cpu", "fp32",
                                            model_path=directory, tokens=["hello"])["verified"])
        model.translate_batch.assert_called_once_with([["hello"]], max_decoding_length=8)
        model.unload_model.assert_called_once()
        runtime.Translator.assert_called_once_with(directory, device="cpu", device_index=0, compute_type="float32")

    def test_ctranslate2_missing_model_unsupported_precision_and_fallback(self):
        runtime, model = self.ct2_module(cuda=1)
        with self.assertRaisesRegex(ValueError, "model_path"):
            hardware.execute("ctranslate2_cpu", "cpu", "fp32")
        with self.assertRaises(ValueError):
            hardware.execute("ctranslate2_cpu", "cpu", "fp16")
        runtime.get_supported_compute_types = lambda *args: {"int8"}
        with self.assertRaisesRegex(RuntimeError, "does not support float32"):
            hardware.execute("ctranslate2_cpu", "cpu", "fp32")
        runtime.get_supported_compute_types = lambda *args: {"float32", "float16"}
        with tempfile.TemporaryDirectory() as directory:
            for changes in ({"device": "cpu"}, {"device": "cuda", "device_index": [1]},
                            {"device": "cuda", "device_index": [0], "compute_type": "float32"}):
                model.__dict__.update(changes)
                with self.subTest(changes=changes), self.assertRaisesRegex(RuntimeError, "differs"):
                    hardware.execute("ctranslate2_cuda", "cuda:0", "fp16", model_path=directory, tokens=["x"])
            model.device, model.device_index, model.compute_type = "cuda", [0], "float16"
            self.assertTrue(hardware.execute("ctranslate2_cuda", "cuda:0", "fp16",
                                            model_path=directory, tokens=["x"])["verified"])
            model.translate_batch.side_effect = RuntimeError("CT2 inference failed")
            with self.assertRaisesRegex(RuntimeError, "CT2 inference failed"):
                hardware.execute("ctranslate2_cuda", "cuda:0", "fp16", model_path=directory, tokens=["x"])


class HostRuntimeTests(unittest.TestCase):
    def setUp(self):
        self.host = PluginHost(ROOT / "plugins" / "builtin")
        self.plugin = SimpleNamespace(
            runtime_capabilities=lambda: [{"backend": "remote_api", "devices": ["remote"], "precisions": ["fp32"]}],
            initialize_runtime=Mock(return_value={"model_ready": True}),
            runtime_status=Mock(return_value=dict(REMOTE)), unload=Mock(),
        )
        self.enterContext(patch.object(self.host, "_load", return_value=self.plugin))

    def initialize(self, runtime=REMOTE):
        return self.host.dispatch("initialize", {"plugin_id": "mock_tts", "runtime": runtime})

    def test_opt_in_acknowledgement_precedes_active_readiness(self):
        self.assertEqual(self.initialize()["runtime"], REMOTE)
        self.plugin.initialize_runtime.assert_called_once_with({}, REMOTE)
        self.assertEqual(self.host.dispatch("status", {})["services"]["tts"]["runtime"], REMOTE)
        self.host.dispatch("unload", {"kind": "tts"})
        self.assertEqual(self.host.dispatch("status", {})["services"], {})

    def test_legacy_plugin_cannot_get_runtime_readiness_from_host_echo(self):
        del self.plugin.runtime_status
        with self.assertRaisesRegex(ValueError, "explicit runtime"):
            self.initialize()
        self.plugin.initialize_runtime.assert_not_called()
        self.assertEqual(self.host.dispatch("status", {})["services"], {})

    def test_undeclared_backend_device_or_precision_rejected_before_load(self):
        for entry in ({"backend": "torch_cpu", "devices": ["remote"], "precisions": ["fp32"]},
                      {"backend": "remote_api", "devices": ["cpu"], "precisions": ["fp32"]},
                      {"backend": "remote_api", "devices": ["remote"], "precisions": ["fp16"]}):
            self.plugin.runtime_capabilities = lambda: [entry]
            with self.subTest(entry=entry), self.assertRaisesRegex(ValueError, "did not declare"):
                self.initialize()
        self.plugin.initialize_runtime.assert_not_called()

    def test_failed_ack_or_not_ready_rolls_back_and_cannot_infer(self):
        for observed in (None, {**REMOTE, "device": "cpu"}, {**REMOTE, "precision": "fp16"},
                         {**REMOTE, "backend": "torch_cpu"}):
            self.plugin.runtime_status.return_value = observed
            with self.subTest(observed=observed), self.assertRaisesRegex(ValueError, "actual runtime"):
                self.initialize()
            self.assertEqual(self.host.dispatch("status", {})["services"], {})
        self.plugin.runtime_status.return_value = dict(REMOTE)
        self.plugin.initialize_runtime.return_value = {"model_ready": False}
        with self.assertRaisesRegex(ValueError, "readiness"):
            self.initialize()
        self.assertEqual(self.plugin.unload.call_count, 5)
        with self.assertRaisesRegex(ValueError, "not initialized"):
            self.host.dispatch("infer", {"kind": "tts", "input": {}})

    def test_initialize_cannot_mutate_requested_profile_to_its_own_device(self):
        def initialize(settings, runtime):
            runtime["device"] = "cpu"
            return {"model_ready": True}
        self.plugin.initialize_runtime.side_effect = initialize
        self.plugin.runtime_status.return_value = {**REMOTE, "device": "cpu"}
        with self.assertRaisesRegex(ValueError, "actual runtime"):
            self.initialize(dict(REMOTE))

    def test_actual_plugin_tensor_confirms_runtime_instead_of_settings_echo(self):
        torch = torch_module()
        plugin = self.plugin
        plugin.runtime_capabilities = lambda: [{"backend": "torch_cpu", "devices": ["cpu"], "precisions": ["fp32"]}]
        def initialize(settings, runtime):
            plugin.tensor = torch.tensor([1, 2], device=runtime["device"], dtype=torch.float32)
            return {"model_ready": True}
        plugin.initialize_runtime = initialize
        plugin.runtime_status = lambda: {"backend": "torch_cpu", "device": str(plugin.tensor.device),
                                         "precision": "fp32" if plugin.tensor.dtype == torch.float32 else "fp16"}
        with patch.object(hardware, "verify_runtime", side_effect=lambda value: dict(value)):
            self.assertEqual(self.initialize(CPU)["runtime"], CPU)
            self.host.dispatch("unload", {"kind": "tts"})
            torch.tensor.side_effect = lambda values, **kwargs: Tensor(values, device="cuda:0")
            with self.assertRaisesRegex(ValueError, "actual runtime"):
                self.initialize(CPU)

    def test_no_heavy_dependencies_host_process_legacy_mock_and_runtime_rejection(self):
        requests = [
            {"id": "inventory", "op": "hardware"},
            {"id": "explicit", "op": "initialize", "args": {"plugin_id": "mock_tts", "runtime": REMOTE}},
            {"id": "before", "op": "status"},
            {"id": "legacy", "op": "initialize", "args": {"plugin_id": "mock_tts"}},
            {"id": "infer", "op": "infer", "args": {"kind": "tts", "input": {"text": "test"}}},
            {"id": "status", "op": "status"},
        ]
        process = subprocess.run(
            [sys.executable, "-S", "-m", "neuromita_ai_sdk.host", "--plugins-dir", str(ROOT / "plugins" / "builtin")],
            input="".join(json.dumps(request) + "\n" for request in requests),
            capture_output=True, text=True, timeout=15,
            env={**os.environ, "PYTHONPATH": str(ROOT)},
        )
        self.assertEqual(process.returncode, 0, process.stderr)
        hello, inventory, explicit, before, legacy, inference, status = map(json.loads, process.stdout.splitlines())
        self.assertEqual(hello, {"type": "hello", "api_version": 1})
        self.assertEqual(inventory["result"]["devices"], [])
        self.assertFalse(explicit["ok"])
        self.assertEqual(before["result"]["services"], {})
        self.assertTrue(legacy["ok"])
        self.assertEqual(inference["result"]["format"], "wav")
        self.assertIsNone(status["result"]["services"]["tts"]["runtime"])


class BuiltinProductionRuntimeTests(unittest.TestCase):
    def setUp(self):
        self.host = PluginHost(ROOT / "plugins" / "builtin")
        self.directory = self.enterContext(tempfile.TemporaryDirectory())
        self.enterContext(patch.dict(os.environ, {
            "NEUROMITA_AI_MODELS_DIR": self.directory,
            "NEUROMITA_AI_MODEL_REVISIONS": json.dumps({
                name: "selected-revision" for name in ("gigaam", "gigaam_onnx", "omnivoice")
            }),
        }))

    def gigaam_modules(self, cuda=False):
        class Device(str):
            @property
            def type(self):
                return self.split(":")[0]

        def parameter(device="cpu", dtype="float32", floating=True):
            return SimpleNamespace(device=Device(device), dtype=dtype, is_floating_point=lambda: floating)

        torch = torch_module(cuda=cuda)
        torch.cuda.device = Mock(side_effect=lambda device: contextlib.nullcontext())
        parameters = [parameter(), parameter()]
        buffers = [parameter(), parameter(dtype="int64", floating=False)]
        model = SimpleNamespace(
            parameters=lambda: iter(parameters), buffers=lambda: iter(buffers),
            forward=Mock(), decoding=SimpleNamespace(decode=Mock()), head=object(), eval=Mock(),
        )
        def move(*, device, dtype):
            for tensor in parameters + buffers:
                tensor.device = Device(device)
                if tensor.is_floating_point():
                    tensor.dtype = dtype
            return model
        model.to = Mock(side_effect=move)
        loader = Mock(return_value=model)
        self.enterContext(patch.dict(sys.modules, {"torch": torch, "gigaam": SimpleNamespace(load_model=loader)}))
        probe = self.enterContext(patch("neuromita_asr.gigaam.probe", return_value={"installed": True, "missing": []}))
        return torch, model, parameters, buffers, loader, probe

    def test_production_gigaam_loads_selected_assets_and_confirms_actual_cpu_model(self):
        torch, model, parameters, _, loader, probe = self.gigaam_modules()
        settings = {"model": "v2_ctc", "language": "ru"}
        result = self.host.dispatch("initialize", {"plugin_id": "gigaam", "settings": settings, "runtime": CPU})
        self.assertEqual(result["runtime"], CPU)
        self.assertTrue(result["model_ready"])
        loader.assert_called_once_with("v2_ctc", device="cpu", use_flash=False,
                                       download_root=str(Path(self.directory) / "gigaam" / "selected-revision"))
        probe.assert_called_once_with("gigaam", {"model": "v2_ctc", "language": "ru", "device": "cpu"})
        model.to.assert_called_once_with(device="cpu", dtype=torch.float32)
        self.assertEqual(settings, {"model": "v2_ctc", "language": "ru"})
        plugin = self.host.active_by_kind["asr"]
        plugin.options["device"] = "cuda"
        self.assertEqual(plugin.runtime_status(), CPU)
        parameters[0].dtype = "float16"
        with self.assertRaisesRegex(RuntimeError, "fp32"):
            plugin.runtime_status()

    def test_production_gigaam_cuda_context_and_status_use_actual_model(self):
        torch, _, _, _, loader, _ = self.gigaam_modules(cuda=True)
        runtime = {"backend": "torch_cuda", "device": "cuda:0", "precision": "fp32"}
        result = self.host.dispatch("initialize", {"plugin_id": "gigaam", "runtime": runtime,
                                                   "settings": {"device": "cuda", "model": "v2_ctc"}})
        self.assertEqual(result["runtime"], runtime)
        torch.cuda.device.assert_called_once_with("cuda:0")
        self.assertEqual(loader.call_args.kwargs["device"], "cuda")
        self.assertEqual(self.host.active_by_kind["asr"].runtime_status(), runtime)

    def test_production_gigaam_ignoring_selected_device_never_becomes_ready(self):
        _, model, _, _, _, _ = self.gigaam_modules(cuda=True)
        model.to.side_effect = None
        model.to.return_value = model
        runtime = {"backend": "torch_cuda", "device": "cuda:0", "precision": "fp32"}
        with self.assertRaisesRegex(RuntimeError, "does not match"):
            self.host.dispatch("initialize", {"plugin_id": "gigaam", "runtime": runtime})
        self.assertEqual(self.host.dispatch("status", {})["services"], {})
        with self.assertRaisesRegex(ValueError, "not initialized"):
            self.host.dispatch("infer", {"kind": "asr", "input": {}})

    def test_production_gigaam_placement_failure_cleans_model_and_allows_retry(self):
        _, model, _, _, _, _ = self.gigaam_modules()
        model.to.side_effect = RuntimeError("placement failed")
        plugin = self.host._load("gigaam")
        with self.assertRaisesRegex(RuntimeError, "placement failed"):
            plugin.initialize_runtime({}, CPU)
        self.assertIsNone(plugin.engine)
        self.assertIsNone(plugin.options)
        self.assertIsNone(plugin.torch)

    def test_production_gigaam_rejects_conflicting_settings_half_and_mixed_buffers(self):
        _, model, _, buffers, loader, _ = self.gigaam_modules(cuda=True)
        plugin = self.host._load("gigaam")
        for settings, runtime in (({"device": "cuda"}, CPU), ({"unknown": True}, CPU),
                                  ({}, {**CPU, "precision": "fp16"}),
                                  ({}, {**CPU, "backend": "onnx_cpu"})):
            with self.subTest(settings=settings, runtime=runtime), self.assertRaises(ValueError):
                plugin.initialize_runtime(settings, runtime)
        loader.assert_not_called()
        plugin.initialize_runtime({}, CPU)
        buffers[0].device = type(buffers[0].device)("cuda:0")
        with self.assertRaisesRegex(RuntimeError, "split across"):
            plugin.runtime_status()
        plugin.unload()
        with self.assertRaisesRegex(RuntimeError, "not initialized"):
            plugin.runtime_status()

    def test_production_gigaam_missing_assets_and_invalid_parameters_fail_before_readiness(self):
        _, model, _, _, loader, probe = self.gigaam_modules()
        probe.return_value = {"installed": False, "missing": ["asset:v2_ctc.ckpt"]}
        with self.assertRaisesRegex(RuntimeError, "assets missing"):
            self.host.dispatch("initialize", {"plugin_id": "gigaam", "runtime": CPU})
        loader.assert_not_called()
        probe.return_value = {"installed": True, "missing": []}
        model.parameters = lambda: iter(())
        with self.assertRaisesRegex(RuntimeError, "no parameters"):
            self.host.dispatch("initialize", {"plugin_id": "gigaam", "runtime": CPU})
        self.assertEqual(self.host.dispatch("status", {})["services"], {})

    def test_production_gigaam_legacy_initialize_retains_base_model_loading(self):
        _, model, _, _, loader, _ = self.gigaam_modules()
        result = self.host.dispatch("initialize", {"plugin_id": "gigaam", "settings": {"model": "v2_ctc"}})
        self.assertTrue(result["model_ready"])
        self.assertNotIn("runtime", result)
        loader.assert_called_once()
        model.to.assert_not_called()

    def test_gigaam_real_forward_and_infer_disable_nested_autocast_only_for_explicit_fp32(self):
        torch, model, parameters, _, _, _ = self.gigaam_modules(cuda=True)
        state = {"enabled": True}
        observations = []
        @contextlib.contextmanager
        def autocast(device_type, dtype=None, enabled=True):
            before = state["enabled"]
            state["enabled"] = enabled
            try:
                yield
            finally:
                state["enabled"] = before
        torch.autocast = Mock(side_effect=autocast)
        torch.device = object
        torch.dtype = object
        torch.Tensor = object
        torch.nn = SimpleNamespace(Module=object)
        @contextlib.contextmanager
        def inference_mode():
            yield
        torch.inference_mode = inference_mode
        aliases = {
            "_gigaam_precision": SimpleNamespace(__path__=[]),
            "_gigaam_precision.preprocess": SimpleNamespace(SAMPLE_RATE=16000, load_audio=Mock()),
            "_gigaam_precision.utils": SimpleNamespace(onnx_converter=Mock()),
            "hydra": SimpleNamespace(), "omegaconf": SimpleNamespace(DictConfig=object),
        }
        with patch.dict(sys.modules, aliases):
            spec = importlib.util.spec_from_file_location("_gigaam_precision.model", ROOT / "gigaam/model.py")
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
        def process(features, lengths):
            observations.append(("preprocess", state["enabled"]))
            return features, lengths
        def encode(features, lengths):
            observations.append(("encoder", state["enabled"]))
            return features, lengths
        actual = module.GigaAM.__new__(module.GigaAM)
        actual.parameters = model.parameters
        actual.preprocessor, actual.encoder = process, encode
        parameters[0].device = type(parameters[0].device)("cuda:0")
        actual.forward("features", "lengths")
        self.assertEqual(observations, [("preprocess", True), ("encoder", True)])
        torch.autocast.assert_called_with(device_type="cuda", dtype="float16")
        observations.clear()
        actual.inference_precision = "fp32"
        actual.forward("features", "lengths")
        self.assertEqual(observations, [("preprocess", False), ("encoder", False)])
        self.assertTrue(state["enabled"])

        plugin = self.host._load("gigaam")
        plugin.initialize_runtime({}, {"backend": "torch_cuda", "device": "cuda:0", "precision": "fp32"})
        actual.inference_precision = model.inference_precision
        model.forward = actual.forward
        class Audio:
            def to(self, **kwargs): return self
            def numel(self): return 128
            def unsqueeze(self, axis): return self
        torch.from_numpy = lambda audio: Audio()
        torch.long = "int64"
        torch.tensor.side_effect = lambda *args, **kwargs: "lengths"
        def decode(head, encoded, lengths):
            observations.append(("decode", state["enabled"]))
            return ["recognized"]
        model.decoding.decode = decode
        observations.clear()
        with patch.dict(sys.modules, {"torchaudio": SimpleNamespace()}), \
                patch("neuromita_asr.gigaam._decode_pcm16_wav", return_value=(b"pcm", 16000)), \
                patch("neuromita_asr.gigaam._samples", return_value="samples"):
            self.assertEqual(plugin.infer({})["text"], "recognized")
        self.assertEqual(observations, [("preprocess", False), ("encoder", False), ("decode", False)])
        self.assertTrue(state["enabled"])
        model.inference_precision = None
        with self.assertRaisesRegex(RuntimeError, "execution mode"):
            plugin.runtime_status()
        with self.assertRaisesRegex(RuntimeError, "execution mode"):
            plugin.infer({})

    def test_production_google_remote_initialization_never_calls_network(self):
        class Recognizer:
            recognize_google = Mock(return_value="recognized")
        speech = SimpleNamespace(Recognizer=Recognizer, AudioData=Mock(), UnknownValueError=type("UnknownValueError", (Exception,), {}))
        with patch.dict(sys.modules, {"speech_recognition": speech}), \
                patch("neuromita_asr.runtime.probe", return_value={"installed": True, "missing": []}), \
                patch.object(hardware, "probe", side_effect=AssertionError("remote hardware probe")):
            result = self.host.dispatch("initialize", {"plugin_id": "google", "runtime": REMOTE,
                                                       "settings": {"language": "en-US"}})
            self.assertEqual(result["runtime"], REMOTE)
            self.assertTrue(result["model_ready"])
            Recognizer.recognize_google.assert_not_called()
            plugin = self.host.active_by_kind["asr"]
            self.assertEqual(plugin.options["language"], "en-US")
            self.assertEqual(plugin.runtime_status(), REMOTE)
            plugin.engine = object()
            with self.assertRaisesRegex(RuntimeError, "adapter is not loaded"):
                plugin.runtime_status()

    def test_production_google_missing_adapter_never_becomes_ready(self):
        with patch.dict(sys.modules, {"speech_recognition": SimpleNamespace()}), \
                patch("neuromita_asr.runtime.probe", return_value={"installed": True, "missing": []}):
            with self.assertRaisesRegex(RuntimeError, "adapter is not loaded"):
                self.host.dispatch("initialize", {"plugin_id": "google", "runtime": REMOTE})
        self.assertEqual(self.host.dispatch("status", {})["services"], {})

    def omnivoice_modules(self, cuda=False):
        torch, model, parameters, buffers, _, _ = self.gigaam_modules(cuda=cuda)
        parameters.append(SimpleNamespace(**parameters[0].__dict__))
        model.audio_tokenizer = SimpleNamespace(parameters=lambda: iter([parameters[-1]]), buffers=lambda: iter(()))
        model.text_tokenizer = object()
        model.generate = Mock()
        model.create_voice_clone_prompt = Mock()
        loader = Mock(return_value=model)
        self.enterContext(patch.dict(sys.modules, {"omnivoice": SimpleNamespace(OmniVoice=SimpleNamespace(from_pretrained=loader))}))
        self.enterContext(patch("neuromita_voice.runtime.VoicePlugin.probe_install_for_settings", return_value={"installed": True}))
        root = Path(self.directory) / "omnivoice" / "selected-revision"
        (root / "audio_tokenizer").mkdir(parents=True)
        return torch, model, parameters, buffers, loader, root

    def test_production_omnivoice_cpu_uses_local_model_and_confirms_tokenizer_tensors(self):
        torch, model, parameters, _, loader, root = self.omnivoice_modules()
        result = self.host.dispatch("initialize", {"plugin_id": "omnivoice", "runtime": CPU,
                                                   "settings": {"speed": 1.2}})
        self.assertEqual(result["runtime"], CPU)
        loader.assert_called_once_with(str(root), device_map="cpu", dtype=torch.float32,
                                       local_files_only=True, load_asr=False)
        plugin = self.host.active_by_kind["tts"]
        self.assertEqual(plugin.options["speed"], 1.2)
        self.assertEqual(plugin.runtime_status(), CPU)
        plugin.options["device"] = "cuda:0"
        self.assertEqual(plugin.runtime_status(), CPU)
        parameters[-1].dtype = "float16"
        with self.assertRaisesRegex(RuntimeError, "fp32"):
            plugin.runtime_status()
        model.generate.assert_not_called()

    def test_production_omnivoice_cuda_and_ignored_device_fail_closed(self):
        torch, model, _, _, loader, _ = self.omnivoice_modules(cuda=True)
        runtime = {"backend": "torch_cuda", "device": "cuda:0", "precision": "fp32"}
        result = self.host.dispatch("initialize", {"plugin_id": "omnivoice", "runtime": runtime})
        self.assertEqual(result["runtime"], runtime)
        self.assertEqual(loader.call_args.kwargs["device_map"], "cuda:0")
        self.assertEqual(loader.call_args.kwargs["dtype"], torch.float32)
        self.host.dispatch("unload", {"kind": "tts"})
        model.to(device="cpu", dtype=torch.float32)
        model.to.side_effect = None
        model.to.return_value = model
        with self.assertRaisesRegex(RuntimeError, "differs from selection"):
            self.host.dispatch("initialize", {"plugin_id": "omnivoice", "runtime": runtime})
        self.assertEqual(self.host.dispatch("status", {})["services"], {})

    def test_production_omnivoice_missing_tokenizer_conflict_and_half_rejected(self):
        _, model, _, _, loader, _ = self.omnivoice_modules()
        plugin = self.host._load("omnivoice")
        for settings, runtime in (({"device": "cuda:0"}, CPU), ({}, {**CPU, "precision": "fp16"})):
            with self.subTest(settings=settings, runtime=runtime), self.assertRaises(ValueError):
                plugin.initialize_runtime(settings, runtime)
        loader.assert_not_called()
        model.audio_tokenizer = None
        with self.assertRaisesRegex(RuntimeError, "tokenizers are not loaded"):
            plugin.initialize_runtime({}, CPU)
        self.assertFalse(plugin.initialized)
        self.assertIsNone(plugin.engine)

    def test_production_omnivoice_cpu_cuda_initialize_to_wav_with_controlled_model(self):
        import base64
        import wave

        _, model, _, _, _, _ = self.omnivoice_modules(cuda=True)
        model.sampling_rate = 24000
        model.generate.return_value = [[0.0] * 128]
        reference = Path(self.directory) / "reference.wav"
        reference.write_bytes(b"controlled-reference")
        def write(path, data, rate, subtype):
            self.assertEqual(subtype, "PCM_16")
            with wave.open(str(path), "wb") as output:
                output.setnchannels(1)
                output.setsampwidth(2)
                output.setframerate(rate)
                output.writeframes(bytes(len(data) * 2))
        with patch.dict(sys.modules, {"soundfile": SimpleNamespace(write=write)}):
            for runtime in (CPU, {"backend": "torch_cuda", "device": "cuda:0", "precision": "fp32"}):
                with self.subTest(runtime=runtime):
                    self.host.dispatch("initialize", {"plugin_id": "omnivoice", "runtime": runtime})
                    result = self.host.dispatch("infer", {"kind": "tts", "input": {
                        "text": "test", "reference_audio": str(reference), "reference_text": "reference",
                    }})
                    self.assertEqual(result["format"], "wav")
                    self.assertTrue(base64.b64decode(result["audio_base64"]).startswith(b"RIFF"))
                    self.assertEqual(self.host.active_by_kind["tts"].runtime_status(), runtime)
                    self.host.dispatch("unload", {"kind": "tts"})
        self.assertEqual(model.generate.call_count, 2)

    def test_composite_silero_runtime_is_explicitly_unsupported(self):
        profiles = {
            "silero_rvc_cuda": {"backend": "torch_cuda", "device": "cuda:0", "precision": "fp32"},
            "silero_rvc_onnx": {"backend": "onnx_cpu", "device": "cpu", "precision": "fp32"},
        }
        with patch.object(hardware, "verify_runtime", side_effect=lambda value: dict(value)):
            for name, runtime in profiles.items():
                with self.subTest(plugin=name), self.assertRaisesRegex(ValueError, "explicit runtime selection"):
                    self.host.dispatch("initialize", {"plugin_id": name, "runtime": runtime})
        self.assertEqual(self.host.dispatch("status", {})["services"], {})

    def onnx_builtin_modules(self):
        values = [SimpleNamespace(type="tensor(float)")]
        session = SimpleNamespace(get_providers=lambda: ["CPUExecutionProvider"], disable_fallback=Mock(),
                                  get_inputs=lambda: values, get_outputs=lambda: values)
        sessions = [session]
        cfg = SimpleNamespace(preprocessor=object(), decoding=SimpleNamespace())
        loader = Mock(return_value=(sessions, cfg))
        hydra = SimpleNamespace(utils=SimpleNamespace(instantiate=lambda cfg: SimpleNamespace(tokenizer=object())))
        runtime = SimpleNamespace(InferenceSession=Mock(), SessionOptions=Mock(),
                                  get_available_providers=lambda: ["CPUExecutionProvider"])
        graph = SimpleNamespace(initializer=[SimpleNamespace(data_type=1)], sparse_initializer=[],
                                input=[], output=[], value_info=[], node=[])
        onnx = SimpleNamespace(load=Mock(return_value=SimpleNamespace(graph=graph, functions=[])))
        self.enterContext(patch.dict(sys.modules, {
            "gigaam": SimpleNamespace(), "gigaam.onnx_utils": SimpleNamespace(load_onnx=loader),
            "hydra": hydra, "onnxruntime": runtime, "onnx": onnx,
        }))
        self.enterContext(patch("neuromita_asr.gigaam.probe", return_value={"installed": True}))
        root = Path(self.directory) / "gigaam_onnx" / "selected-revision"
        root.mkdir(parents=True)
        (root / "v2_ctc.onnx").write_bytes(b"controlled-model-fixture")
        return session, sessions, graph, onnx, loader, root

    def test_production_gigaam_onnx_cpu_confirms_sessions_and_artifact_precision(self):
        session, _, graph, onnx, loader, root = self.onnx_builtin_modules()
        runtime = {"backend": "onnx_cpu", "device": "cpu", "precision": "fp32"}
        result = self.host.dispatch("initialize", {"plugin_id": "gigaam_onnx", "runtime": runtime,
                                                   "settings": {"model": "v2_ctc"}})
        self.assertEqual(result["runtime"], runtime)
        loader.assert_called_once_with(onnx_dir=str(root), model_version="v2_ctc", providers=["CPUExecutionProvider"])
        onnx.load.assert_called_with(str(root / "v2_ctc.onnx"), load_external_data=False)
        self.assertTrue(session.disable_fallback.called)
        plugin = self.host.active_by_kind["asr"]
        graph.initializer[0].data_type = 10
        with self.assertRaisesRegex(RuntimeError, "precision"):
            plugin.runtime_status()

    def test_production_gigaam_onnx_rejects_actual_provider_and_half_cast(self):
        session, _, graph, _, _, _ = self.onnx_builtin_modules()
        runtime = {"backend": "onnx_cpu", "device": "cpu", "precision": "fp32"}
        session.get_providers = lambda: ["CUDAExecutionProvider", "CPUExecutionProvider"]
        with self.assertRaisesRegex(RuntimeError, "exclusively CPU"):
            self.host.dispatch("initialize", {"plugin_id": "gigaam_onnx", "runtime": runtime,
                                               "settings": {"model": "v2_ctc"}})
        session.get_providers = lambda: ["CPUExecutionProvider"]
        graph.node = [SimpleNamespace(domain="", attribute=[SimpleNamespace(type=2, name="to", i=10)])]
        with self.assertRaisesRegex(RuntimeError, "precision"):
            self.host.dispatch("initialize", {"plugin_id": "gigaam_onnx", "runtime": runtime,
                                               "settings": {"model": "v2_ctc"}})
        self.assertEqual(self.host.dispatch("status", {})["services"], {})

    def test_production_gigaam_onnx_gpu_and_incomplete_sessions_are_not_ready(self):
        _, sessions, _, _, loader, _ = self.onnx_builtin_modules()
        plugin = self.host._load("gigaam_onnx")
        with self.assertRaisesRegex(ValueError, "CPU fp32 only"):
            plugin.initialize_runtime({}, {"backend": "onnx_directml", "device": "dml:0", "precision": "fp32"})
        loader.assert_not_called()
        sessions.clear()
        with self.assertRaisesRegex(RuntimeError, "incomplete"):
            plugin.initialize_runtime({"model": "v2_ctc"}, {"backend": "onnx_cpu", "device": "cpu", "precision": "fp32"})
        self.assertIsNone(plugin.engine)


if __name__ == "__main__":
    unittest.main()
