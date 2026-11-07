use std::{
    collections::BTreeMap,
    fmt,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser::SerializeMap};
use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

use crate::{request::RequestId, timeline::EventId};

const SECONDS_PER_DAY: i64 = 86_400;
const NANOS_PER_SECOND: u32 = 1_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SampleId(pub Uuid);

impl SampleId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
    pub fn parse(value: &str) -> Result<Self, uuid::Error> {
        Ok(Self(Uuid::parse_str(value)?))
    }

    /// One provider interaction produces one stable sample identity. Reusing
    /// the request UUID keeps the chat reply and its rating correlated across
    /// restarts without a second allocation or mutable pending-slot fallback.
    pub fn for_request(request: RequestId) -> Self {
        Self(request.0)
    }
}

impl Default for SampleId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SampleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "i8", into = "i8")]
pub enum Rating {
    Negative = -1,
    Positive = 1,
}

impl Rating {
    pub fn as_i8(self) -> i8 {
        self as i8
    }
}

impl TryFrom<i8> for Rating {
    type Error = &'static str;

    fn try_from(value: i8) -> Result<Self, Self::Error> {
        match value {
            -1 => Ok(Self::Negative),
            1 => Ok(Self::Positive),
            _ => Err("rating must be 1 or -1"),
        }
    }
}

