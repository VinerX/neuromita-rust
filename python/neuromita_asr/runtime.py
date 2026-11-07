"""Standalone ASR engine adapters for the versioned NeuroMita Python worker."""

from __future__ import annotations

import sys

import base64
import io
import json
from pathlib import Path
import re
import wave

from neuromita_ai_sdk import AssetHook, AssetRequirement, InstallPlan, validate_settings
from neuromita_ai_sdk.assets import selected_model_root
from neuromita_ai_sdk.distributions import check_distributions


WHISPER_MODELS = ("large-v3-turbo", "large-v3")
GIGAAM_MODELS = ("v2_rnnt", "v2_ctc", "v3_rnnt", "v3_ctc", "v3_e2e_ctc", "v3_e2e_rnnt")
WHISPER_FILES = (
    "config.json", "preprocessor_config.json", "tokenizer.json",
    "vocabulary.json", "model.bin",
)
HALLUCINATIONS = (
    "Субтитры сделал DimaTorzok", "Субтитры делал DimaTorzok",
    "Субтитры создавал DimaTorzok", "Субтитры сделаны DimaTorzok",
    "Продолжение следует", "Спасибо за просмотр", "Спасибо за внимание",
    "Подписывайтесь на канал", "Ставьте лайки",
    "Редактор субтитров А.Синецкая Корректор А.Егорова",
    "Субтитры А.Синецкая", "Субтитры и корректура Оксаны Каменской",
    "続きは次回", "Thanks for watching", "Субтитры сделаны сообществом Amara.org",
    "www.amara.org",
)


def _normalized(text: str) -> str:
    return " ".join(re.sub(r"[^0-9a-zа-яё]+", " ", text.lower()).split())


_FILTER = frozenset(map(_normalized, HALLUCINATIONS))


def _language_schema(plugin_id: str) -> dict:
    manifest = json.loads(Path(__file__).with_name("languages.json").read_text(encoding="utf-8"))
    details = manifest[plugin_id]
    return {"type": "string", "default": details["default"], "choices": [entry["id"] for entry in details["choices"]]}


def schema(plugin_id: str) -> dict:
    if plugin_id == "google":
        return {"language": _language_schema("google")}
    if plugin_id == "whisper":
        return {
            "language": _language_schema(plugin_id),
            "device": {"type": "string", "default": "cpu", "choices": ["cpu", "cuda"]},
            "model": {"type": "string", "default": "large-v3-turbo", "choices": list(WHISPER_MODELS)},
            "compute_type": {"type": "string", "default": "auto", "choices": ["auto", "int8", "float16", "float32", "int8_float16"]},
            "beam_size": {"type": "integer", "default": 5, "choices": [1, 2, 3, 5, 8]},
        }
    if plugin_id in ("gigaam", "gigaam_onnx"):
        return {
            "language": _language_schema(plugin_id),
            "model": {"type": "string", "default": "v3_e2e_rnnt" if plugin_id == "gigaam" else "v2_rnnt", "choices": list(GIGAAM_MODELS)},
            "device": {"type": "string", "default": "cpu", "choices": ["cpu", "cuda"] if plugin_id == "gigaam" else ["cpu", "dml"]},
        }
    if plugin_id == "whisper_onnx":
        return {
            "language": _language_schema(plugin_id),
            "device": {"type": "string", "default": "cpu", "choices": ["cpu", "dml"]},
            "max_tokens": {"type": "integer", "default": 192, "choices": [96, 128, 160, 192, 224, 256]},
        }
    raise ValueError("unknown ASR engine")


def _gigaam_onnx_files(model: str) -> tuple[str, ...]:
    if "ctc" in model and "rnnt" not in model:
        required = (f"{model}.yaml", f"{model}.onnx")
    else:
        required = (f"{model}.yaml", *(f"{model}_{part}.onnx" for part in ("encoder", "decoder", "joint")))
    if "e2e" in model:
        required += (f"{model}_tokenizer.model",)
    return required


WHISPER_ONNX_FILES = (
    "config.json", "tokenizer.json", "tokenizer_config.json",
    "special_tokens_map.json", "preprocessor_config.json", "generation_config.json",
    "onnx/encoder_model.onnx", "onnx/encoder_model.onnx_data",
    "onnx/decoder_model.onnx", "onnx/decoder_with_past_model.onnx",
)


