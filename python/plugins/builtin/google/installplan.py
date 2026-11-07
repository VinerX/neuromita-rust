from neuromita_asr.runtime import install_plan, probe, schema

class InstallContract:
    def install_plan(self):
        return install_plan("google")

    def probe_install(self):
        return probe("google")

    def settings_schema(self):
        return schema("google")

    def install_plan_for_settings(self, settings):
        return install_plan("google", settings)

    def probe_install_for_settings(self, settings):
        return probe("google", settings)
