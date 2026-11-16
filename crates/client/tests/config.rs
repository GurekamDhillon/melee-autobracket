//! `init`, the config file and the engine build.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use autobracket_client::config::{self, ClientConfig, InitOptions, Preset, MIN_DISC_BYTES};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn fake_melee() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_melee.sh")
}

/// A sparse file of exactly the smallest disc size accepted.
fn disc(dir: &Path) -> PathBuf {
    let path = dir.join("melee.iso");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(MIN_DISC_BYTES)
        .unwrap();
    path
}

fn opts<'a>(iso: &'a Path, work: &'a Path) -> InitOptions<'a> {
    // Leaked so the borrowed melee path lives as long as the options.
    let melee: &'static Path = Box::leak(fake_melee().into_boxed_path());
    InitOptions {
        melee,
        iso,
        work_dir: work,
        force: false,
        headless: false,
        preset: None,
    }
}

fn count(args: &[String], flag: &str) -> usize {
    args.iter().filter(|a| *a == flag).count()
}

#[test]
fn init_writes_0600_config() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("autobracket/client.toml");
    let iso = disc(dir.path());
    let work = dir.path().join("runs");

    let cfg = config::init(&path, &opts(&iso, &work)).unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(cfg.host_args, config::default_host_args());
    assert_eq!(cfg.join_args, config::default_join_args());
    assert_eq!(cfg.code_regex, config::DEFAULT_CODE_REGEX);
    assert_eq!(cfg.no_code_wait_secs, 600);
    assert!(!cfg.allow_direct_connect);
    assert_eq!(ClientConfig::load(&path).unwrap(), cfg);
    cfg.signing_key().unwrap();

    let before = std::fs::read(&path).unwrap();
    assert!(config::init(&path, &opts(&iso, &work)).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before, "not overwritten");

    let forced = config::init(
        &path,
        &InitOptions {
            force: true,
            ..opts(&iso, &work)
        },
    )
    .unwrap();
    assert_ne!(forced.secret_key_hex, cfg.secret_key_hex);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn init_refuses_a_missing_or_small_disc_image() {
    let dir = TempDir::new().unwrap();
    let work = dir.path().join("runs");
    let small = dir.path().join("small.iso");
    std::fs::write(&small, b"not a disc").unwrap();

    for (iso, needle) in [
        (dir.path().join("missing.iso"), "disc image"),
        (small.clone(), "too small"),
        (dir.path().to_path_buf(), "not a regular file"),
    ] {
        let path = dir.path().join("client.toml");
        let err = config::init(&path, &opts(&iso, &work)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains(needle), "{msg}");
        assert!(msg.contains(&iso.display().to_string()), "{msg}");
        assert!(!path.exists(), "a config was written for {}", iso.display());
    }
}

#[test]
fn the_default_templates_verify_the_disc() {
    for args in [config::default_host_args(), config::default_join_args()] {
        assert_eq!(count(&args, "--verify-disc"), 1, "{args:?}");
        assert_eq!(count(&args, "--np-css"), 1, "{args:?}");
        // Optional flags of newer engine builds are not in the defaults.
        assert_eq!(count(&args, "--np-join-wait"), 0, "{args:?}");
        assert_eq!(count(&args, "--np-no-name-entry"), 0, "{args:?}");
    }
}

#[test]
fn init_headless_adds_the_display_less_flags_once() {
    let dir = TempDir::new().unwrap();
    let iso = disc(dir.path());
    let work = dir.path().join("runs");

    let path = dir.path().join("headless.toml");
    let cfg = config::init(
        &path,
        &InitOptions {
            headless: true,
            ..opts(&iso, &work)
        },
    )
    .unwrap();
    for args in [&cfg.host_args, &cfg.join_args] {
        assert_eq!(count(args, "--headless"), 1, "{args:?}");
        assert_eq!(count(args, "--no-audio"), 1, "{args:?}");
    }
    assert_eq!(ClientConfig::load(&path).unwrap(), cfg);
}

#[test]
fn init_match_writes_preset_templates() {
    let dir = TempDir::new().unwrap();
    let iso = disc(dir.path());
    let work = dir.path().join("runs");
    let preset = Preset::parse("falcon,32").unwrap();

    let path = dir.path().join("client.toml");
    let cfg = config::init(
        &path,
        &InitOptions {
            preset: Some(&preset),
            ..opts(&iso, &work)
        },
    )
    .unwrap();

    assert!(!cfg.uses_character_select());
    for (args, value) in [(&cfg.host_args, "falcon,32"), (&cfg.join_args, "falcon")] {
        assert_eq!(count(args, "--np-css"), 0, "{args:?}");
        assert_eq!(count(args, "--quit-after-matches"), 0, "{args:?}");
        let i = args.iter().position(|a| a == "--match").unwrap();
        assert_eq!(args[i + 1], value, "{args:?}");
        assert_eq!(count(args, "--verify-disc"), 1, "{args:?}");
    }
    assert!(ClientConfig::load(&path)
        .unwrap()
        .host_args
        .contains(&"--match".to_string()));

    let path = dir.path().join("plain.toml");
    assert!(config::init(&path, &opts(&iso, &work))
        .unwrap()
        .uses_character_select());
}

