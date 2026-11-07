use std::{fmt, time::Duration};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::{
    clock::Clock,
    time::{LocalDateTime, LocalTimePolicy, TimeError, TimeZoneId, UtcInstant},
};
use crate::{
    character::CharacterId,
    identity::{EventId, RequestId},
    timeline::domain::BranchId,
};

pub const MAX_TEXT_BYTES: usize = 8 * 1024;
pub const MAX_REMINDERS: usize = 128;
pub const MAX_DELIVERY_HISTORY: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReminderId(pub Uuid);

impl ReminderId {
    pub(crate) fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for ReminderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeliveryId(pub EventId);

impl fmt::Display for DeliveryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderKind {
    Reminder,
    Timer,
}

impl ReminderKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Reminder => "reminder",
            Self::Timer => "timer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RepeatRule {
    Once,
    EverySeconds { seconds: u64 },
    EveryLocalDays { days: u32 },
}

impl RepeatRule {
    pub fn validate(self) -> Result<(), String> {
        match self {
            Self::Once => Ok(()),
            Self::EverySeconds { seconds: 0 } => Err("repeat interval must be positive".into()),
            Self::EverySeconds { seconds } if seconds > 366 * 24 * 60 * 60 => {
                Err("repeat interval is too large".into())
            }
            Self::EveryLocalDays { days: 0 } => Err("repeat day count must be positive".into()),
            Self::EveryLocalDays { days } if days > 3660 => {
                Err("repeat day count is too large".into())
            }
            Self::EverySeconds { .. } | Self::EveryLocalDays { .. } => Ok(()),
        }
    }
    pub fn label(self) -> String {
        match self {
            Self::Once => "once".into(),
            Self::EverySeconds { seconds } => format!("every {seconds}s"),
            Self::EveryLocalDays { days } => format!("every {days} local day(s)"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DueSpec {
    After(Duration),
    Utc(UtcInstant),
    Local {
        local: LocalDateTime,
        zone: TimeZoneId,
    },
}

impl DueSpec {
    pub fn local(local: LocalDateTime, zone: TimeZoneId) -> Self {
        Self::Local { local, zone }
    }
    pub fn after(duration: Duration) -> Result<Self, ReminderError> {
        if duration.is_zero() {
            return Err(ReminderError::Invalid("delay must be positive".into()));
        }
        if duration > Duration::from_secs(366 * 24 * 60 * 60) {
            return Err(ReminderError::Invalid(
                "delay is longer than one year".into(),
            ));
        }
        Ok(Self::After(duration))
    }
}

#[derive(Debug, Clone)]
pub struct CreateReminder {
    pub character: CharacterId,
    pub kind: ReminderKind,
    pub text: String,
    pub due: DueSpec,
    pub repeat: RepeatRule,
    pub policy: LocalTimePolicy,
}

impl CreateReminder {
    pub fn new(character: CharacterId, text: impl Into<String>, due: DueSpec) -> Self {
        Self {
            character,
            kind: ReminderKind::Reminder,
            text: text.into(),
            due,
            repeat: RepeatRule::Once,
            policy: LocalTimePolicy::LENIENT,
        }
    }
    pub fn kind(mut self, kind: ReminderKind) -> Self {
        self.kind = kind;
        self
    }
    pub fn repeat(mut self, repeat: RepeatRule) -> Self {
        self.repeat = repeat;
        self
    }
    pub fn policy(mut self, policy: LocalTimePolicy) -> Self {
        self.policy = policy;
        self
    }
    pub fn validate(&self) -> Result<(), ReminderError> {
        validate_text(&self.text)?;
        self.repeat.validate().map_err(ReminderError::Invalid)?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct CreateTimer {
    pub character: CharacterId,
    pub instruction: String,
    pub delay: Duration,
}

impl CreateTimer {
    pub fn new(character: CharacterId, instruction: impl Into<String>, delay: Duration) -> Self {
        Self {
            character,
            instruction: instruction.into(),
            delay,
        }
    }
    pub fn validate(&self) -> Result<(), ReminderError> {
        validate_text(&self.instruction)?;
        if self.delay.is_zero() || self.delay > Duration::from_secs(366 * 24 * 60 * 60) {
            return Err(ReminderError::Invalid(
                "timer delay must be positive and no longer than one year".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct UpdateReminder {
    pub character: CharacterId,
    pub number: u64,
    pub expected_revision: Option<u64>,
    pub text: Option<String>,
    pub due: Option<DueSpec>,
    pub repeat: Option<RepeatRule>,
    pub enabled: Option<bool>,
    pub policy: Option<LocalTimePolicy>,
}

impl UpdateReminder {
    pub fn new(character: CharacterId, number: u64) -> Self {
        Self {
            character,
            number,
            expected_revision: None,
            text: None,
            due: None,
            repeat: None,
            enabled: None,
            policy: None,
        }
    }
    pub fn expected_revision(mut self, revision: u64) -> Self {
        self.expected_revision = Some(revision);
        self
    }
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }
    pub fn due(mut self, due: DueSpec) -> Self {
        self.due = Some(due);
        self
    }
    pub fn repeat(mut self, repeat: RepeatRule) -> Self {
        self.repeat = Some(repeat);
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = Some(enabled);
        self
    }
    pub fn policy(mut self, policy: LocalTimePolicy) -> Self {
        self.policy = Some(policy);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReminderRecord {
    pub id: ReminderId,
    pub number: u64,
    pub character: CharacterId,
    pub kind: ReminderKind,
    pub text: String,
    pub due: UtcInstant,
    pub due_local: LocalDateTime,
    pub zone: TimeZoneId,
    pub repeat: RepeatRule,
    pub policy: LocalTimePolicy,
    pub enabled: bool,
    pub active: bool,
    pub revision: u64,
    pub schedule_revision: u64,
    pub next_occurrence: u64,
    pub created_at: UtcInstant,
    pub updated_at: UtcInstant,
}

impl ReminderRecord {
    pub fn validate(&self) -> Result<(), String> {
        if self.number == 0 {
            return Err("reminder number must be positive".into());
        }
        validate_text(&self.text).map_err(|error| error.to_string())?;
        self.zone
            .zone()
            .id()
            .eq_ignore_ascii_case(self.zone.as_str())
            .then_some(())
            .ok_or_else(|| "invalid time zone".to_owned())?;
        self.repeat.validate()?;
        if self.revision == 0 || self.schedule_revision == 0 {
            return Err("reminder revision must be positive".into());
        }
        Ok(())
    }
    pub fn summary(&self) -> String {
        format!(
            "#{} [{}] {} | due {} | {}{}",
            self.number,
            self.kind.label(),
            self.text,
            self.due_local,
            self.zone,
            match self.repeat {
                RepeatRule::Once => String::new(),
                other => format!(", {}", other.label()),
            }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NextOccurrence {
    pub occurrence: u64,
    pub due: UtcInstant,
    pub due_local: LocalDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FiredOccurrence {
    pub reminder_id: ReminderId,
    pub occurrence: u64,
    pub schedule_revision: u64,
    pub character: CharacterId,
    pub kind: ReminderKind,
    pub text: String,
    pub scheduled_for: UtcInstant,
    pub fired_at: UtcInstant,
    pub next: Option<NextOccurrence>,
}

impl FiredOccurrence {
    pub fn delivery_id(&self, event_id: EventId) -> DeliveryId {
        DeliveryId(event_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryFailure {
    pub fired_event_id: EventId,
    pub character: CharacterId,
    /// Monotonic delivery generation; a user retry starts the next one.
    pub generation: u32,
    /// Monotonic attempt counter for this occurrence; never reset by a retry.
    pub attempt: u32,
    pub error: String,
    pub next_attempt: UtcInstant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryOutcome {
    Delivered,
    Cancelled,
    Blocked,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliverySettlement {
    pub fired_event_id: EventId,
    pub character: CharacterId,
    pub generation: u32,
    pub outcome: DeliveryOutcome,
    pub reason: String,
    pub settled_at: UtcInstant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryRetry {
    pub fired_event_id: EventId,
    pub character: CharacterId,
    /// The generation that starts with this retry.
    pub generation: u32,
    pub at: UtcInstant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReminderChange {
    Scheduled(ReminderRecord),
    Updated(ReminderRecord),
    Deleted {
        id: ReminderId,
        number: u64,
        character: CharacterId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReminderDeletion {
    pub reminder_id: ReminderId,
    pub number: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReminderRequest {
    StructuredAdd {
        due_iso: String,
        text: String,
    },
    StructuredTimer {
        delay_seconds: f64,
        instruction: String,
    },
    StructuredDelete {
        number: u64,
    },
    Tool(ReminderToolCommand),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReminderToolCommand {
    List,
    Add {
        text: String,
        due: String,
        repeat: Option<RepeatRule>,
    },
    Timer {
        instruction: String,
        delay_seconds: f64,
    },
    Delete {
        number: u64,
    },
    Update {
        number: u64,
        text: Option<String>,
        due: Option<String>,
        repeat: Option<RepeatRule>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderToolReply {
    pub ok: bool,
    pub text: String,
}

impl ReminderToolReply {
    pub(crate) fn ok(text: impl Into<String>) -> Self {
        Self {
            ok: true,
            text: text.into(),
        }
    }
    pub(crate) fn error(text: impl Into<String>) -> Self {
        Self {
            ok: false,
            text: text.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Pending,
    Retrying,
    Delivered,
    Cancelled,
    Blocked,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryView {
    pub character: CharacterId,
    pub id: DeliveryId,
    pub reminder_id: ReminderId,
    pub occurrence: u64,
    pub fired_event_id: EventId,
    pub state: DeliveryState,
    /// Lifetime attempt counter for the occurrence; never reset by a retry.
    pub attempts: u32,
    /// Attempts spent in the current delivery generation, used for the
    /// bounded transport retry budget.
    pub transport_attempts: u32,
    pub next_attempt: Option<UtcInstant>,
    pub last_error: Option<String>,
    pub request_id: Option<RequestId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderView {
    pub id: ReminderId,
    pub number: u64,
    pub character: CharacterId,
    pub kind: ReminderKind,
    pub text: String,
    pub due_utc: UtcInstant,
    pub due_local: String,
    pub zone: TimeZoneId,
    pub repeat: RepeatRule,
    pub enabled: bool,
    pub active: bool,
    pub revision: u64,
    pub next_occurrence: u64,
    pub delivery: Option<DeliveryView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderPrompt {
    pub number: u64,
    pub kind: ReminderKind,
    pub text: String,
    pub due_local: String,
}

impl ReminderView {
    pub fn summary(&self) -> String {
        format!(
            "#{} [{}] {} | due {}{}",
            self.number,
            self.kind.label(),
            self.text,
            self.due_local,
            match self.repeat {
                RepeatRule::Once => String::new(),
                other => format!(", {}", other.label()),
            }
        )
    }
}

#[derive(Debug, Clone)]
pub struct RemindersSnapshot {
    pub closed: bool,
    pub branch_id: BranchId,
    pub enabled: bool,
    pub timezone: TimeZoneId,
    pub next_number: u64,
    pub reminders: Vec<ReminderView>,
    pub deliveries: Vec<DeliveryView>,
}

#[derive(Debug, Clone, Copy)]
pub struct DeliveryPolicy {
    pub transport_retry_initial: Duration,
    pub transport_retry_max: Duration,
    pub transport_retry_limit: u32,
}

impl Default for DeliveryPolicy {
    fn default() -> Self {
        Self {
            transport_retry_initial: Duration::from_secs(2),
            transport_retry_max: Duration::from_secs(60),
            transport_retry_limit: 10,
        }
    }
}

#[derive(Clone)]
pub struct ReminderConfig {
    pub clock: std::sync::Arc<dyn Clock>,
    pub delivery: DeliveryPolicy,
    pub start_scheduler: bool,
    pub idle_poll: Duration,
}

impl ReminderConfig {
    pub fn system() -> Self {
        Self {
            clock: std::sync::Arc::new(super::clock::SystemClock),
            delivery: DeliveryPolicy::default(),
            start_scheduler: true,
            idle_poll: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, Error)]
pub enum ReminderError {
    #[error("reminders are not initialized")]
    Uninitialized,
    #[error("reminders are stopping")]
    Stopped,
    #[error("the reminder scheduler is disabled")]
    Disabled,
    #[error("invalid reminder: {0}")]
    Invalid(String),
    #[error("due time {due} is not in the future")]
    Past { due: String },
    #[error(transparent)]
    Time(#[from] TimeError),
    #[error("reminder #{number} was not found for {character}")]
    NotFound { character: CharacterId, number: u64 },
    #[error("delivery {0} was not found")]
    DeliveryNotFound(DeliveryId),
    #[error("delivery {delivery} is {state:?} and cannot be retried")]
    InvalidState {
        delivery: DeliveryId,
        state: DeliveryState,
    },
    #[error("reminder state changed: expected revision {expected}, actual {actual}")]
    Conflict { expected: u64, actual: u64 },
    #[error("reminder branch is not selected")]
    BranchUnavailable,
    #[error("reminder storage: {0}")]
    Storage(String),
    #[error("reminder service failed: {0}")]
    Service(String),
}

impl ReminderError {
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Uninitialized | Self::Stopped | Self::Service(_))
    }
}

fn validate_text(text: &str) -> Result<(), ReminderError> {
    if text.trim().is_empty() {
        return Err(ReminderError::Invalid(
            "reminder text must not be empty".into(),
        ));
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err(ReminderError::Invalid("reminder text is too large".into()));
    }
    Ok(())
}
