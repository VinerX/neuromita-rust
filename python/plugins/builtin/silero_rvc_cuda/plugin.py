from neuromita_voice.runtime import VoicePlugin


class Plugin(VoicePlugin):
    def __init__(self):
        super().__init__("silero_rvc_cuda")
