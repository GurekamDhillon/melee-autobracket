//! Results derived from brackets: placings, set and game records, player profiles. The
//! bracket is the source of truth; nothing here is stored. [`load_events`] is the one query,
//! the rest are pure functions over what it loaded.
//!
//! Counting rules:
//! - A **set** counts for both players once its bracket set is `Done` with an entrant in both
//!   slots ([`Bracket::was_played`]), whatever decided it (played games, award, DQ). Sets of
//!   running events count as they finish.
//! - **Games** are the set's stored wins (`[slot 0, slot 1]`): the games played, including
//!   those before an award or DQ. A set with no stored score counts `[0, 0]`.
//! - **Events** counts entries in any state; **titles** counts complete events the player won.
//! - **Place** comes from [`Bracket::placings`] and exists only for a complete event.
//! - `seed` is the entrant's index in the event's tags (0 = top seed).

use std::collections::HashMap;

use bracket::{Bracket, SetState, Slot};
use proto::{EventSummary, HeadToHead, Placing, PlayerEvent, PlayerProfile, PlayerSummary};
use sqlx::SqlitePool;

use crate::routes::ApiError;

/// One event with everything results need: its entrants' canonical tags in seed order, the
/// bracket (none before the start) and the stored score of every set that has been opened.
#[derive(Clone, Debug)]
pub struct LoadedEvent {
    pub summary: EventSummary,
    pub tags: Vec<String>,
    pub bracket: Option<Bracket>,
    pub scores: HashMap<u32, [u8; 2]>,
}

/// One `events` row: id, name, state, `bracket_json`, `starts_at`, `description`, `created_at`.
type EventRow = (
    i64,
    String,
    String,
    Option<String>,
    Option<i64>,
    Option<String>,
    i64,
);

/// Every event, oldest first, read in one deferred transaction (one consistent snapshot).
pub async fn load_events(db: &SqlitePool) -> Result<Vec<LoadedEvent>, ApiError> {
    load_matching(db, "", "id", NO_LIMIT).await
}

/// The newest `limit` complete events (every one when `None`), newest first; only their
/// entrants and sets are read, so the cost is bounded by `limit`, not by the table.
pub async fn load_latest_complete(
    db: &SqlitePool,
    limit: Option<u32>,
) -> Result<Vec<LoadedEvent>, ApiError> {
    let limit = limit.map_or(NO_LIMIT, i64::from);
    load_matching(db, "WHERE state = 'complete'", "id DESC", limit).await
}

/// SQLite's "no limit".
const NO_LIMIT: i64 = -1;

/// At most `limit` events in `order` among those `filter` (a fixed `WHERE` clause or empty;
/// both are code, never data) keeps, with their entrants and sets, in one deferred
/// transaction (one consistent snapshot).
async fn load_matching(
    db: &SqlitePool,
    filter: &str,
    order: &str,
    limit: i64,
) -> Result<Vec<LoadedEvent>, ApiError> {
    let chosen = format!("SELECT id FROM events {filter} ORDER BY {order} LIMIT ?");
    let mut tx = db.begin().await?;
    let events: Vec<EventRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT id, name, state, bracket_json, starts_at, description, created_at
         FROM events WHERE id IN ({chosen}) ORDER BY {order}"
    )))
    .bind(limit)
    .fetch_all(&mut *tx)
    .await?;
    let entrants: Vec<(i64, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT n.event_id, p.tag FROM entrants n JOIN players p ON p.key = n.player_key
         WHERE n.event_id IN ({chosen}) ORDER BY n.event_id, n.seed"
    )))
    .bind(limit)
    .fetch_all(&mut *tx)
    .await?;
    let sets: Vec<(i64, i64, i64, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT event_id, set_id, wins0, wins1 FROM sets
         WHERE event_id IN ({chosen}) ORDER BY event_id, set_id"
    )))
    .bind(limit)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut tags_by_event: HashMap<i64, Vec<String>> = HashMap::new();
    for (event_id, tag) in entrants {
        tags_by_event.entry(event_id).or_default().push(tag);
    }
    let mut scores_by_event: HashMap<i64, HashMap<u32, [u8; 2]>> = HashMap::new();
    for (event_id, set_id, w0, w1) in sets {
        scores_by_event
            .entry(event_id)
            .or_default()
            .insert(to_int(set_id)?, [to_int(w0)?, to_int(w1)?]);
    }

    events
        .into_iter()
        .map(
            |(id, name, state, bracket_json, starts_at, description, created_at)| {
                let bracket = bracket_json
                    .map(|json| {
                        serde_json::from_str::<Bracket>(&json).map_err(|e| {
                            ApiError::internal(format_args!("event {id} bracket_json: {e}"))
                        })
                    })
                    .transpose()?;
                let tags = tags_by_event.remove(&id).unwrap_or_default();
                Ok(LoadedEvent {
                    summary: EventSummary {
                        id,
                        name,
                        state,
                        entrants: to_int(tags.len() as i64)?,
                        starts_at,
                        description,
                        created_at,
                    },
                    tags,
                    bracket,
                    scores: scores_by_event.remove(&id).unwrap_or_default(),
                })
            },
        )
        .collect()
}

