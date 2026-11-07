"""Concrete TTS implementations called by the Python AI plugin host."""

from __future__ import annotations

import array
import base64
import importlib
import io
import math
from pathlib import Path
import tempfile
import sys
import wave

from .plans import VARIANTS, SILERO_TTS_MODELS, asset_root, family, plan, probe, settings, supported_backends
from neuromita_ai_sdk import validate_settings


class VoicePlugin:
    def __init__(self, plugin_id: str):
        if plugin_id not in VARIANTS:
            raise ValueError(f"unknown TTS implementation: {plugin_id}")
        self.plugin_id = plugin_id
        self.options = {}
        self.engine = None
        self.rvc = None
        self.initialized = False

    def describe(self):
        return {"id": self.plugin_id, "kind": "tts", "family": family(self.plugin_id),
                "variant": self.plugin_id}

    def install_plan(self):
        return plan(self.plugin_id)

    def probe_install(self):
        return probe(self.plugin_id)

    def install_plan_for_settings(self, selected_settings):
        return plan(self.plugin_id, selected_settings)

    def probe_install_for_settings(self, selected_settings):
        return probe(self.plugin_id, selected_settings)

    def settings_schema(self):
        schema = settings(self.plugin_id)
        if self.initialized and self.engine is not None and family(self.plugin_id) == "silero":
            speakers = getattr(self.engine, "speakers", None)
            if speakers is not None:
                schema["silero.speaker_id"]["choices"] = list(speakers)
        return schema

    def initialize(self, values):
        self.options = validate_settings(self.settings_schema(), values)
        _, mode, _, _ = VARIANTS[self.plugin_id]
        if mode == "cuda" and not self.options["device"].startswith("cuda"):
            raise ValueError("CUDA TTS variant requires a CUDA device")
        supported_backends(self.plugin_id, self.options)
        state = self.probe_install_for_settings(self.options)
        if not state["installed"]:
            raise RuntimeError(f"TTS dependencies/assets missing: {state}")
        engine, mode, cross, rvc = VARIANTS[self.plugin_id]
        root = asset_root(self.plugin_id)
        try:
            if engine in ("edge", "silero") or rvc:
                self.rvc = self._init_rvc(root, mode)
            if engine == "silero":
                from torch.package import PackageImporter
                model_id, relative_path, _, _ = SILERO_TTS_MODELS[self.options["language"]]
                model_path = root / relative_path
                if model_path.is_symlink() or not model_path.is_file() or model_path.stat().st_size == 0:
                    raise RuntimeError(f"Silero {model_id} package is not installed: {relative_path}")
                self.engine = PackageImporter(str(model_path)).load_pickle("tts_models", "model")
                self.engine.to("cpu" if mode == "onnx" else self.options["device"])
                speakers = getattr(self.engine, "speakers", None)
                if speakers is not None and self.options["silero.speaker_id"] not in speakers:
                    raise ValueError("Silero speaker is not present in the loaded model")
            elif engine == "f5":
                from .pipelines.f5_pipeline import F5TTSPipeline
                if cross:
                    from .pipelines.clf5_pipeline import CrossLingualF5TTSPipeline
                    klass = CrossLingualF5TTSPipeline
                else:
                    klass = F5TTSPipeline
                folder = root / ("en" if cross else "ru")
                options = {"model": "F5TTS_v1_Base", "ckpt_file": str(folder / "model.safetensors"),
                           "vocab_file": str(folder / "vocab.txt"), "load_vocoder_from_local": True,
                           "vocoder_local_path": str(root / "vocos"), "device": self.options["device"]}
                if cross:
                    options["speaking_rate_ckpt_file"] = str(folder / "speaking_rate.safetensors")
                self.engine = klass(**options)
            elif engine == "fish":
                import fish_speech_lib
                from fish_speech_lib.inference import FishSpeech
                if not (Path(fish_speech_lib.__file__).parent / ".project-root").is_file():
                    raise RuntimeError("fish_speech_lib .project-root marker missing: installer hook required")
                self.engine = FishSpeech(
                    device=self.options["device"], half=False,
                    compile_model=mode == "cuda",
                    llama_checkpoint_path=str(root),
                    decoder_checkpoint_path=str(root / "firefly-gan-vq-fsq-8x1024-21hz-generator.pth"),
                )
            elif engine == "omni":
                import torch
                from omnivoice import OmniVoice
                device = self.options["device"]
                if device.startswith("cuda") and not torch.cuda.is_available():
                    raise RuntimeError("CUDA requested but not available")
                self.engine = OmniVoice.from_pretrained(
                    str(root), device_map=device,
                    dtype=torch.float16 if device.startswith("cuda") else torch.float32,
                )
            else:
                self.engine = self.rvc
        except BaseException:
            self.unload()
            raise
        self.initialized = True
        return {"model_ready": True, "plugin_id": self.plugin_id}

    def _init_rvc(self, root, mode):
        candidates = ("tts_with_rvc", "tts_with_rvc_onnx") if mode == "onnx" else ("tts_with_rvc",)
        module = None
        for name in candidates:
            try:
                module = importlib.import_module(name)
                break
            except ImportError:
                continue
        if module is None:
            raise ImportError(f"RVC runtime missing: {candidates}")
        ext = "onnx" if mode == "onnx" else "pth"
        device = self.options["device"]
        if mode == "onnx" and device.startswith("cuda"):
            raise ValueError("ONNX RVC requires a supported ONNX device")
        runtime = module.TTS_RVC(model_path=str(root / "rvc" / f"voice.{ext}"),
                                 device=device, f0_method=self.options["rvc.f0_method"])
        index = root / "rvc" / "voice.index"
        runtime.set_index_path(str(index) if index.is_file() else "")
        return runtime

    def _rvc_convert(self, input_file, options):
        output = self.rvc.voiceover_file(
            input_path=str(input_file), pitch=float(options["rvc.pitch_shift_semitones"]),
            index_rate=float(options["rvc.index_rate"]),
            protect=float(options["rvc.protect"]),
            f0method=options["rvc.f0_method"],
        )
        path = Path(output)
        if not path.is_file() or not path.stat().st_size:
            raise RuntimeError("RVC produced no audio")
        return path

    def infer(self, input_data):
        if not self.initialized:
            raise RuntimeError("TTS not initialized")
        text = input_data.get("text")
        if not isinstance(text, str) or not text.strip():
            raise ValueError("nonempty text is required")
        request_settings = input_data.get("settings", {})
        overrides = validate_settings(self.settings_schema(), request_settings, include_defaults=False)
        options = {**self.options, **overrides}
        engine, mode, cross, rvc = VARIANTS[self.plugin_id]
        with tempfile.TemporaryDirectory(prefix="neuromita-tts-") as tmp:
            path = Path(tmp) / "audio.wav"
            if engine == "silero":
                import soundfile as sf
                audio = self.engine.apply_tts(
                    ssml_text=text, speaker=options["silero.speaker_id"],
                    sample_rate=options["sample_rate"],
                    put_accent=True, put_yo=True, put_stress_homo=True, put_yo_homo=True)
                sf.write(path, audio.cpu().numpy(), options["sample_rate"])
            elif engine == "edge":
                self.rvc.set_voice(options["edge_voice"])
                output = self.rvc(text=text, pitch=float(options["rvc.pitch_shift_semitones"]),
                                  index_rate=float(options["rvc.index_rate"]),
                                  protect=float(options["rvc.protect"]),
                                  f0method=options["rvc.f0_method"],
                                  tts_rate=int(options["tts_rate"]))
                path = Path(output)
            elif engine == "f5":
                ref_audio = input_data.get("reference_audio")
                ref_text = input_data.get("reference_text")
                if not isinstance(ref_audio, str) or not Path(ref_audio).is_file() or not isinstance(ref_text, str) or not ref_text:
                    raise ValueError("F5-TTS requires an explicit reference_audio and reference_text")
                self.engine.generate(text_to_generate=text, output_path=str(path),
                                     ref_audio=ref_audio, ref_text=ref_text,
                                     speed=options["speed"], nfe_step=options["nfe_step"],
                                     seed=options["seed"], remove_silence=True)
            elif engine == "fish":
                import soundfile as sf
                ref_audio = input_data.get("reference_audio")
                ref_text = input_data.get("reference_text", "")
                if ref_audio and not Path(ref_audio).is_file():
                    raise ValueError("Fish Speech reference audio does not exist")
                sample_rate, data = self.engine(
                    text=text, reference_audio=ref_audio, reference_audio_text=ref_text,
                    top_p=options["top_p"], temperature=options["temperature"],
                    repetition_penalty=options["repetition_penalty"],
                    max_new_tokens=options["max_new_tokens"],
                    chunk_length=options["chunk_length"], seed=options["seed"],
                    use_memory_cache=True)
                sf.write(path, data, sample_rate)
            elif engine == "omni":
                import soundfile as sf
                ref_audio = input_data.get("reference_audio")
                ref_text = input_data.get("reference_text")
                if not isinstance(ref_audio, str) or not Path(ref_audio).is_file() or not isinstance(ref_text, str) or not ref_text:
                    raise ValueError("OmniVoice requires explicit reference_audio and reference_text")
                prompt = self.engine.create_voice_clone_prompt(ref_audio=ref_audio, ref_text=ref_text)
                generated = self.engine.generate(
                    text=text, voice_clone_prompt=prompt, speed=options["speed"],
                    num_step=options["num_step"],
                    postprocess_output=options["postprocess_output"])
                if not generated:
                    raise RuntimeError("OmniVoice returned no audio")
                sf.write(path, generated[0], int(self.engine.sampling_rate), subtype="PCM_16")
            if rvc or engine == "silero":
                path = self._rvc_convert(path, options)
            if not path.is_file() or not path.stat().st_size:
                raise RuntimeError("TTS produced no audio")
            data = path.read_bytes()
            volume = input_data.get("volume", options["volume"])
            data = _scale_wave(data, volume)
            if len(data) > 10 * 1024 * 1024:
                raise RuntimeError("TTS audio exceeds IPC size limit; streaming transport required")
            if data[:4] != b"RIFF" or data[8:12] != b"WAVE":
                raise ValueError("TTS backend did not return a RIFF/WAVE file")
            return {"format": "wav", "audio_base64": base64.b64encode(data).decode("ascii")}

    def unload(self):
        self.engine = None
        self.rvc = None
        self.initialized = False


def _scale_wave(data: bytes, volume: float) -> bytes:
    if isinstance(volume, bool) or not isinstance(volume, (int, float)) or not math.isfinite(volume) or not 0 <= volume <= 2:
        raise ValueError("volume must be a finite number between 0 and 2")
    if volume == 1:
        return data
    with wave.open(io.BytesIO(data), "rb") as source:
        if source.getcomptype() != "NONE" or source.getsampwidth() != 2:
            raise ValueError("volume control requires PCM16 WAV")
        rate, channels, frames = source.getframerate(), source.getnchannels(), source.readframes(source.getnframes())
    samples = array.array("h")
    samples.frombytes(frames)
    if sys.byteorder != "little":
        samples.byteswap()
    for index, sample in enumerate(samples):
        samples[index] = max(-32768, min(32767, round(sample * volume)))
    if sys.byteorder != "little":
        samples.byteswap()
    output = io.BytesIO()
    with wave.open(output, "wb") as target:
        target.setnchannels(channels)
        target.setsampwidth(2)
        target.setframerate(rate)
        target.writeframes(samples.tobytes())
    return output.getvalue()