impl From<Rating> for i8 {
    fn from(value: Rating) -> Self {
        value.as_i8()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    Unlimited,
    Limited { max: usize },
}

impl Retention {
    pub fn limited(max: usize) -> Result<Self, FineTuneError> {
        if !(1..=1_000_000).contains(&max) {
            return Err(FineTuneError::Retention(
                "max records must be between 1 and 1000000".into(),
            ));
        }
        Ok(Self::Limited { max })
    }
    pub fn max_records(self) -> Option<usize> {
        match self {
            Self::Unlimited => None,
            Self::Limited { max } => Some(max),
        }
    }
}

impl Serialize for Retention {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(2))?;
        match self {
            Self::Unlimited => map.serialize_entry("kind", "unlimited")?,
            Self::Limited { max } => {
                map.serialize_entry("kind", "limited")?;
                map.serialize_entry("max", max)?;
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Retention {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum Wire {
            Unlimited,
            Limited { max: usize },
        }
        match Wire::deserialize(deserializer)? {
            Wire::Unlimited => Ok(Self::Unlimited),
            Wire::Limited { max } => Self::limited(max).map_err(de::Error::custom),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TimeError {
    #[error("timestamp must be RFC 3339 with an explicit UTC offset")]
    Format,
    #[error("invalid calendar date or time")]
    Invalid,
    #[error("date must use YYYY-MM-DD")]
    DateFormat,
    #[error("time range start is after end")]
    Reversed,
}

/// UTC instant without a local time zone. Parsing accepts an explicit offset and
/// normalizes it; formatting always emits `Z`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcTimestamp {
    unix_seconds: i64,
    nanos: u32,
}

impl UtcTimestamp {
    pub fn from_unix(seconds: i64, nanos: u32) -> Result<Self, TimeError> {
        if nanos >= NANOS_PER_SECOND {
            return Err(TimeError::Invalid);
        }
        let first_day = days_from_civil(1, 1, 1) * SECONDS_PER_DAY;
        let last_second = days_from_civil(9999, 12, 31) * SECONDS_PER_DAY + SECONDS_PER_DAY - 1;
        if !(first_day..=last_second).contains(&seconds) {
            return Err(TimeError::Invalid);
        }
        Ok(Self {
            unix_seconds: seconds,
            nanos,
        })
    }

    pub fn now() -> Self {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            unix_seconds: i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX),
            nanos: elapsed.subsec_nanos(),
        }
    }

    pub fn unix_seconds(self) -> i64 {
        self.unix_seconds
    }

    pub fn nanos(self) -> u32 {
        self.nanos
    }

    pub fn to_rfc3339(self) -> String {
        let days = self.unix_seconds.div_euclid(SECONDS_PER_DAY);
        let seconds = self.unix_seconds.rem_euclid(SECONDS_PER_DAY);
        let (year, month, day) = civil_from_days(days);
        let hour = seconds / 3600;
        let minute = (seconds % 3600) / 60;
        let second = seconds % 60;
        format!(
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{:09}Z",
            self.nanos
        )
    }

    pub fn parse(value: &str) -> Result<Self, TimeError> {
        let bytes = value.as_bytes();
        if bytes.len() < 20 {
            return Err(TimeError::Format);
        }
        let year = digits(bytes, 0, 4)? as i64;
        expect(bytes, 4, b'-')?;
        let month = digits(bytes, 5, 2)?;
        expect(bytes, 7, b'-')?;
        let day = digits(bytes, 8, 2)?;
        if bytes[10] != b'T' && bytes[10] != b' ' {
            return Err(TimeError::Format);
        }
        let hour = digits(bytes, 11, 2)?;
        expect(bytes, 13, b':')?;
        let minute = digits(bytes, 14, 2)?;
        expect(bytes, 16, b':')?;
        let second = digits(bytes, 17, 2)?;
        let mut index = 19;
        let mut nanos = 0u32;
        if bytes.get(index) == Some(&b'.') {
            index += 1;
            let start = index;
            while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                index += 1;
            }
            let count = index - start;
            if count == 0 || count > 9 {
                return Err(TimeError::Format);
            }
            for byte in &bytes[start..index] {
                nanos = nanos * 10 + u32::from(byte - b'0');
            }
            for _ in count..9 {
                nanos *= 10;
            }
        }
        let offset = match bytes.get(index) {
            Some(b'Z' | b'z') => {
                index += 1;
                0i64
            }
            Some(sign @ (b'+' | b'-')) => {
                let direction = if *sign == b'+' { 1 } else { -1 };
                let hours = digits(bytes, index + 1, 2)? as i64;
                expect(bytes, index + 3, b':')?;
                let minutes = digits(bytes, index + 4, 2)? as i64;
                if hours > 23 || minutes > 59 {
                    return Err(TimeError::Invalid);
                }
                index += 6;
                direction * (hours * 3600 + minutes * 60)
            }
            _ => return Err(TimeError::Format),
        };
        if index != bytes.len() {
            return Err(TimeError::Format);
        }
        if !(1..=9999).contains(&year)
            || !(1..=12).contains(&month)
            || day == 0
            || day > days_in_month(year, month)
            || hour > 23
            || minute > 59
            || second > 59
        {
            return Err(TimeError::Invalid);
        }
        let local = days_from_civil(year, month, day) * SECONDS_PER_DAY
            + i64::from(hour * 3600 + minute * 60 + second);
        Self::from_unix(local - offset, nanos)
    }

    pub fn utc_date_bounds(date: &str) -> Result<(Self, Self), TimeError> {
        let (year, month, day) = parse_date(date)?;
        let start = days_from_civil(year, month, day) * SECONDS_PER_DAY;
        let end = (days_from_civil(year, month, day) + 1) * SECONDS_PER_DAY - 1;
        Ok((
            Self::from_unix(start, 0)?,
            Self::from_unix(end, NANOS_PER_SECOND - 1)?,
        ))
    }

    pub fn month_key(self) -> String {
        let days = self.unix_seconds.div_euclid(SECONDS_PER_DAY);
        let (year, month, _) = civil_from_days(days);
        format!("{year:04}{month:02}")
    }
}

impl Serialize for UtcTimestamp {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_rfc3339())
    }
}

impl<'de> Deserialize<'de> for UtcTimestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

fn digits(bytes: &[u8], start: usize, count: usize) -> Result<u32, TimeError> {
    let end = start.checked_add(count).ok_or(TimeError::Format)?;
    let slice = bytes.get(start..end).ok_or(TimeError::Format)?;
    if !slice.iter().all(u8::is_ascii_digit) {
        return Err(TimeError::Format);
    }
    Ok(slice
        .iter()
        .fold(0u32, |value, byte| value * 10 + u32::from(byte - b'0')))
}