/// Narrows a stored integer, treating an out-of-range value as corrupt data.
fn to_int<T: TryFrom<i64>>(v: i64) -> Result<T, ApiError> {
    T::try_from(v).map_err(|_| ApiError::internal(format_args!("stored integer {v} out of range")))
}

/// Final placings, ordered by place then seed; empty unless the event is complete.
pub fn placings(ev: &LoadedEvent) -> Vec<Placing> {
    let Some(places) = ev.bracket.as_ref().and_then(Bracket::placings) else {
        return Vec::new();
    };
    let mut out: Vec<Placing> = places
        .iter()
        .zip(&ev.tags)
        .map(|(&place, tag)| Placing {
            place,
            tag: tag.clone(),
        })
        .collect();
    // Stable, so entrants of one place stay in seed order.
    out.sort_by_key(|p| p.place);
    out
}

/// A decided set: who won and lost (as seeds), and the games `[winner's, loser's]`.
struct SetResult {
    winner: usize,
    loser: usize,
    games: [u32; 2],
}

/// Every set of `ev` that was played ([`Bracket::was_played`]).
fn set_results(ev: &LoadedEvent) -> impl Iterator<Item = SetResult> + '_ {
    let sets = ev
        .bracket
        .as_ref()
        .map(|b| b.sets.as_slice())
        .unwrap_or(&[]);
    sets.iter().filter_map(move |set| {
        let SetState::Done { winner } = set.state else {
            return None;
        };
        let (w, l) = (usize::from(winner), usize::from(1 - winner));
        let (Slot::Entrant(won), Slot::Entrant(lost)) = (set.slots[w], set.slots[l]) else {
            return None;
        };
        let wins = ev.scores.get(&set.id).copied().unwrap_or([0, 0]);
        Some(SetResult {
            winner: won as usize,
            loser: lost as usize,
            games: [u32::from(wins[w]), u32::from(wins[l])],
        })
    })
}

/// The tag of the champion of a complete event.
fn champion(ev: &LoadedEvent) -> Option<&str> {
    let id = ev.bracket.as_ref()?.champion()?;
    ev.tags.get(id as usize).map(String::as_str)
}

/// Sets and games `[won, lost]` of the entrant at `seed`, and the same against each opponent.
#[derive(Default)]
struct Record {
    sets: [u32; 2],
    games: [u32; 2],
}

impl Record {
    fn add(&mut self, won: bool, games: [u32; 2]) {
        let (me, them) = if won { (0, 1) } else { (1, 0) };
        self.sets[me] += 1;
        self.games[me] += games[0];
        self.games[them] += games[1];
    }
}

/// The record of the entrant at `seed` in `ev`, and against each opponent (by seed).
fn record_of(ev: &LoadedEvent, seed: usize) -> (Record, HashMap<usize, Record>) {
    let mut total = Record::default();
    let mut against: HashMap<usize, Record> = HashMap::new();
    for set in set_results(ev) {
        let (won, opponent) = if set.winner == seed {
            (true, set.loser)
        } else if set.loser == seed {
            (false, set.winner)
        } else {
            continue;
        };
        total.add(won, set.games);
        against.entry(opponent).or_default().add(won, set.games);
    }
    (total, against)
}

/// One row per registered tag, ordered by tag ignoring case.
pub fn player_summaries(players: &[String], events: &[LoadedEvent]) -> Vec<PlayerSummary> {
    let mut out: Vec<PlayerSummary> = players
        .iter()
        .map(|tag| {
            let mut summary = PlayerSummary {
                tag: tag.clone(),
                events: 0,
                titles: 0,
                sets: [0, 0],
            };
            for ev in events {
                let Some(seed) = ev.tags.iter().position(|t| t == tag) else {
                    continue;
                };
                summary.events += 1;
                if champion(ev) == Some(tag.as_str()) {
                    summary.titles += 1;
                }
                let (record, _) = record_of(ev, seed);
                summary.sets[0] += record.sets[0];
                summary.sets[1] += record.sets[1];
            }
            summary
        })
        .collect();
    out.sort_by(|a, b| {
        a.tag
            .to_lowercase()
            .cmp(&b.tag.to_lowercase())
            .then_with(|| a.tag.cmp(&b.tag))
    });
    out
}

