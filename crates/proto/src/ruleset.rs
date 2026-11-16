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
    /// The stages game 1's strikes are over, by stage id; empty means every `legal_stages`
    /// entry ([`Ruleset::starters`]). Left out of the JSON when empty, so a ruleset without
    /// stage selection encodes as it did before this field existed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub starter_stages: Vec<u16>,
    /// How many stages the winner of a game bans before the loser picks the next game's stage;
    /// absent means the default ([`Ruleset::effective_stage_bans`]). Left out of the JSON when
    /// absent, as `starter_stages` when empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage_bans: Option<u8>,
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
            starter_stages: vec![],
            stage_bans: None,
        }
    }
}

impl Ruleset {
    /// The stages game 1's strikes are over: `starter_stages`, or `legal_stages` when that is
    /// empty.
    pub fn starters(&self) -> &[u16] {
        if self.starter_stages.is_empty() {
            &self.legal_stages
        } else {
            &self.starter_stages
        }
    }

    /// How many stages the winner of a game bans: `stage_bans`, or 2 when unset, but never
    /// more than one fewer than the legal stages, so a stage is always left to pick.
    pub fn effective_stage_bans(&self) -> u8 {
        let fit = u8::try_from(self.legal_stages.len().saturating_sub(1)).unwrap_or(u8::MAX);
        self.stage_bans.unwrap_or(2).min(fit)
    }

    /// Checks that games can be played and judged under these rules: `best_of` and
    /// `best_of_finals` odd (so not 0), `stocks` at least 1, at least one legal stage, and
    /// every `allowed_gecko` entry shaped like a SHA-256 ([`is_gecko_hash`]), every
    /// `starter_stages` entry legal, and an explicit `stage_bans` fewer than the legal stages
    /// when there is more than one (so a stage is left to pick). The error names
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
        if let Some(bad) = self
            .starter_stages
            .iter()
            .find(|s| !self.legal_stages.contains(s))
        {
            return Err(format!(
                "starter_stages: stage {bad} is not in legal_stages"
            ));
        }
        if let Some(bans) = self.stage_bans {
            let legal = self.legal_stages.len();
            if legal > 1 && usize::from(bans) >= legal {
                return Err(format!(
                    "stage_bans must be fewer than the {legal} legal stages, not {bans}"
                ));
            }
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
