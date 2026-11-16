//! External verifier mode: the server queues re-simulation jobs, a verifier claims them
//! over the signed API, and the posted result decides the game.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use autobracket_server::db::now_unix;
use autobracket_server::resim::{ExternalResim, Resim};
use autobracket_server::testkit::{TestApp, TestPlayer};
use autobracket_server::worker::judge_due;
use autobracket_server::Config;
use bracket::Format;
use proto::{sign_request, ResimOutcome, SigningKey, Verdict, VerifyJob, VerifyResult};
use rand::rngs::OsRng;
use replay::testkit::{simple_game, SlpBuilder};
use replay::PlayerKind;
use reqwest::{Method, StatusCode};

const START_A: &str = "2026-11-15T20:00:00Z";
const START_B: &str = "2026-11-15T20:00:01Z";

/// A client signing with a key of its own (here: a verifier key).
struct Signer {
    sk: SigningKey,
    url: String,
    client: reqwest::Client,
}

impl Signer {
    fn new(sk: SigningKey, url: &str) -> Self {
        Signer {
            sk,
            url: url.into(),
            client: reqwest::Client::new(),
        }
    }

    async fn send(&self, method: Method, path: &str, body: Vec<u8>) -> reqwest::Response {
        let h = sign_request(&self.sk, method.as_str(), path, now_unix(), &body);
        self.client
            .request(method, format!("{}{path}", self.url))
            .header("x-ab-key", h.key)
            .header("x-ab-time", h.time)
            .header("x-ab-nonce", h.nonce)
            .header("x-ab-sig", h.sig)
            .body(body)
            .send()
            .await
            .expect("send request")
    }

    async fn claim(&self) -> reqwest::Response {
        self.send(Method::POST, "/v1/verify/claim", Vec::new())
            .await
    }

    async fn post_result(&self, id: i64, result: &VerifyResult) -> StatusCode {
        let body = serde_json::to_vec(result).unwrap();
        self.send(Method::POST, &format!("/v1/verify/jobs/{id}/result"), body)
            .await
            .status()
    }
}

fn hex_key(sk: &SigningKey) -> String {
    hex::encode(sk.verifying_key().to_bytes())
}

/// External mode with `verifier` registered as a verifier key.
fn external(verifier: &SigningKey) -> impl FnOnce(&mut Config) + '_ {
    move |c| {
        c.verifier.mode = autobracket_server::config::VerifierMode::External;
        c.verifier_keys = vec![hex_key(verifier)];
    }
}

/// A started 2-player single elimination event: `(host, guest, event)`.
async fn two_player(app: &TestApp) -> (TestPlayer, TestPlayer, i64) {
    let host = app.player("Alice").await;
    let guest = app.player("Bob").await;
    let event = app
        .started_event(Format::SingleElim, &[&host, &guest])
        .await;
    (host, guest, event)
}

/// Both players upload game 1 of set 0; returns the host's file.
async fn upload_pair(
    host: &TestPlayer,
    guest: &TestPlayer,
    event: i64,
    file: impl Fn(&str) -> Vec<u8>,
) -> Vec<u8> {
    let path = format!("/v1/sets/{event}/0/games/1/replay");
    let host_file = file(START_A);
    for (p, bytes) in [(host, host_file.clone()), (guest, file(START_B))] {
        let res = p.post_bytes(&path, bytes, &[("x-ab-build", "b1")]).await;
        assert_eq!(res.status(), StatusCode::ACCEPTED, "{} uploading", p.tag);
    }
    host_file
}

/// `(state, verdict, verifier_build)` of game 1 of set 0.
async fn game(app: &TestApp, event: i64) -> (String, Option<Verdict>, Option<String>) {
    let (state, verdict, build): (String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT state, verdict_json, verifier_build FROM games
         WHERE event_id = ? AND set_id = 0 AND game_no = 1",
    )
    .bind(event)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    (
        state,
        verdict.map(|v| serde_json::from_str(&v).unwrap()),
        build,
    )
}

