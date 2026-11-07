use std::{fmt, str::FromStr, time::Duration};

use chrono::{
    DateTime, Duration as ChronoDuration, LocalResult, NaiveDate, NaiveDateTime, NaiveTime,
    SecondsFormat, TimeZone as _, Utc,
};
use chrono_tz::{GapInfo, Tz};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum TimeError {
    #[error("invalid date/time '{0}'")]
    Format(String),
    #[error("unknown time zone '{0}'; use an IANA name such as Europe/Berlin")]
    UnknownZone(String),
    #[error("local time {0} does not exist in {1}")]
    Nonexistent(String, String),
    #[error("local time {0} is ambiguous in {1}")]
    Ambiguous(String, String),
    #[error("instant is outside the supported range")]
    Range,
    #[error("system time zone could not be detected: {0}")]
    SystemZone(String),
}

/// Explicit host time-zone detection. Never used as a silent default: the
/// deterministic initial profile is UTC and callers must persist the result.
pub fn system_timezone() -> Result<TimeZoneId, TimeError> {
    let zone =
        iana_time_zone::get_timezone().map_err(|error| TimeError::SystemZone(error.to_string()))?;
    TimeZoneId::parse(&zone)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UtcInstant(DateTime<Utc>);

impl UtcInstant {
    pub const EPOCH: Self = Self(DateTime::UNIX_EPOCH);

    pub fn try_from_unix_micros(micros: i64) -> Option<Self> {
        DateTime::from_timestamp_micros(micros).map(Self)
    }
    pub fn from_unix_micros(micros: i64) -> Self {
        Self::try_from_unix_micros(micros).expect("timestamp within chrono range")
    }
    pub fn unix_micros(self) -> i64 {
        self.0.timestamp_micros()
    }
    pub fn from_unix_seconds(seconds: i64) -> Self {
        Self(DateTime::from_timestamp(seconds, 0).expect("timestamp within chrono range"))
    }
    pub fn datetime(self) -> DateTime<Utc> {
        self.0
    }
    pub fn checked_add(self, duration: Duration) -> Option<Self> {
        let duration = ChronoDuration::from_std(duration).ok()?;
        self.0.checked_add_signed(duration).map(Self)
    }
    pub fn checked_add_seconds(self, seconds: i64) -> Option<Self> {
        self.0
            .checked_add_signed(ChronoDuration::try_seconds(seconds)?)
            .map(Self)
    }
    pub fn duration_since(self, earlier: Self) -> Duration {
        self.0
            .signed_duration_since(earlier.0)
            .to_std()
            .unwrap_or(Duration::ZERO)
    }
    pub fn to_rfc3339(self) -> String {
        self.0.to_rfc3339_opts(SecondsFormat::AutoSi, true)
    }
    pub fn parse_rfc3339(value: &str) -> Result<Self, TimeError> {
        DateTime::parse_from_rfc3339(value.trim())
            .map(|value| Self(value.with_timezone(&Utc)))
            .map_err(|_| TimeError::Format(value.trim().to_owned()))
    }
    pub fn local(self, zone: &TimeZone) -> LocalDateTime {
        LocalDateTime::from_naive(self.0.with_timezone(&zone.tz).naive_local())
    }
    pub fn local_text(self, zone: &TimeZone) -> String {
        let local = self.0.with_timezone(&zone.tz);
        format!(
            "{} {}",
            local.format("%Y-%m-%d %H:%M:%S"),
            local.format("%:z")
        )
    }
}

impl fmt::Display for UtcInstant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_rfc3339())
    }
}

