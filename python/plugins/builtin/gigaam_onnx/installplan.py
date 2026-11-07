from neuromita_asr.runtime import install_plan, probe, schema


class InstallContract:
    def install_plan(self):
        return install_plan("gigaam_onnx")

    def probe_install(self):
        return probe("gigaam_onnx")

    def settings_schema(self):
        return schema("gigaam_onnx")

    def install_plan_for_settings(self, settings):
        return install_plan("gigaam_onnx", settings)

    def probe_install_for_settings(self, settings):
        return probe("gigaam_onnx", settings)