def install_plan(plugin_id: str, settings: dict | None = None) -> InstallPlan:
    options = validate_settings(schema(plugin_id), settings or {})
    if plugin_id == "google":
        return InstallPlan(
            requirements=("SpeechRecognition>=3.10,<4",),
            supported_backends=("remote-api",),
            supported_python_minors=("3.11", "3.12"),
        )
    if plugin_id == "gigaam":
        model = options["model"]
        assets = [AssetRequirement("gigaam_checkpoint", f"{model}.ckpt")]
        if "e2e" in model:
            assets.append(AssetRequirement("gigaam_tokenizer", f"{model}_tokenizer.model"))
        return InstallPlan(
            requirements=("numpy==1.26.0", "torch>=2.3,<3", "torchaudio>=2.3,<3", "omegaconf>=2,<3", "hydra-core>=1.3,<2", "sentencepiece>=0.2,<1", "tqdm>=4.60,<5"),
            assets=tuple(assets),
            supported_backends=(f"torch-{options['device']}",),
            supported_python_minors=("3.11", "3.12"),
        )
    if plugin_id == "gigaam_onnx":
        ort = "onnxruntime-directml>=1.18,<2" if options["device"] == "dml" else "onnxruntime>=1.18,<2"
        return InstallPlan(
            requirements=("numpy==1.26.0", "torch>=2.3,<3", "torchaudio>=2.3,<3", "onnx>=1.16,<2", ort, "omegaconf>=2,<3", "hydra-core>=1.3,<2", "sentencepiece>=0.2,<1", "tqdm>=4.60,<5"),
            assets=tuple(AssetRequirement("gigaam_onnx", name) for name in _gigaam_onnx_files(options["model"])),
            asset_hooks=(AssetHook("export_gigaam_onnx", "gigaam"),),
            required_backends=("torch-cpu",),
            supported_backends=("onnx-directml" if options["device"] == "dml" else "onnx-cpu",),
            supported_python_minors=("3.11", "3.12"),
        )
    if plugin_id == "whisper_onnx":
        ort = "onnxruntime-directml>=1.18,<2" if options["device"] == "dml" else "onnxruntime>=1.18,<2"
        return InstallPlan(
            requirements=("numpy==1.26.0", "scipy>=1.12,<2", "torch>=2.3,<3", "transformers>=4.40,<5", "optimum>=1.20,<3", ort),
            assets=tuple(AssetRequirement("whisper_onnx", name) for name in WHISPER_ONNX_FILES),
            required_backends=("torch-cpu",),
            supported_backends=("onnx-directml" if options["device"] == "dml" else "onnx-cpu",),
            supported_python_minors=("3.11", "3.12"),
        )
    selected = options["model"]
    assets = tuple(AssetRequirement("whisper_ct2", f"{selected}/{name}") for name in WHISPER_FILES)
    return InstallPlan(
        requirements=("numpy==1.26.0", "faster-whisper>=1.1,<2", "ctranslate2>=4.4,<5"),
        assets=assets,
        supported_backends=(f"ctranslate2-{options['device']}",),
        supported_python_minors=("3.11", "3.12"),
    )


def probe(plugin_id: str, settings: dict | None = None) -> dict:
    options = validate_settings(schema(plugin_id), settings or {})
    distributions = {
        "google": ("SpeechRecognition",),
        "whisper": ("numpy", "faster-whisper", "ctranslate2"),
        "gigaam": ("numpy", "torch", "torchaudio", "omegaconf", "hydra-core", "sentencepiece", "tqdm"),
        "gigaam_onnx": ("numpy", "torch", "torchaudio", "onnxruntime", "omegaconf", "hydra-core", "sentencepiece", "tqdm"),
        "whisper_onnx": ("numpy", "scipy", "torch", "transformers", "optimum", "onnxruntime"),
    }
    required = list(distributions[plugin_id])
    if plugin_id in ("gigaam_onnx", "whisper_onnx") and options["device"] == "dml":
        required[required.index("onnxruntime")] = "onnxruntime-directml"
    missing = check_distributions(
        required,
        exact_versions={"numpy": "1.26.0"} if plugin_id != "google" else None,
    )
    if plugin_id in ("gigaam_onnx", "whisper_onnx") and options["device"] == "dml" and sys.platform != "win32":
        missing.append("device:DirectML requires Windows")
    if plugin_id != "google":
        try:
            folder = selected_model_root(plugin_id) / options["model"] if plugin_id == "whisper" else selected_model_root(plugin_id)
            files = WHISPER_FILES if plugin_id == "whisper" else [asset.relative_path for asset in install_plan(plugin_id, options).assets]
            for name in files:
                file = folder / name
                if file.is_symlink() or not file.is_file() or file.stat().st_size == 0:
                    missing.append(f"asset:{options['model']}/{name}" if plugin_id == "whisper" else f"asset:{name}")
        except (ValueError, OSError) as error:
            missing.append(f"model:{error}")
    return {"installed": not missing, "missing": missing}


