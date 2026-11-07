from neuromita_voice.plans import plan, probe, settings


class InstallContract:
    def install_plan(self):
        return plan("f5_tts_ru_rvc")

    def probe_install(self):
        return probe("f5_tts_ru_rvc")

    def settings_schema(self):
        return settings("f5_tts_ru_rvc")

    def install_plan_for_settings(self, selected_settings):
        return plan("f5_tts_ru_rvc", selected_settings)

    def probe_install_for_settings(self, selected_settings):
        return probe("f5_tts_ru_rvc", selected_settings)
