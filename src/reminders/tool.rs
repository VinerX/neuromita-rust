use std::time::Duration;

use chrono::NaiveTime;

use super::{
    model::{DueSpec, ReminderError, ReminderToolCommand, ReminderToolReply, RepeatRule},
    time::{LocalDateTime, LocalTimePolicy, TimeZone, TimeZoneId, UtcInstant},
};

pub(crate) fn parse_repeat(value: &str) -> Result<RepeatRule, ReminderError> {
    let value = value.trim().to_lowercase();
    match value.as_str() {
        "" | "once" => return Ok(RepeatRule::Once),
        "daily" => return Ok(RepeatRule::EveryLocalDays { days: 1 }),
        "weekly" => return Ok(RepeatRule::EveryLocalDays { days: 7 }),
        _ => {}
    }
    let words: Vec<&str> = value.split_whitespace().collect();
    if words.len() != 3 || words[0] != "every" {
        return Err(ReminderError::Invalid(format!(
            "unsupported repeat '{value}'; use once, daily, weekly or every N seconds/minutes/hours/days"
        )));
    }
    let amount = words[1]
        .parse::<u64>()
        .map_err(|_| ReminderError::Invalid(format!("invalid repeat amount in '{value}'")))?;
    if amount == 0 {
        return Err(ReminderError::Invalid(
            "repeat amount must be positive".into(),
        ));
    }
    let seconds = match words[2] {
        "second" | "seconds" => amount,
        "minute" | "minutes" => amount.saturating_mul(60),
        "hour" | "hours" => amount.saturating_mul(3_600),
        "day" | "days" => {
            let days = u32::try_from(amount)
                .map_err(|_| ReminderError::Invalid("repeat day count is too large".into()))?;
            return Ok(RepeatRule::EveryLocalDays { days });
        }
        _ => {
            return Err(ReminderError::Invalid(format!(
                "unknown repeat unit '{}'",
                words[2]
            )));
        }
    };
    Ok(RepeatRule::EverySeconds { seconds })
}

/// Public parser used by typed user controls; the same grammar as the model
/// `repeat` tool argument.
pub fn parse_structured_repeat(value: &str) -> Result<RepeatRule, ReminderError> {
    parse_repeat(value)
}
use crate::character::CharacterId;

pub struct ReminderTool;

impl ReminderTool {
    pub async fn run(
        character: CharacterId,
        command: ReminderToolCommand,
    ) -> Result<ReminderToolReply, ReminderError> {
        super::RemindersApi::run_tool(character, command).await
    }
}

pub fn parse_structured_due(value: &str, zone: &TimeZoneId) -> Result<DueSpec, ReminderError> {
    let value = value.trim();
    if let Ok(instant) = UtcInstant::parse_rfc3339(value) {
        return Ok(DueSpec::Utc(instant));
    }
    let local = LocalDateTime::parse(value)?;
    Ok(DueSpec::Local {
        local,
        zone: zone.clone(),
    })
}

pub fn parse_friendly_due(
    value: &str,
    now: UtcInstant,
    zone: &TimeZoneId,
    policy: LocalTimePolicy,
) -> Result<DueSpec, ReminderError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ReminderError::Invalid("due date is empty".into()));
    }
    if let Ok(instant) = UtcInstant::parse_rfc3339(value) {
        return Ok(DueSpec::Utc(instant));
    }
    let lower = value.to_lowercase();
    if let Some(duration) = parse_relative(&lower)? {
        return DueSpec::after(duration);
    }
    let timezone = zone.zone();
    if let Some(local) = parse_day_words(&lower, now, &timezone, policy)? {
        return Ok(DueSpec::Local {
            local,
            zone: zone.clone(),
        });
    }
    if let Ok(local) = LocalDateTime::parse(value) {
        return Ok(DueSpec::Local {
            local,
            zone: zone.clone(),
        });
    }
    Err(ReminderError::Invalid(format!(
        "unsupported due date '{value}'; use 'через 2 часа', 'in 30 minutes', 'завтра в 18:00', '2026-12-01T10:00:00' or RFC 3339"
    )))
}

