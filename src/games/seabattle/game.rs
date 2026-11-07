use serde::{Deserialize, Serialize};
use std::fmt;

use super::coordinate::{BOARD_SIZE, Coordinate};

/// The verified Python fleet: 4,3,3,2,2,2,1,1,1,1 ships.
pub const FLEET: [u8; 10] = [4, 3, 3, 2, 2, 2, 1, 1, 1, 1];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    Player,
    Mita,
}

impl Side {
    pub const fn other(self) -> Self {
        match self {
            Self::Player => Self::Mita,
            Self::Mita => Self::Player,
        }
    }

    pub const fn index(self) -> u8 {
        match self {
            Self::Player => 0,
            Self::Mita => 1,
        }
    }

    pub fn from_index(index: u8) -> Option<Self> {
        match index {
            0 => Some(Self::Player),
            1 => Some(Self::Mita),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Player => "player",
            Self::Mita => "mita",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    Placement,
    Battle,
    GameOver,
}

impl Phase {
    /// Wire name used by the Python state projection.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Placement => "placement",
            Self::Battle => "battle",
            Self::GameOver => "game_over",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Orientation {
    Horizontal,
    Vertical,
}

impl Orientation {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "H" => Some(Self::Horizontal),
            "V" => Some(Self::Vertical),
            _ => None,
        }
    }

    pub const fn code(self) -> &'static str {
        match self {
            Self::Horizontal => "H",
            Self::Vertical => "V",
        }
    }

