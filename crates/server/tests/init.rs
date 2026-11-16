//! `autobracket init`: the one-command setup, driven through the built binary.

use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use autobracket_server::config::VerifierMode;
use autobracket_server::verify_worker::read_key_file;
use autobracket_server::Config;

const BIN: &str = env!("CARGO_BIN_EXE_autobracket");

/// Runs `autobracket init --dir DIR ARGS...`.
fn init(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .arg("init")
        .arg("--dir")
        .arg(dir)
        .args(args)
        .output()
        .expect("run autobracket init")
}

fn ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// The file contents of `organiser.key` and `autobracket.toml` in `dir`.
fn files(dir: &Path) -> (Vec<u8>, Vec<u8>) {
    (
        std::fs::read(dir.join("organiser.key")).unwrap(),
        std::fs::read(dir.join("autobracket.toml")).unwrap(),
    )
}

/// `KEY = ...` or `# KEY = ...`, with KEY lowercase letters and underscores.
fn key_line(line: &str) -> Option<&str> {
    let line = line.strip_prefix("# ").unwrap_or(line);
    let (key, _) = line.split_once(" = ")?;
    (!key.is_empty() && key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')).then_some(key)
}

#[test]
fn fresh_dir_gets_a_key_a_loadable_config_and_a_replay_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("ab");
    let stdout = ok(&init(&dir, &[]));
    let dir = dir.canonicalize().unwrap();

    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join("organiser.key")), 0o600);
    assert_eq!(mode(&dir.join("autobracket.toml")), 0o600);
    let replays = dir.join("replays");
    assert!(replays.is_dir());
    assert_eq!(std::fs::read_dir(&replays).unwrap().count(), 0);

    let cfg = Config::load(&dir.join("autobracket.toml")).unwrap();
    let sk = read_key_file(&dir.join("organiser.key")).unwrap();
    let public = hex::encode(sk.verifying_key().to_bytes());
    assert_eq!(cfg.to_keys, vec![public.clone()]);
    assert!(!cfg.allow_no_organiser);
    assert_eq!(cfg.bind, "127.0.0.1:8080");
    assert_eq!(cfg.db_path, dir.join("autobracket.sqlite"));
    assert_eq!(cfg.replay_dir, replays);
    assert_eq!(cfg.verifier.mode, VerifierMode::Off);
    assert!(cfg.trusted_proxies.is_empty());

    // The public key is printed; the secret never is.
    assert!(stdout.contains(&public), "{stdout}");
    let secret = hex::encode(sk.to_bytes());
    assert!(!stdout.contains(&secret), "the secret key was printed");

    // Next steps name the real paths and point at the README sections.
    let config = dir.join("autobracket.toml");
    assert!(
        stdout.contains(&format!("autobracket serve --config {}", config.display())),
        "{stdout}"
    );
    assert!(stdout.contains("event create"), "{stdout}");
    assert!(stdout.contains("http://127.0.0.1:8080/"), "{stdout}");
    for section in ["TLS and reverse proxies", "Verifier modes", "organiser.key"] {
        assert!(stdout.contains(section), "{section}: {stdout}");
    }
}

#[test]
fn every_key_is_documented_and_the_commented_defaults_are_the_real_ones() {
    let tmp = tempfile::tempdir().unwrap();
    ok(&init(tmp.path(), &[]));
    let path = tmp.path().join("autobracket.toml");
    let text = std::fs::read_to_string(&path).unwrap();

    let lines: Vec<&str> = text.lines().collect();
    let mut keys = 0;
    for (i, line) in lines.iter().enumerate() {
        if key_line(line).is_none() {
            continue;
        }
        keys += 1;
        let above = i.checked_sub(1).map(|j| lines[j]).unwrap_or("");
        assert!(
            above.starts_with("# ") && key_line(above).is_none(),
            "key line {line:?} has no comment of its own above it"
        );
    }
    assert!(keys >= 25, "only {keys} keys written:\n{text}");
    assert!(!text.contains("allow_no_organiser = true"), "{text}");

    // Uncommenting every default changes nothing: the defaults shown are the server's.
    let uncommented: String = lines
        .iter()
        .map(|line| match line.strip_prefix("# ") {
            Some(rest) if key_line(line).is_some() => format!("{rest}\n"),
            _ => format!("{line}\n"),
        })
        .collect();
    let full = tmp.path().join("full.toml");
    std::fs::write(&full, uncommented).unwrap();
    let a = Config::load(&path).unwrap();
    let b = Config::load(&full).unwrap();
    assert_eq!(format!("{a:?}"), format!("{b:?}"));
}