fn parse_relative(value: &str) -> Result<Option<Duration>, ReminderError> {
    let words: Vec<&str> = value.split_whitespace().collect();
    if words.is_empty() {
        return Ok(None);
    }
    let mut rest = if words[0] == "через" || words[0] == "in" {
        &words[1..]
    } else {
        return Ok(None);
    };
    if rest.is_empty() {
        return Ok(None);
    }
    if rest.len() == 1 {
        let fixed = match rest[0] {
            "полчаса" | "half" => Some(30 * 60),
            "час" | "hour" => Some(60 * 60),
            "неделю" | "week" => Some(7 * 24 * 60 * 60),
            "месяц" | "month" => Some(30 * 24 * 60 * 60),
            _ => None,
        };
        if let Some(seconds) = fixed {
            return Ok(Some(Duration::from_secs(seconds)));
        }
    }
    if rest.len() == 2 && matches!(rest[0], "a" | "an") && rest[1] == "hour" {
        return Ok(Some(Duration::from_secs(60 * 60)));
    }
    if rest.len() == 3 && rest[0] == "half" && rest[1] == "an" && rest[2] == "hour" {
        return Ok(Some(Duration::from_secs(30 * 60)));
    }
    let amount = rest[0]
        .replace(',', ".")
        .parse::<f64>()
        .map_err(|_| ReminderError::Invalid(format!("invalid amount in '{value}'")))?;
    if !amount.is_finite() || amount <= 0.0 {
        return Err(ReminderError::Invalid("amount must be positive".into()));
    }
    rest = &rest[1..];
    if rest.len() != 1 {
        return Ok(None);
    }
    let unit = rest[0];
    let multiplier = match unit {
        "секунда" | "секунды" | "секунд" | "секунду" | "сек" | "second" | "seconds" => {
            1.0
        }
        "минута" | "минуты" | "минут" | "минуту" | "minute" | "minutes" => {
            60.0
        }
        "час" | "часа" | "часов" | "hour" | "hours" => 3_600.0,
        "день" | "дня" | "дней" | "day" | "days" => 86_400.0,
        "неделя" | "недели" | "недель" | "неделю" | "week" | "weeks" => {
            7.0 * 86_400.0
        }
        "месяц" | "месяца" | "месяцев" | "month" | "months" => 30.0 * 86_400.0,
        _ => {
            return Err(ReminderError::Invalid(format!(
                "unknown relative unit '{unit}'"
            )));
        }
    };
    let seconds = amount * multiplier;
    if seconds > 366.0 * 86_400.0 {
        return Err(ReminderError::Invalid(
            "delay is longer than one year".into(),
        ));
    }
    Duration::try_from_secs_f64(seconds)
        .map(Some)
        .map_err(|_| ReminderError::Invalid("delay is outside the supported range".into()))
}

fn parse_day_words(
    value: &str,
    now: UtcInstant,
    zone: &TimeZone,
    policy: LocalTimePolicy,
) -> Result<Option<LocalDateTime>, ReminderError> {
    let (days, rest) = if let Some(rest) = value.strip_prefix("послезавтра") {
        (2_i64, rest.trim())
    } else if let Some(rest) = value.strip_prefix("завтра") {
        (1, rest.trim())
    } else if let Some(rest) = value.strip_prefix("сегодня") {
        (0, rest.trim())
    } else if let Some(rest) = value.strip_prefix("day after tomorrow") {
        (2, rest.trim())
    } else if let Some(rest) = value.strip_prefix("tomorrow") {
        (1, rest.trim())
    } else if let Some(rest) = value.strip_prefix("today") {
        (0, rest.trim())
    } else {
        return Ok(None);
    };
    let base = now.local(zone);
    let date = base
        .date()
        .checked_add_signed(chrono::Duration::days(days))
        .ok_or(ReminderError::Invalid("date is out of range".into()))?;
    let time = if rest.is_empty() {
        if days == 0 {
            return Err(ReminderError::Invalid(
                "for 'сегодня'/'today' specify a time, for example 'сегодня в 18:00'".into(),
            ));
        }
        NaiveTime::from_hms_opt(9, 0, 0).expect("valid default time")
    } else {
        let rest = rest
            .strip_prefix("в ")
            .or_else(|| rest.strip_prefix("at "))
            .unwrap_or(rest)
            .trim();
        parse_time(rest)?
    };
    let local = LocalDateTime::new(date, time);
    zone.resolve(local, policy)?;
    Ok(Some(local))
}

