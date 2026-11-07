mod clock;
mod model;
pub(crate) mod projection;
mod service;
mod time;
mod tool;

pub use clock::{Clock, ClockFuture, ManualClock, SystemClock};
pub use model::*;
pub(crate) use projection::ReminderProjection;
pub use service::RemindersHandle;
pub(crate) use service::{ReminderService, resolve_requests};
pub use time::{
    AmbiguousPolicy, LocalDateTime, LocalTimePolicy, NonexistentPolicy, TimeError, TimeZone,
    TimeZoneId, UtcInstant, system_timezone,
};
pub(crate) use tool::parse_repeat;
pub use tool::{ReminderTool, parse_friendly_due, parse_structured_due, parse_structured_repeat};

use crate::{
    character::CharacterId,
    identity::EventId,
    runtime::{self, ApiUnavailable},
    timeline::domain::{CausalLink, CausalRelation, EventDraft, EventKind},
};

pub struct RemindersApi;

impl RemindersApi {
    fn handle() -> Result<std::sync::Arc<RemindersHandle>, ReminderError> {
        runtime::current()
            .map(|bindings| std::sync::Arc::clone(&bindings.reminders))
            .map_err(|error| match error {
                ApiUnavailable::Uninitialized => ReminderError::Uninitialized,
                ApiUnavailable::Stopped => ReminderError::Stopped,
            })
    }

    pub async fn create(input: CreateReminder) -> Result<ReminderView, ReminderError> {
        Self::handle()?.create(input).await
    }
    pub async fn create_timer(input: CreateTimer) -> Result<ReminderView, ReminderError> {
        Self::handle()?.create_timer(input).await
    }
    pub async fn update(input: UpdateReminder) -> Result<ReminderView, ReminderError> {
        Self::handle()?.update(input).await
    }
    pub async fn delete(
        character: CharacterId,
        number: u64,
    ) -> Result<ReminderView, ReminderError> {
        Self::handle()?.delete(character, number).await
    }
    pub async fn list(character: CharacterId) -> Result<Vec<ReminderView>, ReminderError> {
        Self::handle()?.list(character).await
    }
    pub async fn set_enabled(enabled: bool) -> Result<(), ReminderError> {
        Self::handle()?.set_enabled(enabled).await
    }
    pub async fn set_timezone(timezone: TimeZoneId) -> Result<(), ReminderError> {
        Self::handle()?.set_timezone(timezone).await
    }
    pub async fn retry(delivery: DeliveryId) -> Result<(), ReminderError> {
        Self::handle()?.retry(delivery).await
    }
    pub async fn now() -> Result<UtcInstant, ReminderError> {
        Self::handle()?.now().await
    }
    pub async fn tick() -> Result<usize, ReminderError> {
        Self::handle()?.tick().await
    }
    pub fn snapshot() -> Result<std::sync::Arc<RemindersSnapshot>, ReminderError> {
        Ok(Self::handle()?.snapshot())
    }
    pub fn observe()
    -> Result<tokio::sync::watch::Receiver<std::sync::Arc<RemindersSnapshot>>, ReminderError> {
        Ok(Self::handle()?.observe())
    }
    pub async fn run_tool(
        character: CharacterId,
        command: ReminderToolCommand,
    ) -> Result<ReminderToolReply, ReminderError> {
        Self::handle()?.run_tool(character, command).await
    }
}

pub(crate) fn change_draft(
    change: &ReminderChange,
    caused_by: Option<EventId>,
    at: UtcInstant,
) -> Result<EventDraft, ReminderError> {
    let (kind, actor, character, payload_key, payload) = match change {
        ReminderChange::Scheduled(record) => (
            EventKind::ReminderScheduled,
            record.character,
            record.character,
            "reminder",
            serde_json::to_value(record).map_err(storage_error)?,
        ),
        ReminderChange::Updated(record) => (
            EventKind::ReminderUpdated,
            record.character,
            record.character,
            "reminder",
            serde_json::to_value(record).map_err(storage_error)?,
        ),
        ReminderChange::Deleted {
            id,
            number,
            character,
        } => (
            EventKind::ReminderDeleted,
            *character,
            *character,
            "deleted",
            serde_json::to_value(ReminderDeletion {
                reminder_id: *id,
                number: *number,
            })
            .map_err(storage_error)?,
        ),
    };
    let mut draft = EventDraft::new(kind);
    draft.actor = Some(actor.to_string());
    draft.observers.insert(character);
    draft.occurred_at_unix_ms = u64::try_from(at.unix_micros() / 1000).unwrap_or(0);
    draft.payload.insert(payload_key.into(), payload);
    if let Some(event_id) = caused_by {
        draft.links.push(CausalLink {
            relation: CausalRelation::CausedBy,
            event_id,
        });
    }
    Ok(draft)
}

pub(crate) fn tool_result_draft(
    character: CharacterId,
    content: &str,
    caused_by: Option<EventId>,
    at: UtcInstant,
) -> Result<EventDraft, ReminderError> {
    let mut draft = EventDraft::new(EventKind::ReminderToolResult);
    draft.actor = Some(character.to_string());
    draft.observers.insert(character);
    draft.occurred_at_unix_ms = u64::try_from(at.unix_micros() / 1000).unwrap_or(0);
    draft.payload.insert(
        "content".into(),
        serde_json::Value::String(content.to_owned()),
    );
    if let Some(event_id) = caused_by {
        draft.links.push(CausalLink {
            relation: CausalRelation::CausedBy,
            event_id,
        });
    }
    Ok(draft)
}

fn storage_error(error: serde_json::Error) -> ReminderError {
    ReminderError::Storage(error.to_string())
}

fn effect_draft(
    kind: EventKind,
    character: CharacterId,
    key: &str,
    value: serde_json::Value,
    at: UtcInstant,
) -> EventDraft {
    let mut draft = EventDraft::new(kind);
    draft.actor = Some(character.to_string());
    draft.observers.insert(character);
    draft.occurred_at_unix_ms = u64::try_from(at.unix_micros() / 1000).unwrap_or(0);
    draft.payload.insert(key.into(), value);
    draft
}

pub(crate) fn fired_draft(
    fired: &FiredOccurrence,
    at: UtcInstant,
) -> Result<EventDraft, ReminderError> {
    Ok(effect_draft(
        EventKind::ReminderFired,
        fired.character,
        "fired",
        serde_json::to_value(fired).map_err(storage_error)?,
        at,
    ))
}

pub(crate) fn failure_draft(
    failure: &DeliveryFailure,
    at: UtcInstant,
) -> Result<EventDraft, ReminderError> {
    Ok(effect_draft(
        EventKind::ReminderDeliveryFailed,
        failure.character,
        "failure",
        serde_json::to_value(failure).map_err(storage_error)?,
        at,
    ))
}

pub(crate) fn settlement_draft(
    settlement: &DeliverySettlement,
    at: UtcInstant,
) -> Result<EventDraft, ReminderError> {
    Ok(effect_draft(
        EventKind::ReminderDeliverySettled,
        settlement.character,
        "settlement",
        serde_json::to_value(settlement).map_err(storage_error)?,
        at,
    ))
}

pub(crate) fn retry_draft(
    retry: &DeliveryRetry,
    at: UtcInstant,
) -> Result<EventDraft, ReminderError> {
    Ok(effect_draft(
        EventKind::ReminderDeliveryRetried,
        retry.character,
        "retry",
        serde_json::to_value(retry).map_err(storage_error)?,
        at,
    ))
}
