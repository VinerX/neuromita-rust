use std::sync::Arc;

use tokio::sync::watch;

use crate::server::{ServerHandle, ServerSnapshot};

#[derive(Clone)]
pub struct ConnectionsPresenter {
    server: ServerHandle,
}

impl ConnectionsPresenter {
    pub(crate) fn new(server: ServerHandle) -> Self {
        Self { server }
    }

    pub fn snapshot(&self) -> Arc<ServerSnapshot> {
        self.server.snapshot()
    }

    pub fn observe(&self) -> watch::Receiver<Arc<ServerSnapshot>> {
        self.server.observe()
    }
}
