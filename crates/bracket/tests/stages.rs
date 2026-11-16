use bracket::stages::*;
use StageKind::*;

fn mv(slot: u8, kind: StageKind, stage: u16) -> Move {
    Move { slot, kind, stage }
}

/// Strikes the first stage of the pool until done; returns the turn seen before each move.
fn run_strikes(starters: &[u16]) -> (Vec<(u8, StageKind, u8)>, Selection) {
    let ctx = GameCtx::First { starters };
    let mut moves = Vec::new();
    let mut seen = Vec::new();
    let mut sel = replay(ctx, &moves).unwrap();
    while let Some(t) = sel.turn {
        seen.push((t.slot, t.kind, t.count));
        let m = mv(t.slot, Strike, sel.pool[0]);
        sel = apply(ctx, &moves, m).unwrap();
        moves.push(m);
    }
    (seen, sel)
}

#[test]
fn five_starters_strike_1_2_1() {
    let (seen, sel) = run_strikes(&[31, 32, 28, 3, 8]);
    assert_eq!(
        seen,
        vec![
            (0, Strike, 1),
            (1, Strike, 2),
            (1, Strike, 1),
            (0, Strike, 1)
        ]
    );
    assert_eq!(sel.chosen, Some(8));
    assert_eq!(sel.turn, None);
    assert_eq!(sel.removed, vec![31, 32, 28, 3]);
}

#[test]
fn three_starters_1_1() {
    let (seen, sel) = run_strikes(&[31, 32, 28]);
    assert_eq!(seen, vec![(0, Strike, 1), (1, Strike, 1)]);
    assert_eq!(sel.chosen, Some(28));
}

