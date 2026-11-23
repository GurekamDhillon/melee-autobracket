use serde::{Deserialize, Serialize};

/// Match rules for an event. Missing fields deserialise to the defaults; unknown fields are
/// refused, so a misspelt rule is an error rather than a silent default.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
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
    /// "Any client" mode (README, "Any client mode"; off by default): the two uploads may come
    /// from different clients, so peer agreement compares the derived outcome (winner, final
    /// stocks, characters, stage, game end) instead of byte-identical `raw` elements, and a
    /// re-simulation that diverges counts the game as `UnverifiedForeign` instead of
    /// quarantining it as an engine fault. Left out of the encoding when off.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub any_client: bool,
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
            any_client: false,
        }
    }
}

impl Ruleset {
    /// Checks that games can be played and judged under these rules: `best_of` and
    /// `best_of_finals` odd (so not 0), `stocks` at least 1, at least one legal stage, and
    /// every `allowed_gecko` entry shaped like a SHA-256 ([`is_gecko_hash`]). The error names
    /// the field first (e.g. `best_of must be ...`).
    pub fn validate(&self) -> Result<(), String> {
        for (field, n) in [
            ("best_of", self.best_of),
            ("best_of_finals", self.best_of_finals),
        ] {
            if n % 2 == 0 {
                return Err(format!("{field} must be an odd number of games, not {n}"));
            }
        }
        if self.stocks == 0 {
            return Err("stocks must be at least 1".into());
        }
        if self.legal_stages.is_empty() {
            return Err("legal_stages must list at least one stage".into());
        }
        if let Some(bad) = self.allowed_gecko.iter().find(|h| !is_gecko_hash(h)) {
            return Err(format!(
                "allowed_gecko: {:?} is not a SHA-256 (64 lowercase hex characters)",
                bad.chars().take(80).collect::<String>()
            ));
        }
        Ok(())
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
    fn default_ruleset_is_valid() {
        Ruleset::default().validate().unwrap();
        let r = Ruleset {
            allowed_gecko: vec![SHA.into()],
            ..Ruleset::default()
        };
        r.validate().unwrap();
    }

    #[test]
    fn validate_names_the_bad_field() {
        let cases: Vec<(Ruleset, &str)> = vec![
            (
                Ruleset {
                    best_of: 0,
                    ..Ruleset::default()
                },
                "best_of",
            ),
            (
                Ruleset {
                    best_of: 4,
                    ..Ruleset::default()
                },
                "best_of",
            ),
            (
                Ruleset {
                    best_of_finals: 6,
                    ..Ruleset::default()
                },
                "best_of_finals",
            ),
            (
                Ruleset {
                    stocks: 0,
                    ..Ruleset::default()
                },
                "stocks",
            ),
            (
                Ruleset {
                    legal_stages: vec![],
                    ..Ruleset::default()
                },
                "legal_stages",
            ),
            (
                Ruleset {
                    allowed_gecko: vec![SHA.to_uppercase()],
                    ..Ruleset::default()
                },
                "allowed_gecko",
            ),
            (
                Ruleset {
                    allowed_gecko: vec![SHA.into(), "abc".into()],
                    ..Ruleset::default()
                },
                "allowed_gecko",
            ),
        ];
        for (r, field) in cases {
            let err = r.validate().expect_err(field);
            assert!(err.starts_with(field), "{field}: {err}");
        }
    }

    #[test]
    fn unknown_fields_are_refused() {
        let err = serde_json::from_str::<Ruleset>(r#"{"best_of_final":5}"#).unwrap_err();
        assert!(err.to_string().contains("best_of_final"), "{err}");
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
