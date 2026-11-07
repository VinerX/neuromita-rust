"""Dependency and asset contracts derived from the original NeuroMita voice models."""

from __future__ import annotations

import importlib.metadata
import os
from pathlib import Path

from neuromita_ai_sdk import AssetRequirement, InstallPlan, InstallerAction, validate_settings

SILERO_TTS_MODELS = {
    "ru": ("v5_ru", "silero/v5_ru.pt", "https://models.silero.ai/models/tts/ru/v5_ru.pt", "7ba04d42340fe0398042eed2e0d12d62e23096d626b1b9feff4dcb1309197ab4"),
    "en": ("v3_en", "silero/v3_en.pt", "https://models.silero.ai/models/tts/en/v3_en.pt", "02b71034d9f13bc4001195017bac9db1c6bb6115e03fea52983e8abcff13b665"),
}

RVC_CUDA_ASSETS = ("hubert_base.pt", "rmvpe.pt")
RVC_ONNX_ASSETS = ("vec-768-layer-12.onnx", "rmvpe.onnx")
FISH_ASSETS = (
    "model.pth", "tokenizer.tiktoken", "config.json",
    "firefly-gan-vq-fsq-8x1024-21hz-generator.pth",
)
OMNI_ASSETS = (
    "config.json", "model.safetensors", "tokenizer.json",
    "tokenizer_config.json", "chat_template.jinja",
    "audio_tokenizer/config.json", "audio_tokenizer/model.safetensors",
    "audio_tokenizer/preprocessor_config.json",
)

VARIANTS = {
    "edge_tts_rvc_cuda": ("edge", "cuda", False, False),
    "edge_tts_rvc_onnx": ("edge", "onnx", False, False),
    "silero_rvc_cuda": ("silero", "cuda", False, False),
    "silero_rvc_onnx": ("silero", "onnx", False, False),
    "f5_tts_ru": ("f5", "torch", False, False),
    "f5_tts_en": ("f5", "torch", True, False),
    "f5_tts_ru_rvc": ("f5", "cuda", False, True),
    "f5_tts_en_rvc": ("f5", "cuda", True, True),
    "fish_speech": ("fish", "torch", False, False),
    "fish_speech_compiled": ("fish", "cuda", False, False),
    "fish_speech_rvc": ("fish", "cuda", False, True),
    "omnivoice": ("omni", "torch", False, False),
}


def family(plugin_id: str) -> str:
    return VARIANTS[plugin_id][0]


def backend(plugin_id: str) -> str:
    return VARIANTS[plugin_id][1]

def supported_backends(plugin_id: str, selected_settings: dict | None = None) -> tuple[str, ...]:
    mode = backend(plugin_id)
    options = validate_settings(settings(plugin_id), selected_settings or {})
    device = options["device"]
    if mode == "onnx":
        if device not in ("cpu", "dml"):
            raise ValueError("ONNX RVC device must be cpu or dml")
        return ("onnx-directml" if device == "dml" else "onnx-cpu",)
    if device != "cpu" and not (device.startswith("cuda:") and device[5:].isdigit()):
        raise ValueError("Torch device must be cpu or cuda:<index>")
    if mode == "cuda" and device == "cpu":
        raise ValueError("CUDA TTS variant cannot use CPU")
    return ("torch-cpu" if device == "cpu" else "torch-cuda",)


def models_root() -> Path:
    value = os.environ.get("NEUROMITA_AI_MODELS_DIR")
    return Path(value).resolve() if value else Path.cwd() / "AIData" / "models"


def asset_root(plugin_id: str) -> Path:
    import json
    value = os.environ.get("NEUROMITA_AI_MODEL_REVISIONS", "{}")
    revisions = json.loads(value)
    if not isinstance(revisions, dict):
        raise ValueError("invalid model revision map")
    revision = revisions.get(plugin_id)
    if revision is not None and (not isinstance(revision, str) or not revision.isascii()
                                 or not revision.replace("-", "").replace("_", "").isalnum()):
        raise ValueError("invalid selected model revision")
    return models_root() / plugin_id / revision if revision else models_root() / plugin_id


def asset_requirements(plugin_id: str, selected_settings: dict | None = None) -> tuple[AssetRequirement, ...]:
    engine, mode, cross, rvc = VARIANTS[plugin_id]
    assets = []
    if engine == "silero":
        options = validate_settings(settings(plugin_id), selected_settings or {})
        _, relative_path, _, checksum = SILERO_TTS_MODELS[options["language"]]
        assets.append(AssetRequirement("silero_tts_model", relative_path, sha256=checksum))
    if engine == "f5":
        prefix = "en/" if cross else "ru/"
        assets += [AssetRequirement("f5_checkpoint", prefix + "model.safetensors"),
                   AssetRequirement("f5_vocab", prefix + "vocab.txt"),
                   AssetRequirement("vocoder_config", "vocos/config.yaml"),
                   AssetRequirement("vocoder_weights", "vocos/pytorch_model.bin")]
        if cross:
            assets.append(AssetRequirement("speaking_rate", "en/speaking_rate.safetensors"))
    elif engine == "fish":
        assets += [AssetRequirement("fish_model", x) for x in FISH_ASSETS]
    elif engine == "omni":
        assets += [AssetRequirement("omnivoice_model", x) for x in OMNI_ASSETS]
    if engine in ("edge", "silero") or rvc:
        ext = "onnx" if mode == "onnx" else "pth"
        assets += [AssetRequirement("rvc_voice_weights", "rvc/voice." + ext),
                   AssetRequirement("rvc_voice_index", "rvc/voice.index", False)]
        assets += [AssetRequirement("rvc_runtime", "rvc-runtime/" + x)
                   for x in (RVC_ONNX_ASSETS if mode == "onnx" else RVC_CUDA_ASSETS)]
    return tuple(assets)


