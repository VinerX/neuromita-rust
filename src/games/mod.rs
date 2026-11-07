//! Character-owned desktop mini-games.
//!
//! The rules engine is pure and deterministic. The authoritative match lives
//! inside a `CharacterHandler`; this module only defines typed commands,
//! sanitized public facts and the read-only projections handed to the model.

pub mod api;
pub mod seabattle;
mod session;

pub use api::{GameApi, GameOutcome};

use std::fmt;
use std::sync::Arc;

use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::identity::EventId;
use crate::timeline::domain::BranchId;
use seabattle::{Coordinate, Orientation, Phase, ShotResult, Side};

pub use session::{GameError, GameReceipt, GameReply, GameSlot, InviteCause, PendingInvite};

pub type GameSessionId = Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameId {
    SeaBattle,
}

impl GameId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SeaBattle => "seabattle",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "seabattle" => Some(Self::SeaBattle),
            _ => None,
        }
    }
}

impl fmt::Display for GameId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Session identity plus the accepted-action revision the model saw. Any
/// change (or a close/recreate) invalidates a pending generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameKey {
    pub session: GameSessionId,
    pub branch: BranchId,
    pub revision: u64,
}

/// Explicit application gates. `enabled` forbids new matches; the per-match
/// flag is separate and only the owner may change it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GamePolicy {
    pub enabled: bool,
    pub auto_reactions: bool,
}

impl Default for GamePolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_reactions: true,
        }
    }
}

/// Commands accepted from the human owner. They never carry an acting side:
/// the owner always plays the player side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserGameCommand {
    Start {
        auto_reactions: bool,
    },
    PlaceShip {
        coord: Coordinate,
        length: u8,
        orientation: Orientation,
    },
    PlaceFleetRandomly {
        seed: u64,
    },
    Fire {
        coord: Coordinate,
    },
    RequestMitaTurn,
    SetAutoReactions(bool),
    Close,
}

/// Actions a model response may propose. There is deliberately no actor field:
/// the only model role is Mita, so actor spoofing is not expressible.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ModelGameAction {
    Start,
    PlaceFleetRandomly,
    PlaceShip {
        coord: Coordinate,
        length: u8,
        orientation: Orientation,
    },
    Fire {
        coord: Coordinate,
    },
    End,
}

impl ModelGameAction {
    /// Parse one command string from a structured response. Anything outside
    /// the verified Sea Battle vocabulary is rejected.
    pub fn parse_command(value: &str) -> Option<Self> {
        let command = value.trim();
        let (head, rest) = match command.split_once(',') {
            Some((head, rest)) => (head.trim().to_ascii_lowercase(), Some(rest)),
            None => (command.to_ascii_lowercase(), None),
        };
        match (head.as_str(), rest) {
            ("placeshipsrandomly", None) => Some(Self::PlaceFleetRandomly),
            ("makemove", Some(rest)) => {
                let coord = Coordinate::parse(rest)?;
                Some(Self::Fire { coord })
            }
            ("placeship", Some(rest)) => {
                let mut parts = rest.split(',');
                let coord = Coordinate::parse(parts.next()?)?;
                let length: u8 = parts.next()?.trim().parse().ok()?;
                let orientation = Orientation::parse(parts.next()?)?;
                if parts.next().is_some() {
                    return None;
                }
                Some(Self::PlaceShip {
                    coord,
                    length,
                    orientation,
                })
            }
            _ => None,
        }
    }

    pub fn parse_start(value: &str) -> Option<Self> {
        (GameId::parse(value) == Some(GameId::SeaBattle)).then_some(Self::Start)
    }

    pub fn parse_end(value: &str) -> Option<Self> {
        (GameId::parse(value) == Some(GameId::SeaBattle)).then_some(Self::End)
    }
}

impl fmt::Debug for ModelGameAction {
    /// Placement coordinates reveal a fleet; diagnostics keep only the shape.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start => f.write_str("Start"),
            Self::PlaceFleetRandomly => f.write_str("PlaceFleetRandomly"),
            Self::PlaceShip {
                length,
                orientation,
                ..
            } => f
                .debug_struct("PlaceShip")
                .field("length", length)
                .field("orientation", orientation)
                .finish_non_exhaustive(),
            Self::Fire { coord } => f.debug_tuple("Fire").field(coord).finish(),
            Self::End => f.write_str("End"),
        }
    }
}

/// Why a public match event happened. Payloads are built here so a board can
/// never leak into the shared timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    Closed,
    Replaced,
    Finished,
}

