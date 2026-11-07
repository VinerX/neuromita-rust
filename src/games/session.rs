use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    EndReason, GameFact, GameId, GameKey, GameProjection, GameSessionId, ModelGameAction,
    ReactionInvite, UserGameCommand, seabattle,
};
use crate::identity::EventId;
use crate::timeline::domain::BranchId;

const START_INSTRUCTION: &str = "[Sea Battle] The player started a Sea Battle match with you. \
React briefly in character to the invitation. Do not start or end a game in this reply.";
const PLACEMENT_INSTRUCTION: &str = "[Sea Battle] The player has placed all ships. Place your own \
fleet now: put PlaceShip,<coordinate>,<length>,<H|V> commands (or PlaceShipsRandomly) in commands. \
Do not wait for the player to ask again.";
const CLOSED_INSTRUCTION: &str =
    "[Sea Battle] The player closed the Sea Battle match. React briefly and naturally in character
to the end of this match.";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GameError {
    #[error("mini-games are disabled")]
    Disabled,
    #[error("there is no active game")]
    NoActiveGame,
    #[error("the game is not in that phase")]
    WrongPhase,
    #[error("it is not that side's turn")]
    NotYourTurn,
    #[error("that cell was already shot")]
    AlreadyShot,
    #[error("invalid game command: {0}")]
    Invalid(&'static str),
    #[error("game state is stale for this generation")]
    StaleState,
    #[error("character owner is unavailable")]
    Unavailable,
    #[error("game operation failed: {0}")]
    Internal(String),
}

pub struct GameReceipt {
    pub game: GameId,
    pub key: Option<GameKey>,
    pub view: Option<GameProjection>,
    pub events: Vec<EventId>,
    pub auto_reactions: bool,
}

impl std::fmt::Debug for GameReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GameReceipt")
            .field("game", &self.game)
            .field("key", &self.key)
            .field("events", &self.events.len())
            .field("auto_reactions", &self.auto_reactions)
            .finish_non_exhaustive()
    }
}

pub struct GameReply {
    pub receipt: GameReceipt,
    pub invite: Option<ReactionInvite>,
}

impl std::fmt::Debug for GameReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GameReply")
            .field("receipt", &self.receipt)
            .field("invite", &self.invite.as_ref().map(|_| "reaction"))
            .finish()
    }
}

pub enum InviteCause {
    /// Index into the facts produced by the same command.
    Fact(usize),
    /// The last event this session already committed.
    LastEvent,
}

pub struct PendingInvite {
    pub cause: InviteCause,
    pub instruction: Arc<str>,
    pub auto_gated: bool,
}

pub struct Applied {
    pub facts: Vec<GameFact>,
    pub invite: Option<PendingInvite>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Session {
    id: GameSessionId,
    generation: u64,
    branch: BranchId,
    game: seabattle::SeaBattle,
    auto_reactions: bool,
    last_event: Option<EventId>,
}

impl Session {
    fn new(branch: BranchId, generation: u64, auto_reactions: bool) -> Self {
        Self {
            id: GameSessionId::new_v4(),
            generation,
            branch,
            game: seabattle::SeaBattle::new(),
            auto_reactions,
            last_event: None,
        }
    }

    fn key(&self) -> GameKey {
        GameKey {
            session: self.id,
            branch: self.branch,
            revision: self.game.revision(),
        }
    }

    fn projection(&self, side: seabattle::Side) -> GameProjection {
        GameProjection {
            game: GameId::SeaBattle,
            session: self.id,
            revision: self.game.revision(),
            phase: self.game.phase(),
            your_turn: self.game.turn() == side,
            side,
            own_board: self.game.own_board_text(side),
            opponent_board: self.game.opponent_board_text(side),
            remaining: self.game.remaining(side).to_vec(),
            shot_history: self.game.shot_history(side),
            last_shot: self.game.last_shot(),
            wounded: self.game.wounded(side),
            hunt_targets: self.game.hunt_targets(side),
            winner: self.game.winner(),
            status: self.game.status().map(str::to_owned),
            auto_reactions: self.auto_reactions,
        }
    }
}

/// Transient owner working copy reconstructed from canonical private Timeline state.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameSlot {
    current: Option<Session>,
    next_generation: u64,
}

impl std::fmt::Debug for GameSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GameSlot")
            .field("active", &self.current.is_some())
            .field("last_generation", &self.next_generation.saturating_sub(1))
            .finish_non_exhaustive()
    }
}

impl GameSlot {
    pub fn key(&self) -> Option<GameKey> {
        self.current.as_ref().map(Session::key)
    }

