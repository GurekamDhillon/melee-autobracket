//! The rules shown to players, and the address checks.

use autobracket_client::rules::{
    character_name, describe_ruleset, stage_name, valid_relay, valid_room_code,
};
use proto::Ruleset;

#[test]
fn the_ruleset_is_shown_with_stage_and_character_names() {
    let r = Ruleset {
        banned_characters: vec![14, 99],
        legal_stages: vec![2, 3, 8, 28, 31, 32, 40],
        ..Ruleset::default()
    };
    let text = describe_ruleset(&r);
    for needle in [
        "4 stocks",
        "8:00 timer",
        "best of 3",
        "finals best of 5",
        "Fountain of Dreams (2)",
        "Pokémon Stadium (3)",
        "Yoshi's Story (8)",
        "Dream Land N64 (28)",
        "Battlefield (31)",
        "Final Destination (32)",
        "stage 40",
        "ICs (14)",
        "character 99",
    ] {
        assert!(text.contains(needle), "{needle:?} missing from:\n{text}");
    }
    assert!(describe_ruleset(&Ruleset::default()).contains("Banned characters: none"));
}

#[test]
fn names_follow_the_external_ids() {
    assert_eq!(character_name(0), "Falcon (0)");
    assert_eq!(character_name(2), "Fox (2)");
    assert_eq!(character_name(25), "Ganon (25)");
    assert_eq!(character_name(26), "character 26");
    assert_eq!(stage_name(31), "Battlefield (31)");
}

#[test]
fn relays_are_host_and_optional_port() {
    for good in [
        "netplay.example.org",
        "netplay.example.org:51600",
        "127.0.0.1:51711",
        "[2001:db8::1]:51600",
        "[::1]",
    ] {
        assert!(valid_relay(good), "{good}");
    }
    for bad in [
        "",
        "--np-css",
        "-x:1",
        "host:0",
        "host:65536",
        "host:",
        "a b:1",
        "host;rm:1",
        "[nonsense]:1",
        "host:1:2",
    ] {
        assert!(!valid_relay(bad), "{bad}");
    }
}

#[test]
fn room_codes_are_match_codes_unless_direct_connect_is_allowed() {
    assert!(valid_room_code("K7QX", false));
    assert!(valid_room_code("74AV", false));
    for bad in ["k7qx", "K7Q", "K7QXX", "K7-Q", "--np", "1.2.3.4:5000"] {
        assert!(!valid_room_code(bad, false), "{bad}");
    }
    assert!(valid_room_code("1.2.3.4:5000", true));
    assert!(valid_room_code("[2001:db8::1]:5000", true));
    assert!(!valid_room_code("1.2.3.4", true), "a port is needed");
    assert!(!valid_room_code("-np:5000", true));
}