#[test]
fn second_run_without_force_fails_and_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    ok(&init(tmp.path(), &[]));
    let before = files(tmp.path());

    let out = init(tmp.path(), &["--bind", "127.0.0.1:9999"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--force"), "{err}");
    assert_eq!(files(tmp.path()), before);

    // Either file alone is enough to refuse.
    std::fs::remove_file(tmp.path().join("autobracket.toml")).unwrap();
    assert!(!init(tmp.path(), &[]).status.success());
    assert!(!tmp.path().join("autobracket.toml").exists());
}

/// A `--force` run that cannot move the config into place changes nothing: the old key and
/// the old config, which names it, stay together.
#[test]
fn force_that_cannot_place_the_config_leaves_the_old_pair() {
    let tmp = tempfile::tempdir().unwrap();
    ok(&init(tmp.path(), &[]));
    let key = std::fs::read(tmp.path().join("organiser.key")).unwrap();
    // A non-empty directory where the config goes: renaming a file over it fails.
    let config = tmp.path().join("autobracket.toml");
    std::fs::remove_file(&config).unwrap();
    std::fs::create_dir_all(config.join("blocker")).unwrap();

    let out = init(tmp.path(), &["--force"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("nothing was changed"), "{err}");
    assert_eq!(
        std::fs::read(tmp.path().join("organiser.key")).unwrap(),
        key
    );
    assert!(!tmp.path().join(".organiser.key.new").exists());
    assert!(!tmp.path().join(".autobracket.toml.new").exists());
}

/// When the config is in place but the key cannot follow, the message says where the new
/// key is, and the config names that key.
#[test]
fn force_that_cannot_place_the_key_says_where_the_new_key_is() {
    let tmp = tempfile::tempdir().unwrap();
    ok(&init(tmp.path(), &[]));
    let key = tmp.path().join("organiser.key");
    std::fs::remove_file(&key).unwrap();
    std::fs::create_dir_all(key.join("blocker")).unwrap();

    let out = init(tmp.path(), &["--force"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains(".organiser.key.new"), "{err}");
    let cfg = Config::load(&tmp.path().join("autobracket.toml")).unwrap();
    let new = read_key_file(&tmp.path().join(".organiser.key.new")).unwrap();
    assert_eq!(
        cfg.to_keys,
        vec![hex::encode(new.verifying_key().to_bytes())]
    );
}

#[test]
fn force_replaces_both_files() {
    let tmp = tempfile::tempdir().unwrap();
    ok(&init(tmp.path(), &[]));
    let (key, config) = files(tmp.path());

    ok(&init(
        tmp.path(),
        &[
            "--force",
            "--bind",
            "127.0.0.1:9999",
            "--proxy-cidr",
            "127.0.0.1/32",
        ],
    ));
    let (new_key, new_config) = files(tmp.path());
    assert_ne!(key, new_key);
    assert_ne!(config, new_config);
    assert_eq!(mode(&tmp.path().join("organiser.key")), 0o600);
    assert_eq!(mode(&tmp.path().join("autobracket.toml")), 0o600);
    let cfg = Config::load(&tmp.path().join("autobracket.toml")).unwrap();
    let sk = read_key_file(&tmp.path().join("organiser.key")).unwrap();
    assert_eq!(
        cfg.to_keys,
        vec![hex::encode(sk.verifying_key().to_bytes())]
    );
    assert_eq!(cfg.bind, "127.0.0.1:9999");
    assert_eq!(cfg.trusted_proxies, vec!["127.0.0.1/32".to_string()]);
    // No temporary files are left behind.
    let mut names: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["autobracket.toml", "organiser.key", "replays"]);
}

#[test]
fn proxy_cidrs_are_written_in_order() {
    let tmp = tempfile::tempdir().unwrap();
    ok(&init(
        tmp.path(),
        &["--proxy-cidr", "172.16.0.0/12", "--proxy-cidr", "::1/128"],
    ));
    let cfg = Config::load(&tmp.path().join("autobracket.toml")).unwrap();
    assert_eq!(cfg.trusted_proxies, vec!["172.16.0.0/12", "::1/128"]);
}

#[test]
fn bad_input_is_refused_before_anything_is_written() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("never");
    for (args, needle) in [
        (
            &[
                "--proxy-cidr",
                "10.0.0.0/8",
                "--proxy-cidr",
                "172.16.0.0/33",
            ][..],
            "172.16.0.0/33",
        ),
        (&["--proxy-cidr", "nonsense"][..], "trusted_proxies"),
        (
            &["--public-url", "ftp://bracket.example.org"][..],
            "--public-url",
        ),
        (&["--public-url", "bracket.example.org"][..], "--public-url"),
    ] {
        let out = init(&dir, args);
        assert!(!out.status.success(), "{args:?} accepted");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(needle), "{args:?}: {err}");
        assert!(!dir.exists(), "{args:?} created the directory");
    }
}