    pub fn is_empty(&self) -> bool {
        self.current.is_none()
    }

    pub fn projection(&self, side: seabattle::Side) -> Option<GameProjection> {
        self.current
            .as_ref()
            .map(|session| session.projection(side))
    }

    pub fn auto_reactions(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(|session| session.auto_reactions)
    }

    pub fn record_event(&mut self, event: EventId) {
        if let Some(session) = self.current.as_mut() {
            session.last_event = Some(event);
        }
    }

    pub fn last_event(&self) -> Option<EventId> {
        self.current.as_ref().and_then(|session| session.last_event)
    }

    pub fn clear(&mut self) {
        self.current = None;
    }

    pub(crate) fn bind_branch(&mut self, branch: BranchId) {
        if let Some(session) = self.current.as_mut() {
            session.branch = branch;
        }
    }

    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if let Some(session) = &self.current
            && (session.id.is_nil()
                || session.branch.0.is_nil()
                || session.generation.checked_add(1) != Some(self.next_generation))
        {
            return Err("invalid game generation");
        }
        Ok(())
    }

    pub fn apply_user(
        &mut self,
        branch: BranchId,
        command: UserGameCommand,
    ) -> Result<Applied, GameError> {
        let mut applied = Applied {
            facts: Vec::new(),
            invite: None,
        };
        match command {
            UserGameCommand::Start { auto_reactions } => {
                self.start(branch, auto_reactions, &mut applied)?;
            }
            UserGameCommand::PlaceShip {
                coord,
                length,
                orientation,
            } => {
                self.place(
                    seabattle::Side::Player,
                    coord,
                    length,
                    orientation,
                    &mut applied,
                )?;
                self.invite_player_placement(&mut applied);
            }
            UserGameCommand::PlaceFleetRandomly { seed: requested } => {
                self.place_random(seabattle::Side::Player, requested, &mut applied)?;
                self.invite_player_placement(&mut applied);
            }
            UserGameCommand::Fire { coord } => {
                self.fire(seabattle::Side::Player, coord, &mut applied)?;
            }
            UserGameCommand::RequestMitaTurn => {
                let session = self.current.as_ref().ok_or(GameError::NoActiveGame)?;
                let instruction = match session.game.phase() {
                    seabattle::Phase::Placement => {
                        if session.game.remaining(seabattle::Side::Mita).is_empty() {
                            return Err(GameError::WrongPhase);
                        }
                        PLACEMENT_INSTRUCTION
                    }
                    seabattle::Phase::Battle => {
                        if session.game.turn() != seabattle::Side::Mita {
                            return Err(GameError::NotYourTurn);
                        }
                        "[Sea Battle manual turn request] The player asked you to take your turn. \
React briefly in character and put exactly one legal MakeMove,<coordinate> command in commands."
                    }
                    seabattle::Phase::GameOver => return Err(GameError::WrongPhase),
                };
                applied.invite = Some(PendingInvite {
                    cause: InviteCause::LastEvent,
                    instruction: Arc::from(instruction),
                    auto_gated: false,
                });
            }
            UserGameCommand::SetAutoReactions(enabled) => {
                let session = self.current.as_mut().ok_or(GameError::NoActiveGame)?;
                session.auto_reactions = enabled;
            }
            UserGameCommand::Close => {
                let session = self.current.take().ok_or(GameError::NoActiveGame)?;
                let gated = session.auto_reactions;
                applied.facts.push(GameFact::Ended {
                    session: session.id,
                    generation: session.generation,
                    revision: session.game.revision(),
                    reason: EndReason::Closed,
                    winner: session.game.winner(),
                });
                applied.invite = Self::auto_invite(
                    gated,
                    InviteCause::Fact(applied.facts.len() - 1),
                    CLOSED_INSTRUCTION.to_owned(),
                );
            }
        }
        Ok(applied)
    }

    pub fn apply_model(
        &mut self,
        branch: BranchId,
        actions: &[ModelGameAction],
        seed: u64,
    ) -> Result<Applied, GameError> {
        let mut applied = Applied {
            facts: Vec::new(),
            invite: None,
        };
        for action in actions {
            match *action {
                ModelGameAction::Start => self.start(branch, true, &mut applied)?,
                ModelGameAction::PlaceFleetRandomly => {
                    self.place_random(seabattle::Side::Mita, seed, &mut applied)?;
                }
                ModelGameAction::PlaceShip {
                    coord,
                    length,
                    orientation,
                } => {
                    self.place(
                        seabattle::Side::Mita,
                        coord,
                        length,
                        orientation,
                        &mut applied,
                    )?;
                }
                ModelGameAction::Fire { coord } => {
                    self.fire(seabattle::Side::Mita, coord, &mut applied)?;
                }
                ModelGameAction::End => {
                    let session = self.current.take().ok_or(GameError::NoActiveGame)?;
                    applied.facts.push(GameFact::Ended {
                        session: session.id,
                        generation: session.generation,
                        revision: session.game.revision(),
                        reason: EndReason::Closed,
                        winner: session.game.winner(),
                    });
                    applied.invite = None;
                }
            }
        }
        Ok(applied)
    }

    fn start(
        &mut self,
        branch: BranchId,
        auto_reactions: bool,
        applied: &mut Applied,
    ) -> Result<(), GameError> {
        let next = self
            .next_generation
            .checked_add(1)
            .ok_or(GameError::Invalid("game generation overflow"))?;
        if let Some(previous) = self.current.take() {
            applied.facts.push(GameFact::Ended {
                session: previous.id,
                generation: previous.generation,
                revision: previous.game.revision(),
                reason: EndReason::Replaced,
                winner: previous.game.winner(),
            });
        }
        let generation = self.next_generation;
        self.next_generation = next;
        let session = Session::new(branch, generation, auto_reactions);
        applied.facts.push(GameFact::Started {
            session: session.id,
            generation,
            auto_reactions,
        });
        applied.invite = Self::auto_invite(
            auto_reactions,
            InviteCause::Fact(applied.facts.len() - 1),
            START_INSTRUCTION.to_owned(),
        );
        self.current = Some(session);
        Ok(())
    }

    fn place(
        &mut self,
        side: seabattle::Side,
        coord: seabattle::Coordinate,
        length: u8,
        orientation: seabattle::Orientation,
        applied: &mut Applied,
    ) -> Result<(), GameError> {
        let session = self.current.as_mut().ok_or(GameError::NoActiveGame)?;
        let before = session.game.phase();
        session
            .game
            .place(side, coord, length, orientation)
            .map_err(map_placement)?;
        Self::record_phase(session, before, applied);
        Ok(())
    }

    fn place_random(
        &mut self,
        side: seabattle::Side,
        seed: u64,
        applied: &mut Applied,
    ) -> Result<(), GameError> {
        let session = self.current.as_mut().ok_or(GameError::NoActiveGame)?;
        let before = session.game.phase();
        let mut rng = seabattle::DeterministicRng::new(seed);
        session
            .game
            .place_random_fleet(side, &mut rng)
            .map_err(map_placement)?;
        Self::record_phase(session, before, applied);
        Ok(())
    }

    fn record_phase(session: &Session, before: seabattle::Phase, applied: &mut Applied) {
        let after = session.game.phase();
        if before != after {
            applied.facts.push(GameFact::PhaseChanged {
                session: session.id,
                generation: session.generation,
                revision: session.game.revision(),
                phase: after,
            });
        }
    }

    fn fire(
        &mut self,
        attacker: seabattle::Side,
        coord: seabattle::Coordinate,
        applied: &mut Applied,
    ) -> Result<(), GameError> {
        let session = self.current.as_mut().ok_or(GameError::NoActiveGame)?;
        let gated = session.auto_reactions;
        let result = session.game.shot(attacker, coord).map_err(map_shot)?;
        let index = applied.facts.len();
        applied.facts.push(GameFact::Shot {
            session: session.id,
            generation: session.generation,
            revision: session.game.revision(),
            attacker,
            coord,
            result,
            turn_after: session.game.turn(),
        });
        let over = session.game.phase() == seabattle::Phase::GameOver;
        if over {
            applied.facts.push(GameFact::Ended {
                session: session.id,
                generation: session.generation,
                revision: session.game.revision(),
                reason: EndReason::Finished,
                winner: session.game.winner(),
            });
            let outcome = if session.game.winner() == Some(seabattle::Side::Mita) {
                "you won"
            } else {
                "the player won"
            };
            applied.invite = Self::auto_invite(
                gated,
                InviteCause::Fact(applied.facts.len() - 1),
                format!(
                    "[Sea Battle] The match is over: {outcome}. React briefly in character; \
do not take another shot."
                ),
            );
            return Ok(());
        }
        match (attacker, result) {
            (seabattle::Side::Player, seabattle::ShotResult::Miss) => {
                applied.invite = Self::auto_invite(
                    gated,
                    InviteCause::Fact(index),
                    format!(
                        "[Sea Battle automatic turn request] The player fired at {coord}: miss. \
It is now your turn. React briefly in character and put exactly one legal \
MakeMove,<coordinate> command in commands. Do not wait for the player to ask again."
                    ),
                );
            }
            (
                seabattle::Side::Player,
                seabattle::ShotResult::Hit | seabattle::ShotResult::Sunk { .. },
            ) => {
                applied.invite = Self::auto_invite(
                    gated,
                    InviteCause::Fact(index),
                    format!(
                        "[Sea Battle] The player fired at {coord}: {}. The player keeps the turn, \
so do not take a shot now. React briefly in character and do not put a move command here.",
                        result.code()
                    ),
                );
            }
            (
                seabattle::Side::Mita,
                seabattle::ShotResult::Hit | seabattle::ShotResult::Sunk { .. },
            ) => {
                applied.invite = Self::auto_invite(
                    gated,
                    InviteCause::Fact(index),
                    format!(
                        "[Sea Battle follow-up turn request] You fired at {coord}: {}. It is still \
your turn. React briefly in character and put exactly one legal MakeMove,<coordinate> \
command in commands. Do not wait for the player to ask again.",
                        result.code()
                    ),
                );
            }
            _ => {}
        }
        Ok(())
    }

    fn invite_player_placement(&mut self, applied: &mut Applied) {
        let Some(session) = self.current.as_ref() else {
            return;
        };
        if session.game.phase() != seabattle::Phase::Placement
            || !session.game.remaining(seabattle::Side::Player).is_empty()
            || session.game.remaining(seabattle::Side::Mita).is_empty()
        {
            return;
        }
        let cause = match applied.facts.len() {
            0 => InviteCause::LastEvent,
            index => InviteCause::Fact(index - 1),
        };
        applied.invite = Self::auto_invite(
            session.auto_reactions,
            cause,
            PLACEMENT_INSTRUCTION.to_owned(),
        );
    }

    /// Automatic reactions exist only while the per-match flag is on, and they
    /// still pass the global policy gate before admission. Explicit manual
    /// requests bypass this helper.
    fn auto_invite(gated: bool, cause: InviteCause, instruction: String) -> Option<PendingInvite> {
        gated.then(|| PendingInvite {
            cause,
            instruction: Arc::from(instruction),
            auto_gated: true,
        })
    }
}

