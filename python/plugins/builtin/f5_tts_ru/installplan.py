from neuromita_voice.plans import plan, probe, settings


class InstallContract:
    def install_plan(self):
        return plan("f5_tts_ru")

    def probe_install(self):
        return probe("f5_tts_ru")

    def settings_schema(self):
        return settings("f5_tts_ru")

    def install_plan_for_settings(self, selected_settings):
        return plan("f5_tts_ru", selected_settings)

    def probe_install_for_settings(self, selected_settings):
        return probe("f5_tts_ru", selected_settings)
