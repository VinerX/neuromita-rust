use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum CharacterId {
    Crazy,
    Kind,
    Cappie,
    ShortHair,
    Mila,
    Sleepy,
    Ghost,
    Creepy,
    GameMaster,
}

impl CharacterId {
    pub const ALL: [Self; 9] = [
        Self::Crazy,
        Self::Kind,
        Self::Cappie,
        Self::ShortHair,
        Self::Mila,
        Self::Sleepy,
        Self::Ghost,
        Self::Creepy,
        Self::GameMaster,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Crazy => "Crazy",
            Self::Kind => "Kind",
            Self::Cappie => "Cappie",
            Self::ShortHair => "ShortHair",
            Self::Mila => "Mila",
            Self::Sleepy => "Sleepy",
            Self::Ghost => "Ghost",
            Self::Creepy => "Creepy",
            Self::GameMaster => "GameMaster",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|id| *id == self)
            .expect("the built-in catalog is exhaustive")
    }

    pub fn from_index(index: i32) -> Option<Self> {
        usize::try_from(index)
            .ok()
            .and_then(|index| Self::ALL.get(index).copied())
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|id| id.as_str() == value)
    }
}

impl fmt::Display for CharacterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestId(pub Uuid);

impl RequestId {
    pub(crate) fn new() -> Self {
        Self(Uuid::new_v4())
    }
}
impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EventId(pub Uuid);

impl EventId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}
impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}