#[test]
fn match_presets_are_checked() {
    assert_eq!(
        Preset::parse("fox").unwrap(),
        Preset {
            fighter: "fox".into(),
            stage: None
        }
    );
    assert_eq!(
        Preset::parse("marth, battlefield")
            .unwrap()
            .stage
            .as_deref(),
        Some("battlefield")
    );
    for bad in [
        "",
        ",32",
        "fox,",
        "fox,32,8",
        "--np-css",
        "fox,--verify-disc",
        "fo x",
        "fox,-1;rm",
    ] {
        let parsed = Preset::parse(bad);
        assert!(parsed.is_err(), "{bad:?} parsed as {parsed:?}");
    }
}

#[test]
fn the_written_config_explains_the_alternatives() {
    let dir = TempDir::new().unwrap();
    let iso = disc(dir.path());
    let path = dir.path().join("client.toml");
    config::init(&path, &opts(&iso, &dir.path().join("runs"))).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let comments: String = text
        .lines()
        .filter(|l| l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    for needle in [
        "init --headless",
        "init --match",
        "--np-css",
        "port/np-nameentry",
        "--np-join-wait",
        "--np-no-name-entry",
        "--verify-disc",
        "no_code_wait_secs",
        "allow_direct_connect",
        "relative",
    ] {
        assert!(
            comments.contains(needle),
            "{needle:?} missing from:\n{comments}"
        );
    }
}

#[test]
fn init_makes_relative_paths_absolute() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("client.toml");
    disc(dir.path());
    // The disc, relative to the test's working directory.
    let cwd = std::env::current_dir().unwrap();
    let up: PathBuf = cwd.components().skip(1).map(|_| "..").collect();
    let iso = up
        .join(dir.path().strip_prefix("/").unwrap())
        .join("melee.iso");
    assert!(iso.is_relative());

    let work = up.join(dir.path().strip_prefix("/").unwrap()).join("runs");

    let cfg = config::init(&path, &opts(&iso, &work)).unwrap();

    assert_eq!(cfg.iso, cwd.join(&iso));
    assert_eq!(
        cfg.work_dir,
        dir.path().canonicalize().unwrap().join("runs")
    );
    assert_eq!(ClientConfig::load(&path).unwrap(), cfg);
}

#[test]
fn init_creates_the_work_dir_and_stores_its_resolved_path() {
    let dir = TempDir::new().unwrap();
    let iso = disc(dir.path());
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, dir.path().join("link")).unwrap();
    // Through a symlink and `..`, and not there yet.
    let work = dir.path().join("link/../link/runs");

    let path = dir.path().join("client.toml");
    let cfg = config::init(&path, &opts(&iso, &work)).unwrap();

    let resolved = dir.path().canonicalize().unwrap().join("real/runs");
    assert!(resolved.is_dir(), "the work directory was not created");
    assert_eq!(cfg.work_dir, resolved);
    assert_eq!(ClientConfig::load(&path).unwrap().work_dir, resolved);
}

#[test]
fn an_old_config_with_engine_build_still_loads() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("client.toml");
    std::fs::write(
        &path,
        format!(
            "server = \"\"\nsecret_key_hex = \"{}\"\nengine_build = \"old\"\nmelee = \"/m\"\n\
             iso = \"/i\"\nwork_dir = \"/w\"\nhost_args = []\njoin_args = []\ncode_regex = \"x\"\n",
            "00".repeat(32)
        ),
    )
    .unwrap();
    let cfg = ClientConfig::load(&path).unwrap();
    assert_eq!(cfg.no_code_wait_secs, 600);
    assert!(!cfg.allow_direct_connect);
}

#[test]
fn the_engine_build_is_the_version_line_and_a_hash_of_the_binary() {
    let digest = Sha256::digest(std::fs::read(fake_melee()).unwrap());
    assert_eq!(
        config::engine_build(&fake_melee()).unwrap(),
        format!(
            "melee (fake) 0.0.1 test build+{}",
            hex::encode(&digest[..6])
        )
    );
}

fn help(sub: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_autobracket-client"))
        .args([sub, "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn help_says_what_the_installed_engine_does() {
    let init = help("init");
    for needle in ["--headless", "--no-audio", "--match", "port/np-nameentry"] {
        assert!(init.contains(needle), "{needle:?} missing from:\n{init}");
    }
    let play = help("play");
    for needle in [
        "no connection after 60 s",
        "no opponent joined",
        "--verify-disc",
        "takes over",
        "client-failure",
    ] {
        assert!(play.contains(needle), "{needle:?} missing from:\n{play}");
    }
    assert!(!play.contains("exits 2"), "stale claim in:\n{play}");
    let no_show = help("no-show");
    assert!(no_show.contains("--note"), "{no_show}");
}

#[test]
fn init_on_the_command_line_warns_about_character_select() {
    let dir = TempDir::new().unwrap();
    let iso = disc(dir.path());
    let run = |extra: &[&str], name: &str| {
        Command::new(env!("CARGO_BIN_EXE_autobracket-client"))
            .arg("--config")
            .arg(dir.path().join(name))
            .arg("init")
            .arg("--melee")
            .arg(fake_melee())
            .arg("--iso")
            .arg(&iso)
            .arg("--work-dir")
            .arg(dir.path().join("runs"))
            .args(extra)
            .output()
            .unwrap()
    };
    let out = run(&[], "a.toml");
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--match"), "{stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Engine build: melee (fake) 0.0.1 test build+"),
        "{stdout}"
    );

    let out = run(&["--match", "fox"], "b.toml");
    assert!(out.status.success());
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("warning"),
        "no warning with a preset"
    );
}
