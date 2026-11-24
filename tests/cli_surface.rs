//! Black-box tests of the binary's surface. None reaches Embryo Options:
//! each renders help or info, is refused by argument checks that run before
//! a session is read, or finds no session stored. The config directory is a
//! temp dir, never the developer's, and `EMBRYOOPTIONS_PROFILE` is cleared.
//!
//! None reaches the real keychain or browser profile:
//! `EMBRYOOPTIONS_KEYCHAIN_SERVICE` names a service that holds nothing (a missing
//! item needs no approval), and under it the binary never touches the kept
//! profile. `XDG_DATA_HOME`, where Linux keeps that profile, is a temp dir
//! besides. No test stores a session, so no test writes the vault's key or
//! reads one: the vault reads its key only when an entry exists (cli-common
//! DESIGN.md §2.5).
//!
//! The temp config of [`embryooptions`] configures one profile, `adult1`.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

fn tmp() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "embryooptions-cli-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The empty keychain service every test reads instead of `piekstra.embryooptions`.
const HERMETIC_SERVICE: &str = "piekstra.embryooptions.hermetic-tests";

/// A config directory whose config configures `profiles`.
fn config_with(profiles: &[&str]) -> std::path::PathBuf {
    let base = tmp();
    std::fs::create_dir_all(base.join("embryooptions")).unwrap();
    let names: serde_json::Map<String, Value> = profiles
        .iter()
        .map(|p| (p.to_string(), serde_json::json!({})))
        .collect();
    std::fs::write(
        base.join("embryooptions").join("config.json"),
        serde_json::json!({ "profiles": names }).to_string(),
    )
    .unwrap();
    base
}

fn bare() -> Command {
    let mut cmd = Command::cargo_bin("embryooptions").unwrap();
    cmd.env("EMBRYOOPTIONS_KEYCHAIN_SERVICE", HERMETIC_SERVICE)
        .env("XDG_DATA_HOME", tmp())
        .env_remove("EMBRYOOPTIONS_CHROME")
        .env_remove("EMBRYOOPTIONS_PROFILE")
        .env_remove("EMBRYOOPTIONS_BASE_URL")
        .env_remove("NO_COLOR");
    cmd
}

/// A loopback origin no test listens on. Under the test keychain service a
/// command that would send a request needs `EMBRYOOPTIONS_BASE_URL`; with this
/// one, a test that finds no session still reaches its own refusal, and a
/// request that went out anyway would be refused by this machine.
const NO_SERVER: &str = "http://127.0.0.1:9";

fn embryooptions() -> Command {
    let mut cmd = bare();
    cmd.env("XDG_CONFIG_HOME", config_with(&["adult1"]))
        .env("EMBRYOOPTIONS_BASE_URL", NO_SERVER);
    cmd
}

fn json_of(out: &assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&out.get_output().stdout).expect("one JSON document on stdout")
}

#[test]
fn every_command_renders_help() {
    for args in [
        vec!["--help"],
        vec!["auth", "--help"],
        vec!["auth", "login", "--help"],
        vec!["auth", "status", "--help"],
        vec!["auth", "logout", "--help"],
        vec!["auth", "reset", "--help"],
        vec!["profiles", "--help"],
        vec!["profiles", "list", "--help"],
        vec!["profiles", "add", "--help"],
        vec!["profiles", "remove", "--help"],
        vec!["config", "--help"],
        vec!["config", "path", "--help"],
        vec!["config", "show", "--help"],
        vec!["api", "--help"],
        vec!["patients", "list", "--help"],
        vec!["accounts", "list", "--help"],
        vec!["bills", "list", "--help"],
        vec!["bills", "show", "--help"],
        vec!["payments", "list", "--help"],
        vec!["records", "export", "--help"],
        vec!["documents", "list", "--help"],
        vec!["documents", "download", "--help"],
        vec!["self-update", "--help"],
        vec!["completions", "--help"],
        vec!["info", "--help"],
    ] {
        embryooptions().args(&args).assert().success();
    }
}

