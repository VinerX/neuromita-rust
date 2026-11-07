mod handler;
mod lexical_dedup;
pub mod memory;
pub(crate) mod memory_access;
mod model;
pub(crate) mod projection;
pub mod state;

pub use crate::identity::CharacterId;
pub(crate) use handler::AcceptanceContext;
pub(crate) use handler::CharacterRegistry;
pub(crate) use handler::ReminderBridge;
pub use handler::{AcceptedResponse, CharacterError, CharacterHandle};
pub use model::{
    CharacterContext, CharacterDefinition, CharacterState, CharacterView, GenerationFocus,
    ReactionCause, character_catalog, character_definition,
};

pub use memory_access::{CharacterMemorySnapshot, MemoryEditResult};
