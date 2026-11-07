from neuromita_asr.runtime import AsrPlugin

class Plugin(AsrPlugin):
    def __init__(self):
        super().__init__("whisper")