def requirements(plugin_id: str, selected_settings: dict | None = None) -> tuple[str, ...]:
    engine, mode, cross, rvc = VARIANTS[plugin_id]
    options = validate_settings(settings(plugin_id), selected_settings or {})
    req = ["numpy==1.26.0", "scipy==1.12.0", "soundfile"]
    if engine in ("edge", "silero"):
        req += ["omegaconf", "silero"] if engine == "silero" else ["omegaconf", "edge-tts>=6.1.9,<8"]
    elif engine == "f5":
        req += ["f5-tts", "pyarrow<21", "cached_path", "google-api-core", "librosa==0.9.1", "numba==0.60.0"]
        req += ["pyphen"] if cross else ["ruaccent"]
    elif engine == "fish":
        req += ["fish-speech-lib"]
        if mode == "cuda":
            req += ["triton-windows<3.4"]
    elif engine == "omni":
        req += ["omnivoice==0.2.1", "transformers==5.3.0"]
    if engine in ("edge", "silero") or rvc:
        req += [("tts-with-rvc-onnx[dml]" if options["device"] == "dml" else "tts-with-rvc-onnx") if mode == "onnx" else "tts-with-rvc"]
    return tuple(req)


def plan(plugin_id: str, selected_settings: dict | None = None) -> InstallPlan:
    actions = (InstallerAction("fish_project_root", "create_empty_marker", "fish_speech_lib/.project-root"),) if family(plugin_id) == "fish" else ()
    return InstallPlan(assets=asset_requirements(plugin_id, selected_settings),
                       supported_backends=supported_backends(plugin_id, selected_settings),
                       required_backends=("torch-cpu",) if family(plugin_id) == "silero" and backend(plugin_id) == "onnx" else (),
                       requirements=requirements(plugin_id, selected_settings),
                       supported_python_minors=("3.11", "3.12"), actions=actions)


def probe(plugin_id: str, selected_settings: dict | None = None) -> dict:
    root = asset_root(plugin_id)
    missing = [entry.relative_path for entry in asset_requirements(plugin_id, selected_settings)
               if entry.required and not (root / entry.relative_path).is_file()]
    missing_distributions = []
    wrong_versions = []
    for entry in requirements(plugin_id, selected_settings):
        name = entry.split("[", 1)[0].split("<", 1)[0].split("=", 1)[0]
        if not name:
            continue
        try:
            installed = importlib.metadata.version(name)
        except importlib.metadata.PackageNotFoundError:
            missing_distributions.append(name)
            continue
        if "==" in entry:
            exact = entry.split("==", 1)[1].split(",", 1)[0]
            if installed != exact:
                wrong_versions.append({"name": name, "expected": exact, "actual": installed})
    return {"installed": not missing and not missing_distributions and not wrong_versions,
            "missing_assets": missing, "missing_distributions": missing_distributions,
            "wrong_versions": wrong_versions}


def settings(plugin_id: str) -> dict:
    engine, mode, cross, rvc = VARIANTS[plugin_id]
    result = {"device": {"type": "string", "default": "cuda:0" if mode == "cuda" else "cpu"},
              "volume": {"type": "number", "default": 1.0, "min": 0.0, "max": 2.0}}
    if engine in ("edge", "silero") or rvc:
        result.update({
            "rvc.pitch_shift_semitones": {"type": "number", "default": 6.0, "min": -24.0, "max": 24.0},
            "rvc.f0_method": {"type": "string", "default": "rmvpe", "choices": ["pm", "dio", "crepe", "rmvpe", "harvest", "fcpe"]},
            "rvc.index_rate": {"type": "number", "default": 0.75, "min": 0.0, "max": 1.0},
            "rvc.protect": {"type": "number", "default": 0.33, "min": 0.0, "max": 0.5},
        })
    if engine == "silero":
        result.update({"silero.speaker_id": {"type": "string", "default": "kseniya"},
                       "language": {"type": "string", "default": "ru", "choices": ["ru", "en"]},
                       "sample_rate": {"type": "integer", "default": 48000, "choices": [8000, 24000, 48000]}})
    if engine == "edge":
        result.update({"edge_voice": {"type": "string", "default": "ru-RU-SvetlanaNeural"},
                       "tts_rate": {"type": "integer", "default": 0, "min": -50, "max": 100}})
    if engine == "f5":
        result.update({"speed": {"type": "number", "default": 1.0},
                       "nfe_step": {"type": "integer", "default": 32, "min": 1, "max": 128},
                       "seed": {"type": "integer", "default": 42}})
    if engine == "fish":
        result.update({"top_p": {"type": "number", "default": 0.7},
                       "temperature": {"type": "number", "default": 0.7},
                       "repetition_penalty": {"type": "number", "default": 1.2},
                       "chunk_length": {"type": "integer", "default": 200},
                       "max_new_tokens": {"type": "integer", "default": 1024},
                       "seed": {"type": "integer", "default": 42}})
    if engine == "omni":
        result.update({"speed": {"type": "number", "default": 1.0},
                       "num_step": {"type": "integer", "default": 16, "min": 1, "max": 64},
                       "postprocess_output": {"type": "boolean", "default": True}})
    return result