#[test]
fn seven_starters_1_2_2_1() {
    let (seen, sel) = run_strikes(&[1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(
        seen,
        vec![
            (0, Strike, 1),
            (1, Strike, 2),
            (1, Strike, 1),
            (0, Strike, 2),
            (0, Strike, 1),
            (1, Strike, 1)
        ]
    );
    assert_eq!(sel.chosen, Some(7));
}

#[test]
fn two_and_four_starters() {
    let (seen, sel) = run_strikes(&[9, 10]);
    assert_eq!(seen, vec![(0, Strike, 1)]);
    assert_eq!(sel.chosen, Some(10));
    let (seen, sel) = run_strikes(&[1, 2, 3, 4]);
    assert_eq!(seen, vec![(0, Strike, 1), (1, Strike, 2), (1, Strike, 1)]);
    assert_eq!(sel.chosen, Some(4));
}

#[test]
fn one_starter_is_chosen_at_once() {
    let ctx = GameCtx::First { starters: &[31] };
    let sel = replay(ctx, &[]).unwrap();
    assert_eq!(sel.chosen, Some(31));
    assert_eq!(sel.turn, None);
    assert_eq!(
        apply(ctx, &[], mv(0, Strike, 31)),
        Err(StageError::Complete)
    );
}

#[test]
fn empty_list_has_no_turn_and_rejects_moves() {
    let ctx = GameCtx::First { starters: &[] };
    let sel = replay(ctx, &[]).unwrap();
    assert_eq!((sel.chosen, sel.turn), (None, None));
    assert_eq!(apply(ctx, &[], mv(0, Strike, 1)), Err(StageError::Complete));
}

#[test]
fn duplicate_starters_count_once() {
    let (seen, sel) = run_strikes(&[31, 31, 32, 32, 28]);
    assert_eq!(seen, vec![(0, Strike, 1), (1, Strike, 1)]);
    assert_eq!(sel.chosen, Some(28));
    let sel = replay(GameCtx::First { starters: &[5, 5] }, &[]).unwrap();
    assert_eq!(sel.chosen, Some(5));
}

#[test]
fn later_game_winner_bans_two_loser_picks() {
    let legal = [1, 2, 3, 4, 5, 6];
    let ctx = GameCtx::Later {
        legal: &legal,
        bans: 2,
        prev_winner: 1,
    };
    let sel = replay(ctx, &[]).unwrap();
    assert_eq!(
        sel.turn,
        Some(Turn {
            slot: 1,
            kind: Ban,
            count: 2
        })
    );
    let mut moves = vec![mv(1, Ban, 1)];
    let sel = apply(ctx, &[], moves[0]).unwrap();
    assert_eq!(
        sel.turn,
        Some(Turn {
            slot: 1,
            kind: Ban,
            count: 1
        })
    );
    let m = mv(1, Ban, 2);
    let sel = apply(ctx, &moves, m).unwrap();
    moves.push(m);
    assert_eq!(
        sel.turn,
        Some(Turn {
            slot: 0,
            kind: Pick,
            count: 1
        })
    );
    assert_eq!(sel.pool, vec![3, 4, 5, 6]);
    let sel = apply(ctx, &moves, mv(0, Pick, 5)).unwrap();
    assert_eq!(sel.chosen, Some(5));
    assert_eq!(sel.turn, None);
    assert_eq!(sel.removed, vec![1, 2]);
}

#[test]
fn later_game_zero_bans_goes_straight_to_pick() {
    let ctx = GameCtx::Later {
        legal: &[1, 2, 3],
        bans: 0,
        prev_winner: 0,
    };
    assert_eq!(
        replay(ctx, &[]).unwrap().turn,
        Some(Turn {
            slot: 1,
            kind: Pick,
            count: 1
        })
    );
    assert_eq!(replay(ctx, &[mv(1, Pick, 2)]).unwrap().chosen, Some(2));
}

#[test]
fn later_game_bans_are_clamped_to_leave_one_stage() {
    let ctx = GameCtx::Later {
        legal: &[1, 2, 2],
        bans: 9,
        prev_winner: 0,
    };
    let sel = replay(ctx, &[]).unwrap();
    assert_eq!(
        sel.turn,
        Some(Turn {
            slot: 0,
            kind: Ban,
            count: 1
        })
    );
    let sel = replay(ctx, &[mv(0, Ban, 1)]).unwrap();
    assert_eq!(
        sel.turn,
        Some(Turn {
            slot: 1,
            kind: Pick,
            count: 1
        })
    );
    assert_eq!(sel.pool, vec![2]);
}

#[test]
fn later_game_bad_prev_winner() {
    let ctx = GameCtx::Later {
        legal: &[1, 2],
        bans: 1,
        prev_winner: 2,
    };
    assert_eq!(replay(ctx, &[]), Err(StageError::BadSlot));
}

#[test]
fn errors() {
    let starters = [31, 32, 28, 3, 8];
    let ctx = GameCtx::First {
        starters: &starters,
    };
    // wrong slot
    assert_eq!(
        apply(ctx, &[], mv(1, Strike, 31)),
        Err(StageError::NotYourTurn)
    );
    // pick / ban during strikes
    assert_eq!(apply(ctx, &[], mv(0, Pick, 31)), Err(StageError::WrongKind));
    assert_eq!(apply(ctx, &[], mv(0, Ban, 31)), Err(StageError::WrongKind));
    // stage not in the list
    assert_eq!(
        apply(ctx, &[], mv(0, Strike, 99)),
        Err(StageError::NotInPool)
    );
    // bad slot
    assert_eq!(apply(ctx, &[], mv(2, Strike, 31)), Err(StageError::BadSlot));
    // struck stage again
    let h = [mv(0, Strike, 31)];
    assert_eq!(
        apply(ctx, &h, mv(1, Strike, 31)),
        Err(StageError::AlreadyRemoved)
    );
    // same slot twice in a one-move turn
    assert_eq!(
        apply(ctx, &h, mv(0, Strike, 32)),
        Err(StageError::NotYourTurn)
    );
    // slot 1's two-move turn: third consecutive move by slot 1 is rejected
    let h = [mv(0, Strike, 31), mv(1, Strike, 32), mv(1, Strike, 28)];
    assert_eq!(
        apply(ctx, &h, mv(1, Strike, 3)),
        Err(StageError::NotYourTurn)
    );
    // any move after chosen
    let h = [
        mv(0, Strike, 31),
        mv(1, Strike, 32),
        mv(1, Strike, 28),
        mv(0, Strike, 3),
    ];
    assert_eq!(replay(ctx, &h).unwrap().chosen, Some(8));
    assert_eq!(apply(ctx, &h, mv(0, Strike, 8)), Err(StageError::Complete));
    assert_eq!(apply(ctx, &h, mv(2, Pick, 99)), Err(StageError::Complete));
    // a bad stored history fails replay
    assert_eq!(
        replay(ctx, &[mv(0, Strike, 31), mv(1, Strike, 31)]),
        Err(StageError::AlreadyRemoved)
    );
}

#[test]
fn later_game_errors() {
    let legal = [1, 2, 3, 4, 5, 6];
    let ctx = GameCtx::Later {
        legal: &legal,
        bans: 2,
        prev_winner: 1,
    };
    // loser tries to act first, or the winner picks
    assert_eq!(apply(ctx, &[], mv(0, Ban, 1)), Err(StageError::NotYourTurn));
    assert_eq!(apply(ctx, &[], mv(1, Pick, 1)), Err(StageError::WrongKind));
    assert_eq!(
        apply(ctx, &[], mv(1, Strike, 1)),
        Err(StageError::WrongKind)
    );
    assert_eq!(apply(ctx, &[], mv(1, Ban, 9)), Err(StageError::NotInPool));
    // repeated ban
    let h = [mv(1, Ban, 1)];
    assert_eq!(
        apply(ctx, &h, mv(1, Ban, 1)),
        Err(StageError::AlreadyRemoved)
    );
    // winner cannot ban a third, loser must pick, ban kind refused
    let h = [mv(1, Ban, 1), mv(1, Ban, 2)];
    assert_eq!(apply(ctx, &h, mv(1, Ban, 3)), Err(StageError::NotYourTurn));
    assert_eq!(apply(ctx, &h, mv(0, Ban, 3)), Err(StageError::WrongKind));
    // loser picks a banned stage
    assert_eq!(
        apply(ctx, &h, mv(0, Pick, 2)),
        Err(StageError::AlreadyRemoved)
    );
    assert_eq!(apply(ctx, &h, mv(0, Pick, 77)), Err(StageError::NotInPool));
    assert_eq!(apply(ctx, &h, mv(3, Pick, 3)), Err(StageError::BadSlot));
    // after the pick
    let h = [mv(1, Ban, 1), mv(1, Ban, 2), mv(0, Pick, 3)];
    assert_eq!(apply(ctx, &h, mv(1, Ban, 4)), Err(StageError::Complete));
}

#[test]
fn chosen_stays_in_pool_in_both_modes() {
    let sel = replay(
        GameCtx::First {
            starters: &[1, 2, 3],
        },
        &[mv(0, Strike, 1), mv(1, Strike, 3)],
    )
    .unwrap();
    assert_eq!((sel.chosen, sel.pool.clone()), (Some(2), vec![2]));
    let ctx = GameCtx::Later {
        legal: &[1, 2, 3],
        bans: 1,
        prev_winner: 0,
    };
    let sel = replay(ctx, &[mv(0, Ban, 1), mv(1, Pick, 3)]).unwrap();
    assert_eq!(sel.chosen, Some(3));
    assert_eq!(sel.pool, vec![2, 3]);
    assert_eq!(sel.removed, vec![1]);
}

#[test]
fn later_empty_list_has_no_turn() {
    let ctx = GameCtx::Later {
        legal: &[],
        bans: 2,
        prev_winner: 0,
    };
    let sel = replay(ctx, &[]).unwrap();
    assert_eq!((sel.chosen, sel.turn), (None, None));
    assert_eq!(apply(ctx, &[], mv(1, Pick, 1)), Err(StageError::Complete));
}

#[test]
fn later_single_stage_is_chosen_at_once() {
    let ctx = GameCtx::Later {
        legal: &[7, 7],
        bans: 2,
        prev_winner: 1,
    };
    let sel = replay(ctx, &[]).unwrap();
    assert_eq!((sel.chosen, sel.turn), (Some(7), None));
    assert_eq!(apply(ctx, &[], mv(0, Pick, 7)), Err(StageError::Complete));
}

#[test]
fn opponent_cannot_move_mid_two_move_turn() {
    let ctx = GameCtx::First {
        starters: &[1, 2, 3, 4, 5],
    };
    let h = [mv(0, Strike, 1), mv(1, Strike, 2)];
    assert_eq!(
        apply(ctx, &h, mv(0, Strike, 3)),
        Err(StageError::NotYourTurn)
    );
}

#[test]
fn strikes_in_any_order_keep_pool_in_list_order() {
    let ctx = GameCtx::First {
        starters: &[10, 20, 30, 40, 50],
    };
    let h = [
        mv(0, Strike, 30),
        mv(1, Strike, 50),
        mv(1, Strike, 10),
        mv(0, Strike, 40),
    ];
    let sel = replay(ctx, &h).unwrap();
    assert_eq!(sel.removed, vec![30, 50, 10, 40]);
    assert_eq!(sel.pool, vec![20]);
    assert_eq!(sel.chosen, Some(20));
    let sel = replay(ctx, &h[..2]).unwrap();
    assert_eq!(sel.pool, vec![10, 20, 40]);
}

#[test]
fn apply_tells_bad_history_from_bad_move() {
    let ctx = GameCtx::First {
        starters: &[1, 2, 3],
    };
    let bad = [mv(1, Strike, 1)];
    assert_eq!(replay(ctx, &bad), Err(StageError::NotYourTurn));
    assert_eq!(
        apply(ctx, &bad, mv(0, Strike, 2)),
        Err(StageError::BadHistory)
    );
    assert_eq!(
        apply(ctx, &[], mv(1, Strike, 2)),
        Err(StageError::NotYourTurn)
    );
}

#[test]
fn serde_round_trip() {
    let m = mv(1, Ban, 28);
    let t = Turn {
        slot: 0,
        kind: Pick,
        count: 1,
    };
    let ctx = GameCtx::Later {
        legal: &[1, 2, 3],
        bans: 1,
        prev_winner: 0,
    };
    let sel = replay(ctx, &[mv(0, Ban, 1)]).unwrap();
    assert_eq!(
        serde_json::from_str::<Move>(&serde_json::to_string(&m).unwrap()).unwrap(),
        m
    );
    assert_eq!(
        serde_json::from_str::<Turn>(&serde_json::to_string(&t).unwrap()).unwrap(),
        t
    );
    assert_eq!(
        serde_json::from_str::<Selection>(&serde_json::to_string(&sel).unwrap()).unwrap(),
        sel
    );
}
