"""Deterministic ASR fixture for IPC and shared-worker contract tests."""

from neuromita_ai_sdk import InstallPlan


class Plugin:
    def describe(self):
        return {"kind": "asr", "languages": ["en", "ru"]}

    def install_plan(self):
        return InstallPlan(supported_backends=("remote-api",))

    def probe_install(self):
        return {"installed": True, "missing": []}

    def settings_schema(self):
        return {"language": {"type": "string", "choices": ["en", "ru"], "default": "ru"}}

    def initialize(self, settings):
        if settings.get("language", "ru") not in ("en", "ru"):
            raise ValueError("unsupported language")
        return {"model_ready": True}

    def infer(self, input_data):
        if "audio_base64" in input_data:
            import base64
            import io
            import wave
            payload = base64.b64decode(input_data["audio_base64"], validate=True)
            with wave.open(io.BytesIO(payload), "rb") as wav:
                if wav.getnchannels() != 1 or wav.getsampwidth() != 2 or wav.getframerate() != 16000:
                    raise ValueError("mock ASR requires 16-kHz mono PCM16 WAV")
                if wav.getnframes() == 0:
                    raise ValueError("empty mock ASR audio")
            return {"text": input_data.get("mock_transcript", ""), "language": "en"}
        return {"text": input_data.get("mock_transcript", "") }

    def unload(self):
        return None