    fn cells(self, start: Coordinate, length: usize) -> Option<Vec<Coordinate>> {
        (0..length)
            .map(|offset| match self {
                Self::Horizontal => Coordinate::new(start.x() + offset as u8, start.y()),
                Self::Vertical => Coordinate::new(start.x(), start.y() + offset as u8),
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cell {
    Empty,
    /// Reserved buffer around a placed ship; placement and shots treat it as water.
    Forbidden,
    Ship,
    Hit,
    Miss,
    Sunk,
}

impl Cell {
    const fn own_char(self) -> char {
        match self {
            Self::Empty | Self::Forbidden => '.',
            Self::Ship => 'S',
            Self::Hit => 'H',
            Self::Miss => 'M',
            Self::Sunk => 'X',
        }
    }
}

/// Opponent-facing cell. Untouched ships stay `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpponentCell {
    Unknown,
    Hit,
    Miss,
    Sunk,
}

impl OpponentCell {
    const fn as_char(self) -> char {
        match self {
            Self::Unknown => '.',
            Self::Hit => 'H',
            Self::Miss => 'M',
            Self::Sunk => 'X',
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShotResult {
    Miss,
    Hit,
    Sunk { length: u8 },
    Win { length: u8 },
}

impl ShotResult {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Miss => "miss",
            Self::Hit => "hit",
            Self::Sunk { .. } => "sunk",
            Self::Win { .. } => "win",
        }
    }

    pub const fn keeps_turn(self) -> bool {
        !matches!(self, Self::Miss)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShotError {
    WrongPhase,
    NotYourTurn,
    OutOfBounds,
    AlreadyShot,
}

impl ShotError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongPhase => "invalid_phase",
            Self::NotYourTurn => "not_your_turn",
            Self::OutOfBounds => "invalid_coord",
            Self::AlreadyShot => "already_shot",
        }
    }

    pub const fn message(self) -> &'static str {
        match self {
            Self::WrongPhase => "Неверная фаза игры",
            Self::NotYourTurn => "Сейчас не ваш ход",
            Self::OutOfBounds => "Неверные координаты",
            Self::AlreadyShot => "Вы уже стреляли в эту клетку",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementError {
    WrongPhase,
    InvalidLength,
    LengthUnavailable(u8),
    OutOfBounds,
    /// Occupied cell or an adjacent (including diagonal) ship.
    Conflict,
    /// Bounded random placement did not fit the whole fleet; nothing changed.
    RandomFailed(u8),
}

impl PlacementError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongPhase => "invalid_phase",
            Self::InvalidLength => "invalid_length",
            Self::LengthUnavailable(_) => "length_unavailable",
            Self::OutOfBounds => "out_of_bounds",
            Self::Conflict => "conflict",
            Self::RandomFailed(_) => "random_failed",
        }
    }
}

impl fmt::Display for PlacementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongPhase => f.write_str("Неверная фаза игры"),
            Self::InvalidLength => f.write_str("Неверная длина корабля"),
            Self::LengthUnavailable(length) => {
                write!(f, "Корабль длины {length} уже размещен или недоступен")
            }
            Self::OutOfBounds => f.write_str("Выход за границы поля"),
            Self::Conflict => f.write_str("Нельзя ставить корабли вплотную или на занятую клетку"),
            Self::RandomFailed(length) => {
                write!(f, "Не удалось разместить {length}-палубный корабль")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastShot {
    pub attacker: Side,
    pub coord: Coordinate,
    pub result: ShotResult,
}

impl LastShot {
    pub const fn turn_after(self) -> Side {
        if self.result.keeps_turn() {
            self.attacker
        } else {
            self.attacker.other()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Ship {
    coords: Vec<Coordinate>,
    hits: Vec<bool>,
    sunk: bool,
}

impl Ship {
    fn length(&self) -> u8 {
        self.coords.len() as u8
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Board {
    cells: [[Cell; BOARD_SIZE]; BOARD_SIZE],
    ships: Vec<Ship>,
    remaining: Vec<u8>,
}

impl Board {
    fn new() -> Self {
        Self {
            cells: [[Cell::Empty; BOARD_SIZE]; BOARD_SIZE],
            ships: Vec::new(),
            remaining: FLEET.to_vec(),
        }
    }

    fn cell(&self, coord: Coordinate) -> Cell {
        self.cells[coord.y() as usize][coord.x() as usize]
    }

    fn set(&mut self, coord: Coordinate, cell: Cell) {
        self.cells[coord.y() as usize][coord.x() as usize] = cell;
    }

    fn place(
        &mut self,
        start: Coordinate,
        length: u8,
        orientation: Orientation,
    ) -> Result<(), PlacementError> {
        if !FLEET.contains(&length) {
            return Err(PlacementError::InvalidLength);
        }
        let Some(index) = self
            .remaining
            .iter()
            .position(|candidate| *candidate == length)
        else {
            return Err(PlacementError::LengthUnavailable(length));
        };
        let coords = orientation
            .cells(start, length as usize)
            .ok_or(PlacementError::OutOfBounds)?;
        if coords.iter().any(|coord| self.cell(*coord) != Cell::Empty) {
            return Err(PlacementError::Conflict);
        }
        for coord in &coords {
            self.set(*coord, Cell::Ship);
            for neighbor in coord.surrounding() {
                if self.cell(neighbor) == Cell::Empty {
                    self.set(neighbor, Cell::Forbidden);
                }
            }
        }
        self.ships.push(Ship {
            coords,
            hits: vec![false; length as usize],
            sunk: false,
        });
        self.remaining.remove(index);
        Ok(())
    }

    fn all_sunk(&self) -> bool {
        !self.ships.is_empty() && self.ships.iter().all(|ship| ship.sunk)
    }

    fn receive(&mut self, coord: Coordinate) -> Result<ShotResult, ShotError> {
        match self.cell(coord) {
            Cell::Empty | Cell::Forbidden => {
                self.set(coord, Cell::Miss);
                Ok(ShotResult::Miss)
            }
            Cell::Hit | Cell::Miss | Cell::Sunk => Err(ShotError::AlreadyShot),
            Cell::Ship => {
                self.set(coord, Cell::Hit);
                let ship = self
                    .ships
                    .iter_mut()
                    .find(|ship| ship.coords.contains(&coord))
                    .expect("ship cell belongs to a ship");
                let index = ship
                    .coords
                    .iter()
                    .position(|candidate| *candidate == coord)
                    .expect("ship contains its own cell");
                ship.hits[index] = true;
                let length = ship.length();
                if !ship.hits.iter().all(|hit| *hit) {
                    return Ok(ShotResult::Hit);
                }
                ship.sunk = true;
                let sunk_cells = ship.coords.clone();
                for sunk in sunk_cells {
                    self.set(sunk, Cell::Sunk);
                }
                Ok(if self.all_sunk() {
                    ShotResult::Win { length }
                } else {
                    ShotResult::Sunk { length }
                })
            }
        }
    }

    fn own_row(&self, row: usize) -> String {
        self.cells[row].iter().map(|cell| cell.own_char()).collect()
    }

    fn opponent_row(&self, row: usize) -> String {
        self.cells[row]
            .iter()
            .map(|cell| match cell {
                Cell::Hit => OpponentCell::Hit.as_char(),
                Cell::Miss => OpponentCell::Miss.as_char(),
                Cell::Sunk => OpponentCell::Sunk.as_char(),
                Cell::Empty | Cell::Forbidden | Cell::Ship => OpponentCell::Unknown.as_char(),
            })
            .collect()
    }

    fn shot_cells(&self) -> Vec<Coordinate> {
        let mut cells = Vec::new();
        for (y, row) in self.cells.iter().enumerate() {
            for (x, cell) in row.iter().enumerate() {
                if matches!(cell, Cell::Hit | Cell::Miss | Cell::Sunk) {
                    cells.push(Coordinate::new(x as u8, y as u8).expect("board coordinate"));
                }
            }
        }
        cells
    }
}

/// Pure, deterministic Sea Battle rules. It performs no I/O and knows nothing
/// about characters, timelines or the model; the owner applies it to a clone
/// and commits only after validation.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "GameRecord", into = "GameRecord")]
pub struct SeaBattle {
    record: Vec<RecordedAction>,
    player: Board,
    mita: Board,
    phase: Phase,
    turn: Side,
    winner: Option<Side>,
    last_shot: Option<LastShot>,
    status: Option<String>,
    revision: u64,
}

impl fmt::Debug for SeaBattle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SeaBattle")
            .field("phase", &self.phase)
            .field("turn", &self.turn)
            .field("winner", &self.winner)
            .field("revision", &self.revision)
            .field("player_ships", &self.player.ships.len())
            .field("mita_ships", &self.mita.ships.len())
            .finish_non_exhaustive()
    }
}

impl SeaBattle {
    pub fn new() -> Self {
        Self {
            record: Vec::new(),
            player: Board::new(),
            mita: Board::new(),
            phase: Phase::Placement,
            turn: Side::Player,
            winner: None,
            last_shot: None,
            status: None,
            revision: 0,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn turn(&self) -> Side {
        self.turn
    }

    pub fn winner(&self) -> Option<Side> {
        self.winner
    }

    pub fn last_shot(&self) -> Option<LastShot> {
        self.last_shot
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Last rejected or accepted operation description for the owning model.
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn remaining(&self, side: Side) -> &[u8] {
        &self.board(side).remaining
    }

    fn board(&self, side: Side) -> &Board {
        match side {
            Side::Player => &self.player,
            Side::Mita => &self.mita,
        }
    }

    fn board_mut(&mut self, side: Side) -> &mut Board {
        match side {
            Side::Player => &mut self.player,
            Side::Mita => &mut self.mita,
        }
    }

    pub fn place(
        &mut self,
        side: Side,
        start: Coordinate,
        length: u8,
        orientation: Orientation,
    ) -> Result<(), PlacementError> {
        if self.phase != Phase::Placement {
            return Err(PlacementError::WrongPhase);
        }
        self.board_mut(side).place(start, length, orientation)?;
        self.record.push(RecordedAction::Place {
            side,
            start: start.to_string(),
            length,
            orientation,
        });
        self.revision += 1;
        self.status = None;
        if self.player.remaining.is_empty() && self.mita.remaining.is_empty() {
            self.phase = Phase::Battle;
            self.turn = Side::Player;
        }
        Ok(())
    }

    /// Atomic fleet placement: a failed attempt leaves the board untouched.
    pub fn place_random_fleet(
        &mut self,
        side: Side,
        rng: &mut DeterministicRng,
    ) -> Result<(), PlacementError> {
        if self.phase != Phase::Placement {
            return Err(PlacementError::WrongPhase);
        }
        let initial_rng = rng.clone();
        let mut working = self.board(side).clone();
        for length in working.remaining.clone() {
            let mut placed = false;
            for _ in 0..100 {
                let start = Coordinate::new(rng.index(BOARD_SIZE), rng.index(BOARD_SIZE))
                    .expect("rng inside board");
                let orientation = if rng.index(2) == 0 {
                    Orientation::Horizontal
                } else {
                    Orientation::Vertical
                };
                if working.place(start, length, orientation).is_ok() {
                    placed = true;
                    break;
                }
            }
            if !placed {
                return Err(PlacementError::RandomFailed(length));
            }
        }
        *self.board_mut(side) = working;
        self.record.push(RecordedAction::Random {
            side,
            rng: initial_rng,
        });
        self.revision += 1;
        self.status = None;
        if self.player.remaining.is_empty() && self.mita.remaining.is_empty() {
            self.phase = Phase::Battle;
            self.turn = Side::Player;
        }
        Ok(())
    }

    pub fn shot(&mut self, attacker: Side, coord: Coordinate) -> Result<ShotResult, ShotError> {
        if self.phase != Phase::Battle {
            return Err(ShotError::WrongPhase);
        }
        if self.turn != attacker {
            return Err(ShotError::NotYourTurn);
        }
        let defender = attacker.other();
        let result = self.board_mut(defender).receive(coord)?;
        if matches!(result, ShotResult::Miss) {
            self.turn = defender;
        }
        if let ShotResult::Win { .. } = result {
            self.phase = Phase::GameOver;
            self.winner = Some(attacker);
        }
        self.last_shot = Some(LastShot {
            attacker,
            coord,
            result,
        });
        self.record.push(RecordedAction::Shot {
            side: attacker,
            coord: coord.to_string(),
        });
        self.revision += 1;
        Ok(result)
    }

    /// Record why a model command was rejected without mutating the board.
    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = Some(status.into());
    }

    pub fn own_board_text(&self, side: Side) -> Vec<String> {
        (0..BOARD_SIZE)
            .map(|row| self.board(side).own_row(row))
            .collect()
    }

    pub fn opponent_board_text(&self, side: Side) -> Vec<String> {
        (0..BOARD_SIZE)
            .map(|row| self.board(side.other()).opponent_row(row))
            .collect()
    }

    pub fn shot_history(&self, side: Side) -> Vec<Coordinate> {
        self.board(side.other()).shot_cells()
    }

    /// Observed hit cells on the opponent board. This is derived from the
    /// visible grid only, so two different hidden fleets with the same
    /// observed cells produce the same answer.
    pub fn wounded(&self, side: Side) -> Vec<Coordinate> {
        let board = self.board(side.other());
        let mut cells = Vec::new();
        for (y, row) in board.cells.iter().enumerate() {
            for (x, cell) in row.iter().enumerate() {
                if *cell == Cell::Hit {
                    cells.push(Coordinate::new(x as u8, y as u8).expect("board coordinate"));
                }
            }
        }
        cells.sort();
        cells
    }

    /// Conservative tactical hints computed only from observed cells.
    pub fn hunt_targets(&self, side: Side) -> Vec<Coordinate> {
        let board = self.board(side.other());
        let observed =
            |coord: Coordinate| matches!(board.cell(coord), Cell::Hit | Cell::Miss | Cell::Sunk);
        let hits = self.wounded(side);
        let mut targets: Vec<Coordinate> = Vec::new();
        for hit in &hits {
            targets.extend(hit.neighbors().filter(|coord| !observed(*coord)));
        }
        for hit in &hits {
            for (dx, dy) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
                let Some(previous) = hit.offset(dx, dy) else {
                    continue;
                };
                if board.cell(previous) != Cell::Hit {
                    continue;
                }
                let mut cursor = previous;
                while let Some(next) = cursor.offset(dx, dy) {
                    if board.cell(next) != Cell::Hit {
                        break;
                    }
                    cursor = next;
                }
                if let Some(extended) = cursor.offset(dx, dy)
                    && !observed(extended)
                {
                    targets.push(extended);
                }
            }
        }
        targets.sort();
        targets.dedup();
        targets
    }
}

impl Default for SeaBattle {
    fn default() -> Self {
        Self::new()
    }
}

/// Small deterministic generator so placement fixtures are reproducible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeterministicRng {
    state: u64,
    script: Vec<u8>,
    cursor: usize,
}

impl DeterministicRng {
    pub fn new(seed: u64) -> Self {
        // xorshift64* degenerates at zero, so the absorbing state is excluded.
        let state = seed ^ 0x9E37_79B9_7F4A_7C15;
        Self {
            state: if state == 0 {
                0x2545_F491_4F6C_DD1D
            } else {
                state
            },
            script: Vec::new(),
            cursor: 0,
        }
    }

    /// Scripted indices for deterministic negative-path tests.
    #[cfg(test)]
    pub(crate) fn scripted(script: Vec<u8>) -> Self {
        Self {
            state: 0x5DEE_CE66_D1CE_B00F,
            script,
            cursor: 0,
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        self.state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn index(&mut self, length: usize) -> u8 {
        assert!(length > 0 && length <= u8::MAX as usize);
        if let Some(value) = self.script.get(self.cursor) {
            self.cursor += 1;
            return *value % length as u8;
        }
        (self.next_u64() % length as u64) as u8
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", deny_unknown_fields)]
enum RecordedAction {
    Place {
        side: Side,
        start: String,
        length: u8,
        orientation: Orientation,
    },
    Random {
        side: Side,
        rng: DeterministicRng,
    },
    Shot {
        side: Side,
        coord: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GameRecord {
    actions: Vec<RecordedAction>,
}

impl From<SeaBattle> for GameRecord {
    fn from(game: SeaBattle) -> Self {
        Self {
            actions: game.record,
        }
    }
}

impl TryFrom<GameRecord> for SeaBattle {
    type Error = &'static str;
    fn try_from(record: GameRecord) -> Result<Self, Self::Error> {
        if record.actions.len() > 220 {
            return Err("too many game actions");
        }
        let mut game = Self::new();
        for action in record.actions {
            match action {
                RecordedAction::Place {
                    side,
                    start,
                    length,
                    orientation,
                } => {
                    let coord = Coordinate::parse(&start).ok_or("invalid coordinate")?;
                    if coord.to_string() != start {
                        return Err("noncanonical coordinate");
                    }
                    game.place(side, coord, length, orientation)
                        .map_err(|_| "invalid placement")?;
                }
                RecordedAction::Random { side, mut rng } => {
                    if rng.state == 0 || !rng.script.is_empty() || rng.cursor != 0 {
                        return Err("invalid random generator");
                    }
                    game.place_random_fleet(side, &mut rng)
                        .map_err(|_| "invalid random placement")?;
                }
                RecordedAction::Shot { side, coord } => {
                    let cell = Coordinate::parse(&coord).ok_or("invalid coordinate")?;
                    if cell.to_string() != coord {
                        return Err("noncanonical coordinate");
                    }
                    game.shot(side, cell).map_err(|_| "invalid shot")?;
                }
            }
        }
        Ok(game)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place_fleet(game: &mut SeaBattle, side: Side, layout: &[(&str, u8, Orientation)]) {
        for (coord, length, orientation) in layout {
            game.place(
                side,
                Coordinate::parse(coord).unwrap(),
                *length,
                *orientation,
            )
            .unwrap();
        }
    }

    pub(crate) const LAYOUT: [(&str, u8, Orientation); 10] = [
        ("A1", 4, Orientation::Horizontal),
        ("F1", 3, Orientation::Horizontal),
        ("A3", 3, Orientation::Horizontal),
        ("E3", 2, Orientation::Horizontal),
        ("H3", 2, Orientation::Horizontal),
        ("A5", 2, Orientation::Horizontal),
        ("D5", 1, Orientation::Horizontal),
        ("F5", 1, Orientation::Horizontal),
        ("H5", 1, Orientation::Horizontal),
        ("J5", 1, Orientation::Horizontal),
    ];

    fn battle() -> SeaBattle {
        let mut game = SeaBattle::new();
        place_fleet(&mut game, Side::Player, &LAYOUT);
        place_fleet(&mut game, Side::Mita, &LAYOUT);
        assert_eq!(game.phase(), Phase::Battle);
        game
    }

    fn assert_replay_roundtrip(game: &SeaBattle) {
        let record = serde_json::to_value(game).unwrap();
        let restored: SeaBattle = serde_json::from_value(record.clone()).unwrap();
        assert_eq!(&restored, game);
        assert_eq!(serde_json::to_value(restored).unwrap(), record);
    }

    #[test]
    fn canonical_replay_restores_random_fleet_turn_hits_sunk_and_game_over() {
        let mut random = SeaBattle::new();
        random
            .place_random_fleet(Side::Player, &mut DeterministicRng::new(42))
            .unwrap();
        random
            .place_random_fleet(Side::Mita, &mut DeterministicRng::new(43))
            .unwrap();
        assert_replay_roundtrip(&random);
        let mut game = battle();
        assert_replay_roundtrip(&game);
        game.shot(Side::Player, Coordinate::parse("J10").unwrap())
            .unwrap();
        assert_replay_roundtrip(&game);
        game.shot(Side::Mita, Coordinate::parse("J10").unwrap())
            .unwrap();
        for (start, length, orientation) in LAYOUT {
            for cell in orientation
                .cells(Coordinate::parse(start).unwrap(), length as usize)
                .unwrap()
            {
                game.shot(Side::Player, cell).unwrap();
                assert_replay_roundtrip(&game);
            }
        }
        assert_eq!(game.phase(), Phase::GameOver);
        assert_eq!(game.winner(), Some(Side::Player));
    }

    #[test]
    fn adjacency_including_diagonals_is_rejected_without_partial_placement() {
        let mut game = SeaBattle::new();
        game.place(
            Side::Player,
            Coordinate::parse("A1").unwrap(),
            4,
            Orientation::Horizontal,
        )
        .unwrap();
        for (coord, length, orientation) in [
            ("A2", 3, Orientation::Horizontal),
            ("B2", 3, Orientation::Horizontal),
            ("E1", 3, Orientation::Horizontal),
        ] {
            assert_eq!(
                game.place(
                    Side::Player,
                    Coordinate::parse(coord).unwrap(),
                    length,
                    orientation
                ),
                Err(PlacementError::Conflict),
                "{coord}"
            );
        }
        assert_eq!(game.remaining(Side::Player), &[3, 3, 2, 2, 2, 1, 1, 1, 1]);
    }

    #[test]
    fn hit_and_sunk_keep_the_turn_and_miss_passes_it() {
        let mut game = battle();
        assert_eq!(
            game.shot(Side::Player, Coordinate::parse("J10").unwrap()),
            Ok(ShotResult::Miss)
        );
        assert_eq!(game.turn(), Side::Mita);
        assert_eq!(
            game.shot(Side::Mita, Coordinate::parse("A1").unwrap()),
            Ok(ShotResult::Hit)
        );
        assert_eq!(game.turn(), Side::Mita);
        for coord in ["B1", "C1"] {
            game.shot(Side::Mita, Coordinate::parse(coord).unwrap())
                .unwrap();
        }
        assert_eq!(
            game.shot(Side::Mita, Coordinate::parse("D1").unwrap()),
            Ok(ShotResult::Sunk { length: 4 })
        );
        assert_eq!(game.turn(), Side::Mita);
        assert_eq!(
            game.shot(Side::Mita, Coordinate::parse("D1").unwrap()),
            Err(ShotError::AlreadyShot)
        );
        assert_eq!(
            game.shot(Side::Player, Coordinate::parse("J9").unwrap()),
            Err(ShotError::NotYourTurn)
        );
    }

    #[test]
    fn random_placement_failure_rolls_back_the_whole_fleet() {
        let mut game = SeaBattle::new();
        // The first ship lands at A1..D1; every later attempt targets the
        // occupied A1 cell, so the next ship exhausts its bounded attempts
        // after the clone already placed something.
        // 3 indices for the first ship, then 3 per attempt for 100 attempts.
        let mut script = vec![0, 0, 0];
        script.extend(std::iter::repeat_n(0, 300));
        let mut rng = DeterministicRng::scripted(script);
        let before = game.clone();
        assert_eq!(
            game.place_random_fleet(Side::Player, &mut rng),
            Err(PlacementError::RandomFailed(3))
        );
        assert_eq!(game.revision(), before.revision());
        assert_eq!(game.remaining(Side::Player), before.remaining(Side::Player));
        assert_eq!(
            game.own_board_text(Side::Player),
            before.own_board_text(Side::Player)
        );
    }

    #[test]
    fn random_placement_is_reproducible_and_fills_the_fleet() {
        for seed in 0..32 {
            let mut game = SeaBattle::new();
            let mut rng = DeterministicRng::new(seed);
            game.place_random_fleet(Side::Mita, &mut rng).unwrap();
            assert!(game.remaining(Side::Mita).is_empty());
            let mut replay = SeaBattle::new();
            let mut replay_rng = DeterministicRng::new(seed);
            replay
                .place_random_fleet(Side::Mita, &mut replay_rng)
                .unwrap();
            assert_eq!(
                game.own_board_text(Side::Mita),
                replay.own_board_text(Side::Mita)
            );
        }
    }

    #[test]
    fn a_seed_that_would_zero_the_generator_still_produces_indices() {
        let mut rng = DeterministicRng::new(0x9E37_79B9_7F4A_7C15);
        assert!(rng.next_u64() != 0);
        let mut game = SeaBattle::new();
        game.place_random_fleet(Side::Mita, &mut rng).unwrap();
    }

    #[test]
    fn opponent_view_hides_unshot_ships() {
        let mut game = battle();
        game.shot(Side::Player, Coordinate::parse("A1").unwrap())
            .unwrap();
        let opponent = game.opponent_board_text(Side::Player);
        assert_eq!(opponent[0], "H.........");
        assert!(opponent.iter().all(|row| !row.contains('S')));
        assert!(game.own_board_text(Side::Player)[0].starts_with("SSSS"));
    }

    #[test]
    fn debug_output_does_not_contain_boards() {
        let game = battle();
        let text = format!("{game:?}");
        assert!(!text.contains("SSSS"));
        assert!(!text.contains("........."));
        assert!(text.contains("player_ships"));
    }

    #[test]
    fn tactical_hints_depend_only_on_observed_hits_not_hidden_ship_identity() {
        // Two fleets cover the same observed cells with different hidden
        // ships: one four-deck ship versus two two-deck ships.
        const TAIL_A: [(&str, u8); 9] = [
            ("A5", 3),
            ("F5", 3),
            ("A8", 2),
            ("F8", 2),
            ("A10", 2),
            ("D10", 1),
            ("F10", 1),
            ("H10", 1),
            ("J10", 1),
        ];
        const TAIL_B: [(&str, u8); 8] = [
            ("A5", 4),
            ("F5", 3),
            ("A8", 3),
            ("F8", 2),
            ("D10", 1),
            ("F10", 1),
            ("H10", 1),
            ("J10", 1),
        ];
        let mut fleet_a = SeaBattle::new();
        let mut fleet_b = SeaBattle::new();
        for game in [&mut fleet_a, &mut fleet_b] {
            let mut rng = DeterministicRng::new(5);
            game.place_random_fleet(Side::Player, &mut rng).unwrap();
        }
        fleet_a
            .place(
                Side::Mita,
                Coordinate::parse("A1").unwrap(),
                4,
                Orientation::Horizontal,
            )
            .unwrap();
        for coord in ["A1", "D1"] {
            fleet_b
                .place(
                    Side::Mita,
                    Coordinate::parse(coord).unwrap(),
                    2,
                    Orientation::Horizontal,
                )
                .unwrap();
        }
        let tails: [&[(&str, u8)]; 2] = [&TAIL_A, &TAIL_B];
        for (game, tail) in [&mut fleet_a, &mut fleet_b].into_iter().zip(tails) {
            for (coord, length) in tail {
                game.place(
                    Side::Mita,
                    Coordinate::parse(coord).unwrap(),
                    *length,
                    Orientation::Horizontal,
                )
                .unwrap();
            }
            assert_eq!(game.phase(), Phase::Battle);
            for coord in ["A1", "D1"] {
                assert_eq!(
                    game.shot(Side::Player, Coordinate::parse(coord).unwrap()),
                    Ok(ShotResult::Hit),
                    "{coord}"
                );
            }
        }
        assert_eq!(
            fleet_a.opponent_board_text(Side::Player),
            fleet_b.opponent_board_text(Side::Player)
        );
        assert_eq!(fleet_a.wounded(Side::Player), fleet_b.wounded(Side::Player));
        assert_eq!(
            fleet_a.hunt_targets(Side::Player),
            fleet_b.hunt_targets(Side::Player)
        );
    }
}