fn expect(bytes: &[u8], index: usize, expected: u8) -> Result<(), TimeError> {
    if bytes.get(index) == Some(&expected) {
        Ok(())
    } else {
        Err(TimeError::Format)
    }
}

fn parse_date(value: &str) -> Result<(i64, u32, u32), TimeError> {
    let bytes = value.as_bytes();
    if bytes.len() != 10 {
        return Err(TimeError::DateFormat);
    }
    let year = digits(bytes, 0, 4)? as i64;
    expect(bytes, 4, b'-')?;
    let month = digits(bytes, 5, 2)?;
    expect(bytes, 7, b'-')?;
    let day = digits(bytes, 8, 2)?;
    if !(1..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
    {
        return Err(TimeError::Invalid);
    }
    Ok((year, month, day))
}

pub fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

// Howard Hinnant's civil calendar algorithms, valid for proleptic Gregorian
// dates. No local time zone or DST is involved.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = if month > 2 {
        i64::from(month) - 3
    } else {
        i64::from(month) + 9
    };
    let day_of_year = (153 * month_prime + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    if month <= 2 {
        year += 1;
    }
    (year, month as u32, day as u32)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
    Other(String),
}

impl MessageRole {
    pub fn as_str(&self) -> &str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
            Self::Other(value) => value,
        }
    }

    pub fn sharegpt_from(&self) -> Option<&'static str> {
        match self {
            Self::System => Some("system"),
            Self::User => Some("human"),
            Self::Assistant => Some("gpt"),
            Self::Tool | Self::Other(_) => None,
        }
    }

    pub fn chatml_role(&self) -> Option<&'static str> {
        match self {
            Self::System => Some("system"),
            Self::User => Some("user"),
            Self::Assistant => Some("assistant"),
            Self::Tool | Self::Other(_) => None,
        }
    }
}

impl Serialize for MessageRole {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for MessageRole {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "system" => Self::System,
            "user" => Self::User,
            "assistant" => Self::Assistant,
            "tool" => Self::Tool,
            _ => Self::Other(value),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl MessageContent {
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }

    pub fn flatten(&self) -> FlattenedContent {
        match self {
            Self::Text(value) => FlattenedContent {
                text: value.clone(),
                dropped_parts: 0,
            },
            Self::Parts(parts) => {
                let mut text = Vec::new();
                let mut dropped_parts = 0;
                for part in parts {
                    match part {
                        ContentPart::Text(value) if !value.is_empty() => text.push(value.as_str()),
                        ContentPart::Text(_) => {}
                        ContentPart::Other(_) => dropped_parts += 1,
                    }
                }
                FlattenedContent {
                    text: text.join(" "),
                    dropped_parts,
                }
            }
        }
    }

