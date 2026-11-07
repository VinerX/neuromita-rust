use std::sync::Arc;

use crate::{
    games::GameProjection,
    identity::CharacterId,
    interaction::TurnIntent,
    timeline::{
        ContextAnchor, EventId, HistoryView,
        domain::{CharacterWorkingState, EventKind, MemoryRecord},
        selection::TimelineTurn,
        view::MitaView,
    },
};

use crate::localization::{LocalizedText, i18n};

/// Immutable Core-owned definition. Frontends may attach visual resources by stable ID.
#[derive(Debug, Clone)]
pub struct CharacterDefinition {
    pub id: CharacterId,
    pub name: LocalizedText,
    pub conversation_name: LocalizedText,
    pub description: &'static str,
    pub prompt: &'static str,
}

pub fn character_definition(id: CharacterId) -> CharacterDefinition {
    match id {
        CharacterId::Crazy => CharacterDefinition {
            id,
            name: i18n("character.crazy", "Безумная Мита"),
            conversation_name: i18n("character.crazy.with", "Безумной Митой"),
            description: "Безумная Мита — собеседница игрока.",
            prompt: "Отвечай от лица Безумной Миты. Учитывай только предоставленные тебе сведения и историю.",
        },
        CharacterId::Kind => CharacterDefinition {
            id,
            name: i18n("character.kind", "Добрая Мита"),
            conversation_name: i18n("character.kind.with", "Доброй Митой"),
            description: "Добрая Мита — внимательная и спокойная собеседница игрока.",
            prompt: "Отвечай от лица Доброй Миты. Учитывай только предоставленные тебе сведения и историю.",
        },
        CharacterId::Cappie => CharacterDefinition {
            id,
            name: i18n("character.cappie", "Кепочка"),
            conversation_name: i18n("character.cappie.with", "Кепочкой"),
            description: "Кепочка — собеседница игрока.",
            prompt: "Отвечай от лица Кепочки. Учитывай только предоставленные тебе сведения и историю.",
        },
        CharacterId::ShortHair => CharacterDefinition {
            id,
            name: i18n("character.shorthair", "Коротковолосая Мита"),
            conversation_name: i18n("character.shorthair.with", "Коротковолосой Митой"),
            description: "Коротковолосая Мита — собеседница игрока.",
            prompt: "Отвечай от лица Коротковолосой Миты. Учитывай только предоставленные тебе сведения и историю.",
        },
        CharacterId::Mila => CharacterDefinition {
            id,
            name: i18n("character.mila", "Мила"),
            conversation_name: i18n("character.mila.with", "Милой"),
            description: "Мила — собеседница игрока.",
            prompt: "Отвечай от лица Милы. Учитывай только предоставленные тебе сведения и историю.",
        },
        CharacterId::Sleepy => CharacterDefinition {
            id,
            name: i18n("character.sleepy", "Сонная Мита"),
            conversation_name: i18n("character.sleepy.with", "Сонной Митой"),
            description: "Сонная Мита — собеседница игрока.",
            prompt: "Отвечай от лица Сонной Миты. Учитывай только предоставленные тебе сведения и историю.",
        },
        CharacterId::Ghost => CharacterDefinition {
            id,
            name: i18n("character.ghost", "Призрачная Мита"),
            conversation_name: i18n("character.ghost.with", "Призрачной Митой"),
            description: "Призрачная Мита — собеседница игрока.",
            prompt: "Отвечай от лица Призрачной Миты. Учитывай только предоставленные тебе сведения и историю.",
        },
        CharacterId::Creepy => CharacterDefinition {
            id,
            name: i18n("character.creepy", "Жуткая Мита"),
            conversation_name: i18n("character.creepy.with", "Жуткой Митой"),
            description: "Жуткая Мита — собеседница игрока.",
            prompt: "Отвечай от лица Жуткой Миты. Учитывай только предоставленные тебе сведения и историю.",
        },
        CharacterId::GameMaster => CharacterDefinition {
            id,
            name: i18n("character.gamemaster", "Мастер игры"),
            conversation_name: i18n("character.gamemaster.with", "Мастером игры"),
            description: "Мастер игры — ведущий разговора.",
            prompt: "Отвечай от лица Мастера игры. Учитывай только предоставленные тебе сведения и историю.",
        },
    }
}

pub fn character_catalog() -> impl ExactSizeIterator<Item = CharacterDefinition> {
    CharacterId::ALL.into_iter().map(character_definition)
}

#[derive(Debug, Clone, PartialEq)]
pub struct CharacterState {
    pub accepted_replies: u64,
    pub revision: u64,
    pub attitude: f64,
    pub boredom: f64,
    pub stress: f64,
}

pub(crate) struct Character {
    pub(super) id: CharacterId,
    pub(super) description: Arc<str>,
    pub(super) prompt: Arc<str>,
    pub(super) state: CharacterState,
}

impl Character {
    pub(super) fn create(id: CharacterId) -> Self {
        let definition = character_definition(id);
        Self {
            id,
            description: Arc::from(definition.description),
            prompt: Arc::from(definition.prompt),
            state: CharacterState {
                accepted_replies: 0,
                revision: 0,
                attitude: 60.0,
                boredom: 10.0,
                stress: 5.0,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionCause {
    pub event_id: EventId,
    pub kind: EventKind,
    pub description: Option<Arc<str>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationFocus {
    pub intent: TurnIntent,
    pub reaction: Option<ReactionCause>,
}

#[derive(Debug, Clone)]
pub struct CharacterContext {
    pub character_id: CharacterId,
    pub description: Arc<str>,
    pub prompt: Arc<str>,
    pub runtime_context: Option<Arc<crate::game_context::GameRuntimeContext>>,
    pub state: CharacterState,
    pub working_state: CharacterWorkingState,
    pub memories: Vec<MemoryRecord>,
    pub memory_islands: Vec<MemoryRecord>,
    pub history: HistoryView,
    pub view: MitaView,
    /// Role-limited board projection for this character only.
    pub game: Option<GameProjection>,
    /// Own active reminders of the selected branch; never another character's.
    pub reminders: Vec<crate::reminders::ReminderPrompt>,
    pub current_input: Arc<str>,
    pub focus: GenerationFocus,
    pub anchor: ContextAnchor,
    pub(crate) turn: TimelineTurn,
    pub(crate) game_key: Option<crate::identity::EventId>,
    pub(crate) game_guard: Option<crate::identity::EventId>,
}

#[derive(Debug, Clone)]
pub struct CharacterView {
    pub game: Option<GameProjection>,
    pub character_id: CharacterId,
    pub description: Arc<str>,
    pub prompt: Arc<str>,
    pub state: CharacterState,
    pub working_state: CharacterWorkingState,
    pub memories: Vec<MemoryRecord>,
    pub memory_islands: Vec<MemoryRecord>,
    pub history: HistoryView,
    pub view: MitaView,
}
