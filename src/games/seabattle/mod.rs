mod coordinate;
mod game;

pub use coordinate::{BOARD_SIZE, Coordinate};
pub use game::{
    Cell, DeterministicRng, FLEET, LastShot, OpponentCell, Orientation, Phase, PlacementError,
    SeaBattle, ShotError, ShotResult, Side,
};
