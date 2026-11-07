use std::{collections::HashMap, sync::Arc};

use thiserror::Error;
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

use crate::{
    character::CharacterId,
    request::{
        CancelResult, ChatApiState, DiscoveryError, InitiateRequest, InputError, ReactRequest,
        ReplyRequest, RequestDiscovery, RequestHandle, RequestId, RequestMetadata, RequestSnapshot,
        RequestSource, SubmitError, UserInput,
    },
    settings::SettingsError,
    timeline::{ContextPosition, EventId},
};

#[derive(Debug, Clone)]
pub struct RequestView {
    pub metadata: RequestMetadata,
    pub snapshot: Arc<RequestSnapshot>,
    pub observation_error: Option<Arc<str>>,
}

#[derive(Debug, Clone)]
pub struct ChatModel {
    pub closed: bool,
    pub revision: u64,
    pub requests: Vec<RequestView>,
    pub discovery_error: Option<Arc<str>>,
}

#[derive(Debug, Error)]
pub enum PresenterError {
    #[error(transparent)]
    Input(#[from] InputError),
    #[error(transparent)]
    Submit(#[from] SubmitError),
    #[error("request is no longer active")]
    NotActive,
    #[error(transparent)]
    Settings(#[from] SettingsError),
}

#[derive(Clone)]
pub struct ChatPresenter {
    api: Arc<ChatApiState>,
    model: watch::Receiver<Arc<ChatModel>>,
}

impl ChatPresenter {
    pub fn trace(&self, id: RequestId) -> Option<Arc<crate::request::RequestTrace>> {
        self.api.registry.trace(id)
    }

    pub fn reply(
        &self,
        character: CharacterId,
        text: String,
    ) -> Result<RequestHandle, PresenterError> {
        let input = UserInput::new(text)?;
        let event = self.api.publish_message(character, input)?;
        let request = ReplyRequest::new(character, event, RequestSource::Presenter);
        Ok(self.api.send(request)?)
    }
    pub fn react(
        &self,
        character: CharacterId,
        target: EventId,
        instruction: String,
    ) -> Result<RequestHandle, PresenterError> {
        let request = ReactRequest::new(character, target, instruction, RequestSource::Presenter);
        Ok(self.api.send(request)?)
    }
    pub fn initiate(
        &self,
        character: CharacterId,
        instruction: String,
    ) -> Result<RequestHandle, PresenterError> {
        let request = InitiateRequest::new(character, instruction, RequestSource::Presenter);
        Ok(self.api.send(request)?)
    }
    pub fn reply_at(
        &self,
        character: CharacterId,
        text: String,
        at: ContextPosition,
    ) -> Result<RequestHandle, PresenterError> {
        let event = self.api.publish_message(character, UserInput::new(text)?)?;
        let request = ReplyRequest::new(character, event, RequestSource::Presenter).at(at);
        Ok(self.api.send(request)?)
    }

    pub fn cancel(&self, id: RequestId) -> Result<CancelResult, PresenterError> {
        let handle = self
            .api
            .active_request(id)?
            .ok_or(PresenterError::NotActive)?;
        Ok(handle.cancel())
    }

    pub fn snapshot(&self) -> Arc<ChatModel> {
        Arc::clone(&self.model.borrow())
    }
    pub fn observe(&self) -> watch::Receiver<Arc<ChatModel>> {
        self.model.clone()
    }
}

enum Update {
    Snapshot(Arc<RequestSnapshot>),
    Error(RequestId, Arc<str>),
}

pub(crate) struct PresenterRunner {
    api: Arc<ChatApiState>,
    model: ChatModel,
    publish: watch::Sender<Arc<ChatModel>>,
    observers: JoinSet<()>,
    observing: HashMap<RequestId, RequestHandle>,
    updates: mpsc::Sender<Update>,
    receive: mpsc::Receiver<Update>,
    discovery: RequestDiscovery,
    retained_finished: usize,
}

impl PresenterRunner {
    pub(crate) fn new(
        api: Arc<ChatApiState>,
        discovery: RequestDiscovery,
        capacity: usize,
        retained_finished: usize,
    ) -> (ChatPresenter, Self) {
        let model = ChatModel {
            closed: false,
            revision: 0,
            requests: Vec::new(),
            discovery_error: None,
        };
        let (publish, receive_model) = watch::channel(Arc::new(model.clone()));
        let (updates, receive) = mpsc::channel(capacity);
        let presenter = ChatPresenter {
            api: Arc::clone(&api),
            model: receive_model,
        };
        let runner = Self {
            api,
            model,
            publish,
            observers: JoinSet::new(),
            observing: HashMap::new(),
            updates,
            receive,
            discovery,
            retained_finished,
        };
        (presenter, runner)
    }

    pub(crate) async fn run(mut self, stop: CancellationToken) {
        for handle in std::mem::take(&mut self.discovery.active) {
            self.attach(handle);
        }
        loop {
            tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                update = self.receive.recv() => {
                    if let Some(update) = update { self.apply(update); }
                }
                request = self.discovery.next() => {
                    match request {
                        Ok(handle) => self.attach(handle),
                        Err(DiscoveryError::Closed) => break,
                        Err(error) => {
                            self.model.discovery_error = Some(Arc::from(error.to_string()));
                            self.publish();
                            match self.api.observe_requests() {
                                Ok(mut discovery) => {
                                    for handle in std::mem::take(&mut discovery.active) { self.attach(handle); }
                                    self.discovery = discovery;
                                }
                                Err(_) => break,
                            }
                        }
                    }
                }
                result = self.observers.join_next(), if !self.observers.is_empty() => {
                    if let Some(Err(error)) = result {
                        self.model.discovery_error = Some(Arc::from(format!("request observer failed: {error}")));
                        self.publish();
                    }
                }
            }
        }
        while let Some(request) = self.discovery.try_next() {
            match request {
                Ok(handle) => self.attach(handle),
                Err(error) => self.model.discovery_error = Some(Arc::from(error.to_string())),
            }
        }
        self.observers.abort_all();
        while self.observers.join_next().await.is_some() {}
        for row in &mut self.model.requests {
            if let Some(handle) = self.observing.get(&row.metadata.id) {
                row.snapshot = handle.snapshot();
            }
        }
        self.model.closed = true;
        self.publish();
    }

    fn attach(&mut self, handle: RequestHandle) {
        let id = handle.id();
        if self.observing.contains_key(&id) {
            return;
        }
        self.observing.insert(id, handle.clone());
        let snapshot = handle.snapshot();
        self.model.requests.push(RequestView {
            metadata: snapshot.metadata.clone(),
            snapshot,
            observation_error: None,
        });
        self.publish();
        let updates = self.updates.clone();
        self.observers.spawn(async move {
            let mut observer = handle.observe();
            loop {
                match observer.next().await {
                    Ok(Some(snapshot)) => {
                        if updates.send(Update::Snapshot(snapshot)).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        let _ = updates
                            .send(Update::Error(id, Arc::from(error.to_string())))
                            .await;
                        break;
                    }
                }
            }
        });
    }

    fn apply(&mut self, update: Update) {
        let id = match &update {
            Update::Snapshot(snapshot) => snapshot.metadata.id,
            Update::Error(id, _) => *id,
        };
        if let Some(row) = self
            .model
            .requests
            .iter_mut()
            .find(|row| row.metadata.id == id)
        {
            match update {
                Update::Snapshot(snapshot) => {
                    if snapshot.revision <= row.snapshot.revision {
                        return;
                    }
                    row.snapshot = snapshot;
                }
                Update::Error(_, error) => {
                    row.observation_error = Some(error);
                }
            }
            self.publish();
        }
    }

    fn publish(&mut self) {
        let finished = self
            .model
            .requests
            .iter()
            .filter(|row| row.snapshot.state.is_terminal())
            .count();
        let mut remove = finished.saturating_sub(self.retained_finished);
        self.model.requests.retain(|row| {
            if remove > 0 && row.snapshot.state.is_terminal() {
                remove -= 1;
                self.observing.remove(&row.metadata.id);
                false
            } else {
                true
            }
        });
        self.model.revision += 1;
        self.publish.send_replace(Arc::new(self.model.clone()));
    }
}
