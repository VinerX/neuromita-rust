"""Deterministic, dependency-free TTS fixture; not a production voice backend."""

import base64
import io
import math
import struct
import wave

from neuromita_ai_sdk import InstallPlan


class Plugin:
    def __init__(self):
        self.initialized = False
        self.speaker = "default"

    def describe(self):
        return {"kind": "tts", "voices": ["default", "bright"]}

    def install_plan(self):
        return InstallPlan(supported_backends=("remote-api",))

    def probe_install(self):
        return {"installed": True, "missing": []}

    def settings_schema(self):
        return {"speaker": {"type": "string", "choices": ["default", "bright"], "default": "default"}}

    def initialize(self, settings):
        speaker = settings.get("speaker", "default")
        if speaker not in ("default", "bright"):
            raise ValueError("unknown speaker")
        self.speaker = speaker
        self.initialized = True
        return {"model_ready": True}

    def infer(self, input_data):
        if not self.initialized:
            raise ValueError("model not ready")
        text = input_data.get("text")
        if not isinstance(text, str):
            raise ValueError("text must be a string")
        frequency = 440 if self.speaker == "default" else 660
        sample_rate = 16000
        duration = min(0.1 + len(text) * 0.008, 0.5)
        samples = int(duration * sample_rate)
        frames = b"".join(struct.pack("<h", int(2500 * math.sin(2 * math.pi * frequency * n / sample_rate))) for n in range(samples))
        output = io.BytesIO()
        with wave.open(output, "wb") as wav:
            wav.setnchannels(1)
            wav.setsampwidth(2)
            wav.setframerate(sample_rate)
            wav.writeframes(frames)
        return {"format": "wav", "audio_base64": base64.b64encode(output.getvalue()).decode("ascii")}

    def unload(self):
        self.initialized = False