impl EndReason {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Replaced => "replaced",
            Self::Finished => "finished",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameFact {
    Started {
        session: GameSessionId,
        generation: u64,
        auto_reactions: bool,
    },
    PhaseChanged {
        session: GameSessionId,
        generation: u64,
        revision: u64,
        phase: Phase,
    },
    Shot {
        session: GameSessionId,
        generation: u64,
        revision: u64,
        attacker: Side,
        coord: Coordinate,
        result: ShotResult,
        turn_after: Side,
    },
    Ended {
        session: GameSessionId,
        generation: u64,
        revision: u64,
        reason: EndReason,
        winner: Option<Side>,
    },
}

impl GameFact {
    pub const fn game(&self) -> GameId {
        GameId::SeaBattle
    }

    pub const fn session(&self) -> GameSessionId {
        match self {
            Self::Started { session, .. }
            | Self::PhaseChanged { session, .. }
            | Self::Shot { session, .. }
            | Self::Ended { session, .. } => *session,
        }
    }

    pub const fn generation(&self) -> u64 {
        match self {
            Self::Started { generation, .. }
            | Self::PhaseChanged { generation, .. }
            | Self::Shot { generation, .. }
            | Self::Ended { generation, .. } => *generation,
        }
    }

    /// Sanitized event payload: game identity, public move facts and outcome.
    pub fn payload(&self) -> Map<String, Value> {
        let mut payload = Map::new();
        payload.insert("game_id".into(), json!(self.game().as_str()));
        payload.insert("session_id".into(), json!(self.session()));
        payload.insert("generation".into(), json!(self.generation()));
        match self {
            Self::Started { auto_reactions, .. } => {
                payload.insert("event".into(), json!("started"));
                payload.insert("auto_reactions".into(), json!(auto_reactions));
            }
            Self::PhaseChanged {
                revision, phase, ..
            } => {
                payload.insert("event".into(), json!("phase_changed"));
                payload.insert("revision".into(), json!(revision));
                payload.insert("phase".into(), json!(phase.code()));
            }
            Self::Shot {
                revision,
                attacker,
                coord,
                result,
                turn_after,
                ..
            } => {
                payload.insert("event".into(), json!("shot"));
                payload.insert("revision".into(), json!(revision));
                payload.insert("actor".into(), json!(attacker.label()));
                payload.insert("coordinate".into(), json!(coord.to_string()));
                payload.insert("result".into(), json!(result.code()));
                payload.insert("turn_after".into(), json!(turn_after.label()));
            }
            Self::Ended {
                revision,
                reason,
                winner,
                ..
            } => {
                payload.insert("event".into(), json!("ended"));
                payload.insert("revision".into(), json!(revision));
                payload.insert("reason".into(), json!(reason.code()));
                payload.insert("winner".into(), json!(winner.map(Side::label)));
            }
        }
        payload
    }
}

/// Read-only board projection for one side. The opponent board only contains
/// already-observed cells, so this type is safe to show to its owner.
#[derive(Clone, PartialEq, Eq)]
pub struct GameProjection {
    pub game: GameId,
    pub session: GameSessionId,
    pub revision: u64,
    pub phase: Phase,
    pub your_turn: bool,
    pub side: Side,
    pub own_board: Vec<String>,
    pub opponent_board: Vec<String>,
    pub remaining: Vec<u8>,
    pub shot_history: Vec<Coordinate>,
    pub last_shot: Option<seabattle::LastShot>,
    pub wounded: Vec<Coordinate>,
    pub hunt_targets: Vec<Coordinate>,
    pub winner: Option<Side>,
    pub status: Option<String>,
    pub auto_reactions: bool,
}

impl fmt::Debug for GameProjection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GameProjection")
            .field("game", &self.game)
            .field("session", &self.session)
            .field("revision", &self.revision)
            .field("phase", &self.phase)
            .field("your_turn", &self.your_turn)
            .field("side", &self.side)
            .field("remaining", &self.remaining)
            .finish_non_exhaustive()
    }
}

