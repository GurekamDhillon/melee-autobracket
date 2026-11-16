//! The stage-selection fields of `Ruleset`: defaults, validation, and old JSON.

use proto::Ruleset;
use serde::Deserialize;

#[test]
fn starter_stages_default_to_legal() {
    let r = Ruleset::default();
    assert!(r.starter_stages.is_empty());
    assert_eq!(r.starters(), r.legal_stages.as_slice());
    let r = Ruleset {
        starter_stages: vec![2, 31],
        ..Ruleset::default()
    };
    assert_eq!(r.starters(), &[2, 31]);
}

#[test]
fn validate_rejects_starter_outside_legal() {
    let r = Ruleset {
        starter_stages: vec![99],
        ..Ruleset::default()
    };
    let err = r.validate().unwrap_err();
    assert!(err.contains("starter_stages"), "{err}");
    let r = Ruleset {
        starter_stages: vec![2, 3, 8],
        ..Ruleset::default()
    };
    r.validate().unwrap();
}

#[test]
fn validate_rejects_too_many_bans() {
    let legal = Ruleset::default().legal_stages.len();
    let r = Ruleset {
        stage_bans: Some(u8::try_from(legal).unwrap()),
        ..Ruleset::default()
    };
    let err = r.validate().unwrap_err();
    assert!(err.contains("stage_bans"), "{err}");
    let r = Ruleset {
        stage_bans: Some(u8::try_from(legal - 1).unwrap()),
        ..Ruleset::default()
    };
    r.validate().unwrap();
}

#[test]
fn old_ruleset_json_still_decodes() {
    // The `docs/api.md` Ruleset example as it was before the stage-selection fields.
    let old = r#"{
      "stocks": 4,
      "timer_secs": 480,
      "best_of": 3,
      "best_of_finals": 5,
      "legal_stages": [2, 3, 8, 28, 31, 32],
      "banned_characters": [],
      "allowed_gecko": []
    }"#;
    let r: Ruleset = serde_json::from_str(old).unwrap();
    assert_eq!(r.stage_bans, None);
    assert_eq!(r.effective_stage_bans(), 2);
    assert!(r.starter_stages.is_empty());
    assert_eq!(r, Ruleset::default());
}

/// `stage_bans` left out is 2, or fewer when there are not enough legal stages to leave one to
/// pick; only an explicit value is checked against the legal stages, and only when there is
/// more than one.
#[test]
fn stage_bans_default_fits_the_legal_stages() {
    for (legal, bans) in [(vec![32], 0), (vec![31, 32], 1), (vec![8, 31, 32], 2)] {
        let r = Ruleset {
            legal_stages: legal.clone(),
            ..Ruleset::default()
        };
        r.validate().unwrap();
        assert_eq!(r.effective_stage_bans(), bans, "{legal:?}");
    }
    let one = Ruleset {
        legal_stages: vec![32],
        stage_bans: Some(2),
        ..Ruleset::default()
    };
    one.validate().unwrap();
    // Clamped so a stage is always left to pick.
    assert_eq!(one.effective_stage_bans(), 0);
    let three = Ruleset {
        legal_stages: vec![8, 31, 32],
        stage_bans: Some(1),
        ..Ruleset::default()
    };
    assert_eq!(three.effective_stage_bans(), 1);
    let two = Ruleset {
        legal_stages: vec![31, 32],
        stage_bans: Some(2),
        ..Ruleset::default()
    };
    assert!(two.validate().unwrap_err().contains("stage_bans"));
}

/// `Ruleset` as it was before the stage-selection fields, as deployed clients and servers
/// still decode it: unknown fields refused.
#[allow(dead_code)]
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct OldRuleset {
    stocks: u8,
    timer_secs: u32,
    best_of: u8,
    best_of_finals: u8,
    legal_stages: Vec<u16>,
    banned_characters: Vec<u8>,
    allowed_gecko: Vec<String>,
}

impl Default for OldRuleset {
    fn default() -> Self {
        OldRuleset {
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

/// A ruleset that leaves the stage-selection fields unset encodes exactly as before, so an
/// older client (or a rolled-back server reading stored rulesets) still decodes it.
#[test]
fn default_ruleset_encodes_as_before() {
    let json = serde_json::to_value(Ruleset::default()).unwrap();
    let mut keys: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "allowed_gecko",
            "banned_characters",
            "best_of",
            "best_of_finals",
            "legal_stages",
            "stocks",
            "timer_secs"
        ]
    );
    serde_json::from_value::<OldRuleset>(json).unwrap();
    // Set fields are encoded, and decode back.
    let r = Ruleset {
        starter_stages: vec![31, 32],
        stage_bans: Some(1),
        ..Ruleset::default()
    };
    let json = serde_json::to_string(&r).unwrap();
    assert!(json.contains(r#""starter_stages":[31,32]"#), "{json}");
    assert!(json.contains(r#""stage_bans":1"#), "{json}");
    assert_eq!(serde_json::from_str::<Ruleset>(&json).unwrap(), r);
}