fn map_placement(error: seabattle::PlacementError) -> GameError {
    use seabattle::PlacementError;
    match error {
        PlacementError::WrongPhase => GameError::WrongPhase,
        PlacementError::OutOfBounds => GameError::Invalid("placement is outside the board"),
        PlacementError::Conflict => GameError::Invalid("ships must not touch, including diagonals"),
        PlacementError::InvalidLength => GameError::Invalid("ship length must be 1..=4"),
        PlacementError::LengthUnavailable(_) => GameError::Invalid("that ship is already placed"),
        PlacementError::RandomFailed(_) => {
            GameError::Invalid("random placement did not fit the whole fleet")
        }
    }
}

fn map_shot(error: seabattle::ShotError) -> GameError {
    use seabattle::ShotError;
    match error {
        ShotError::WrongPhase => GameError::WrongPhase,
        ShotError::NotYourTurn => GameError::NotYourTurn,
        ShotError::AlreadyShot => GameError::AlreadyShot,
        ShotError::OutOfBounds => GameError::Invalid("coordinates are outside the board"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::seabattle::{Coordinate, Orientation};

    const LAYOUT: [(&str, u8); 10] = [
        ("A1", 4),
        ("F1", 3),
        ("A3", 3),
        ("E3", 2),
        ("H3", 2),
        ("A5", 2),
        ("D5", 1),
        ("F5", 1),
        ("H5", 1),
        ("J5", 1),
    ];

    fn place_player(slot: &mut GameSlot, branch: BranchId) {
        for (coord, length) in LAYOUT {
            slot.apply_user(
                branch,
                UserGameCommand::PlaceShip {
                    coord: Coordinate::parse(coord).unwrap(),
                    length,
                    orientation: Orientation::Horizontal,
                },
            )
            .unwrap();
        }
    }

    fn place_mita(slot: &mut GameSlot, branch: BranchId) -> Applied {
        let actions: Vec<_> = LAYOUT
            .iter()
            .map(|(coord, length)| ModelGameAction::PlaceShip {
                coord: Coordinate::parse(coord).unwrap(),
                length: *length,
                orientation: Orientation::Horizontal,
            })
            .collect();
        slot.apply_model(branch, &actions, 0).unwrap()
    }

    fn slot_with_battle(auto: bool) -> (GameSlot, BranchId) {
        let branch = BranchId::new();
        let mut slot = GameSlot::default();
        slot.apply_user(
            branch,
            UserGameCommand::Start {
                auto_reactions: auto,
            },
        )
        .unwrap();
        place_player(&mut slot, branch);
        place_mita(&mut slot, branch);
        assert_eq!(
            slot.current.as_ref().unwrap().game.phase(),
            seabattle::Phase::Battle
        );
        (slot, branch)
    }

    #[test]
    fn placement_invite_is_not_emitted_after_the_fleets_are_complete() {
        let branch = BranchId::new();
        let mut slot = GameSlot::default();
        slot.apply_user(
            branch,
            UserGameCommand::Start {
                auto_reactions: true,
            },
        )
        .unwrap();
        let mut last = None;
        for (coord, length) in LAYOUT {
            last = Some(
                slot.apply_user(
                    branch,
                    UserGameCommand::PlaceShip {
                        coord: Coordinate::parse(coord).unwrap(),
                        length,
                        orientation: Orientation::Horizontal,
                    },
                )
                .unwrap(),
            );
        }
        assert!(last.unwrap().invite.is_some());
        let applied = place_mita(&mut slot, branch);
        assert!(applied.invite.is_none());
        assert!(matches!(
            applied.facts.first(),
            Some(GameFact::PhaseChanged {
                phase: seabattle::Phase::Battle,
                ..
            })
        ));
    }

    #[test]
    fn player_hit_keeps_turn_and_miss_passes_it() {
        let (mut slot, branch) = slot_with_battle(true);
        let hit = slot
            .apply_user(
                branch,
                UserGameCommand::Fire {
                    coord: Coordinate::parse("A1").unwrap(),
                },
            )
            .unwrap();
        let invite = hit.invite.expect("automatic hit reaction");
        assert!(invite.auto_gated);
        assert!(invite.instruction.contains("keeps the turn"));
        assert!(!invite.instruction.contains("MakeMove,<coordinate>"));
        assert_eq!(
            slot.current.as_ref().unwrap().game.turn(),
            seabattle::Side::Player
        );
        let miss = slot
            .apply_user(
                branch,
                UserGameCommand::Fire {
                    coord: Coordinate::parse("J10").unwrap(),
                },
            )
            .unwrap();
        assert!(
            miss.invite
                .unwrap()
                .instruction
                .contains("MakeMove,<coordinate>")
        );
        assert_eq!(
            slot.current.as_ref().unwrap().game.turn(),
            seabattle::Side::Mita
        );
    }

    #[test]
    fn per_match_flag_disables_automatic_invites_but_not_manual_turns() {
        let (mut slot, branch) = slot_with_battle(false);
        let miss = slot
            .apply_user(
                branch,
                UserGameCommand::Fire {
                    coord: Coordinate::parse("J10").unwrap(),
                },
            )
            .unwrap();
        assert!(miss.invite.is_none());
        let manual = slot
            .apply_user(branch, UserGameCommand::RequestMitaTurn)
            .unwrap();
        let invite = manual.invite.expect("manual turn invite");
        assert!(!invite.auto_gated);
        assert!(invite.instruction.contains("MakeMove,<coordinate>"));
    }

    #[test]
    fn replacing_a_match_keeps_the_previous_one_until_the_new_one_is_valid() {
        let (slot, branch) = slot_with_battle(true);
        let before_key = slot.key();
        let before_view = slot.projection(seabattle::Side::Player);
        let mut attempted = slot.clone();
        let result = attempted.apply_model(
            branch,
            &[
                ModelGameAction::Start,
                ModelGameAction::PlaceShip {
                    coord: Coordinate::parse("A1").unwrap(),
                    length: 4,
                    orientation: Orientation::Horizontal,
                },
                ModelGameAction::Fire {
                    coord: Coordinate::parse("J10").unwrap(),
                },
            ],
            0,
        );
        assert_eq!(result.err(), Some(GameError::WrongPhase));
        assert_eq!(slot.key(), before_key);
        assert_eq!(slot.projection(seabattle::Side::Player), before_view);
    }

    #[test]
    fn fork_projection_binds_inherited_match_to_selected_branch() {
        let (mut slot, branch) = slot_with_battle(true);
        let view = slot.projection(seabattle::Side::Player).unwrap();
        assert!(!view.own_board.is_empty());
        let child = BranchId::new();
        slot.bind_branch(child);
        assert_ne!(child, branch);
        assert_eq!(slot.key().unwrap().branch, child);
        assert_eq!(slot.projection(seabattle::Side::Player).unwrap(), view);
    }
}