/// The profile of the player with canonical tag `tag`, registered at `since`: their events
/// newest first and their head-to-head records by sets played descending, then tag.
pub fn player_profile(tag: &str, since: i64, events: &[LoadedEvent]) -> PlayerProfile {
    let mut entries = Vec::new();
    let mut against: HashMap<&str, Record> = HashMap::new();
    for ev in events.iter().rev() {
        let Some(seed) = ev.tags.iter().position(|t| t == tag) else {
            continue;
        };
        let (record, opponents) = record_of(ev, seed);
        for (opponent, r) in opponents {
            let entry = against.entry(&ev.tags[opponent]).or_default();
            for i in 0..2 {
                entry.sets[i] += r.sets[i];
                entry.games[i] += r.games[i];
            }
        }
        let place = ev
            .bracket
            .as_ref()
            .and_then(Bracket::placings)
            .and_then(|places| places.get(seed).copied());
        entries.push(PlayerEvent {
            event: ev.summary.clone(),
            seed: seed as u32,
            place,
            sets: record.sets,
            games: record.games,
        });
    }
    let mut head_to_head: Vec<HeadToHead> = against
        .into_iter()
        .map(|(tag, r)| HeadToHead {
            tag: tag.to_owned(),
            sets: r.sets,
            games: r.games,
        })
        .collect();
    head_to_head.sort_by(|a, b| {
        let played = |h: &HeadToHead| h.sets[0] + h.sets[1];
        played(b).cmp(&played(a)).then_with(|| a.tag.cmp(&b.tag))
    });
    PlayerProfile {
        tag: tag.to_owned(),
        since,
        events: entries,
        head_to_head,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A complete SE 4 bracket in which slot 0 won every set, with the given set scores.
    fn se4(tags: &[&str], scores: &[(u32, [u8; 2])]) -> LoadedEvent {
        let mut bracket = bracket::build(bracket::Format::SingleElim, 4).unwrap();
        while !bracket.is_complete() {
            for set in bracket.ready_sets() {
                bracket.report(set, 0).unwrap();
            }
        }
        LoadedEvent {
            summary: EventSummary {
                id: 1,
                name: "Weekly".into(),
                state: "complete".into(),
                entrants: 4,
                starts_at: None,
                description: None,
                created_at: 0,
            },
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            bracket: Some(bracket),
            scores: scores.iter().copied().collect(),
        }
    }

    #[test]
    fn placings_are_by_place_then_seed() {
        let ev = se4(&["A", "B", "C", "D"], &[]);
        let got: Vec<(u32, String)> = placings(&ev)
            .into_iter()
            .map(|p| (p.place, p.tag))
            .collect();
        // Set 0: A v D, set 1: B v C, final: A v B.
        let want: Vec<(u32, String)> = [(1, "A"), (2, "B"), (3, "C"), (3, "D")]
            .into_iter()
            .map(|(place, tag)| (place, tag.to_owned()))
            .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn running_event_has_no_placings() {
        let mut ev = se4(&["A", "B", "C", "D"], &[]);
        ev.bracket = Some(bracket::build(bracket::Format::SingleElim, 4).unwrap());
        assert!(placings(&ev).is_empty());
        ev.bracket = None;
        assert!(placings(&ev).is_empty());
    }

    #[test]
    fn missing_scores_count_as_no_games() {
        let ev = se4(&["A", "B", "C", "D"], &[(0, [2, 1])]);
        let profile = player_profile("A", 0, &[ev]);
        assert_eq!(profile.events[0].sets, [2, 0]);
        assert_eq!(profile.events[0].games, [2, 1]);
        assert_eq!(profile.head_to_head[0].tag, "B");
        assert_eq!(profile.head_to_head[0].games, [0, 0]);
        assert_eq!(profile.head_to_head[1].tag, "D");
        assert_eq!(profile.head_to_head[1].games, [2, 1]);
    }

    #[test]
    fn summaries_order_ignores_case() {
        let players = vec!["zain".to_owned(), "Armada".to_owned(), "mango".to_owned()];
        let got: Vec<String> = player_summaries(&players, &[])
            .into_iter()
            .map(|p| p.tag)
            .collect();
        assert_eq!(got, vec!["Armada", "mango", "zain"]);
    }
}
