use std::sync::{Arc, Mutex};

use tokio::sync::Notify;
use uuid::Uuid;

use crate::{
    character::CharacterRegistry, games::GamePolicy, generation::PresetCatalog,
    request::ChatApiState, settings::SettingsOwner,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationStatus {
    Stopped,
    Starting,
    Running,
    Stopping,
}

pub(crate) struct ApiBindings {
    pub(crate) chat: Arc<ChatApiState>,
    pub(crate) timeline: Arc<Mutex<crate::timeline::SessionTimeline>>,
    pub(crate) characters: CharacterRegistry,
    pub(crate) settings: Arc<SettingsOwner>,
    pub(crate) presets: Arc<PresetCatalog>,
    pub(crate) games: Arc<GamePolicy>,
    pub(crate) reminders: Arc<crate::reminders::RemindersHandle>,
}

struct Slot {
    status: ApplicationStatus,
    owner: Option<Uuid>,
    bindings: Option<Arc<ApiBindings>>,
    initialized: bool,
}

static SLOT: Mutex<Slot> = Mutex::new(Slot {
    status: ApplicationStatus::Stopped,
    owner: None,
    bindings: None,
    initialized: false,
});
static CHANGED: Notify = Notify::const_new();

pub(crate) enum ApiUnavailable {
    Uninitialized,
    Stopped,
}

pub(crate) fn current() -> Result<Arc<ApiBindings>, ApiUnavailable> {
    let slot = SLOT.lock().expect("application slot poisoned");
    if let Some(bindings) = &slot.bindings {
        return Ok(Arc::clone(bindings));
    }
    Err(
        if slot.initialized || slot.status != ApplicationStatus::Stopped {
            ApiUnavailable::Stopped
        } else {
            ApiUnavailable::Uninitialized
        },
    )
}

pub(crate) fn status() -> ApplicationStatus {
    SLOT.lock().expect("application slot poisoned").status
}

pub(crate) async fn wait_stopped() {
    loop {
        let changed = CHANGED.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if status() == ApplicationStatus::Stopped {
            return;
        }
        changed.await;
    }
}

pub(crate) struct RuntimeLease {
    id: Uuid,
}

impl RuntimeLease {
    pub(crate) fn reserve() -> Option<Self> {
        let mut slot = SLOT.lock().expect("application slot poisoned");
        if slot.owner.is_some() {
            return None;
        }
        let id = Uuid::new_v4();
        slot.owner = Some(id);
        slot.status = ApplicationStatus::Starting;
        CHANGED.notify_waiters();
        Some(Self { id })
    }

    pub(crate) fn id(&self) -> Uuid {
        self.id
    }

    pub(crate) fn publish(&self, bindings: Arc<ApiBindings>) {
        let mut slot = SLOT.lock().expect("application slot poisoned");
        assert_eq!(slot.owner, Some(self.id));
        assert_eq!(slot.status, ApplicationStatus::Starting);
        slot.bindings = Some(bindings);
        slot.initialized = true;
        slot.status = ApplicationStatus::Running;
        CHANGED.notify_waiters();
    }
}

pub(crate) fn begin_stopping(id: Uuid) {
    let mut slot = SLOT.lock().expect("application slot poisoned");
    if slot.owner == Some(id) {
        slot.bindings = None;
        slot.status = ApplicationStatus::Stopping;
        CHANGED.notify_waiters();
    }
}

impl Drop for RuntimeLease {
    fn drop(&mut self) {
        let mut slot = SLOT.lock().expect("application slot poisoned");
        if slot.owner == Some(self.id) {
            slot.bindings = None;
            slot.owner = None;
            slot.status = ApplicationStatus::Stopped;
            CHANGED.notify_waiters();
        }
    }
}