impl GameProjection {
    /// Status text from the model's point of view. The CLI must use
    /// [`Self::status_line_for`] so a human never sees "your turn" for Mita.
    pub fn status_line(&self) -> &'static str {
        self.status_line_for(Side::Mita)
    }

    pub fn status_line_for(&self, viewer: Side) -> &'static str {
        match self.phase {
            Phase::Placement => "Расстановка кораблей",
            Phase::GameOver => "Игра окончена",
            Phase::Battle => {
                let viewer_turn = (viewer == self.side) == self.your_turn;
                match (viewer_turn, viewer) {
                    (true, _) => "Твой ход",
                    (false, Side::Mita) => "Ход игрока",
                    (false, Side::Player) => "Ход Миты",
                }
            }
        }
    }

    fn board_text(&self, board: &[String]) -> String {
        let header = format!(
            "  {}",
            ('A'..='J')
                .map(|letter| letter.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        );
        let rows = board
            .iter()
            .enumerate()
            .map(|(index, row)| {
                format!(
                    "{:<2}{}",
                    index + 1,
                    row.chars()
                        .map(|cell| cell.to_string())
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("{header}\n{rows}")
    }

    /// Bounded prompt block for the owning model. It contains only this side's
    /// own fleet and the cells it has already observed on the opponent board.
    pub fn prompt_block(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!("[SEA BATTLE] Фаза: {}", self.phase.code()));
        lines.push(format!("Ход: {}.", self.status_line()));
        if let Some(winner) = self.winner {
            lines.push(format!(
                "Победитель: {}.",
                if winner == self.side {
                    "ты"
                } else {
                    "игрок"
                }
            ));
        }
        if !self.remaining.is_empty() {
            lines.push(format!(
                "Корабли к расстановке: {}.",
                self.remaining
                    .iter()
                    .map(u8::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        lines.push("Твоё поле (S — твой корабль, H — попадание, X — потоплен, M — мимо):".into());
        lines.push(self.board_text(&self.own_board));
        lines.push("Поле противника (только уже отмеченные клетки):".into());
        lines.push(self.board_text(&self.opponent_board));
        if !self.shot_history.is_empty() {
            lines.push(format!(
                "Твои выстрелы: {}.",
                self.shot_history
                    .iter()
                    .map(Coordinate::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(last) = self.last_shot {
            lines.push(format!(
                "Последний выстрел: {} по {} — {}. Ход после него: {}.",
                last.attacker.label(),
                last.coord,
                last.result.code(),
                last.turn_after().label()
            ));
        }
        if !self.wounded.is_empty() {
            lines.push(format!(
                "Подбитые, но не потопленные корабли: {}.",
                self.wounded
                    .iter()
                    .map(Coordinate::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !self.hunt_targets.is_empty() {
            lines.push(format!(
                "Возможные цели: {}.",
                self.hunt_targets
                    .iter()
                    .map(Coordinate::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(status) = &self.status {
            lines.push(format!("Последняя ошибка: {status}"));
        }
        lines.push(
            "Команды: PlaceShip,<A1..J10>,<1..4>,<H|V>; PlaceShipsRandomly; MakeMove,<A1..J10>."
                .into(),
        );
        lines.join("\n")
    }
}

/// Identity of the event a reaction refers to. It is always a committed match
/// fact, never a synthetic player or world event.
#[derive(Debug, Clone)]
pub struct ReactionInvite {
    pub event_id: EventId,
    pub instruction: Arc<str>,
    /// Automatic shot-driven reactions honour the global and per-match gates.
    pub auto_gated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::seabattle::{Coordinate, ShotResult};

    fn session() -> GameSessionId {
        Uuid::nil()
    }

    #[test]
    fn public_game_facts_never_serialize_boards_or_commands() {
        let facts = [
            GameFact::Started {
                session: session(),
                generation: 1,
                auto_reactions: true,
            },
            GameFact::PhaseChanged {
                session: session(),
                generation: 1,
                revision: 1,
                phase: Phase::Battle,
            },
            GameFact::Shot {
                session: session(),
                generation: 1,
                revision: 2,
                attacker: Side::Mita,
                coord: Coordinate::parse("D5").unwrap(),
                result: ShotResult::Hit,
                turn_after: Side::Mita,
            },
            GameFact::Ended {
                session: session(),
                generation: 1,
                revision: 3,
                reason: EndReason::Finished,
                winner: Some(Side::Mita),
            },
        ];
        for fact in facts {
            let serialized = Value::Object(fact.payload()).to_string();
            for forbidden in ["SSSS", "S.........", "PlaceShip", "MakeMove", "board"] {
                assert!(!serialized.contains(forbidden), "{serialized}");
            }
        }
    }

    #[test]
    fn status_text_is_written_from_the_viewer_side() {
        let projection = GameProjection {
            game: GameId::SeaBattle,
            session: session(),
            revision: 1,
            phase: Phase::Battle,
            your_turn: true,
            side: Side::Mita,
            own_board: Vec::new(),
            opponent_board: Vec::new(),
            remaining: Vec::new(),
            shot_history: Vec::new(),
            last_shot: None,
            wounded: Vec::new(),
            hunt_targets: Vec::new(),
            winner: None,
            status: None,
            auto_reactions: true,
        };
        assert_eq!(projection.status_line(), "Твой ход");
        assert_eq!(projection.status_line_for(Side::Player), "Ход Миты");
        assert_eq!(projection.status_line_for(Side::Mita), "Твой ход");
    }
}