    pub fn non_text_parts(&self) -> usize {
        match self {
            Self::Text(_) => 0,
            Self::Parts(parts) => parts
                .iter()
                .filter(|part| matches!(part, ContentPart::Other(_)))
                .count(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ContentPart {
    Text(String),
    Other(Value),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlattenedContent {
    pub text: String,
    pub dropped_parts: usize,
}

impl Serialize for MessageContent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Text(value) => serializer.serialize_str(value),
            Self::Parts(parts) => {
                let values: Vec<Value> = parts
                    .iter()
                    .map(|part| match part {
                        ContentPart::Text(value) => {
                            json!({"type": "text", "text": value})
                        }
                        ContentPart::Other(value) => value.clone(),
                    })
                    .collect();
                values.serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for MessageContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Text(String),
            Parts(Vec<PartWire>),
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum PartWire {
            Text(String),
            Object(Value),
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Text(value) => Self::Text(value),
            Wire::Parts(parts) => Self::Parts(
                parts
                    .into_iter()
                    .map(|part| match part {
                        PartWire::Text(value) => ContentPart::Text(value),
                        PartWire::Object(value) => {
                            if let Some(text) = value.get("text").and_then(Value::as_str) {
                                ContentPart::Text(text.to_owned())
                            } else if let Some(text) = value.get("content").and_then(Value::as_str)
                            {
                                ContentPart::Text(text.to_owned())
                            } else {
                                ContentPart::Other(value)
                            }
                        }
                    })
                    .collect(),
            ),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SampleMessage {
    pub role: MessageRole,
    pub content: MessageContent,
}

impl SampleMessage {
    pub fn text(role: MessageRole, value: impl Into<String>) -> Self {
        Self {
            role,
            content: MessageContent::text(value),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextStage {
    #[default]
    ProviderRaw,
    Cleaned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FineTuneSample {
    pub id: SampleId,
    pub timestamp: UtcTimestamp,
    pub character_id: String,
    #[serde(default)]
    pub character_name: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider_name: Option<String>,
    #[serde(default)]
    pub protocol_id: Option<String>,
    #[serde(default)]
    pub dialect_id: Option<String>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub top_k: Option<u64>,
    #[serde(default)]
    pub presence_penalty: Option<f64>,
    #[serde(default)]
    pub frequency_penalty: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub game_connected: bool,
    #[serde(default)]
    pub messages: Vec<SampleMessage>,
    /// Exact JSON request body prepared by the provider adapter.
    #[serde(default)]
    pub request_payload: Option<String>,
    #[serde(default)]
    pub response: String,
    /// Exact provider text for captures made by this implementation. Legacy
    /// Python records may have no separate raw field; `response` is then the
    /// captured provider text because Python saved before cleaning.
    #[serde(default)]
    pub raw_response: Option<String>,
    /// Exact HTTP response body retained before provider decoding or
    /// application validation. `raw_response` keeps the extracted model text.
    #[serde(default)]
    pub provider_response: Option<String>,
    /// Present only if a later pipeline stage actually produced cleaned text.
    #[serde(default)]
    pub cleaned_response: Option<String>,
    #[serde(default)]
    pub text_stage: TextStage,
    #[serde(default)]
    pub rating: Option<Rating>,
    #[serde(default)]
    pub usage: Option<Value>,
    #[serde(default)]
    pub usage_source: Option<String>,
    #[serde(default)]
    pub request_id: Option<RequestId>,
    #[serde(default)]
    pub accepted_event_id: Option<EventId>,
    #[serde(default)]
    pub intent: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default = "provider_capture_stage")]
    pub capture_stage: String,
}

fn provider_capture_stage() -> String {
    "provider_response".into()
}

impl FineTuneSample {
    pub fn new(
        timestamp: UtcTimestamp,
        character_id: impl Into<String>,
        model: Option<String>,
        messages: Vec<SampleMessage>,
        response: impl Into<String>,
    ) -> Self {
        let response = response.into();
        Self {
            id: SampleId::new(),
            timestamp,
            character_id: character_id.into(),
            character_name: String::new(),
            model,
            provider_name: None,
            protocol_id: None,
            dialect_id: None,
            temperature: None,
            top_p: None,
            top_k: None,
            presence_penalty: None,
            frequency_penalty: None,
            max_tokens: None,
            game_connected: false,
            messages,
            request_payload: None,
            raw_response: Some(response.clone()),
            response,
            provider_response: None,
            cleaned_response: None,
            text_stage: TextStage::ProviderRaw,
            rating: None,
            usage: None,
            usage_source: None,
            request_id: None,
            accepted_event_id: None,
            intent: None,
            source: None,
            capture_stage: provider_capture_stage(),
        }
    }

    pub fn matches(&self, filters: &SampleFilters, outcome: Option<&OutcomeRecord>) -> bool {
        if !filters.range.contains(self.timestamp) {
            return false;
        }
        if !filters.characters.is_empty()
            && !filters.characters.iter().any(|id| id == &self.character_id)
        {
            return false;
        }
        if !filters.models.is_empty()
            && !self
                .model
                .as_ref()
                .is_some_and(|model| filters.models.iter().any(|value| value == model))
        {
            return false;
        }
        if !filters.rating.accepts(self.rating) {
            return false;
        }
        filters
            .outcome
            .accepts(outcome.map(|record| record.outcome))
    }

    pub fn non_text_parts(&self) -> usize {
        self.messages
            .iter()
            .map(|message| message.content.non_text_parts())
            .sum()
    }

    pub fn dropped_text_export_messages(&self) -> usize {
        self.messages
            .iter()
            .filter(|message| message.role.sharegpt_from().is_none())
            .count()
    }

    pub fn has_non_text_parts(&self) -> bool {
        self.non_text_parts() > 0
    }

    pub fn sharegpt_value(&self) -> Value {
        let mut conversations = Vec::new();
        for message in &self.messages {
            let Some(from) = message.role.sharegpt_from() else {
                continue;
            };
            conversations.push(json!({
                "from": from,
                "value": message.content.flatten().text,
            }));
        }
        conversations.push(json!({"from": "gpt", "value": self.response}));
        json!({"conversations": conversations})
    }

    pub fn chatml_value(&self) -> Value {
        let mut messages = Vec::new();
        for message in &self.messages {
            let Some(role) = message.role.chatml_role() else {
                continue;
            };
            messages.push(json!({
                "role": role,
                "content": message.content.flatten().text,
            }));
        }
        messages.push(json!({"role": "assistant", "content": self.response}));
        json!({"messages": messages})
    }

    pub fn summary(&self, outcome: Option<&OutcomeRecord>) -> SampleSummary {
        SampleSummary {
            id: self.id,
            timestamp: self.timestamp,
            character_id: self.character_id.clone(),
            model: self.model.clone(),
            rating: self.rating,
            request_id: self.request_id,
            outcome: outcome.map(|record| record.outcome),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TimeRange {
    pub start: Option<UtcTimestamp>,
    pub end: Option<UtcTimestamp>,
}

impl TimeRange {
    pub fn new(start: Option<UtcTimestamp>, end: Option<UtcTimestamp>) -> Result<Self, TimeError> {
        if let (Some(start), Some(end)) = (start, end)
            && start > end
        {
            return Err(TimeError::Reversed);
        }
        Ok(Self { start, end })
    }

    pub fn contains(self, value: UtcTimestamp) -> bool {
        self.start.is_none_or(|start| value >= start) && self.end.is_none_or(|end| value <= end)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RatingFilter {
    #[default]
    All,
    NoNegative,
    PositiveOnly,
}

impl RatingFilter {
    pub fn accepts(self, rating: Option<Rating>) -> bool {
        match self {
            Self::All => true,
            Self::NoNegative => rating != Some(Rating::Negative),
            Self::PositiveOnly => rating == Some(Rating::Positive),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutcomeFilter {
    #[default]
    All,
    Accepted,
    NotAccepted,
}

impl OutcomeFilter {
    pub fn accepts(self, outcome: Option<SampleOutcome>) -> bool {
        match self {
            Self::All => true,
            Self::Accepted => outcome.is_some_and(SampleOutcome::is_accepted),
            Self::NotAccepted => outcome.is_some_and(|outcome| !outcome.is_accepted()),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SampleFilters {
    pub range: TimeRange,
    pub characters: Vec<String>,
    pub models: Vec<String>,
    pub rating: RatingFilter,
    pub outcome: OutcomeFilter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleOutcome {
    Accepted,
    AcceptedVoiceFailed,
    AcceptanceFailed,
    CancelledBeforeAccept,
}

impl SampleOutcome {
    pub fn is_accepted(self) -> bool {
        matches!(self, Self::Accepted | Self::AcceptedVoiceFailed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutcomeRecord {
    pub sample_id: SampleId,
    #[serde(default)]
    pub request_id: Option<RequestId>,
    #[serde(default)]
    pub accepted_event_id: Option<EventId>,
    pub outcome: SampleOutcome,
    pub recorded_at: UtcTimestamp,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanSummary {
    pub matched: usize,
    pub unreadable_lines: usize,
    pub unreadable_outcomes: usize,
    pub multimodal_samples: usize,
    pub dropped_non_text_parts: usize,
    pub dropped_text_export_messages: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub files: usize,
    pub outcome_files: usize,
    pub total: usize,
    pub rated: usize,
    pub positive: usize,
    pub negative: usize,
    pub accepted: usize,
    pub not_accepted: usize,
    pub outcome_unknown: usize,
    pub by_character: BTreeMap<String, usize>,
    pub by_model: BTreeMap<String, usize>,
    pub unreadable_lines: usize,
    pub unreadable_outcomes: usize,
}

#[derive(Debug, Clone)]
pub struct SampleSummary {
    pub id: SampleId,
    pub timestamp: UtcTimestamp,
    pub character_id: String,
    pub model: Option<String>,
    pub rating: Option<Rating>,
    pub request_id: Option<RequestId>,
    pub outcome: Option<SampleOutcome>,
}

#[derive(Debug, Clone)]
pub struct ListOutcome {
    pub rows: Vec<SampleSummary>,
    pub matched: usize,
    pub unreadable_lines: usize,
    pub unreadable_outcomes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    RawJsonl,
    ShareGpt,
    ChatMl,
}

impl ExportFormat {
    pub fn parse(value: &str) -> Result<Self, FineTuneError> {
        match value {
            "raw" | "jsonl" => Ok(Self::RawJsonl),
            "sharegpt" => Ok(Self::ShareGpt),
            "chatml" => Ok(Self::ChatMl),
            _ => Err(FineTuneError::ExportFormat(value.to_owned())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExportReport {
    pub format: ExportFormat,
    pub output: PathBuf,
    pub matched: usize,
    pub written: usize,
    pub unreadable_lines: usize,
    pub unreadable_outcomes: usize,
    pub multimodal_samples: usize,
    pub dropped_non_text_parts: usize,
    pub dropped_text_export_messages: usize,
}

#[derive(Debug, Clone)]
pub struct FileFailure {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct ClearOutcome {
    pub removed: Vec<PathBuf>,
    pub failures: Vec<FileFailure>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetentionOutcome {
    pub removed_samples: usize,
    pub remaining_samples: usize,
    pub removed_files: usize,
    pub removed_outcomes: usize,
    pub preserved_unreadable_lines: usize,
}

#[derive(Debug, Error)]
pub enum FineTuneError {
    #[error("fine-tune data directory must be absolute: {0}")]
    RelativeDirectory(PathBuf),
    #[error("fine-tune data path is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("fine-tune I/O at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("monthly JSONL file has an incomplete final record: {0}")]
    IncompleteTail(PathBuf),
    #[error("sample {0} was not found")]
    SampleNotFound(SampleId),
    #[error("export destination already exists: {0}")]
    DestinationExists(PathBuf),
    #[error(
        "refusing to write an export into the managed store root or a managed dataset filename: {0}"
    )]
    ManagedOutput(PathBuf),
    #[error("no samples match the selected filters")]
    NoMatchingSamples,
    #[error("clear requires explicit confirmation")]
    ConfirmationRequired,
    #[error("fine-tune settings file not found: {0}")]
    SettingsNotFound(PathBuf),
    #[error("fine-tune settings are corrupt: {0}")]
    SettingsCorrupt(String),
    #[error("invalid fine-tune settings: {0}")]
    SettingsInvalid(String),
    #[error(transparent)]
    Time(#[from] TimeError),
    #[error("invalid retention: {0}")]
    Retention(String),
    #[error("unsupported export format: {0}")]
    ExportFormat(String),
}

impl From<std::io::Error> for FineTuneError {
    fn from(source: std::io::Error) -> Self {
        Self::Io {
            path: PathBuf::new(),
            source,
        }
    }
}
