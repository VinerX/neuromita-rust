use std::collections::{BTreeMap, HashSet};

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::{
    model::{
        DeliveryFailure, DeliveryOutcome, DeliveryRetry, DeliverySettlement, DeliveryState,
        DeliveryView, DueSpec, FiredOccurrence, ReminderChange, ReminderError, ReminderId,
        ReminderKind, ReminderPrompt, ReminderRecord, ReminderView, RepeatRule, UpdateReminder,
    },
    time::{LocalDateTime, LocalTimePolicy, TimeZoneId, UtcInstant},
};
use crate::{
    character::CharacterId,
    identity::EventId,
    timeline::domain::{BranchId, CausalRelation, EventKind, TimelineFact},
};

#[derive(Debug, Clone)]
pub(crate) struct DeliveryEntry {
    fired_event_id: EventId,
    sequence: u64,
    fired: FiredOccurrence,
    state: DeliveryState,
    attempts: u32,
    retries: u32,
    generation: u32,
    generation_attempts: u32,
    next_attempt: Option<UtcInstant>,
    last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ReminderProjection {
    pub(crate) character: CharacterId,
    records: BTreeMap<ReminderId, ReminderRecord>,
    deliveries: BTreeMap<EventId, DeliveryEntry>,
    delivered_replies: HashSet<EventId>,
    next_number: u64,
}

impl ReminderProjection {
    pub(crate) fn empty(_branch: BranchId, character: CharacterId) -> Self {
        Self {
            character,
            records: BTreeMap::new(),
            deliveries: BTreeMap::new(),
            delivered_replies: HashSet::new(),
            next_number: 1,
        }
    }

    pub(crate) fn from_facts(
        branch: BranchId,
        character: CharacterId,
        facts: &[TimelineFact],
    ) -> Result<Self, ReminderError> {
        let mut projection = Self::empty(branch, character);
        for fact in facts {
            projection.apply_fact(fact)?;
        }
        projection.finalize_deliveries();
        Ok(projection)
    }

