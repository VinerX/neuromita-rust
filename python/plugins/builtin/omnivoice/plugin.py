from itertools import chain

from neuromita_ai_sdk import validate_settings
from neuromita_ai_sdk.assets import selected_model_root
from neuromita_voice.runtime import VoicePlugin


class Plugin(VoicePlugin):
    def __init__(self):
        super().__init__("omnivoice")

    def runtime_capabilities(self):
        import torch

        result = [{"backend": "torch_cpu", "devices": ["cpu"], "precisions": ["fp32"]}]
        if torch.cuda.is_available():
            result.append({"backend": "torch_cuda", "precisions": ["fp32"],
                           "devices": [f"cuda:{index}" for index in range(torch.cuda.device_count())]})
        return result

    def initialize_runtime(self, settings, runtime):
        if self.initialized or self.engine is not None:
            raise ValueError("OmniVoice is already initialized")
        if not isinstance(runtime, dict) or not any(
            runtime == {"backend": item["backend"], "device": device, "precision": precision}
            for item in self.runtime_capabilities()
            for device in item["devices"] for precision in item["precisions"]
        ):
            raise ValueError("OmniVoice explicit runtime supports Torch CPU/CUDA fp32 only")
        options = validate_settings(self.settings_schema(), settings, include_defaults=False)
        if "device" in options and options["device"] != runtime["device"]:
            raise ValueError("OmniVoice settings device conflicts with selected runtime")
        options = validate_settings(self.settings_schema(), {**options, "device": runtime["device"]})
        root = selected_model_root(self.plugin_id)
        if not (root / "audio_tokenizer").is_dir():
            raise RuntimeError("OmniVoice requires a local audio_tokenizer directory")
        state = self.probe_install_for_settings(options)
        if not state["installed"]:
            raise RuntimeError(f"OmniVoice dependencies/assets missing: {state}")
        import torch
        from omnivoice import OmniVoice

        try:
            self.engine = OmniVoice.from_pretrained(
                str(root), device_map=runtime["device"], dtype=torch.float32,
                local_files_only=True, load_asr=False,
            )
            self.engine = self.engine.to(device=runtime["device"], dtype=torch.float32)
            self.engine.eval()
            self.options = options
            self.initialized = True
            if self.runtime_status() != runtime:
                raise RuntimeError("OmniVoice actual model runtime differs from selection")
            return {"model_ready": True, "plugin_id": self.plugin_id}
        except Exception:
            self.unload()
            raise

    def runtime_status(self):
        import torch

        if not self.initialized or self.engine is None:
            raise RuntimeError("OmniVoice is not initialized")
        if not all(callable(getattr(self.engine, name, None))
                   for name in ("generate", "create_voice_clone_prompt")):
            raise RuntimeError("OmniVoice model does not expose the TTS inference contract")
        if getattr(self.engine, "_asr_pipe", None) is not None:
            raise RuntimeError("OmniVoice explicit runtime does not support an additional ASR pipeline")
        tokenizer = getattr(self.engine, "audio_tokenizer", None)
        if tokenizer is None or getattr(self.engine, "text_tokenizer", None) is None:
            raise RuntimeError("OmniVoice tokenizers are not loaded")
        device = None
        for model in (self.engine, tokenizer):
            parameters = list(model.parameters())
            if not parameters:
                raise RuntimeError("OmniVoice component has no parameters to confirm runtime")
            if device is None:
                device = parameters[0].device
            dtypes = set()
            for tensor in chain(parameters, model.buffers()):
                if tensor.device != device:
                    raise RuntimeError("OmniVoice components are split across devices")
                if tensor.is_floating_point():
                    dtypes.add(tensor.dtype)
            if dtypes != {torch.float32}:
                raise RuntimeError("OmniVoice explicit runtime requires actual fp32 tensors")
        if device.type not in ("cpu", "cuda"):
            raise RuntimeError("OmniVoice model is on an unsupported device")
        return {"backend": f"torch_{device.type}", "device": str(device), "precision": "fp32"}