/// Claims until a job is offered (the judging task queues it asynchronously).
async fn claim_job(verifier: &Signer) -> VerifyJob {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let res = verifier.claim().await;
        if res.status() == StatusCode::OK {
            return res.json().await.unwrap();
        }
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert!(Instant::now() < deadline, "no job was queued");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn job_round_trip() {
    let vk = SigningKey::generate(&mut OsRng);
    let resim = Arc::new(ExternalResim::new(Duration::from_secs(30)));
    let app = TestApp::spawn_with(external(&vk), resim).await;
    let (host, guest, event) = two_player(&app).await;
    let host_file = upload_pair(&host, &guest, event, |s| simple_game(1, s)).await;

    let st = app.state.clone();
    let judging = tokio::spawn(async move { judge_due(&st).await });

    let verifier = Signer::new(vk, &app.url);
    let job = claim_job(&verifier).await;
    assert_eq!(job.replay_last_frame, 5400);
    let res = verifier
        .send(
            Method::GET,
            &format!("/v1/verify/jobs/{}/replay", job.id),
            Vec::new(),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.bytes().await.unwrap().as_ref(), host_file.as_slice());

    let result = VerifyResult {
        outcome: ResimOutcome::Match,
        build: "remote-engine 1.2+abcdef".into(),
    };
    assert_eq!(
        verifier.post_result(job.id, &result).await,
        StatusCode::NO_CONTENT
    );
    let judged = tokio::time::timeout(Duration::from_secs(10), judging)
        .await
        .expect("judging finishes once the result is posted")
        .unwrap()
        .unwrap();
    assert_eq!(judged, 1);
    assert_eq!(
        game(&app, event).await,
        (
            "counted".into(),
            Some(Verdict::Verified),
            Some("remote-engine 1.2+abcdef".into())
        )
    );

    // A finished job keeps its first result.
    let late = VerifyResult {
        outcome: ResimOutcome::Crashed("late".into()),
        build: "other".into(),
    };
    assert_eq!(
        verifier.post_result(job.id, &late).await,
        StatusCode::CONFLICT
    );
    // Unknown jobs.
    assert_eq!(
        verifier
            .send(Method::GET, "/v1/verify/jobs/999/replay", Vec::new())
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        verifier.post_result(999, &result).await,
        StatusCode::NOT_FOUND
    );
}

/// Each game is labelled with the build that re-simulated it: the build a verifier posted
/// for its own job, or none when it was judged without a re-simulation, never a build left
/// over from an earlier game judged by the same task. A lone upload's evidence-only
/// re-simulation records its build in the evidence, not on the game.
#[tokio::test]
async fn verifier_build_belongs_to_its_game() {
    let vk = SigningKey::generate(&mut OsRng);
    let resim = Arc::new(ExternalResim::new(Duration::from_secs(30)));
    // One game at a time, so the jobs arrive in game order.
    let app = TestApp::spawn_with(
        |c| {
            external(&vk)(c);
            c.verifier.jobs = 1;
        },
        resim,
    )
    .await;
    let host = app.player("Alice").await;
    let guest = app.player("Bob").await;
    // Judged in this order (by game id): a game whose verdict turns out moot, a game with
    // only one upload (decided without re-simulation), and an ordinary game.
    let moot = app
        .started_event(Format::SingleElim, &[&host, &guest])
        .await;
    upload_pair(&host, &guest, moot, |s| simple_game(1, s)).await;
    let single = app
        .started_event(Format::SingleElim, &[&host, &guest])
        .await;
    let res = host
        .post_bytes(
            &format!("/v1/sets/{single}/0/games/1/replay"),
            simple_game(1, START_A),
            &[("x-ab-build", "b1")],
        )
        .await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    sqlx::query("UPDATE games SET first_upload_at = 0 WHERE event_id = ?")
        .bind(single)
        .execute(&app.state.db)
        .await
        .unwrap();
    let ordinary = app
        .started_event(Format::SingleElim, &[&host, &guest])
        .await;
    upload_pair(&host, &guest, ordinary, |s| simple_game(1, s)).await;

    let st = app.state.clone();
    let judging = tokio::spawn(async move { judge_due(&st).await });
    let verifier = Signer::new(vk, &app.url);

    // While the first game is re-simulated, it leaves `judging` (as an organiser
    // resolution would), so applying its verdict does nothing.
    let first = claim_job(&verifier).await;
    sqlx::query("UPDATE games SET state = 'void' WHERE event_id = ?")
        .bind(moot)
        .execute(&app.state.db)
        .await
        .unwrap();
    let result = VerifyResult {
        outcome: ResimOutcome::Match,
        build: "first-build".into(),
    };
    assert_eq!(
        verifier.post_result(first.id, &result).await,
        StatusCode::NO_CONTENT
    );

    // The lone upload, re-simulated as evidence only.
    let lone = claim_job(&verifier).await;
    assert_ne!(lone.id, first.id);
    let result = VerifyResult {
        outcome: ResimOutcome::Match,
        build: "lone-build".into(),
    };
    assert_eq!(
        verifier.post_result(lone.id, &result).await,
        StatusCode::NO_CONTENT
    );

    let second = claim_job(&verifier).await;
    assert_ne!(second.id, lone.id);
    let result = VerifyResult {
        outcome: ResimOutcome::Match,
        build: "second-build".into(),
    };
    assert_eq!(
        verifier.post_result(second.id, &result).await,
        StatusCode::NO_CONTENT
    );
    let judged = tokio::time::timeout(Duration::from_secs(10), judging)
        .await
        .expect("judging finishes")
        .unwrap()
        .unwrap();
    assert_eq!(judged, 3);

    assert_eq!(game(&app, moot).await, ("void".into(), None, None));
    assert_eq!(
        game(&app, single).await,
        ("review".into(), Some(Verdict::Single), None)
    );
    let evidence: String = sqlx::query_scalar("SELECT evidence_json FROM games WHERE event_id = ?")
        .bind(single)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    let evidence: serde_json::Value = serde_json::from_str(&evidence).unwrap();
    assert_eq!(evidence["resim"]["build"], "lone-build", "{evidence}");
    assert_eq!(evidence["resim"]["evidence_only"], true, "{evidence}");
    assert_eq!(
        game(&app, ordinary).await,
        (
            "counted".into(),
            Some(Verdict::Verified),
            Some("second-build".into())
        )
    );
}

#[tokio::test]
async fn claim_requires_verifier_key() {
    let vk = SigningKey::generate(&mut OsRng);
    let app = TestApp::spawn_with(
        external(&vk),
        Arc::new(ExternalResim::new(Duration::from_secs(30))),
    )
    .await;
    let player = app.player("Alice").await;
    let res = player.post_bytes("/v1/verify/claim", Vec::new(), &[]).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let res = player.get("/v1/verify/jobs/1/replay").await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let res = player
        .post_json(
            "/v1/verify/jobs/1/result",
            &VerifyResult {
                outcome: ResimOutcome::Match,
                build: "x".into(),
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // Organiser keys are accepted too.
    let res = app
        .as_to()
        .post_bytes("/v1/verify/claim", Vec::new(), &[])
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn no_jobs_is_204() {
    let vk = SigningKey::generate(&mut OsRng);
    let app = TestApp::spawn_with(
        external(&vk),
        Arc::new(ExternalResim::new(Duration::from_secs(30))),
    )
    .await;
    let res = Signer::new(vk, &app.url).claim().await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn stale_claim_is_reoffered() {
    let vk = SigningKey::generate(&mut OsRng);
    let app = TestApp::spawn_with(
        external(&vk),
        Arc::new(ExternalResim::new(Duration::from_secs(30))),
    )
    .await;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO resim_jobs (game_id, path, replay_last_frame, state)
         VALUES (0, '/nowhere.slp', 1234, 'queued') RETURNING id",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let verifier = Signer::new(vk, &app.url);

    let res = verifier.claim().await;
    assert_eq!(res.status(), StatusCode::OK);
    let job: VerifyJob = res.json().await.unwrap();
    assert_eq!(
        job,
        VerifyJob {
            id,
            replay_last_frame: 1234
        }
    );
    // A fresh claim is not offered again.
    assert_eq!(verifier.claim().await.status(), StatusCode::NO_CONTENT);

    // Nor one claimed under 300 s ago; one claimed more than 300 s ago is.
    sqlx::query("UPDATE resim_jobs SET claimed_at = ? WHERE id = ?")
        .bind(now_unix() - 299)
        .bind(id)
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(verifier.claim().await.status(), StatusCode::NO_CONTENT);
    sqlx::query("UPDATE resim_jobs SET claimed_at = ? WHERE id = ?")
        .bind(now_unix() - 301)
        .bind(id)
        .execute(&app.state.db)
        .await
        .unwrap();
    let res = verifier.claim().await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.json::<VerifyJob>().await.unwrap().id, id);
    let claimed_at: i64 = sqlx::query_scalar("SELECT claimed_at FROM resim_jobs WHERE id = ?")
        .bind(id)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert!(now_unix() - claimed_at < 5, "claim time refreshed");
}

#[tokio::test]
async fn external_check_without_verifier_is_unavailable() {
    let vk = SigningKey::generate(&mut OsRng);
    let resim = Arc::new(ExternalResim::new(Duration::from_millis(300)));
    let app = TestApp::spawn_with(external(&vk), resim.clone()).await;
    let started = Instant::now();
    let outcome = resim.check(Path::new("/nowhere.slp"), 100).await;
    assert_eq!(outcome, ResimOutcome::Unavailable("no verifier".into()));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(resim.build(), "external");
    // With no result there is no posted build either.
    assert_eq!(
        resim
            .check_with_build(Path::new("/nowhere.slp"), 100)
            .await
            .unwrap(),
        (
            ResimOutcome::Unavailable("no verifier".into()),
            "external".into()
        )
    );
    // The abandoned job is not handed out any more.
    assert_eq!(
        Signer::new(vk, &app.url).claim().await.status(),
        StatusCode::NO_CONTENT
    );
}

/// With no verifier seen recently, a check gives up after the short wait instead of the full
/// timeout, and its job is no longer offered.
#[tokio::test]
async fn external_check_gives_up_early_when_no_verifier_is_online() {
    let vk = SigningKey::generate(&mut OsRng);
    let resim = Arc::new(
        ExternalResim::new(Duration::from_secs(600))
            .with_presence(Duration::from_secs(60), Duration::from_millis(300)),
    );
    let app = TestApp::spawn_with(external(&vk), resim.clone()).await;
    let started = Instant::now();
    let checked = tokio::time::timeout(
        Duration::from_secs(10),
        resim.check_with_build(Path::new("/nowhere.slp"), 100),
    )
    .await
    .expect("gives up long before the 600 s timeout")
    .unwrap();
    assert_eq!(
        checked,
        (
            ResimOutcome::Unavailable("no verifier online".into()),
            "external".into()
        )
    );
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert_eq!(
        Signer::new(vk, &app.url).claim().await.status(),
        StatusCode::NO_CONTENT
    );
}

/// A verifier that polled recently is online: an unclaimed job is waited for past the short
/// wait (here until the full timeout).
#[tokio::test]
async fn external_check_waits_while_a_verifier_is_online() {
    let vk = SigningKey::generate(&mut OsRng);
    let resim = Arc::new(
        ExternalResim::new(Duration::from_millis(1500))
            .with_presence(Duration::from_secs(60), Duration::from_millis(200)),
    );
    let app = TestApp::spawn_with(external(&vk), resim.clone()).await;
    // An idle poll (nothing queued yet) counts as the verifier being there.
    assert_eq!(
        Signer::new(vk, &app.url).claim().await.status(),
        StatusCode::NO_CONTENT
    );
    let started = Instant::now();
    let (outcome, _) = resim
        .check_with_build(Path::new("/nowhere.slp"), 100)
        .await
        .unwrap();
    assert_eq!(outcome, ResimOutcome::Unavailable("no verifier".into()));
    assert!(started.elapsed() >= Duration::from_millis(1500));
}

/// A claimed job is waited for past the short wait, until its result arrives.
#[tokio::test]
async fn claimed_job_is_waited_for() {
    let vk = SigningKey::generate(&mut OsRng);
    let resim = Arc::new(
        ExternalResim::new(Duration::from_secs(30))
            .with_presence(Duration::from_millis(100), Duration::from_millis(200)),
    );
    let app = TestApp::spawn_with(external(&vk), resim.clone()).await;
    let checking = {
        let resim = resim.clone();
        tokio::spawn(async move { resim.check_with_build(Path::new("/nowhere.slp"), 100).await })
    };
    let verifier = Signer::new(vk, &app.url);
    let job = claim_job(&verifier).await;
    // Longer than both the short wait and the presence window.
    tokio::time::sleep(Duration::from_millis(600)).await;
    let result = VerifyResult {
        outcome: ResimOutcome::Match,
        build: "slow 1.0".into(),
    };
    assert_eq!(
        verifier.post_result(job.id, &result).await,
        StatusCode::NO_CONTENT
    );
    let checked = tokio::time::timeout(Duration::from_secs(10), checking)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(checked, (ResimOutcome::Match, "slow 1.0".into()));
}

/// An internal error (here: no store to queue the job in) is an error, not "unavailable", so
/// the worker retries the game instead of counting it unchecked.
#[tokio::test]
async fn external_internal_error_is_an_error() {
    let resim = ExternalResim::new(Duration::from_secs(30));
    assert!(resim
        .check_with_build(Path::new("/nowhere.slp"), 100)
        .await
        .is_err());
}

/// With `accepted_builds` set, a result posted by a verifier on another build is not used:
/// the game is judged as if no verifier had checked it.
#[tokio::test]
async fn verifier_build_must_be_accepted() {
    for strict in [false, true] {
        let vk = SigningKey::generate(&mut OsRng);
        let resim = Arc::new(ExternalResim::new(Duration::from_secs(30)));
        let app = TestApp::spawn_with(
            |c| {
                external(&vk)(c);
                // The players' build ("b1") and the verifier build the server expects.
                c.accepted_builds = vec!["b1".into(), "melee 1.2".into()];
                c.strict_resim = strict;
            },
            resim,
        )
        .await;
        let (host, guest, event) = two_player(&app).await;
        upload_pair(&host, &guest, event, |s| simple_game(1, s)).await;
        let st = app.state.clone();
        let judging = tokio::spawn(async move { judge_due(&st).await });
        let verifier = Signer::new(vk, &app.url);
        let job = claim_job(&verifier).await;
        let result = VerifyResult {
            outcome: ResimOutcome::Match,
            build: "melee 1.3+0123456789ab".into(),
        };
        assert_eq!(
            verifier.post_result(job.id, &result).await,
            StatusCode::NO_CONTENT
        );
        tokio::time::timeout(Duration::from_secs(10), judging)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let (state, verdict, build) = game(&app, event).await;
        let resim: String = sqlx::query_scalar("SELECT resim_json FROM games")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<ResimOutcome>(&resim).unwrap(),
            ResimOutcome::Unavailable("verifier build not accepted: melee 1.3+0123456789ab".into())
        );
        assert_eq!(build, None, "the engine's run is not used");
        if strict {
            assert_eq!(
                (state.as_str(), verdict),
                ("review", Some(Verdict::VerifierUnavailable))
            );
        } else {
            assert_eq!(
                (state.as_str(), verdict),
                ("counted", Some(Verdict::PeerOnly))
            );
        }
    }
}

/// Kills the child process when dropped, so a failing assertion cannot leave it running.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn fake_engine() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_slpcheck.sh")
}

/// An agreeing pair that port 1 wins, ending on frame 4000 (the fake engine's
/// `match` stops at 5000, so the re-simulation reaches the end).
fn short_game(start_at: &str) -> Vec<u8> {
    SlpBuilder::new()
        .player(1, 2, PlayerKind::Human)
        .player(2, 9, PlayerKind::Human)
        .frame(-123, 1, 4, 0.0)
        .frame(-123, 2, 4, 0.0)
        .frame(4000, 1, 2, 63.0)
        .frame(4000, 2, 0, 0.0)
        .end(2, None)
        .start_at(start_at)
        .build()
}

#[tokio::test]
async fn worker_binary_end_to_end() {
    let vk = SigningKey::generate(&mut OsRng);
    let app = TestApp::spawn_with(
        |c| {
            external(&vk)(c);
            c.worker_enabled = true;
        },
        Arc::new(ExternalResim::new(Duration::from_secs(60))),
    )
    .await;
    let key_file = app.dir.path().join("verifier.key");
    std::fs::write(&key_file, format!("{}\n", hex::encode(vk.to_bytes()))).unwrap();
    // The worker checks the disc image's size at start-up; a sparse file takes no disc space.
    let disc = app.dir.path().join("disc.iso");
    std::fs::File::create(&disc)
        .unwrap()
        .set_len(autobracket_server::resim::MIN_DISC_BYTES)
        .unwrap();
    let log_path = app.dir.path().join("verify-worker.log");
    let log = std::fs::File::create(&log_path).unwrap();

    let child = Command::new(env!("CARGO_BIN_EXE_autobracket"))
        .arg("verify-worker")
        .arg("--server")
        .arg(&app.url)
        .arg("--key-file")
        .arg(&key_file)
        .arg("--engine")
        .arg(fake_engine())
        .arg("--iso")
        .arg(&disc)
        .arg("--timeout-secs")
        .arg("10")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .expect("start verify-worker");
    let _guard = KillOnDrop(child);

    let (host, guest, event) = two_player(&app).await;
    upload_pair(&host, &guest, event, short_game).await;

    let deadline = Instant::now() + Duration::from_secs(30);
    let (state, verdict, build) = loop {
        let g = game(&app, event).await;
        if g.0 != "uploading" && g.0 != "judging" {
            break g;
        }
        assert!(
            Instant::now() < deadline,
            "game never judged: {g:?}; worker log:\n{}",
            std::fs::read_to_string(&log_path).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(state, "counted");
    assert_eq!(verdict, Some(Verdict::Verified));
    let build = build.unwrap();
    assert!(build.starts_with("fake 0.0+"), "verifier_build {build:?}");
}

#[tokio::test]
async fn orphaned_jobs_are_retired_at_startup() {
    let vk = SigningKey::generate(&mut OsRng);
    let app = TestApp::spawn_with(
        external(&vk),
        Arc::new(ExternalResim::new(Duration::from_secs(30))),
    )
    .await;
    // What a previous run left behind: a queued job, a claimed one, and a finished one.
    sqlx::query(
        "INSERT INTO resim_jobs (id, game_id, path, replay_last_frame, state, result_json, claimed_at)
         VALUES (1, 0, '/a.slp', 1, 'queued', NULL, NULL),
                (2, 0, '/b.slp', 1, 'claimed', NULL, ?),
                (3, 0, '/c.slp', 1, 'done', '{\"kept\":true}', NULL)",
    )
    .bind(now_unix())
    .execute(&app.state.db)
    .await
    .unwrap();

    // Start-up attaches the verifier to the store.
    let restarted = ExternalResim::new(Duration::from_secs(30));
    restarted.attach(&app.state.db).await.unwrap();

    let rows: Vec<(i64, String, Option<String>)> =
        sqlx::query_as("SELECT id, state, result_json FROM resim_jobs ORDER BY id")
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    assert_eq!(
        rows,
        vec![
            (1, "done".into(), None),
            (2, "done".into(), None),
            (3, "done".into(), Some("{\"kept\":true}".into())),
        ]
    );
    let verifier = Signer::new(vk, &app.url);
    assert_eq!(verifier.claim().await.status(), StatusCode::NO_CONTENT);
    let result = VerifyResult {
        outcome: ResimOutcome::Match,
        build: "late".into(),
    };
    assert_eq!(verifier.post_result(2, &result).await, StatusCode::CONFLICT);
}
