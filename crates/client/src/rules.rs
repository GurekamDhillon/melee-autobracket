//! Showing an event's rules to the player, and checking the addresses the server hands over
//! before they reach the game's command line.

use proto::Ruleset;

/// Names of the six stages of the default ruleset, by Melee external stage id.
const STAGES: [(u16, &str); 6] = [
    (2, "Fountain of Dreams"),
    (3, "Pokémon Stadium"),
    (8, "Yoshi's Story"),
    (28, "Dream Land N64"),
    (31, "Battlefield"),
    (32, "Final Destination"),
];

/// Characters by Melee external character id (the id in a replay's Game Start), as the
/// engine's own replay browser names them: melee-engine 0cd085d,
/// `rust/melee/src/replays.rs:10-12` (`CHARS`, indexed by the `.slp` player's `ckind`).
const CHARACTERS: [&str; 26] = [
    "Falcon", "DK", "Fox", "G&W", "Kirby", "Bowser", "Link", "Luigi", "Mario", "Marth", "Mewtwo",
    "Ness", "Peach", "Pikachu", "ICs", "Puff", "Samus", "Yoshi", "Zelda", "Sheik", "Falco",
    "YLink", "Doc", "Roy", "Pichu", "Ganon",
];

/// `Final Destination (32)`, or `stage 40` for an id without a name here.
pub fn stage_name(id: u16) -> String {
    match STAGES.iter().find(|(s, _)| *s == id) {
        Some((_, name)) => format!("{name} ({id})"),
        None => format!("stage {id}"),
    }
}

/// `Fox (2)`, or `character 30` for an id without a name here.
pub fn character_name(id: u8) -> String {
    match CHARACTERS.get(usize::from(id)) {
        Some(name) => format!("{name} ({id})"),
        None => format!("character {id}"),
    }
}

/// The rules a player needs before a set, one per line.
pub fn describe_ruleset(r: &Ruleset) -> String {
    let timer = format!("{}:{:02}", r.timer_secs / 60, r.timer_secs % 60);
    let stages: Vec<String> = r.legal_stages.iter().map(|&s| stage_name(s)).collect();
    let banned = if r.banned_characters.is_empty() {
        "none".to_string()
    } else {
        r.banned_characters
            .iter()
            .map(|&c| character_name(c))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "Rules: {} stocks, {timer} timer, best of {} (finals best of {})\n\
         Legal stages: {}\n\
         Banned characters: {banned}",
        r.stocks,
        r.best_of,
        r.best_of_finals,
        stages.join(", ")
    )
}

/// A host name, an IPv4 address or a bracketed IPv6 address.
fn valid_host(host: &str) -> bool {
    if let Some(v6) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return v6.parse::<std::net::Ipv6Addr>().is_ok();
    }
    !host.is_empty()
        && host.len() <= 253
        && !host.starts_with(['-', '.'])
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

fn valid_port(port: &str) -> bool {
    !port.is_empty()
        && port.len() <= 5
        && port.chars().all(|c| c.is_ascii_digit())
        && port.parse::<u16>().is_ok_and(|p| p != 0)
}

/// `HOST:PORT` with both parts checked; `port_required` false also takes a bare `HOST`.
fn valid_address(s: &str, port_required: bool) -> bool {
    // A bracketed IPv6 address carries colons of its own.
    let (host, port) = match s.rfind(':') {
        Some(i) if !s[i..].contains(']') => (&s[..i], Some(&s[i + 1..])),
        _ => (s, None),
    };
    valid_host(host) && port.map_or(!port_required, valid_port)
}

/// A relay address the server sent: `HOST[:PORT]`.
pub fn valid_relay(relay: &str) -> bool {
    valid_address(relay, false)
}

/// A room code the server passed on from the opponent: a match code (four characters
/// `A-Z`/`0-9`), or, only when `allow_direct_connect`, the host's `HOST:PORT`.
pub fn valid_room_code(code: &str, allow_direct_connect: bool) -> bool {
    let match_code = code.len() == 4
        && code
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    match_code || (allow_direct_connect && valid_address(code, true))
}
