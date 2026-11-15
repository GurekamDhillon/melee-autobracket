use serde::{Deserialize, Serialize};

/// Match rules for an event. Missing fields deserialise to the defaults.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Ruleset {
    pub stocks: u8,
    pub timer_secs: u32,
    pub best_of: u8,
    pub best_of_finals: u8,
    pub legal_stages: Vec<u16>,
    pub banned_characters: Vec<u8>,
}

impl Default for Ruleset {
    fn default() -> Self {
        Ruleset {
            stocks: 4,
            timer_secs: 480,
            best_of: 3,
            best_of_finals: 5,
            legal_stages: vec![2, 3, 8, 28, 31, 32],
            banned_characters: vec![],
        }
    }
}
