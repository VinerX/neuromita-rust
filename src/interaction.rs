use std::sync::Arc;

use serde::Serialize;
use thiserror::Error;

use crate::identity::EventId;

#[derive(Debug, Error)]
pub enum InputError {
    #[error("message must not be empty")]
    Empty,
    #[error("message exceeds 64 KiB")]
    TooLarge,
}

#[derive(Debug, Clone)]
pub struct UserInput {
    event_id: EventId,
    text: Arc<str>,
}

impl UserInput {
    pub fn new(text: String) -> Result<Self, InputError> {
        Self::with_event_id(EventId::new(), text)
    }

    /// Supply a caller-retained identity before submitting an input. It can
    /// reconcile a request if the process exits before returning its handle.
    pub fn with_event_id(event_id: EventId, text: String) -> Result<Self, InputError> {
        if text.trim().is_empty() {
            return Err(InputError::Empty);
        }
        if text.len() > 64 * 1024 {
            return Err(InputError::TooLarge);
        }
        Ok(Self {
            event_id,
            text: Arc::from(text),
        })
    }
    pub fn event_id(&self) -> EventId {
        self.event_id
    }
    pub fn text(&self) -> &Arc<str> {
        &self.text
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TurnIntent {
    Reply(EventId),
    React(EventId),
    Initiate,
}
