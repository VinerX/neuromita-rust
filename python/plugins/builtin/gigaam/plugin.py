from contextlib import nullcontext
from itertools import chain

from neuromita_ai_sdk import validate_settings
from neuromita_asr.gigaam import GigaAmPlugin


class Plugin(GigaAmPlugin):
    def __init__(self):
        super().__init__("gigaam")
        self._explicit_fp32 = False

    def runtime_capabilities(self):
        import torch

        capabilities = [{"backend": "torch_cpu", "devices": ["cpu"], "precisions": ["fp32"]}]
        if torch.cuda.is_available():
            capabilities.append({
                "backend": "torch_cuda",
                "devices": [f"cuda:{index}" for index in range(torch.cuda.device_count())],
                "precisions": ["fp32"],
            })
        return capabilities

    def initialize_runtime(self, settings, runtime):
        if self.engine is not None:
            raise ValueError("GigaAM is already initialized")
        if not isinstance(runtime, dict) or not any(
            runtime == {"backend": item["backend"], "device": device, "precision": precision}
            for item in self.runtime_capabilities()
            for device in item["devices"] for precision in item["precisions"]
        ):
            raise ValueError("GigaAM supports explicit Torch CPU/CUDA fp32 runtimes only")
        options = validate_settings(self.settings_schema(), settings, include_defaults=False)
        device_kind = "cpu" if runtime["backend"] == "torch_cpu" else "cuda"
        if "device" in options and options["device"] != device_kind:
            raise ValueError("GigaAM settings device conflicts with selected runtime")
        import torch

        try:
            device_scope = torch.cuda.device(runtime["device"]) if device_kind == "cuda" else nullcontext()
            with device_scope:
                result = super().initialize({**options, "device": device_kind})
                self.engine = self.engine.to(device=runtime["device"], dtype=torch.float32)
                self.engine.inference_precision = "fp32"
                self.engine.eval()
            self._explicit_fp32 = True
            if self.runtime_status() != runtime:
                raise RuntimeError("GigaAM loaded model does not match selected runtime")
            return result
        except Exception:
            self.unload()
            raise

    def runtime_status(self):
        if self.engine is None or self.options is None or self.torch is None:
            raise RuntimeError("GigaAM is not initialized")
        if getattr(self.engine, "inference_precision", None) != "fp32":
            raise RuntimeError("GigaAM explicit fp32 execution mode is not enabled")
        parameters = list(self.engine.parameters())
        if not parameters:
            raise RuntimeError("GigaAM model has no parameters to confirm runtime")
        device = parameters[0].device
        floating_dtypes = set()
        for tensor in chain(parameters, self.engine.buffers()):
            if tensor.device != device:
                raise RuntimeError("GigaAM model is split across devices")
            if tensor.is_floating_point():
                floating_dtypes.add(tensor.dtype)
        if floating_dtypes != {self.torch.float32}:
            raise RuntimeError("GigaAM explicit runtime requires actual fp32 model tensors")
        if device.type not in ("cpu", "cuda"):
            raise RuntimeError("GigaAM model is on an unsupported device")
        return {"backend": f"torch_{device.type}", "device": str(device), "precision": "fp32"}

    def infer(self, payload):
        if not self._explicit_fp32:
            return super().infer(payload)
        runtime = self.runtime_status()
        with self.torch.autocast(device_type="cuda" if runtime["backend"] == "torch_cuda" else "cpu",
                                 enabled=False):
            return super().infer(payload)

    def unload(self):
        self._explicit_fp32 = False
        super().unload()