def _decode_pcm16_wav(payload: dict) -> tuple[bytes, int]:
    encoded = payload.get("audio_base64")
    if not isinstance(encoded, str) or len(encoded) > 950_000:
        raise ValueError("bounded base64 WAV audio is required")
    try:
        raw = base64.b64decode(encoded, validate=True)
    except (ValueError, base64.binascii.Error) as error:
        raise ValueError("invalid base64 WAV audio") from error
    if len(raw) > 700_000:
        raise ValueError("WAV audio exceeds request budget")
    try:
        with wave.open(io.BytesIO(raw), "rb") as reader:
            if reader.getnchannels() != 1 or reader.getsampwidth() != 2 or reader.getcomptype() != "NONE":
                raise ValueError("ASR requires mono PCM16 WAV")
            rate = reader.getframerate()
            count = reader.getnframes()
            if not 8_000 <= rate <= 48_000 or count <= 0 or count > 350_000:
                raise ValueError("unsupported ASR sample rate or duration")
            frames = reader.readframes(count)
            if len(frames) != count * 2:
                raise ValueError("truncated PCM16 audio")
            return frames, rate
    except (EOFError, wave.Error) as error:
        raise ValueError("invalid PCM16 WAV file") from error


class AsrPlugin:
    def __init__(self, plugin_id: str):
        schema(plugin_id)
        self.plugin_id = plugin_id
        self.engine = None
        self.options = None

    def describe(self) -> dict:
        return {"id": self.plugin_id, "kind": "asr", "language_modes": ["ru"] if self.plugin_id.startswith("gigaam") else ["ru", "en", "auto"]}

    def settings_schema(self) -> dict:
        return schema(self.plugin_id)

    def install_plan(self) -> InstallPlan:
        return install_plan(self.plugin_id)

    def probe_install(self) -> dict:
        return probe(self.plugin_id)

    def initialize(self, settings: dict) -> dict:
        if self.engine is not None:
            raise ValueError("ASR engine already initialized")
        options = validate_settings(self.settings_schema(), settings)
        status = probe(self.plugin_id, options)
        if not status["installed"]:
            raise RuntimeError(f"ASR dependencies/assets missing: {status['missing']}")
        if self.plugin_id == "google":
            import speech_recognition as speech
            engine = speech
        else:
            from faster_whisper import WhisperModel
            folder = selected_model_root(self.plugin_id) / options["model"]
            engine = WhisperModel(str(folder), device=options["device"], compute_type=options["compute_type"])
        self.engine = engine
        self.options = options
        return {"model_ready": True, "plugin_id": self.plugin_id}

    def infer(self, payload: dict) -> dict:
        if self.engine is None or self.options is None:
            raise RuntimeError("ASR engine is not initialized")
        pcm, rate = _decode_pcm16_wav(payload)
        if self.plugin_id == "google":
            speech = self.engine
            audio = speech.AudioData(pcm, rate, 2)
            try:
                text = speech.Recognizer().recognize_google(audio, language=self.options["language"])
            except speech.UnknownValueError:
                text = ""
        else:
            if rate != 16_000:
                raise ValueError("Whisper accepts 16 kHz PCM segments only")
            import array
            import sys
            import numpy as np
            samples = array.array("h")
            samples.frombytes(pcm)
            if sys.byteorder != "little":
                samples.byteswap()
            wave_samples = np.asarray(samples, dtype=np.float32) / 32768.0
            lang = self.options["language"]
            segments, _ = self.engine.transcribe(
                wave_samples, language=None if lang == "auto" else lang,
                beam_size=self.options["beam_size"], vad_filter=False,
                condition_on_previous_text=False,
            )
            texts = [segment.text.strip() for segment in segments if segment.text.strip()
                     and _normalized(segment.text) not in _FILTER]
            text = " ".join(texts).strip()
            if _normalized(text) in _FILTER:
                text = ""
        if not isinstance(text, str):
            raise ValueError("ASR returned a non-text transcript")
        return {"text": text.strip(), "language": self.options["language"]}

    def unload(self) -> None:
        self.engine = None
        self.options = None
