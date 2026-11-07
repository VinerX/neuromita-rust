use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::{SettingsError, SpeechSettings};
use crate::generation::PresetId;
use crate::reminders::{LocalTimePolicy, TimeZoneId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Volume(u8);

impl Volume {
    pub fn new(value: u8) -> Result<Self, SettingsError> {
        if value > 100 {
            return Err(SettingsError::Validation(
                "volume must be between 0 and 100".into(),
            ));
        }
        Ok(Self(value))
    }
    pub fn get(self) -> u8 {
        self.0
    }
}
impl TryFrom<u8> for Volume {
    type Error = SettingsError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<Volume> for u8 {
    fn from(value: Volume) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceSettings {
    pub volume: Volume,
    pub in_launcher: bool,
    pub in_game: bool,
    pub voice: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureSettings {
    pub enabled: bool,
    pub interval_ms: u32,
    pub jpeg_quality: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Russian,
    English,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceSettings {
    pub language: Language,
    pub font_size: u16,
    pub visibility: SettingsVisibility,
    pub hide_private: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsArea {
    Api,
    Characters,
    Voice,
    Language,
    Asr,
    AiEngine,
    Games,
    Models,
    Images,
    Updates,
    Datasets,
}

impl SettingsArea {
    pub const ALL: [Self; 11] = [
        Self::Api,
        Self::Characters,
        Self::Voice,
        Self::Language,
        Self::Asr,
        Self::AiEngine,
        Self::Games,
        Self::Models,
        Self::Images,
        Self::Updates,
        Self::Datasets,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    try_from = "BTreeMap<SettingsArea, bool>",
    into = "BTreeMap<SettingsArea, bool>"
)]
pub struct SettingsVisibility(BTreeMap<SettingsArea, bool>);

impl SettingsVisibility {
    pub fn all_visible() -> Self {
        Self(
            SettingsArea::ALL
                .into_iter()
                .map(|area| (area, true))
                .collect(),
        )
    }
    pub fn is_visible(&self, area: SettingsArea) -> bool {
        self.0[&area]
    }
    pub fn set_visible(&mut self, area: SettingsArea, visible: bool) {
        self.0.insert(area, visible);
    }
}

impl TryFrom<BTreeMap<SettingsArea, bool>> for SettingsVisibility {
    type Error = String;
    fn try_from(values: BTreeMap<SettingsArea, bool>) -> Result<Self, Self::Error> {
        if values.len() != SettingsArea::ALL.len()
            || SettingsArea::ALL
                .iter()
                .any(|area| !values.contains_key(area))
        {
            return Err("visibility must explicitly contain every settings area".into());
        }
        Ok(Self(values))
    }
}

impl From<SettingsVisibility> for BTreeMap<SettingsArea, bool> {
    fn from(value: SettingsVisibility) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetSelection {
    #[default]
    Active,
    Specific(PresetId),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationPresets {
    pub reply: PresetSelection,
    pub react: PresetSelection,
    pub initiate: PresetSelection,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterSettings {
    pub presets: OperationPresets,
    #[serde(default)]
    pub prompt_set: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CharactersSettings {
    #[serde(rename = "Crazy")]
    pub crazy: CharacterSettings,
    #[serde(rename = "Kind")]
    pub kind: CharacterSettings,
    #[serde(rename = "Cappie")]
    pub cappie: CharacterSettings,
    #[serde(rename = "ShortHair")]
    pub short_hair: CharacterSettings,
    #[serde(rename = "Mila")]
    pub mila: CharacterSettings,
    #[serde(rename = "Sleepy")]
    pub sleepy: CharacterSettings,
    #[serde(rename = "Ghost")]
    pub ghost: CharacterSettings,
    #[serde(rename = "Creepy")]
    pub creepy: CharacterSettings,
    #[serde(rename = "GameMaster")]
    pub game_master: CharacterSettings,
}

impl CharactersSettings {
    pub fn for_character(&self, id: crate::character::CharacterId) -> &CharacterSettings {
        match id {
            crate::character::CharacterId::Crazy => &self.crazy,
            crate::character::CharacterId::Kind => &self.kind,
            crate::character::CharacterId::Cappie => &self.cappie,
            crate::character::CharacterId::ShortHair => &self.short_hair,
            crate::character::CharacterId::Mila => &self.mila,
            crate::character::CharacterId::Sleepy => &self.sleepy,
            crate::character::CharacterId::Ghost => &self.ghost,
            crate::character::CharacterId::Creepy => &self.creepy,
            crate::character::CharacterId::GameMaster => &self.game_master,
        }
    }

    pub fn for_character_mut(
        &mut self,
        id: crate::character::CharacterId,
    ) -> &mut CharacterSettings {
        match id {
            crate::character::CharacterId::Crazy => &mut self.crazy,
            crate::character::CharacterId::Kind => &mut self.kind,
            crate::character::CharacterId::Cappie => &mut self.cappie,
            crate::character::CharacterId::ShortHair => &mut self.short_hair,
            crate::character::CharacterId::Mila => &mut self.mila,
            crate::character::CharacterId::Sleepy => &mut self.sleepy,
            crate::character::CharacterId::Ghost => &mut self.ghost,
            crate::character::CharacterId::Creepy => &mut self.creepy,
            crate::character::CharacterId::GameMaster => &mut self.game_master,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetSettings {
    pub active: PresetId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemindersSettings {
    pub enabled: bool,
    pub timezone: TimeZoneId,
    pub policy: LocalTimePolicy,
}

impl RemindersSettings {
    pub fn initial_profile() -> Self {
        // Deterministic declared profile: the initial zone is always UTC and
        // never depends on the host. A local zone is set explicitly through
        // SettingsApi or the `zone` terminal command.
        Self {
            enabled: true,
            timezone: TimeZoneId::utc(),
            policy: LocalTimePolicy::LENIENT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSettings {
    pub presets: PresetSettings,
    pub characters: CharactersSettings,
    pub voice: VoiceSettings,
    pub capture: CaptureSettings,
    pub interface: InterfaceSettings,
    pub reminders: RemindersSettings,
    pub speech: SpeechSettings,
}

impl AppSettings {
    pub fn initial_profile() -> Self {
        Self {
            presets: PresetSettings::default(),
            characters: CharactersSettings::default(),
            voice: VoiceSettings {
                volume: Volume(80),
                in_launcher: false,
                in_game: false,
                voice: "default".into(),
            },
            capture: CaptureSettings {
                enabled: false,
                interval_ms: 1000,
                jpeg_quality: 80,
            },
            interface: InterfaceSettings {
                language: Language::Russian,
                font_size: 14,
                visibility: SettingsVisibility::all_visible(),
                hide_private: true,
            },
            reminders: RemindersSettings::initial_profile(),
            speech: SpeechSettings::default(),
        }
    }
    pub fn selected_preset(
        &self,
        character: crate::character::CharacterId,
        intent: crate::interaction::TurnIntent,
    ) -> PresetId {
        let assigned = &self.characters.for_character(character).presets;
        let selection = match intent {
            crate::interaction::TurnIntent::Reply(_) => assigned.reply,
            crate::interaction::TurnIntent::React(_) => assigned.react,
            crate::interaction::TurnIntent::Initiate => assigned.initiate,
        };
        match selection {
            PresetSelection::Active => self.presets.active,
            PresetSelection::Specific(id) => id,
        }
    }

    pub fn validate(&self) -> Result<(), SettingsError> {
        for character in crate::character::CharacterId::ALL {
            if let Some(name) = &self.characters.for_character(character).prompt_set {
                crate::prompts::PromptCatalog::validate_set_name(name)
                    .map_err(|error| SettingsError::Validation(error.to_string()))?;
            }
        }
        self.speech.validate()?;
        if self.voice.voice.trim().is_empty() {
            return Err(SettingsError::Validation(
                "voice identifier must not be empty".into(),
            ));
        }
        if self.capture.interval_ms == 0 || !(1..=100).contains(&self.capture.jpeg_quality) {
            return Err(SettingsError::Validation(
                "invalid capture interval or JPEG quality".into(),
            ));
        }
        if !(6..=96).contains(&self.interface.font_size) {
            return Err(SettingsError::Validation(
                "font size must be between 6 and 96".into(),
            ));
        }
        Ok(())
    }
}

mod sealed {
    pub trait Sealed {}
}
pub trait SettingsSection: sealed::Sealed + Clone + PartialEq + Send + Sync + 'static {
    fn read(settings: &AppSettings) -> Self;
    fn replace(self, settings: &mut AppSettings);
}

macro_rules! section {
    ($ty:ty, $field:ident) => {
        impl sealed::Sealed for $ty {}
        impl SettingsSection for $ty {
            fn read(settings: &AppSettings) -> Self {
                settings.$field.clone()
            }
            fn replace(self, settings: &mut AppSettings) {
                settings.$field = self;
            }
        }
    };
}
section!(PresetSettings, presets);
section!(CharactersSettings, characters);
section!(VoiceSettings, voice);
section!(SpeechSettings, speech);
section!(CaptureSettings, capture);
section!(InterfaceSettings, interface);
section!(RemindersSettings, reminders);

#[derive(Debug, Clone)]
pub struct SectionSnapshot<T> {
    pub revision: u64,
    pub value: T,
}