impl FromStr for UtcInstant {
    type Err = TimeError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse_rfc3339(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LocalDateTime {
    value: NaiveDateTime,
    has_time: bool,
}

impl LocalDateTime {
    pub fn new(date: NaiveDate, time: NaiveTime) -> Self {
        Self {
            value: date.and_time(time),
            has_time: true,
        }
    }
    pub fn from_naive(value: NaiveDateTime) -> Self {
        Self {
            value,
            has_time: true,
        }
    }
    pub fn parse(value: &str) -> Result<Self, TimeError> {
        let value = value.trim();
        let (date_text, time_text) = match value.split_once('T') {
            Some((date, time)) => (date, Some(time.trim())),
            None => match value.split_once(' ') {
                Some((date, time)) => (date, Some(time.trim())),
                None => (value, None),
            },
        };
        let date = NaiveDate::parse_from_str(date_text, "%Y-%m-%d")
            .map_err(|_| TimeError::Format(value.to_owned()))?;
        let Some(time_text) = time_text.filter(|time| !time.is_empty()) else {
            return Ok(Self {
                value: date.and_hms_opt(0, 0, 0).expect("midnight is valid"),
                has_time: false,
            });
        };
        let time = ["%H:%M:%S%.f", "%H:%M"]
            .into_iter()
            .find_map(|format| NaiveTime::parse_from_str(time_text, format).ok())
            .ok_or_else(|| TimeError::Format(value.to_owned()))?;
        Ok(Self {
            value: date.and_time(time),
            has_time: true,
        })
    }
    pub fn date(self) -> NaiveDate {
        self.value.date()
    }
    pub fn time(self) -> NaiveTime {
        self.value.time()
    }
    pub fn has_time(self) -> bool {
        self.has_time
    }
    pub fn naive(self) -> NaiveDateTime {
        self.value
    }
    pub fn checked_add_days(self, days: u32) -> Option<Self> {
        self.value
            .checked_add_signed(ChronoDuration::days(i64::from(days)))
            .map(|value| Self {
                value,
                has_time: self.has_time,
            })
    }
    pub fn format_iso(self) -> String {
        self.value.format("%Y-%m-%dT%H:%M:%S%.f").to_string()
    }
    pub fn format_human(self) -> String {
        self.value.format("%Y-%m-%d %H:%M:%S").to_string()
    }
}

impl fmt::Display for LocalDateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.format_human())
    }
}