#[test]
fn help_says_read_only_and_unofficial() {
    embryooptions()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Unofficial read-only"));
}

#[test]
fn info_declares_the_profile_and_login_profiles() {
    let out = bare().env("XDG_CONFIG_HOME", tmp()).arg("info").assert().success();
    let v = json_of(&out);
    assert_eq!(v["schema"], "cli-info/v1");
    assert_eq!(v["name"], "embryooptions");
    let caps = v["capabilities"].as_array().unwrap();
    assert!(caps.iter().any(|c| c == "login-profiles"), "{caps:?}");
    assert!(!caps.iter().any(|c| c == "records-files"), "no files source: {caps:?}");
    for c in [
        "patients",
        "accounts",
        "bills",
        "payments",
        "records",
        "documents",
        "api",
    ] {
        assert!(caps.iter().any(|x| x == c), "{c}: {caps:?}");
    }
    assert_eq!(v["profiles"], serde_json::json!(["health-records/v1", "documents/v1"]));
    assert_eq!(v["auth"]["required"], true);
}

#[test]
fn completions_and_version_run() {
    embryooptions().args(["completions", "zsh"]).assert().success();
    embryooptions()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn no_color_is_accepted() {
    embryooptions().env("NO_COLOR", "1").arg("info").assert().success();
}

/// A command that signs in needs a configured profile: none given is exit 2
/// listing the configured ones, an unknown one too, and a lone profile is
/// never picked.
#[test]
fn a_login_command_needs_its_profile() {
    for args in [
        vec!["auth", "status"],
        vec!["auth", "logout"],
        vec!["api", "GET", "/api/Members/{memberId}/MemberAccount/Member"],
        vec!["patients", "list"],
        vec!["accounts", "list"],
        vec!["bills", "list"],
        vec!["bills", "show", "501"],
        vec!["payments", "list"],
        vec!["records", "export", "-o", "out"],
        vec!["documents", "list"],
        vec!["documents", "download", "--all", "-o", "out"],
    ] {
        let out = embryooptions().args(&args).arg("--json").assert().code(2);
        let v = json_of(&out);
        assert_eq!(v["error"]["code"], "usage", "{args:?}");
        let message = v["error"]["message"].as_str().unwrap();
        assert!(message.contains("adult1"), "{args:?}: {message}");
    }
    let out = embryooptions()
        .args([
            "api",
            "GET",
            "/api/Members/{memberId}/MemberAccount/Member",
            "--profile",
            "nobody",
            "--json",
        ])
        .assert()
        .code(2);
    assert!(json_of(&out)["error"]["message"].as_str().unwrap().contains("adult1"));
    // With none configured, it names `profiles add`.
    let out = bare()
        .env("XDG_CONFIG_HOME", tmp())
        .args(["api", "GET", "/api/Members/{memberId}/MemberAccount/Member", "--json"])
        .assert()
        .code(2);
    assert!(json_of(&out)["error"].to_string().contains("profiles add"));
}

/// The variable stands in for the flag.
#[test]
fn the_profile_variable_is_read() {
    embryooptions()
        .env("EMBRYOOPTIONS_PROFILE", "adult1")
        .args(["auth", "status", "--json"])
        .assert()
        .success();
}

#[test]
fn a_command_without_a_login_takes_no_profile() {
    for args in [
        vec!["info"],
        vec!["profiles", "list"],
        vec!["config", "path"],
        vec!["auth", "reset", "--force"],
        vec!["completions", "bash"],
    ] {
        embryooptions()
            .args(&args)
            .args(["--profile", "adult1"])
            .assert()
            .code(2);
    }
    // The variable is not read by them.
    embryooptions()
        .env("EMBRYOOPTIONS_PROFILE", "adult1")
        .arg("info")
        .assert()
        .success();
}

/// A signed-out profile: status says so, and a read is exit 3 naming the
/// sign-in. Nothing is stored, so nothing reads a key.
#[test]
fn a_profile_without_a_session() {
    let out = embryooptions()
        .args(["auth", "status", "--profile", "adult1", "--json"])
        .assert()
        .success();
    let v = json_of(&out);
    assert_eq!(v["schema"], "auth-status/v1");
    assert_eq!(v["authenticated"], false);
    assert_eq!(v["method"], "browser-session");
    assert!(v.get("clinic").is_none(), "{v}");
    let out = embryooptions()
        .args([
            "api",
            "GET",
            "/api/Members/{memberId}/MemberAccount/Member",
            "--profile",
            "adult1",
            "--json",
        ])
        .assert()
        .code(3);
    let v = json_of(&out);
    assert_eq!(v["error"]["code"], "auth");
    assert!(
        v["error"]
            .to_string()
            .contains("auth login --browser --clinic <SLUG> --profile adult1"),
        "{v}"
    );
    let out = embryooptions()
        .args(["auth", "logout", "--profile", "adult1", "--json"])
        .assert()
        .success();
    assert_eq!(json_of(&out)["session_removed"], false);
}

/// `api` reads only, and only the member's own paths, refused before the
/// session is read.
#[test]
fn the_api_is_read_only() {
    for args in [
        vec!["api", "POST", "/api/Members/{memberId}/MemberAccount/Member"],
        vec!["api", "DELETE", "/api/Members/{memberId}/MemberAccount/Member"],
        vec!["api", "GET", "/api/Auth/Login"],
        vec!["api", "GET", "/api/Auth/RefreshToken"],
        vec!["api", "GET", "/api/Members/{memberId}/Payment/MemberPayment"],
        vec!["api", "GET", "/api/Members/{memberId}/MemberPreempt/UpdateDisposition"],
        vec!["api", "GET", "/api/Members/{memberId}/CardVaultingOrder"],
        vec!["api", "GET", "/api/Members/{memberId}/Consents"],
        vec![
            "api",
            "GET",
            "https://example.com/api/Members/{memberId}/MemberAccount/Member",
        ],
    ] {
        embryooptions()
            .args(&args)
            .args(["--profile", "adult1"])
            .assert()
            .code(2);
    }
}

#[test]
fn profiles_are_added_listed_and_removed() {
    let config = tmp();
    let run = |args: &[&str]| {
        let mut cmd = bare();
        cmd.env("XDG_CONFIG_HOME", &config).args(args);
        cmd
    };
    let out = run(&["profiles", "list", "--json"]).assert().success();
    assert_eq!(
        json_of(&out),
        serde_json::json!({ "schema": "profile-list/v1", "profiles": [] })
    );
    let out = run(&["profiles", "add", "adult2", "--json"]).assert().success();
    assert_eq!(json_of(&out)["added"], true);
    let out = run(&["profiles", "add", "adult2", "--json"]).assert().success();
    assert_eq!(json_of(&out)["added"], false);
    run(&["profiles", "add", "Bad Name"]).assert().code(2);
    let out = run(&["profiles", "list", "--json"]).assert().success();
    assert_eq!(json_of(&out)["profiles"][0]["name"], "adult2");
    // Removing asks; without a terminal it needs --force (exit 6).
    run(&["profiles", "remove", "adult2"]).assert().code(6);
    run(&["profiles", "remove", "nobody", "--force"]).assert().code(4);
    let out = run(&["profiles", "remove", "adult2", "--force", "--json"])
        .assert()
        .success();
    let v = json_of(&out);
    assert_eq!(v["schema"], "profile-remove/v1");
    assert_eq!(v["session_removed"], false);
    assert!(
        v.get("browser_profile_removed").is_none(),
        "left alone under the test service"
    );
    let out = run(&["profiles", "list", "--json"]).assert().success();
    assert!(json_of(&out)["profiles"].as_array().unwrap().is_empty());
}

/// The flags, and the clinic a first sign-in needs, are checked before
/// Chrome opens, stdin is read or anything is stored.
#[test]
fn auth_login_checks_its_flags_before_anything() {
    for args in [
        vec!["auth", "login"],
        vec!["auth", "login", "--browser", "--stdin"],
        vec!["auth", "login", "--browser", "--user-agent", "x"],
        vec!["auth", "login", "--browser", "--clinic", "www"],
        vec![
            "auth",
            "login",
            "--browser",
            "--clinic",
            "https://exampleclinic.example.com",
        ],
        vec!["auth", "login", "--stdin", "--clinic", "not a clinic"],
    ] {
        embryooptions()
            .args(&args)
            .args(["--profile", "adult1"])
            .assert()
            .code(2);
    }
    // No session is stored, so there is no clinic to reuse. `--browser`
    // shares the check, but under the test keychain service it is refused
    // first (`a_test_run_without_a_base_url_sends_nothing`).
    embryooptions()
        .args(["auth", "login", "--stdin", "--profile", "adult1"])
        .write_stdin("")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--clinic <SLUG>"));
}

#[test]
fn auth_reset_asks_first() {
    embryooptions().args(["auth", "reset"]).assert().code(6);
}

#[test]
fn config_shows_the_profiles() {
    let out = embryooptions().args(["config", "show", "--json"]).assert().success();
    let v = json_of(&out);
    assert_eq!(v["schema"], "config/v1");
    assert!(v["profiles"].get("adult1").is_some(), "{v}");
    embryooptions()
        .args(["config", "path"])
        .assert()
        .success()
        .stdout(predicate::str::ends_with("config.json\n"));
}

/// `EMBRYOOPTIONS_BASE_URL` points reads at a test's loopback server only, and
/// only with the test keychain service, refused before any vault is opened.
#[test]
fn the_base_url_override_is_for_loopback_tests_only() {
    for url in [
        "https://member-api.prod.embryooptions.com.example.com",
        "http://example.com",
        "http://127.0.0.1:1@example.com",
    ] {
        embryooptions()
            .env("EMBRYOOPTIONS_BASE_URL", url)
            .args([
                "api",
                "GET",
                "/api/Members/{memberId}/MemberAccount/Member",
                "--profile",
                "adult1",
            ])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("EMBRYOOPTIONS_BASE_URL"));
    }
    embryooptions()
        .env_remove("EMBRYOOPTIONS_KEYCHAIN_SERVICE")
        .env("EMBRYOOPTIONS_BASE_URL", "http://127.0.0.1:9")
        .args([
            "api",
            "GET",
            "/api/Members/{memberId}/MemberAccount/Member",
            "--profile",
            "adult1",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("EMBRYOOPTIONS_KEYCHAIN_SERVICE"));
    // The real service name is not a test's, wherever the vault is.
    embryooptions()
        .env("EMBRYOOPTIONS_KEYCHAIN_SERVICE", "piekstra.embryooptions")
        .env("EMBRYOOPTIONS_BASE_URL", "http://127.0.0.1:9")
        .args([
            "api",
            "GET",
            "/api/Members/{memberId}/MemberAccount/Member",
            "--profile",
            "adult1",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("piekstra.embryooptions.<suffix>"));
    embryooptions()
        .env("EMBRYOOPTIONS_BASE_URL", "http://127.0.0.1:9")
        .args(["auth", "login", "--browser", "--profile", "adult1"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("real site"));
    // A loopback origin under the test service passes; the signed-out
    // profile then stops it at the session, before a request.
    embryooptions()
        .env("EMBRYOOPTIONS_BASE_URL", "http://127.0.0.1:9")
        .args([
            "api",
            "GET",
            "/api/Members/{memberId}/MemberAccount/Member",
            "--profile",
            "adult1",
        ])
        .assert()
        .code(3);
}

/// Each reader of a signed-out profile is exit 3 naming the sign-in, before
/// any request: nothing is stored, so nothing reads a key. The export
/// writes nothing to stdout, where the stream would go.
#[test]
fn the_readers_of_a_signed_out_profile_are_exit_3() {
    for args in [
        vec!["patients", "list"],
        vec!["accounts", "list"],
        vec!["bills", "list", "--open"],
        vec!["bills", "show", "501"],
        vec!["payments", "list", "--since", "2025-01-01"],
    ] {
        let out = embryooptions()
            .args(&args)
            .args(["--profile", "adult1", "--json"])
            .assert()
            .code(3);
        let v = json_of(&out);
        assert_eq!(v["error"]["code"], "auth", "{args:?}");
        assert!(v["error"].to_string().contains("--profile adult1"), "{args:?}: {v}");
    }
    let out = embryooptions()
        .args(["records", "export", "--ndjson", "--profile", "adult1"])
        .assert()
        .code(3);
    assert!(out.get_output().stdout.is_empty());
}

/// The readers' own checks come before the profile and the vault.
#[test]
fn the_readers_check_their_arguments_first() {
    for args in [
        vec!["bills", "list", "--since", "2026-13-01"],
        vec!["bills", "show", " "],
        vec!["payments", "list", "--until", "yesterday"],
        vec!["records", "export"],
        vec!["records", "export", "--ndjson", "--json"],
        vec!["records", "export", "--ndjson", "--since", "June"],
        vec!["documents", "download", "--all", "-o", "-"],
    ] {
        embryooptions()
            .args(&args)
            .args(["--profile", "adult1"])
            .assert()
            .code(2);
    }
}

/// Embryo Options serves no bill or receipt files: the list is empty, an id
/// is exit 4, and --all saves none. None sends a request or reads a session.
#[test]
fn documents_are_none() {
    let out = embryooptions()
        .args(["documents", "list", "--profile", "adult1", "--json"])
        .assert()
        .success();
    let v = json_of(&out);
    assert_eq!(v["schema"], "document-list/v1");
    assert_eq!(v["items"], serde_json::json!([]));
    embryooptions()
        .args(["documents", "list", "--profile", "adult1"])
        .assert()
        .success()
        .stderr(predicate::str::contains("serves no statement or receipt files"));
    let out = embryooptions()
        .args(["documents", "download", "statement-1", "--profile", "adult1", "--json"])
        .assert()
        .code(4);
    assert_eq!(json_of(&out)["error"]["code"], "not_found");
    let dir = tmp().join("docs");
    let out = embryooptions()
        .args(["documents", "download", "--all", "-o"])
        .arg(&dir)
        .args(["--profile", "adult1", "--json"])
        .assert()
        .success();
    assert_eq!(json_of(&out)["count"], 0);
}

/// Under the test keychain service a command that would send a request
/// needs `EMBRYOOPTIONS_BASE_URL`: without it, it is refused before any
/// request, so a test that forgets its server never reaches the real site.
/// `EMBRYOOPTIONS_CHROME` names no browser, so `auth login --browser` could not
/// open one even if the refusal came too late.
#[test]
fn a_test_run_without_a_base_url_sends_nothing() {
    let cases: &[&[&str]] = &[
        &[
            "api",
            "GET",
            "/api/Members/{memberId}/MemberAccount/Member",
            "--profile",
            "adult1",
        ],
        &["accounts", "list", "--profile", "adult1"],
        &["auth", "login", "--stdin", "--profile", "adult1"],
        &["auth", "login", "--browser", "--profile", "adult1"],
    ];
    for &args in cases {
        embryooptions()
            .env_remove("EMBRYOOPTIONS_BASE_URL")
            .env("EMBRYOOPTIONS_CHROME", "/nonexistent/chrome")
            .args(args)
            .write_stdin("a=b")
            .assert()
            .code(2)
            .stderr(predicate::str::contains("EMBRYOOPTIONS_BASE_URL"))
            .stderr(predicate::str::contains("EMBRYOOPTIONS_KEYCHAIN_SERVICE is set"));
    }
}