#[test]
fn public_url_is_used_in_the_printed_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let stdout = ok(&init(
        tmp.path(),
        &["--public-url", "https://bracket.example.org/"],
    ));
    let admin = stdout.lines().find(|l| l.contains("event create")).unwrap();
    assert!(
        admin.contains("--server https://bracket.example.org "),
        "{admin}"
    );
    assert!(stdout.contains("https://bracket.example.org/"), "{stdout}");
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A TCP port on 127.0.0.1 that was free a moment ago.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Runs one printed command line through `sh`, with this build's `autobracket` first on PATH.
fn shell(line: &str) -> Command {
    let bin_dir = PathBuf::from(BIN).parent().unwrap().to_path_buf();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![bin_dir];
    dirs.extend(std::env::split_paths(&path));
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(line)
        .env("PATH", std::env::join_paths(dirs).unwrap());
    cmd
}

#[test]
fn printed_commands_start_the_server_and_create_an_event() {
    let tmp = tempfile::tempdir().unwrap();
    let port = free_port();
    let bind = format!("127.0.0.1:{port}");
    let stdout = ok(&init(tmp.path(), &["--bind", &bind]));
    let printed = |needle: &str| {
        stdout
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("autobracket ") && l.contains(needle))
            .unwrap_or_else(|| panic!("no `{needle}` command in:\n{stdout}"))
            .to_string()
    };

    let mut server = shell(&format!("exec {}", printed(" serve ")))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let ready = BufReader::new(server.stdout.take().unwrap());
    let _server = KillOnDrop(server);
    // The readiness line; the lines end without it if the server exits.
    let listening = ready
        .lines()
        .any(|l| l.is_ok_and(|l| l.starts_with("listening on")));
    assert!(listening, "the server did not start");

    let out = shell(&printed("event create")).output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let created: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(created["id"], 1, "{created}");

    let page = format!("http://{bind}/");
    assert!(stdout.contains(&page), "{stdout}");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let events: serde_json::Value = rt.block_on(async {
        let page = reqwest::get(&page).await.unwrap();
        assert!(page.status().is_success());
        reqwest::get(format!("http://{bind}/v1/events"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    });
    assert_eq!(events[0]["id"], 1, "{events}");
}