impl TryFrom<String> for LocalDateTime {
    type Error = TimeError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<LocalDateTime> for String {
    fn from(value: LocalDateTime) -> Self {
        value.format_iso()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AmbiguousPolicy {
    #[default]
    Earliest,
    Latest,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NonexistentPolicy {
    #[default]
    ShiftForward,
    Reject,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalTimePolicy {
    pub ambiguous: AmbiguousPolicy,
    pub nonexistent: NonexistentPolicy,
}

impl LocalTimePolicy {
    pub const STRICT: Self = Self {
        ambiguous: AmbiguousPolicy::Earliest,
        nonexistent: NonexistentPolicy::Reject,
    };
    pub const LENIENT: Self = Self {
        ambiguous: AmbiguousPolicy::Earliest,
        nonexistent: NonexistentPolicy::ShiftForward,
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeZone {
    id: String,
    tz: Tz,
}

impl TimeZone {
    pub fn from_id(id: &str) -> Result<Self, TimeError> {
        let id = id.trim();
        let tz = Tz::from_str(id).map_err(|_| TimeError::UnknownZone(id.to_owned()))?;
        Ok(Self {
            id: id.to_owned(),
            tz,
        })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn resolve(
        &self,
        local: LocalDateTime,
        policy: LocalTimePolicy,
    ) -> Result<UtcInstant, TimeError> {
        match self.tz.from_local_datetime(&local.naive()) {
            LocalResult::Single(value) => Ok(UtcInstant(value.with_timezone(&Utc))),
            LocalResult::Ambiguous(earliest, latest) => {
                let value = match policy.ambiguous {
                    AmbiguousPolicy::Earliest => earliest,
                    AmbiguousPolicy::Latest => latest,
                };
                Ok(UtcInstant(value.with_timezone(&Utc)))
            }
            LocalResult::None => match policy.nonexistent {
                NonexistentPolicy::Reject => Err(TimeError::Nonexistent(
                    local.format_human(),
                    self.id.clone(),
                )),
                // The policy is deliberately the first valid instant at the
                // transition boundary, not the original wall time shifted by
                // the gap length. This makes 02:30:37 resolve to 03:00:00.
                NonexistentPolicy::ShiftForward => self.shift_forward(local),
            },
        }
    }
    fn shift_forward(&self, local: LocalDateTime) -> Result<UtcInstant, TimeError> {
        GapInfo::new(&local.naive(), &self.tz)
            .and_then(|gap| gap.end)
            .map(|value| UtcInstant(value.with_timezone(&Utc)))
            .ok_or(TimeError::Range)
    }
    pub fn next_local_days(
        &self,
        local: LocalDateTime,
        days: u32,
        policy: LocalTimePolicy,
    ) -> Result<UtcInstant, TimeError> {
        let next = local.checked_add_days(days).ok_or(TimeError::Range)?;
        self.resolve(next, policy)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TimeZoneId(String);

impl TimeZoneId {
    pub fn parse(value: &str) -> Result<Self, TimeError> {
        TimeZone::from_id(value)?;
        Ok(Self(value.trim().to_owned()))
    }
    pub fn utc() -> Self {
        Self("UTC".to_owned())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn zone(&self) -> TimeZone {
        TimeZone::from_id(&self.0).expect("validated time zone")
    }
}

impl fmt::Display for TimeZoneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for TimeZoneId {
    type Error = TimeError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<TimeZoneId> for String {
    fn from(value: TimeZoneId) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(value: &str) -> LocalDateTime {
        LocalDateTime::parse(value).unwrap()
    }

    #[test]
    fn utc_round_trip_and_offsets() {
        let instant = UtcInstant::parse_rfc3339("2026-11-02T12:34:56+03:00").unwrap();
        assert_eq!(instant.to_rfc3339(), "2026-11-02T09:34:56Z");
        assert_eq!(
            UtcInstant::parse_rfc3339("2026-11-02T09:34:56Z").unwrap(),
            instant
        );
        assert!(UtcInstant::parse_rfc3339("2026-11-02T12:34:56").is_err());
    }

    #[test]
    fn european_dst_gap_and_overlap_use_explicit_policy() {
        let berlin = TimeZone::from_id("Europe/Berlin").unwrap();
        assert!(matches!(
            berlin.resolve(local("2026-03-29T02:30:00"), LocalTimePolicy::STRICT),
            Err(TimeError::Nonexistent(_, _))
        ));
        assert_eq!(
            berlin
                .resolve(local("2026-03-29T02:30:00"), LocalTimePolicy::LENIENT)
                .unwrap()
                .to_rfc3339(),
            "2026-03-29T01:00:00Z"
        );
        assert_eq!(
            berlin
                .resolve(
                    local("2026-10-25T02:30:00"),
                    LocalTimePolicy {
                        ambiguous: AmbiguousPolicy::Latest,
                        nonexistent: NonexistentPolicy::Reject,
                    },
                )
                .unwrap()
                .to_rfc3339(),
            "2026-10-25T01:30:00Z"
        );
    }

    #[test]
    fn daily_recurrence_preserves_wall_clock_across_dst() {
        let berlin = TimeZone::from_id("Europe/Berlin").unwrap();
        let first = berlin
            .resolve(local("2026-03-28T09:00:00"), LocalTimePolicy::LENIENT)
            .unwrap();
        assert_eq!(first.to_rfc3339(), "2026-03-28T08:00:00Z");
        let second = berlin
            .next_local_days(local("2026-03-28T09:00:00"), 1, LocalTimePolicy::LENIENT)
            .unwrap();
        assert_eq!(second.to_rfc3339(), "2026-03-29T07:00:00Z");
    }

    #[test]
    fn shift_forward_uses_exact_transition_boundary() {
        let berlin = TimeZone::from_id("Europe/Berlin").unwrap();
        assert_eq!(
            berlin
                .resolve(local("2026-03-29T02:30:37"), LocalTimePolicy::LENIENT)
                .unwrap()
                .to_rfc3339(),
            "2026-03-29T01:00:00Z"
        );
    }

    #[test]
    fn huge_seconds_do_not_panic_public_clock_arithmetic() {
        assert!(UtcInstant::EPOCH.checked_add_seconds(i64::MAX).is_none());
    }

    #[test]
    fn unknown_zone_is_an_error() {
        assert!(matches!(
            TimeZone::from_id("Mars/Olympus"),
            Err(TimeError::UnknownZone(_))
        ));
    }
}
