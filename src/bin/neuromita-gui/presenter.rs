use neuromita::localization::{LocaleId, LocalizedText, i18n};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Page {
    #[default]
    Home,
    Settings,
    Sandbox,
    Releases,
    Gallery,
    Wiki,
    Logs,
    About,
}

impl Page {
    pub fn title(self) -> LocalizedText {
        match self {
            Self::Home => i18n("page.home.title", "Главная"),
            Self::Settings => i18n("page.settings.title", "Настройки"),
            Self::Sandbox => i18n("page.sandbox.title", "Песочница / Sandbox"),
            Self::Releases => i18n("page.releases.title", "Релизы NeuroMita"),
            Self::Gallery => i18n("page.gallery.title", "Галерея"),
            Self::Wiki => i18n("page.wiki.title", "Вики"),
            Self::Logs => i18n("page.logs.title", "Системные логи"),
            Self::About => i18n("page.about.title", "О приложении"),
        }
    }
}

pub enum ShellCommand {
    Navigate(Page),
    SelectLanguage(LocaleId),
    ReloadTranslations,
    LocalizationFailed(String),
    TranslationsReloaded,
    ToggleBackend,
    ToggleClient,
    LauncherFeedback(LocalizedText),
    Launcher(LauncherAction),
    Social(String),
    DismissNotice,
}

pub enum LauncherAction {
    CheckUpdates,
    InstallUnity,
    VerifyUnity,
    OpenUnityFolder,
    UpdateSettings,
}

impl LauncherAction {
    fn label(&self) -> LocalizedText {
        match self {
            Self::CheckUpdates => i18n("launcher.actions.check-updates", "Проверить обновления"),
            Self::InstallUnity => i18n("launcher.actions.install", "Установить / обновить Unity"),
            Self::VerifyUnity => i18n("launcher.actions.verify", "Проверить файлы Unity"),
            Self::OpenUnityFolder => i18n("launcher.actions.open-folder", "Открыть папку Unity"),
            Self::UpdateSettings => {
                i18n("launcher.actions.update-settings", "Настройки обновлений")
            }
        }
    }
}

pub enum ComponentKind {
    Python,
    Unity,
}

impl ComponentKind {
    pub fn title(&self) -> LocalizedText {
        match self {
            Self::Python => i18n("launcher.component.python", "Python-бэкенд"),
            Self::Unity => i18n("launcher.component.unity", "Unity"),
        }
    }
}

pub struct ComponentPreview {
    pub kind: ComponentKind,
    pub version: &'static str,
    pub update: &'static str,
}

pub struct HomePreview {
    pub backend: ComponentPreview,
    pub client: ComponentPreview,
}

impl HomePreview {
    pub fn runtime() -> Self {
        Self {
            backend: ComponentPreview {
                kind: ComponentKind::Python,
                version: "",
                update: "",
            },
            client: ComponentPreview {
                kind: ComponentKind::Unity,
                version: "",
                update: "",
            },
        }
    }
}

pub struct ShellPresenter {
    page: Page,
    locale: LocaleId,
    backend_selected: bool,
    client_selected: bool,
    notice: Option<LocalizedText>,
    home: HomePreview,
}

impl ShellPresenter {
    pub fn new(home: HomePreview, locale: LocaleId) -> Self {
        Self {
            page: Page::Home,
            locale,
            backend_selected: false,
            client_selected: false,
            notice: None,
            home,
        }
    }
    pub fn page(&self) -> Page {
        self.page
    }
    pub fn locale(&self) -> &LocaleId {
        &self.locale
    }
    pub fn home(&self) -> &HomePreview {
        &self.home
    }
    pub fn selections(&self) -> (bool, bool) {
        (self.backend_selected, self.client_selected)
    }
    pub fn notice(&self) -> Option<&LocalizedText> {
        self.notice.as_ref()
    }

    pub fn dispatch(&mut self, command: ShellCommand) {
        match command {
            ShellCommand::Navigate(page) => {
                self.page = page;
                self.notice = None;
            }
            ShellCommand::SelectLanguage(locale) => self.locale = locale,
            ShellCommand::ToggleBackend => self.backend_selected = !self.backend_selected,
            ShellCommand::ToggleClient => self.client_selected = !self.client_selected,
            ShellCommand::LauncherFeedback(notice) => self.notice = Some(notice),
            ShellCommand::Launcher(LauncherAction::UpdateSettings) => {
                self.page = Page::Settings;
                self.notice = None;
            }
            ShellCommand::Launcher(action) => {
                self.notice = Some(
                    i18n(
                        "preview.action.unavailable",
                        "Макет интерфейса: «{action}» пока не подключено.",
                    )
                    .arg("action", action.label()),
                )
            }
            ShellCommand::Social(name) => {
                self.notice = Some(
                    i18n(
                        "preview.social.unavailable",
                        "Макет интерфейса: ссылка {name} пока не подключена.",
                    )
                    .arg("name", name),
                )
            }
            ShellCommand::LocalizationFailed(error) => {
                self.notice = Some(
                    i18n("localization.error", "Ошибка локализации: {error}").arg("error", error),
                )
            }
            ShellCommand::TranslationsReloaded => {
                self.notice = Some(i18n("localization.reload.success", "Переводы обновлены."))
            }
            ShellCommand::ReloadTranslations => {
                self.notice = Some(i18n(
                    "localization.reload.unconfigured",
                    "Каталог внешних переводов не задан.",
                ))
            }
            ShellCommand::DismissNotice => self.notice = None,
        }
    }
}