    fn apply_fact(&mut self, fact: &TimelineFact) -> Result<(), ReminderError> {
        if !fact.draft.observers.contains(&self.character) {
            return Ok(());
        }
        match fact.draft.kind {
            EventKind::ReminderScheduled | EventKind::ReminderUpdated => {
                let record: ReminderRecord = payload(&fact.draft.payload, "reminder")?;
                if record.character != self.character {
                    return Err(ReminderError::Storage(
                        "reminder fact belongs to another character".into(),
                    ));
                }
                record.validate().map_err(ReminderError::Storage)?;
                self.next_number = self.next_number.max(record.number.saturating_add(1));
                self.records.insert(record.id, record);
            }
            EventKind::ReminderDeleted => {
                let deletion: super::model::ReminderDeletion =
                    payload(&fact.draft.payload, "deleted")?;
                self.records.remove(&deletion.reminder_id);
            }
            EventKind::ReminderFired => {
                let fired: FiredOccurrence = payload(&fact.draft.payload, "fired")?;
                if fired.character != self.character {
                    return Err(ReminderError::Storage(
                        "fired reminder belongs to another character".into(),
                    ));
                }
                if let Some(record) = self.records.get_mut(&fired.reminder_id)
                    && record.schedule_revision == fired.schedule_revision
                {
                    if let Some(next) = &fired.next {
                        record.due = next.due;
                        record.due_local = next.due_local;
                        record.next_occurrence = next.occurrence;
                        record.active = true;
                    } else {
                        record.next_occurrence = fired.occurrence.saturating_add(1);
                        record.active = false;
                    }
                }
                self.deliveries.insert(
                    fact.draft.id,
                    DeliveryEntry {
                        fired_event_id: fact.draft.id,
                        sequence: fact.sequence,
                        fired,
                        state: DeliveryState::Pending,
                        attempts: 0,
                        retries: 0,
                        generation: 0,
                        generation_attempts: 0,
                        next_attempt: None,
                        last_error: None,
                    },
                );
            }
            EventKind::ReminderDeliveryFailed => {
                let failure: DeliveryFailure = payload(&fact.draft.payload, "failure")?;
                if let Some(entry) = self.deliveries.get_mut(&failure.fired_event_id) {
                    entry.state = DeliveryState::Retrying;
                    entry.attempts = failure.attempt;
                    entry.generation = failure.generation;
                    entry.generation_attempts = entry.generation_attempts.saturating_add(1);
                    entry.next_attempt = Some(failure.next_attempt);
                    entry.last_error = Some(failure.error);
                }
            }
            EventKind::ReminderDeliverySettled => {
                let settlement: DeliverySettlement = payload(&fact.draft.payload, "settlement")?;
                if let Some(entry) = self.deliveries.get_mut(&settlement.fired_event_id) {
                    entry.state = match settlement.outcome {
                        DeliveryOutcome::Delivered => DeliveryState::Delivered,
                        DeliveryOutcome::Cancelled => DeliveryState::Cancelled,
                        DeliveryOutcome::Blocked => DeliveryState::Blocked,
                        DeliveryOutcome::Superseded => DeliveryState::Superseded,
                    };
                    entry.generation = settlement.generation;
                    entry.next_attempt = None;
                    entry.last_error = Some(settlement.reason);
                }
            }
            EventKind::ReminderDeliveryRetried => {
                let retry: DeliveryRetry = payload(&fact.draft.payload, "retry")?;
                if let Some(entry) = self.deliveries.get_mut(&retry.fired_event_id) {
                    entry.state = DeliveryState::Pending;
                    entry.retries = entry.retries.saturating_add(1);
                    entry.generation = retry.generation;
                    entry.generation_attempts = 0;
                    entry.next_attempt = Some(retry.at);
                    entry.last_error = None;
                }
            }
            EventKind::CharacterMessage => {
                for link in &fact.draft.links {
                    if link.relation == CausalRelation::ReactTo {
                        self.delivered_replies.insert(link.event_id);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn finalize_deliveries(&mut self) {
        for entry in self.deliveries.values_mut() {
            if self.delivered_replies.contains(&entry.fired_event_id) {
                entry.state = DeliveryState::Delivered;
                entry.next_attempt = None;
                continue;
            }
            if matches!(
                entry.state,
                DeliveryState::Pending | DeliveryState::Retrying | DeliveryState::Blocked
            ) {
                let valid = self
                    .records
                    .get(&entry.fired.reminder_id)
                    .is_some_and(|record| {
                        record.schedule_revision == entry.fired.schedule_revision
                    });
                if !valid {
                    entry.state = DeliveryState::Superseded;
                    entry.next_attempt = None;
                }
            }
        }
    }

    pub(crate) fn record_by_number(&self, number: u64) -> Option<&ReminderRecord> {
        self.records.values().find(|record| record.number == number)
    }

    pub(crate) fn record_by_id(&self, id: ReminderId) -> Option<&ReminderRecord> {
        self.records.get(&id)
    }

    pub(crate) fn next_number(&self) -> u64 {
        self.next_number
    }

    pub(crate) fn next_due(&self) -> Option<UtcInstant> {
        self.records
            .values()
            .filter(|record| record.active && record.enabled)
            .filter(|record| !self.has_unsettled_delivery(record.id))
            .map(|record| record.due)
            .min()
    }

    pub(crate) fn due_records(&self, now: UtcInstant) -> Vec<ReminderRecord> {
        let mut due: Vec<_> = self
            .records
            .values()
            .filter(|record| record.active && record.enabled && record.due <= now)
            .filter(|record| !self.has_unsettled_delivery(record.id))
            .cloned()
            .collect();
        due.sort_by_key(|record| (record.due, record.number));
        due
    }

    pub(crate) fn has_unsettled_delivery(&self, reminder: ReminderId) -> bool {
        self.deliveries.values().any(|entry| {
            entry.fired.reminder_id == reminder
                && matches!(
                    entry.state,
                    DeliveryState::Pending | DeliveryState::Retrying | DeliveryState::Blocked
                )
        })
    }

    /// Fired occurrences of one reminder whose delivery is still running.
    pub(crate) fn unsettled_deliveries(&self, reminder: ReminderId) -> Vec<EventId> {
        self.deliveries
            .values()
            .filter(|entry| {
                entry.fired.reminder_id == reminder
                    && matches!(
                        entry.state,
                        DeliveryState::Pending | DeliveryState::Retrying
                    )
            })
            .map(|entry| entry.fired_event_id)
            .collect()
    }

    pub(crate) fn pending_deliveries(&self, now: UtcInstant) -> Vec<DeliveryView> {
        let mut pending: Vec<_> = self
            .deliveries
            .values()
            .filter(|entry| {
                matches!(
                    entry.state,
                    DeliveryState::Pending | DeliveryState::Retrying
                ) && entry.next_attempt.is_none_or(|at| at <= now)
            })
            .map(|entry| self.delivery_view(entry))
            .collect();
        pending.sort_by_key(|delivery| (delivery.next_attempt, delivery.occurrence));
        pending
    }

    pub(crate) fn delivery_for(&self, reminder: ReminderId) -> Option<DeliveryView> {
        self.deliveries
            .values()
            .filter(|entry| entry.fired.reminder_id == reminder)
            .max_by_key(|entry| entry.sequence)
            .map(|entry| self.delivery_view(entry))
    }

    pub(crate) fn delivery_by_event(&self, event: EventId) -> Option<DeliveryView> {
        self.deliveries
            .get(&event)
            .map(|entry| self.delivery_view(entry))
    }

    pub(crate) fn fired_of(&self, event: EventId) -> Option<FiredOccurrence> {
        self.deliveries.get(&event).map(|entry| entry.fired.clone())
    }

    pub(crate) fn retry_count(&self, event: EventId) -> u32 {
        self.deliveries.get(&event).map_or(0, |entry| entry.retries)
    }

    pub(crate) fn generation_of(&self, event: EventId) -> Option<u32> {
        self.deliveries.get(&event).map(|entry| entry.generation)
    }

    pub(crate) fn delivery_views(&self) -> Vec<DeliveryView> {
        let mut views: Vec<_> = self
            .deliveries
            .values()
            .map(|entry| self.delivery_view(entry))
            .collect();
        views.sort_by_key(|delivery| delivery.occurrence);
        views
    }

    pub(crate) fn delivery_view(&self, entry: &DeliveryEntry) -> DeliveryView {
        DeliveryView {
            character: self.character,
            id: super::model::DeliveryId(entry.fired_event_id),
            reminder_id: entry.fired.reminder_id,
            occurrence: entry.fired.occurrence,
            fired_event_id: entry.fired_event_id,
            state: entry.state,
            attempts: entry.attempts,
            transport_attempts: entry.generation_attempts,
            next_attempt: entry.next_attempt,
            last_error: entry.last_error.clone(),
            request_id: None,
        }
    }

    pub(crate) fn view(&self, record: &ReminderRecord) -> ReminderView {
        let zone = record.zone.zone();
        ReminderView {
            id: record.id,
            number: record.number,
            character: record.character,
            kind: record.kind,
            text: record.text.clone(),
            due_utc: record.due,
            due_local: record.due.local_text(&zone),
            zone: record.zone.clone(),
            repeat: record.repeat,
            enabled: record.enabled,
            active: record.active,
            revision: record.revision,
            next_occurrence: record.next_occurrence,
            delivery: self.delivery_for(record.id),
        }
    }

    pub(crate) fn reminder_views(&self) -> Vec<ReminderView> {
        let mut views: Vec<_> = self
            .records
            .values()
            .filter(|record| record.active)
            .map(|record| self.view(record))
            .collect();
        views.sort_by_key(|view| view.number);
        views
    }

    pub(crate) fn prompt_reminders(&self) -> Vec<ReminderPrompt> {
        self.reminder_views()
            .into_iter()
            .filter(|view| view.enabled)
            .map(|view| ReminderPrompt {
                number: view.number,
                kind: view.kind,
                text: view.text,
                due_local: view.due_local,
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn apply_create(
        &mut self,
        character: CharacterId,
        kind: ReminderKind,
        text: String,
        due: DueSpec,
        repeat: RepeatRule,
        policy: LocalTimePolicy,
        default_zone: &TimeZoneId,
        now: UtcInstant,
    ) -> Result<ReminderRecord, ReminderError> {
        if character != self.character {
            return Err(ReminderError::Invalid(
                "reminder character does not match projection".into(),
            ));
        }
        validate_new_text(&text)?;
        repeat.validate().map_err(ReminderError::Invalid)?;
        let occupied = self
            .records
            .values()
            .filter(|record| record.active || self.has_unsettled_delivery(record.id))
            .count();
        if occupied >= super::model::MAX_REMINDERS {
            return Err(ReminderError::Invalid("too many active reminders".into()));
        }
        let (due, due_local, zone) = resolve_due(due, now, policy, default_zone)?;
        if due <= now {
            return Err(ReminderError::Past {
                due: due.to_rfc3339(),
            });
        }
        let number = self.next_number;
        let record = ReminderRecord {
            id: ReminderId::new(),
            number,
            character,
            kind,
            text,
            due,
            due_local,
            zone,
            repeat,
            policy,
            enabled: true,
            active: true,
            revision: 1,
            schedule_revision: 1,
            next_occurrence: 0,
            created_at: now,
            updated_at: now,
        };
        record.validate().map_err(ReminderError::Invalid)?;
        self.records.insert(record.id, record.clone());
        self.next_number = number.saturating_add(1);
        Ok(record)
    }

    pub(crate) fn apply_update(
        &mut self,
        input: UpdateReminder,
        default_zone: &TimeZoneId,
        now: UtcInstant,
    ) -> Result<ReminderRecord, ReminderError> {
        if input.character != self.character {
            return Err(ReminderError::Invalid(
                "reminder character does not match projection".into(),
            ));
        }
        let existing =
            self.record_by_number(input.number)
                .cloned()
                .ok_or(ReminderError::NotFound {
                    character: self.character,
                    number: input.number,
                })?;
        if let Some(expected) = input.expected_revision
            && expected != existing.revision
        {
            return Err(ReminderError::Conflict {
                expected,
                actual: existing.revision,
            });
        }
        let text = input.text.clone().unwrap_or(existing.text);
        validate_new_text(&text)?;
        let policy = input.policy.unwrap_or(existing.policy);
        let schedule_changed = input.due.is_some()
            || input.repeat.is_some()
            || input.policy.is_some_and(|value| value != existing.policy)
            || !existing.active;
        let (due, due_local, zone) = match input.due {
            Some(due) => resolve_due(due, now, policy, default_zone)?,
            None => (existing.due, existing.due_local, existing.zone.clone()),
        };
        let reenabled = input.enabled == Some(true) && !existing.enabled;
        let has_pending = self.has_unsettled_delivery(existing.id);
        if due <= now && (schedule_changed || (reenabled && !has_pending)) {
            return Err(ReminderError::Past {
                due: due.to_rfc3339(),
            });
        }
        let repeat = input.repeat.unwrap_or(existing.repeat);
        repeat.validate().map_err(ReminderError::Invalid)?;
        let record = ReminderRecord {
            id: existing.id,
            number: existing.number,
            character: existing.character,
            kind: existing.kind,
            text,
            due,
            due_local,
            zone,
            repeat,
            policy,
            enabled: input.enabled.unwrap_or(existing.enabled),
            active: true,
            revision: existing.revision.saturating_add(1),
            schedule_revision: existing
                .schedule_revision
                .saturating_add(u64::from(schedule_changed || !existing.active)),
            next_occurrence: existing.next_occurrence,
            created_at: existing.created_at,
            updated_at: now,
        };
        record.validate().map_err(ReminderError::Invalid)?;
        self.records.insert(record.id, record.clone());
        Ok(record)
    }

    pub(crate) fn apply_delete(
        &mut self,
        character: CharacterId,
        number: u64,
    ) -> Result<ReminderRecord, ReminderError> {
        if character != self.character {
            return Err(ReminderError::Invalid(
                "reminder character does not match projection".into(),
            ));
        }
        let record = self
            .record_by_number(number)
            .cloned()
            .ok_or(ReminderError::NotFound { character, number })?;
        self.records.remove(&record.id);
        Ok(record)
    }

    pub(crate) fn apply_change(&mut self, change: ReminderChange) -> Result<(), ReminderError> {
        match change {
            ReminderChange::Scheduled(record) | ReminderChange::Updated(record) => {
                record.validate().map_err(ReminderError::Invalid)?;
                self.next_number = self.next_number.max(record.number.saturating_add(1));
                self.records.insert(record.id, record);
            }
            ReminderChange::Deleted { id, .. } => {
                self.records.remove(&id);
            }
        }
        Ok(())
    }

    /// Comparable reminder state used to reject a model proposal whose
    /// reminder view changed between context capture and acceptance.
    pub(crate) fn reminder_state(&self) -> Vec<(u64, u64, u64, String, i64, bool)> {
        self.records
            .values()
            .map(|record| {
                (
                    record.number,
                    record.revision,
                    record.schedule_revision,
                    record.text.clone(),
                    record.due.unix_micros(),
                    record.enabled,
                )
            })
            .collect()
    }
}

fn resolve_due(
    due: DueSpec,
    now: UtcInstant,
    policy: LocalTimePolicy,
    default_zone: &TimeZoneId,
) -> Result<(UtcInstant, LocalDateTime, TimeZoneId), ReminderError> {
    match due {
        DueSpec::After(duration) => {
            let instant = now
                .checked_add(duration)
                .ok_or(ReminderError::Invalid("due time is out of range".into()))?;
            let zone = default_zone.clone();
            let local = instant.local(&zone.zone());
            Ok((instant, local, zone))
        }
        DueSpec::Utc(instant) => {
            let zone = default_zone.clone();
            let local = instant.local(&zone.zone());
            Ok((instant, local, zone))
        }
        DueSpec::Local { local, zone } => {
            let instant = zone.zone().resolve(local, policy)?;
            Ok((instant, local, zone))
        }
    }
}

fn validate_new_text(text: &str) -> Result<(), ReminderError> {
    if text.trim().is_empty() {
        return Err(ReminderError::Invalid(
            "reminder text must not be empty".into(),
        ));
    }
    if text.len() > super::model::MAX_TEXT_BYTES {
        return Err(ReminderError::Invalid("reminder text is too large".into()));
    }
    Ok(())
}

fn payload<T: DeserializeOwned>(
    payload: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<T, ReminderError> {
    let value = payload
        .get(key)
        .ok_or_else(|| ReminderError::Storage(format!("missing reminder payload {key}")))?;
    serde_json::from_value(value.clone())
        .map_err(|error| ReminderError::Storage(format!("invalid reminder payload: {error}")))
}

pub(crate) fn prompt_reminders_at(
    timeline: &crate::timeline::SessionTimeline,
    at: crate::timeline::ContextAnchor,
    character: CharacterId,
) -> Result<Vec<ReminderPrompt>, ReminderError> {
    let facts = timeline
        .reminder_facts_at(at)
        .map_err(|error| ReminderError::Storage(error.to_string()))?;
    Ok(
        ReminderProjection::from_facts(BranchId(at.branch_id), character, &facts)?
            .prompt_reminders(),
    )
}
