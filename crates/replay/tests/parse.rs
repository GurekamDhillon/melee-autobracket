use replay::testkit::{simple_game, SlpBuilder};
use replay::{gecko_fnv32, parse, raw_element, winner_port, PlayerKind, ReplayError, ReplayFacts};

/// Two humans on ports 1 and 2 with the given final states and game end.
fn duel(p1: (u8, f32), p2: (u8, f32), end: Option<(u8, Option<u8>)>) -> ReplayFacts {
    let mut b = SlpBuilder::new()
        .player(1, 2, PlayerKind::Human)
        .player(2, 9, PlayerKind::Human)
        .frame(-123, 1, 4, 0.0)
        .frame(-123, 2, 4, 0.0)
        .frame(5000, 1, p1.0, p1.1)
        .frame(5000, 2, p2.0, p2.1);
    if let Some((method, lras)) = end {
        b = b.end(method, lras);
    }
    parse(&b.build()).unwrap()
}

#[test]
fn raw_ignores_metadata() {
    let a = simple_game(1, "2026-11-15T20:00:00Z");
    let b = simple_game(1, "2026-11-15T20:00:07Z");
    assert_ne!(a, b);
    assert_eq!(raw_element(&a).unwrap(), raw_element(&b).unwrap());
}

#[test]
fn facts() {
    let f = parse(&simple_game(2, "t")).unwrap();
    assert_eq!(
        (f.stage, f.timer_secs, f.is_teams, f.has_gecko),
        (32, 480, false, false)
    );
    assert_eq!(f.players.len(), 2);
    assert_eq!(f.players[0].start_stocks, 4);
    assert_eq!(winner_port(&f), Some(2));
}

#[test]
fn facts_detail() {
    let f = parse(&simple_game(1, "t")).unwrap();
    assert_eq!(f.slippi_version, [3, 18, 0, 0]);
    let ports: Vec<u8> = f.players.iter().map(|p| p.port).collect();
    assert_eq!(ports, vec![1, 2]);
    assert!(f.players.iter().all(|p| p.kind == PlayerKind::Human));
    assert!(f.end.is_some());
    assert_eq!(f.finals.len(), 2);
    assert_eq!(f.finals.iter().find(|p| p.port == 2).unwrap().stocks, 0);
    assert_eq!(winner_port(&f), Some(1));
}

#[test]
fn winner_rules() {
    // timeout, stocks 2 v 1 => port with 2
    assert_eq!(
        winner_port(&duel((2, 80.0), (1, 10.0), Some((1, None)))),
        Some(1)
    );
    assert_eq!(
        winner_port(&duel((1, 10.0), (2, 80.0), Some((1, None)))),
        Some(2)
    );
    // stocks equal, percent 40.0 v 90.5 => lower percent
    assert_eq!(
        winner_port(&duel((1, 40.0), (1, 90.5), Some((1, None)))),
        Some(1)
    );
    assert_eq!(
        winner_port(&duel((1, 90.5), (1, 40.0), Some((1, None)))),
        Some(2)
    );
    // equal stocks and percent => None
    assert_eq!(
        winner_port(&duel((1, 40.0), (1, 40.0), Some((1, None)))),
        None
    );
    // method 7 with lras Some(1) => Some(2)
    assert_eq!(
        winner_port(&duel((3, 0.0), (1, 99.0), Some((7, Some(1))))),
        Some(2)
    );
    // method 7, None => None
    assert_eq!(
        winner_port(&duel((3, 0.0), (1, 99.0), Some((7, None)))),
        None
    );
    // no Game End => None
    assert_eq!(winner_port(&duel((2, 0.0), (1, 0.0), None)), None);
    // a third Human player => None
    let three = SlpBuilder::new()
        .player(1, 2, PlayerKind::Human)
        .player(2, 9, PlayerKind::Human)
        .player(3, 20, PlayerKind::Human)
        .frame(100, 1, 2, 0.0)
        .frame(100, 2, 1, 0.0)
        .frame(100, 3, 1, 0.0)
        .end(1, None)
        .build();
    assert_eq!(winner_port(&parse(&three).unwrap()), None);
}

