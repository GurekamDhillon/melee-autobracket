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
    /// Gecko code lists a replay may carry, as the lowercase hex SHA-256 of the
    /// list bytes (`replay::ReplayFacts::gecko_sha256`). A replay without a list
    /// is always allowed on this point; one with a list must match an entry exactly.
    pub allowed_gecko: Vec<String>,
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
            allowed_gecko: vec![],
        }
    }
}

/// Whether `s` has the shape of an `allowed_gecko` entry: exactly 64 lowercase
/// hex characters (a SHA-256).
pub fn is_gecko_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn gecko_hash_shape() {
        assert!(is_gecko_hash(SHA));
        assert!(is_gecko_hash(&"0".repeat(64)));
        assert!(!is_gecko_hash(&SHA.to_uppercase()));
        assert!(!is_gecko_hash(&SHA[..63]));
        assert!(!is_gecko_hash(&format!("{SHA}0")));
        assert!(!is_gecko_hash(&format!("{}g", &SHA[..63])));
        assert!(!is_gecko_hash(&format!(" {}", &SHA[..63])));
        assert!(!is_gecko_hash(""));
        // 64 bytes but not 64 ASCII characters.
        assert!(!is_gecko_hash(&format!("{}é", &SHA[..62])));
        assert!(!is_gecko_hash("0541574b"));
    }

    #[test]
    fn allowed_gecko_defaults_to_empty_and_round_trips() {
        assert!(Ruleset::default().allowed_gecko.is_empty());
        let r: Ruleset = serde_json::from_str(r#"{"stocks":3}"#).unwrap();
        assert!(r.allowed_gecko.is_empty());
        let r: Ruleset =
            serde_json::from_str(&format!(r#"{{"allowed_gecko":["{SHA}"]}}"#)).unwrap();
        assert_eq!(r.allowed_gecko, vec![SHA.to_string()]);
        let back: Ruleset = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
    }
}
