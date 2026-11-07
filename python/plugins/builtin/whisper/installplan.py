from neuromita_asr.runtime import install_plan, probe, schema

class InstallContract:
    def install_plan(self):
        return install_plan("whisper")

    def probe_install(self):
        return probe("whisper")

    def settings_schema(self):
        return schema("whisper")

    def install_plan_for_settings(self, settings):
        return install_plan("whisper", settings)

    def probe_install_for_settings(self, settings):
        return probe("whisper", settings)
