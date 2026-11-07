from neuromita_asr.runtime import AsrPlugin

class Plugin(AsrPlugin):
    def __init__(self):
        super().__init__("google")

    def runtime_capabilities(self):
        return [{"backend": "remote_api", "devices": ["remote"], "precisions": ["fp32"]}]

    def initialize_runtime(self, settings, runtime):
        expected = {"backend": "remote_api", "device": "remote", "precision": "fp32"}
        if runtime != expected:
            raise ValueError("Google ASR requires remote_api on remote with fp32 wire precision")
        if self.engine is not None:
            raise ValueError("Google ASR is already initialized")
        try:
            result = super().initialize(settings)
            self.runtime_status()
            return result
        except Exception:
            self.unload()
            raise

    def runtime_status(self):
        import speech_recognition as speech

        if (self.engine is not speech or self.options is None
                or not callable(getattr(speech, "AudioData", None))
                or not callable(getattr(speech, "Recognizer", None))
                or not callable(getattr(speech.Recognizer, "recognize_google", None))):
            raise RuntimeError("Google ASR adapter is not loaded")
        # Remote precision is a wire placeholder; readiness here confirms the local adapter.
        return {"backend": "remote_api", "device": "remote", "precision": "fp32"}
