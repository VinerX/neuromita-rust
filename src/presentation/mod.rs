mod audio_input;
mod characters;
pub use audio_input::AudioInputPresenter;
mod chat;
mod connections;
mod conversation;
pub mod hardware;
mod presets;
mod releases;
mod runtime;
mod settings;
mod speech;

pub use characters::{
    CharactersPresenter, MemoryCapacityPreview, MemoryListPage, MemoryListQuery, MemoryObserver,
    MemorySummary,
};
pub(crate) use chat::PresenterRunner;
pub use chat::{ChatModel, ChatPresenter, PresenterError, RequestView};
pub use connections::ConnectionsPresenter;
pub use conversation::{
    ConversationChange, ConversationChangePreview, ConversationError, ConversationPresenter,
    ConversationSnapshot,
};
pub use presets::{
    PresetParameterOption, PresetView, PresetsApi, PresetsPresenter, PresetsView, ProviderOption,
};
pub use releases::{
    ReleaseCommandError, ReleaseLoadState, ReleasesLifecycle, ReleasesPresenter, ReleasesSnapshot,
};
pub use runtime::RuntimeStatePresenter;
pub use settings::{
    AsrSettingsPresenter, BoundProperty, CharacterSettingsPresenter, CharactersSettingsPresenter,
    InterfaceSettingsPresenter, PresetSlot, PresetsSettingsPresenter, PropertyObserver,
    SettingsPresenter, TtsSettingsPresenter, VoiceSettingsPresenter,
};
pub use speech::SpeechSessionPresenter;

pub struct GuiPresenter {
    pub audio_input: AudioInputPresenter,
    pub prompts: Option<std::sync::Arc<crate::prompts::PromptCatalog>>,
    pub speech: SpeechSessionPresenter,
    pub ai_runtime: RuntimeStatePresenter,
    pub releases: ReleasesPresenter,
    pub connections: ConnectionsPresenter,
    pub conversation: ConversationPresenter,
    pub chat: ChatPresenter,
    pub settings: SettingsPresenter,
    pub presets: PresetsPresenter,
    pub characters: CharactersPresenter,
}
