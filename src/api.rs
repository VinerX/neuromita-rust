use std::sync::Arc;

use tokio::sync::watch;

use crate::{
    character::{CharacterError, CharacterHandle, CharacterId},
    generation::{PresetCatalogError, PresetId, PresetRecord},
    interaction::UserInput,
    request::{
        DiagnosticScope, DiagnosticsAccess, GenerationCommand, InitiateRequest, ReactRequest,
        ReplyRequest, RequestDiscovery, RequestHandle, RequestId, SubmitError,
    },
    runtime::{self, ApiUnavailable},
    settings::{
        SectionSnapshot, SettingsApi, SettingsError, SettingsImportFormat, SettingsOwner,
        SettingsSection, SettingsSnapshot, SettingsTransfer, SettingsTransferError,
    },
    timeline::EventId,
};

pub struct CharacterApi;
impl CharacterApi {
    pub fn get(id: CharacterId) -> Result<CharacterHandle, CharacterError> {
        runtime::current()
            .ok()
            .and_then(|bindings| bindings.characters.get(id))
            .ok_or(CharacterError::Unavailable)
    }
}

pub struct PresetApi;

impl PresetApi {
    fn catalog() -> Result<Arc<crate::generation::PresetCatalog>, PresetCatalogError> {
        runtime::current()
            .map(|bindings| Arc::clone(&bindings.presets))
            .map_err(|_| PresetCatalogError::Unavailable)
    }

    pub fn list() -> Result<Vec<(PresetId, PresetRecord)>, PresetCatalogError> {
        Ok(Self::catalog()?.list())
    }

    pub fn create(record: PresetRecord) -> Result<PresetId, PresetCatalogError> {
        Self::catalog()?.create(record)
    }

    pub fn update(id: PresetId, record: PresetRecord) -> Result<(), PresetCatalogError> {
        Self::catalog()?.update(id, record)
    }

    pub fn delete(id: PresetId) -> Result<(), PresetCatalogError> {
        Self::catalog()?.delete(id)
    }
}

pub struct ChatApi;

impl ChatApi {
    pub fn publish_conversation_message(
        message: crate::timeline::conversation::ConversationMessage,
    ) -> Result<crate::identity::EventId, SubmitError> {
        runtime::current()
            .map_err(SubmitError::from)?
            .chat
            .publish_conversation_message(message)
    }

    pub fn publish_message(
        character: CharacterId,
        input: UserInput,
    ) -> Result<EventId, SubmitError> {
        runtime::current()
            .map_err(SubmitError::from)?
            .chat
            .publish_message(character, input)
    }

    pub fn reply(request: ReplyRequest) -> Result<RequestHandle, SubmitError> {
        Self::submit(request)
    }
    pub fn react(request: ReactRequest) -> Result<RequestHandle, SubmitError> {
        Self::submit(request)
    }
    pub fn initiate(request: InitiateRequest) -> Result<RequestHandle, SubmitError> {
        Self::submit(request)
    }
    fn submit(request: impl GenerationCommand + 'static) -> Result<RequestHandle, SubmitError> {
        runtime::current()
            .map_err(SubmitError::from)?
            .chat
            .send(request)
    }
    pub fn observe_requests() -> Result<RequestDiscovery, SubmitError> {
        runtime::current()
            .map_err(SubmitError::from)?
            .chat
            .observe_requests()
    }
    pub fn active_request(id: RequestId) -> Result<Option<RequestHandle>, SubmitError> {
        runtime::current()
            .map_err(SubmitError::from)?
            .chat
            .active_request(id)
    }
    pub fn trace(id: RequestId) -> Result<Option<Arc<crate::request::RequestTrace>>, SubmitError> {
        Ok(runtime::current()
            .map_err(SubmitError::from)?
            .chat
            .registry
            .trace(id))
    }

    /// Read diagnostics for one character. Pass an owner scope when caller
    /// identity matters; a missing request is reported, never replaced by a
    /// different trace.
    pub fn diagnostics(character: CharacterId) -> Result<DiagnosticsAccess, SubmitError> {
        Self::diagnostics_for(DiagnosticScope::character(character))
    }

    pub fn diagnostics_for(scope: DiagnosticScope) -> Result<DiagnosticsAccess, SubmitError> {
        runtime::current()
            .map_err(SubmitError::from)?
            .chat
            .diagnostics(scope)
    }
}

impl From<ApiUnavailable> for SubmitError {
    fn from(error: ApiUnavailable) -> Self {
        match error {
            ApiUnavailable::Uninitialized => Self::Uninitialized,
            ApiUnavailable::Stopped => Self::Stopped,
        }
    }
}

impl SettingsApi {
    fn owner() -> Result<Arc<SettingsOwner>, SettingsError> {
        Ok(Arc::clone(
            &runtime::current().map_err(SettingsError::from)?.settings,
        ))
    }
    pub fn snapshot<T: SettingsSection>() -> Result<SectionSnapshot<T>, SettingsError> {
        Self::owner()?.snapshot()
    }
    pub fn snapshot_all() -> Result<Arc<SettingsSnapshot>, SettingsError> {
        Self::owner()?.snapshot_all()
    }
    pub fn observe() -> Result<watch::Receiver<Arc<SettingsSnapshot>>, SettingsError> {
        Self::owner()?.observe()
    }
    /// Pure preparation, also available before the Core is started.
    pub fn prepare_import(
        format: SettingsImportFormat,
        bytes: &[u8],
    ) -> Result<SettingsTransfer, SettingsTransferError> {
        SettingsTransfer::prepare(format, bytes)
    }
    pub fn export() -> Result<SettingsTransfer, SettingsError> {
        Self::owner()?.export()
    }
    pub fn import(
        expected_revision: u64,
        transfer: SettingsTransfer,
    ) -> Result<Arc<SettingsSnapshot>, SettingsError> {
        Self::owner()?.import(expected_revision, transfer)
    }
    pub fn replace<T: SettingsSection>(
        expected_revision: u64,
        value: T,
    ) -> Result<SectionSnapshot<T>, SettingsError> {
        Self::owner()?.replace(expected_revision, value)
    }
    /// The edit must be short and must not block or re-enter SettingsApi.
    pub fn edit<T: SettingsSection>(
        edit: impl FnOnce(&mut T),
    ) -> Result<SectionSnapshot<T>, SettingsError> {
        Self::owner()?.edit(edit)
    }
    pub async fn flush() -> Result<(), SettingsError> {
        Self::owner()?.flush().await
    }
    pub fn retry_save() -> Result<(), SettingsError> {
        Self::owner()?.request_save()
    }
}

impl From<ApiUnavailable> for SettingsError {
    fn from(error: ApiUnavailable) -> Self {
        match error {
            ApiUnavailable::Uninitialized => Self::Uninitialized,
            ApiUnavailable::Stopped => Self::Stopped,
        }
    }
}