#[test]
fn lras_port_is_one_based() {
    let f = duel((4, 0.0), (4, 0.0), Some((7, Some(2))));
    let end = f.end.as_ref().unwrap();
    assert_eq!((end.method, end.lras_initiator), (7, Some(2)));
    assert_eq!(winner_port(&f), Some(1));
}

#[test]
fn cpu_opponent_has_no_winner() {
    let g = SlpBuilder::new()
        .player(1, 2, PlayerKind::Human)
        .player(2, 9, PlayerKind::Cpu)
        .frame(100, 1, 2, 0.0)
        .frame(100, 2, 0, 0.0)
        .end(2, None)
        .build();
    let f = parse(&g).unwrap();
    assert_eq!(f.players[1].kind, PlayerKind::Cpu);
    assert_eq!(winner_port(&f), None);
}

#[test]
fn empty_ports_are_omitted() {
    let g = SlpBuilder::new()
        .player(2, 2, PlayerKind::Human)
        .player(4, 9, PlayerKind::Human)
        .build();
    let ports: Vec<u8> = parse(&g).unwrap().players.iter().map(|p| p.port).collect();
    assert_eq!(ports, vec![2, 4]);
}

#[test]
fn finals_come_from_last_frame_leaders_only() {
    let g = SlpBuilder::new()
        .player(1, 14, PlayerKind::Human) // Ice Climbers
        .player(2, 9, PlayerKind::Human)
        .frame(10, 1, 3, 20.0)
        .follower_frame(10, 1, 3, 70.0)
        .frame(10, 2, 3, 30.0)
        .frame(11, 1, 2, 0.0)
        .follower_frame(11, 1, 2, 99.0)
        .frame(11, 2, 3, 35.0)
        .end(2, None)
        .build();
    let f = parse(&g).unwrap();
    assert_eq!(f.last_frame, 11);
    assert_eq!(f.finals.len(), 2);
    let p1 = f.finals.iter().find(|p| p.port == 1).unwrap();
    assert_eq!((p1.stocks, p1.percent), (2, 0.0));
    let p2 = f.finals.iter().find(|p| p.port == 2).unwrap();
    assert_eq!((p2.stocks, p2.percent), (3, 35.0));
}

#[test]
fn last_frame_is_highest_and_rollback_resend_wins() {
    // Rollback re-sends a frame number; the latest copy of the highest frame is final.
    let g = SlpBuilder::new()
        .player(1, 2, PlayerKind::Human)
        .player(2, 9, PlayerKind::Human)
        .frame(50, 1, 4, 10.0)
        .frame(50, 2, 4, 10.0)
        .frame(50, 1, 4, 12.0)
        .frame(50, 2, 4, 11.0)
        .frame(20, 1, 4, 0.0)
        .end(1, None)
        .build();
    let f = parse(&g).unwrap();
    assert_eq!(f.last_frame, 50);
    assert_eq!(f.finals.iter().find(|p| p.port == 1).unwrap().percent, 12.0);
    assert_eq!(winner_port(&f), Some(2));
}

#[test]
fn gecko_detected() {
    let f = parse(
        &SlpBuilder::new()
            .player(1, 2, PlayerKind::Human)
            .gecko(true)
            .build(),
    )
    .unwrap();
    assert!(f.has_gecko);
    assert!(f.gecko_sha256.is_some());
}

/// A 1300-byte list: three Message Splitter events (512 + 512 + 276 bytes).
fn long_list() -> Vec<u8> {
    (0..1300u32).map(|i| (i % 251) as u8).collect()
}

fn with_list(list: &[u8]) -> Vec<u8> {
    SlpBuilder::new()
        .player(1, 2, PlayerKind::Human)
        .gecko_bytes(list)
        .frame(10, 1, 4, 0.0)
        .build()
}

#[test]
fn no_list_has_no_hashes() {
    for file in [simple_game(1, "t"), with_list(&[])] {
        let f = parse(&file).unwrap();
        assert!(!f.has_gecko);
        assert_eq!(f.gecko_sha256, None);
        assert_eq!(gecko_fnv32(&file).unwrap(), None);
    }
}

