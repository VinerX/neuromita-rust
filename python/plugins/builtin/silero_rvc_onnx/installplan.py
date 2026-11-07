from neuromita_voice.plans import plan, probe, settings


class InstallContract:
    def install_plan(self):
        return plan("silero_rvc_onnx")

    def probe_install(self):
        return probe("silero_rvc_onnx")

    def settings_schema(self):
        return settings("silero_rvc_onnx")

    def install_plan_for_settings(self, selected_settings):
        return plan("silero_rvc_onnx", selected_settings)

    def probe_install_for_settings(self, selected_settings):
        return probe("silero_rvc_onnx", selected_settings)
