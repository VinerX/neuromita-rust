use serde::{Deserialize, Serialize};

/// A proposed change applied only together with the accepted character message.
/// The canonical state is derived from these facts, never from a second store.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct CharacterStateDelta {
    pub attitude: f64,
    pub boredom: f64,
    pub stress: f64,
}

impl CharacterStateDelta {
    pub fn is_empty(self) -> bool {
        self.attitude == 0.0 && self.boredom == 0.0 && self.stress == 0.0
    }

    pub fn is_finite(self) -> bool {
        self.attitude.is_finite() && self.boredom.is_finite() && self.stress.is_finite()
    }
}

/// Materialized state, recorded with a committed reply instead of replaying
/// floating-point deltas under potentially changed configuration.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterBehavior {
    pub attitude: f64,
    pub boredom: f64,
    pub stress: f64,
}

impl Default for CharacterBehavior {
    fn default() -> Self {
        Self {
            attitude: 60.0,
            boredom: 10.0,
            stress: 5.0,
        }
    }
}

/// Compact, untrusted model-produced handoff for the character's next turn.
/// Unlike the user's message, this is private cognitive context, not dialogue.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CharacterWorkingState {
    pub focus: String,
    pub situation: Vec<String>,
    pub assumptions: Vec<String>,
    pub open_loops: Vec<String>,
    pub next_steps: Vec<String>,
}

impl CharacterWorkingState {
    pub fn is_empty(&self) -> bool {
        self.focus.is_empty()
            && self.situation.is_empty()
            && self.assumptions.is_empty()
            && self.open_loops.is_empty()
            && self.next_steps.is_empty()
    }

    /// Reject out-of-bounds snapshots rather than silently truncating
    /// accepted data. The limits follow Python's working-state budget.
    pub fn is_valid(&self) -> bool {
        let arrays = [
            &self.situation,
            &self.assumptions,
            &self.open_loops,
            &self.next_steps,
        ];
        self.focus.chars().count() <= 600
            && arrays.iter().all(|items| {
                items.len() <= 6 && items.iter().all(|item| item.chars().count() <= 400)
            })
            && self.focus.chars().count()
                + arrays
                    .iter()
                    .flat_map(|items| items.iter())
                    .map(|item| item.chars().count())
                    .sum::<usize>()
                <= 2000
    }
}

impl CharacterBehavior {
    /// Current Python default bounds. An explicit stat policy can replace
    /// these defaults without changing already accepted historical values.
    pub fn apply(self, proposed: CharacterStateDelta) -> Option<Self> {
        if !proposed.is_finite() {
            return None;
        }
        fn adjust(current: f64, amount: f64) -> f64 {
            let rounded = (amount * 100.0).round() / 100.0;
            (current + rounded.clamp(-6.0, 6.0)).clamp(0.0, 100.0)
        }
        let next = Self {
            attitude: adjust(self.attitude, proposed.attitude),
            boredom: adjust(self.boredom, proposed.boredom),
            stress: adjust(self.stress, proposed.stress),
        };
        (next.attitude.is_finite() && next.boredom.is_finite() && next.stress.is_finite())
            .then_some(next)
    }
}

impl CharacterBehavior {
    pub fn is_valid(self) -> bool {
        [self.attitude, self.boredom, self.stress]
            .iter()
            .all(|value| value.is_finite() && (0.0..=100.0).contains(value))
    }
}
