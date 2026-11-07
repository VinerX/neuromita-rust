from neuromita_voice.plans import plan, probe, settings


class InstallContract:
    def install_plan(self):
        return plan("fish_speech_compiled")

    def probe_install(self):
        return probe("fish_speech_compiled")

    def settings_schema(self):
        return settings("fish_speech_compiled")

    def install_plan_for_settings(self, selected_settings):
        return plan("fish_speech_compiled", selected_settings)

    def probe_install_for_settings(self, selected_settings):
        return probe("fish_speech_compiled", selected_settings)
