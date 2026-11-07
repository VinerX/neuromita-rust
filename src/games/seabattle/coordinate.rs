use std::fmt;

pub const BOARD_SIZE: usize = 10;

/// Zero-based cell on the 10x10 board. Construction and parsing reject
/// anything outside the board instead of clamping or wrapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Coordinate {
    x: u8,
    y: u8,
}

impl Coordinate {
    pub fn new(x: u8, y: u8) -> Option<Self> {
        ((x as usize) < BOARD_SIZE && (y as usize) < BOARD_SIZE).then_some(Self { x, y })
    }

    pub fn x(self) -> u8 {
        self.x
    }

    pub fn y(self) -> u8 {
        self.y
    }

    /// Parse the player-facing notation used by the Python engine: A1..J10.
    pub fn parse(value: &str) -> Option<Self> {
        let text = value.trim().to_ascii_uppercase();
        let mut chars = text.chars();
        let column = chars.next()?;
        if !column.is_ascii_uppercase() {
            return None;
        }
        let digits = chars.as_str();
        if digits.is_empty()
            || digits.starts_with('0')
            || digits.len() > 2
            || !digits.chars().all(|character| character.is_ascii_digit())
        {
            return None;
        }
        let row: u8 = digits.parse().ok()?;
        Self::new(column as u8 - b'A', row - 1)
    }

    pub fn offset(self, dx: i32, dy: i32) -> Option<Self> {
        let x = self.x as i32 + dx;
        let y = self.y as i32 + dy;
        if (0..BOARD_SIZE as i32).contains(&x) && (0..BOARD_SIZE as i32).contains(&y) {
            Self::new(x as u8, y as u8)
        } else {
            None
        }
    }

    pub fn neighbors(self) -> impl Iterator<Item = Self> {
        [(-1, 0), (1, 0), (0, -1), (0, 1)]
            .into_iter()
            .filter_map(move |(dx, dy)| self.offset(dx, dy))
    }

    pub fn surrounding(self) -> impl Iterator<Item = Self> {
        [
            (-1, -1),
            (-1, 0),
            (-1, 1),
            (0, -1),
            (0, 1),
            (1, -1),
            (1, 0),
            (1, 1),
        ]
        .into_iter()
        .filter_map(move |(dx, dy)| self.offset(dx, dy))
    }
}

impl fmt::Display for Coordinate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let column = (b'A' + self.x) as char;
        write!(f, "{column}{}", self.y + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notation_roundtrips_and_rejects_out_of_board_values() {
        for index in 0..BOARD_SIZE as u8 {
            for row in 0..BOARD_SIZE as u8 {
                let coord = Coordinate::new(index, row).unwrap();
                assert_eq!(Coordinate::parse(&coord.to_string()), Some(coord));
            }
        }
        for bad in ["", "A0", "A11", "K1", "A", "1A", "AA1", "A-1", " J10 "] {
            let parsed = Coordinate::parse(bad);
            if bad.trim() == "J10" {
                assert_eq!(parsed, Coordinate::new(9, 9));
            } else {
                assert!(parsed.is_none(), "{bad}");
            }
        }
        assert_eq!(Coordinate::new(10, 0), None);
        assert_eq!(Coordinate::new(0, 10), None);
    }

    #[test]
    fn surrounding_cells_include_diagonals() {
        let center = Coordinate::new(4, 4).unwrap();
        assert_eq!(center.surrounding().count(), 8);
        assert_eq!(Coordinate::new(0, 0).unwrap().surrounding().count(), 3);
    }
}