#[test]
fn list_hashes_are_sha256_and_engine_fnv32() {
    let file = with_list(b"abc");
    let f = parse(&file).unwrap();
    assert!(f.has_gecko);
    assert_eq!(
        f.gecko_sha256.as_deref(),
        Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
    assert_eq!(gecko_fnv32(&file).unwrap().as_deref(), Some("0541574b"));
}

#[test]
fn multi_event_list_hashes_as_the_concatenation() {
    let file = with_list(&long_list());
    let f = parse(&file).unwrap();
    assert!(f.has_gecko);
    assert_eq!(
        f.gecko_sha256.as_deref(),
        Some("b60f0e05937c77e7c3818dde3d41f69073ae3bf508374d1295df7a55bde99ee9")
    );
    assert_eq!(gecko_fnv32(&file).unwrap().as_deref(), Some("8de65260"));
    // Any change to the list changes the identity.
    let mut other = long_list();
    other[1299] ^= 1;
    assert_ne!(
        parse(&with_list(&other)).unwrap().gecko_sha256,
        f.gecko_sha256
    );
}

/// A Message Splitter event (payload 0x204 bytes) carrying a Gecko List chunk
/// whose "actual size" field says `claimed`.
fn splitter(block: &[u8], claimed: u16) -> Vec<u8> {
    let mut p = vec![0u8; 0x204];
    p[..block.len()].copy_from_slice(block);
    p[0x200..0x202].copy_from_slice(&claimed.to_be_bytes());
    p[0x202] = 0x3D;
    p[0x203] = 1;
    p
}

#[test]
fn splitter_size_beyond_its_block_is_capped_at_512() {
    let block: Vec<u8> = (0..512u32).map(|i| (i * 7) as u8).collect();
    let capped = SlpBuilder::new()
        .player(1, 2, PlayerKind::Human)
        .event(0x10, splitter(&block, 0xFFFF))
        .build();
    let f = parse(&capped).unwrap();
    assert_eq!(
        f.gecko_sha256,
        parse(&with_list(&block)).unwrap().gecko_sha256
    );
    assert_eq!(
        gecko_fnv32(&capped).unwrap(),
        gecko_fnv32(&with_list(&block)).unwrap()
    );
    // A zero actual size contributes nothing: no list.
    let empty = SlpBuilder::new()
        .player(1, 2, PlayerKind::Human)
        .event(0x10, splitter(&block, 0))
        .build();
    let f = parse(&empty).unwrap();
    assert!(!f.has_gecko);
    assert_eq!(f.gecko_sha256, None);
}

#[test]
fn gecko_fnv32_errors_on_garbage() {
    assert!(matches!(gecko_fnv32(b"hello"), Err(ReplayError::NotSlp)));
    let g = with_list(b"abc");
    assert!(gecko_fnv32(&g[..40]).is_err());
}

#[test]
fn teams_stage_timer_stocks() {
    let g = SlpBuilder::new()
        .stage(31)
        .timer_secs(420)
        .stocks(3)
        .teams(true)
        .player(1, 2, PlayerKind::Human)
        .build();
    let f = parse(&g).unwrap();
    assert_eq!(
        (f.stage, f.timer_secs, f.is_teams, f.players[0].start_stocks),
        (31, 420, true, 3)
    );
    assert_eq!(f.end, None);
}

#[test]
fn skips_unknown_events() {
    let g = SlpBuilder::new()
        .player(1, 2, PlayerKind::Human)
        .player(2, 9, PlayerKind::Human)
        .event(0x10, vec![0; 516])
        .event(0x42, vec![0xAB; 7])
        .frame(100, 1, 2, 0.0)
        .frame(100, 2, 0, 50.0)
        .end(2, None)
        .build();
    let f = parse(&g).unwrap();
    assert!(!f.has_gecko);
    assert_eq!(f.last_frame, 100);
    assert_eq!(winner_port(&f), Some(1));
}

#[test]
fn rejects_garbage() {
    assert!(matches!(parse(b"hello"), Err(ReplayError::NotSlp)));
    let g = simple_game(1, "t");
    for cut in [10, 40, g.len() / 2] {
        assert!(parse(&g[..cut]).is_err()); // never panics
    }
    assert!(parse(&[0u8; 4096]).is_err());
}

#[test]
fn error_kinds() {
    assert!(matches!(parse(b""), Err(ReplayError::NotSlp)));
    let g = simple_game(1, "t");
    assert!(matches!(parse(&g[..10]), Err(ReplayError::Truncated)));
    assert!(matches!(parse(&g[..40]), Err(ReplayError::Truncated)));
    // Raw element whose stream does not open with Event Payloads.
    let mut bad = g.clone();
    bad[15] = 0x36;
    assert!(matches!(parse(&bad), Err(ReplayError::BadEventSizes)));
    // Event Payloads alone, no Game Start.
    let only_sizes = with_raw(&[0x35, 4, 0x36, 0x00, 0x10]);
    assert!(matches!(
        parse(&only_sizes),
        Err(ReplayError::MissingGameStart)
    ));
    // An event code missing from the size table.
    let unknown = with_raw(&[0x35, 1, 0x77]);
    assert!(matches!(parse(&unknown), Err(ReplayError::BadEventSizes)));
}

/// Wraps an event stream in the `raw` element framing (no metadata).
fn with_raw(stream: &[u8]) -> Vec<u8> {
    let mut f = b"{U\x03raw[$U#l".to_vec();
    f.extend_from_slice(&(stream.len() as u32).to_be_bytes());
    f.extend_from_slice(stream);
    f.push(b'}');
    f
}

/// Parses `f` and checks the invariants that must hold for any readable file.
fn parse_checked(f: &[u8]) -> Result<ReplayFacts, ReplayError> {
    let parsed = parse(f);
    let fnv = gecko_fnv32(f);
    if let Ok(facts) = &parsed {
        let _ = winner_port(facts);
        assert_eq!(facts.has_gecko, facts.gecko_sha256.is_some());
        let sha = facts.gecko_sha256.as_deref().unwrap_or("");
        assert!(sha.is_empty() || (sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit())));
        // The walk that parse completed also completes for the list alone.
        let fnv = fnv.expect("gecko_fnv32 succeeds on a file parse accepts");
        assert_eq!(fnv.is_some(), facts.has_gecko);
    }
    parsed
}