fn parse_time(value: &str) -> Result<NaiveTime, ReminderError> {
    let normalized;
    let value = if value.matches('.').count() == 1 && !value.contains(':') {
        normalized = value.replace('.', ":");
        normalized.as_str()
    } else {
        value
    };
    ["%H:%M:%S%.f", "%H:%M:%S", "%H:%M"]
        .into_iter()
        .find_map(|format| NaiveTime::parse_from_str(value, format).ok())
        .ok_or_else(|| ReminderError::Invalid(format!("invalid time '{value}'")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reminders::time::TimeZone;

    fn now() -> UtcInstant {
        UtcInstant::parse_rfc3339("2026-11-03T09:00:00Z").unwrap()
    }

    fn zone() -> TimeZoneId {
        TimeZoneId::parse("Europe/Moscow").unwrap()
    }

    #[test]
    fn friendly_parser_does_not_treat_day_after_tomorrow_as_tomorrow() {
        let parsed = parse_friendly_due(
            "послезавтра в 10:00",
            now(),
            &zone(),
            LocalTimePolicy::LENIENT,
        )
        .unwrap();
        let DueSpec::Local { local, .. } = parsed else {
            panic!("expected local");
        };
        assert_eq!(local.date().to_string(), "2026-11-05");
        assert_eq!(local.time().to_string(), "10:00:00");
    }

    #[test]
    fn fractional_seconds_are_preserved() {
        let parsed = parse_friendly_due(
            "через 1.5 секунды",
            now(),
            &zone(),
            LocalTimePolicy::LENIENT,
        )
        .unwrap();
        assert_eq!(parsed, DueSpec::After(Duration::from_millis(1500)));
    }

    #[test]
    fn friendly_parser_requires_full_recognized_forms() {
        for value in [
            "завтрашний день",
            "in 5",
            "через пять минут",
            "tomorrowish",
            "in 2 hours ago",
            "in an hour yesterday",
            "in 2 hourswhatever",
            "через 2 часа назад",
        ] {
            assert!(
                parse_friendly_due(value, now(), &zone(), LocalTimePolicy::LENIENT).is_err(),
                "{value}"
            );
        }
    }

    #[test]
    fn friendly_parser_accepts_russian_and_english_date_forms() {
        for value in [
            "через 2 часа",
            "через 30 минут",
            "через 1 неделю",
            "завтра в 18.30",
            "tomorrow at 18:30",
            "in 2 hours",
            "in an hour",
            "day after tomorrow",
        ] {
            assert!(
                parse_friendly_due(value, now(), &zone(), LocalTimePolicy::LENIENT).is_ok(),
                "{value}"
            );
        }
    }

    #[test]
    fn lord_howe_and_apia_gaps_shift_forward_without_transition_tables() {
        let lord_howe = TimeZone::from_id("Australia/Lord_Howe").unwrap();
        let shifted = lord_howe
            .resolve(
                LocalDateTime::parse("2026-10-04T02:15:00").unwrap(),
                LocalTimePolicy::LENIENT,
            )
            .unwrap();
        assert_eq!(shifted.to_rfc3339(), "2026-10-03T15:30:00Z");
        let apia = TimeZone::from_id("Pacific/Apia").unwrap();
        let skipped_day = apia
            .resolve(
                LocalDateTime::parse("2011-12-30T12:00:00").unwrap(),
                LocalTimePolicy::LENIENT,
            )
            .unwrap();
        assert_eq!(skipped_day.to_rfc3339(), "2011-12-30T10:00:00Z");
    }
}
