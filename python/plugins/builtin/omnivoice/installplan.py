from neuromita_voice.plans import plan, probe, settings


class InstallContract:
    def install_plan(self):
        return plan("omnivoice")

    def probe_install(self):
        return probe("omnivoice")

    def settings_schema(self):
        return settings("omnivoice")

    def install_plan_for_settings(self, selected_settings):
        return plan("omnivoice", selected_settings)

    def probe_install_for_settings(self, selected_settings):
        return probe("omnivoice", selected_settings)
