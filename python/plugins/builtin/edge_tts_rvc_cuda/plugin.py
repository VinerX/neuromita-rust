from neuromita_voice.runtime import VoicePlugin


class Plugin(VoicePlugin):
    def __init__(self):
        super().__init__("edge_tts_rvc_cuda")
