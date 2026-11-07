"""GigaAM PyTorch and exported ONNX recognizers hosted by the Rust-owned worker."""

from __future__ import annotations

import array
import sys
import tempfile
import wave
from pathlib import Path

from neuromita_ai_sdk.assets import selected_model_root
from .runtime import AsrPlugin, _decode_pcm16_wav, probe


def _samples(pcm: bytes):
    import numpy as np

    samples = array.array("h")
    samples.frombytes(pcm)
    if sys.byteorder != "little":
        samples.byteswap()
    return np.asarray(samples, dtype=np.float32) / 32768.0


def _onnx_providers(device: str, runtime):
    available = set(runtime.get_available_providers())
    if device == "cpu":
        if "CPUExecutionProvider" not in available:
            raise RuntimeError("ONNX CPU execution provider is missing")
        return ["CPUExecutionProvider"]
    if device == "dml":
        if "DmlExecutionProvider" not in available:
            raise RuntimeError("requested DirectML execution provider is missing")
        return ["DmlExecutionProvider"]
    if "DmlExecutionProvider" in available:
        return ["DmlExecutionProvider"]
    if "CPUExecutionProvider" in available:
        return ["CPUExecutionProvider"]
    raise RuntimeError("no compatible ONNX execution provider found")


class GigaAmPlugin(AsrPlugin):
    def __init__(self, plugin_id: str):
        if plugin_id not in ("gigaam", "gigaam_onnx"):
            raise ValueError("invalid GigaAM plugin")
        super().__init__(plugin_id)
        self.processor = None
        self.tokenizer = None
        self.model_cfg = None
        self.torch = None

    def initialize(self, settings: dict) -> dict:
        if self.engine is not None:
            raise ValueError("GigaAM is already initialized")
        from neuromita_ai_sdk import validate_settings

        options = validate_settings(self.settings_schema(), settings)
        status = probe(self.plugin_id, options)
        if not status["installed"]:
            raise RuntimeError(f"GigaAM runtime or assets missing: {status['missing']}")
        import gigaam
        root = selected_model_root(self.plugin_id)
        model_name = options["model"]
        if self.plugin_id == "gigaam":
            import torch
            device = options["device"]
            if device == "auto":
                device = "cuda" if torch.cuda.is_available() else "cpu"
            if device == "cuda" and not torch.cuda.is_available():
                raise RuntimeError("CUDA was selected but is unavailable")
            model = gigaam.load_model(model_name, device=device, download_root=str(root), use_flash=False)
            if not all(callable(getattr(model, name, None)) for name in ("forward", "parameters")):
                raise RuntimeError("loaded GigaAM checkpoint is not an ASR model")
            if not hasattr(model, "decoding") or not hasattr(model, "head"):
                raise RuntimeError("GigaAM checkpoint does not provide ASR decoding")
            self.torch = torch
            self.engine = model
        else:
            import hydra
            import onnxruntime
            from gigaam.onnx_utils import load_onnx

            providers = _onnx_providers(options["device"], onnxruntime)
            sessions, model_cfg = load_onnx(
                onnx_dir=str(root), model_version=model_name, providers=providers,
            )
            if "e2e" in model_name:
                model_cfg.decoding.model_path = str(root / f"{model_name}_tokenizer.model")
            processor = hydra.utils.instantiate(model_cfg.preprocessor)
            tokenizer = hydra.utils.instantiate(model_cfg.decoding).tokenizer
            self.model_cfg = model_cfg
            self.processor = processor
            self.tokenizer = tokenizer
            self.engine = sessions
        self.options = options
        return {"model_ready": True, "plugin_id": self.plugin_id, "model": model_name}

    def infer(self, payload: dict) -> dict:
        if self.engine is None or self.options is None:
            raise RuntimeError("GigaAM is not initialized")
        pcm, sample_rate = _decode_pcm16_wav(payload)
        audio = _samples(pcm)
        if self.plugin_id == "gigaam":
            import torchaudio
            torch = self.torch
            audio_tensor = torch.from_numpy(audio)
            if sample_rate != 16000:
                audio_tensor = torchaudio.functional.resample(audio_tensor, sample_rate, 16000)
            parameter = next(self.engine.parameters())
            audio_tensor = audio_tensor.to(device=parameter.device, dtype=parameter.dtype)
            length = torch.tensor([audio_tensor.numel()], device=parameter.device, dtype=torch.long)
            with torch.inference_mode():
                encoded, encoded_len = self.engine.forward(audio_tensor.unsqueeze(0), length)
                decoded = self.engine.decoding.decode(self.engine.head, encoded, encoded_len)
            text = decoded[0] if decoded else ""
        else:
            from gigaam.onnx_utils import transcribe_sample
            with tempfile.TemporaryDirectory(prefix="neuromita-gigaam-") as directory:
                path = Path(directory) / "segment.wav"
                with wave.open(str(path), "wb") as output:
                    output.setnchannels(1)
                    output.setsampwidth(2)
                    output.setframerate(sample_rate)
                    output.writeframes(pcm)
                text = transcribe_sample(
                    wav_file=str(path), model_cfg=self.model_cfg, sessions=self.engine,
                    preprocessor=self.processor, tokenizer=self.tokenizer,
                )
        if text is not None and not isinstance(text, str):
            raise ValueError("GigaAM returned a non-text transcript")
        return {"text": (text or "").strip(), "language": self.options["language"]}

    def unload(self):
        self.processor = None
        self.tokenizer = None
        self.model_cfg = None
        self.torch = None
        super().unload()