#[test]
fn every_truncation_and_mutation_is_safe() {
    let builder = || {
        SlpBuilder::new()
            .player(1, 2, PlayerKind::Human)
            .player(2, 9, PlayerKind::Human)
            .event(0x42, vec![1, 2, 3])
            .frame(-123, 1, 4, 0.0)
            .frame(-123, 2, 4, 0.0)
            .frame(10, 1, 3, 12.5)
            .frame(10, 2, 1, 80.0)
            .end(2, None)
    };
    let files = [
        builder().build(),
        builder().gecko(true).build(),
        // Two splitter events, the second one partly filled.
        builder().gecko_bytes(&long_list()[..600]).build(),
    ];
    for g in &files {
        let raw_len = raw_element(g).unwrap().len();
        // Truncating the file anywhere inside the raw element is an error.
        for cut in 0..15 + raw_len {
            assert!(parse_checked(&g[..cut]).is_err(), "cut {cut}");
        }
        // Truncating inside the metadata leaves the raw element intact.
        for cut in 15 + raw_len..g.len() {
            let _ = parse_checked(&g[..cut]);
        }
        // A stream cut short with a consistent declared length (event split mid-payload).
        for len in 0..raw_len {
            let mut f = g[..15 + len].to_vec();
            f[11..15].copy_from_slice(&(len as u32).to_be_bytes());
            let _ = parse_checked(&f);
        }
        // Single-byte corruption anywhere.
        for i in 0..g.len() {
            for v in [
                0x00, 0x01, 0x02, 0x10, 0x35, 0x36, 0x38, 0x39, 0x3D, 0x7F, 0xFF,
            ] {
                let mut f = g.clone();
                f[i] = v;
                let _ = parse_checked(&f);
            }
        }
    }
}
