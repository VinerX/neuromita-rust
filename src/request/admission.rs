use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use tokio::sync::mpsc;

use super::{
    DiagnosticScope, DiagnosticsAccess, GenerationCommand, RequestDiscovery, RequestHandle,
    RequestRegistry, SubmitError,
};
use crate::{
    identity::{CharacterId, EventId, RequestId},
    interaction::{TurnIntent, UserInput},
    timeline::{conversation::ConversationMessage, selection::TimelineAdmission},
};

pub(crate) trait RequestIngress: Send + Sync {
    fn publish_conversation_message(
        &self,
        message: ConversationMessage,
    ) -> Result<EventId, SubmitError>;
    fn publish_message(
        &self,
        character: CharacterId,
        input: &UserInput,
    ) -> Result<EventId, SubmitError>;

    fn admit(
        &self,
        character: CharacterId,
        intent: TurnIntent,
    ) -> Result<TimelineAdmission, SubmitError>;
}

pub(crate) struct ChatApiState {
    accepting: Mutex<bool>,
    pub(crate) registry: Arc<RequestRegistry>,
    handlers: HashMap<CharacterId, mpsc::Sender<RequestId>>,
    ingress: Arc<dyn RequestIngress>,
}

impl ChatApiState {
    pub(crate) fn new(
        registry: Arc<RequestRegistry>,
        handlers: HashMap<CharacterId, mpsc::Sender<RequestId>>,
        ingress: Arc<dyn RequestIngress>,
    ) -> Self {
        Self {
            accepting: Mutex::new(true),
            registry,
            handlers,
            ingress,
        }
    }

    pub(crate) fn close(&self) {
        *self.accepting.lock().expect("admission mutex poisoned") = false;
        self.registry.cancel_all();
    }
    pub(crate) fn send(
        &self,
        request: impl GenerationCommand + 'static,
    ) -> Result<RequestHandle, SubmitError> {
        let accepting = self.accepting.lock().expect("admission mutex poisoned");
        if !*accepting {
            return Err(SubmitError::Stopped);
        }
        let handler = self
            .handlers
            .get(&request.character_id())
            .ok_or(SubmitError::CharacterUnavailable)?;
        let permit = handler.try_reserve().map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => SubmitError::QueueFull,
            mpsc::error::TrySendError::Closed(_) => SubmitError::QueueClosed,
        })?;
        let admission = self
            .ingress
            .admit(request.character_id(), request.intent())?;
        let id = RequestId::new();
        let handle = self.registry.register(Box::new(request), id, admission);
        permit.send(id);
        Ok(handle)
    }

    pub(crate) fn publish_conversation_message(
        &self,
        message: ConversationMessage,
    ) -> Result<EventId, SubmitError> {
        if !*self.accepting.lock().expect("admission mutex poisoned") {
            return Err(SubmitError::Stopped);
        }
        self.ingress.publish_conversation_message(message)
    }

    pub(crate) fn publish_message(
        &self,
        character: CharacterId,
        input: UserInput,
    ) -> Result<EventId, SubmitError> {
        let accepting = self.accepting.lock().expect("admission mutex poisoned");
        if !*accepting {
            return Err(SubmitError::Stopped);
        }
        self.ingress.publish_message(character, &input)
    }

    pub(crate) fn observe_requests(&self) -> Result<RequestDiscovery, SubmitError> {
        let accepting = self.accepting.lock().expect("admission mutex poisoned");
        if !*accepting {
            return Err(SubmitError::Stopped);
        }
        Ok(self.registry.observe())
    }

    pub(crate) fn active_request(
        &self,
        id: RequestId,
    ) -> Result<Option<RequestHandle>, SubmitError> {
        if !*self.accepting.lock().expect("admission mutex poisoned") {
            return Err(SubmitError::Stopped);
        }
        Ok(self.registry.get(id).map(|record| record.handle()))
    }

    pub(crate) fn diagnostics(
        &self,
        scope: DiagnosticScope,
    ) -> Result<DiagnosticsAccess, SubmitError> {
        if !*self.accepting.lock().expect("admission mutex poisoned") {
            return Err(SubmitError::Stopped);
        }
        if !self.handlers.contains_key(&scope.character_id()) {
            return Err(SubmitError::CharacterUnavailable);
        }
        Ok(self.registry.diagnostics_access(scope))
    }
}
