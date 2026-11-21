//! The reads of Embryo Options's member API, in-process, against a loopback
//! server, the session in an in-memory vault: the requests' paths and
//! headers, the member's id filled in from the token, and the refusals.
//! Nothing reads the real keychain or reaches the network. Every token is
//! synthetic, built here.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use embryooptions::client::api_path;
use embryooptions::portal::{Env, Site};
use embryooptions::session::{Session, SessionStore};
use embryooptions::site::Clinic;
use embryooptions::token::{test_token, Tokens};
use pk_cli_auth::vault::{MemoryKeys, Vault};
use pk_cli_config::profile::ProfileName;
use pk_cli_secrets::Secret;
use serde_json::json;

/// One request the server saw.
#[derive(Debug, Clone)]
struct Seen {
    target: String,
    authorization: Option<String>,
    user_agent: Option<String>,
    cookie: Option<String>,
}

/// A loopback server answering each `GET` by its path and query from
/// `routes` (status, content type, body); anything else is a 404.
struct Server {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

fn serve(routes: BTreeMap<String, (u16, &'static str, String)>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match stream.read(&mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => break,
                }
            }
            let head = String::from_utf8_lossy(&head).to_string();
            let target = head.split_whitespace().nth(1).unwrap_or("").to_string();
            let header = |name: &str| {
                head.lines().find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
                })
            };
            log.lock().unwrap().push(Seen {
                target: target.clone(),
                authorization: header("authorization"),
                user_agent: header("user-agent"),
                cookie: header("cookie"),
            });
            let (status, mime, body) = routes.get(&target).cloned().unwrap_or((
                404,
                "application/json",
                r#"{"message":"not found"}"#.to_string(),
            ));
            let mut answer = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            if status == 302 {
                answer.push_str("Location: /\r\n");
            }
            answer.push_str("\r\n");
            answer.push_str(&body);
            let _ = stream.write_all(answer.as_bytes());
        }
    });
    Server { base, seen }
}

/// The member's account and billing history, synthetic: only the shape of
/// an answer, not the API's fields (which are not yet observed).
const ACCOUNT: &str = r#"{"memberAccount":{"unpaidBalance":12.5}}"#;
const HISTORY: &str = r#"[{"date":"2026-01-02","amount":250.0}]"#;

fn routes() -> BTreeMap<String, (u16, &'static str, String)> {
    let mut r = BTreeMap::new();
    r.insert(
        "/api/Members/4242/MemberAccount/Member?ivfCenterId=7".into(),
        (200, "application/json; charset=utf-8", ACCOUNT.to_string()),
    );
    r.insert(
        "/api/Members/4242/MemberAccount/MemberPaymentHistoryAll".into(),
        (200, "application/json; charset=utf-8", HISTORY.to_string()),
    );
    r.insert(
        "/api/Members/4242/Member/UpComingBills".into(),
        (401, "application/json", String::new()),
    );
    r.insert(
        "/api/Members/4242/FreezeEvents/FreezeEvents".into(),
        (302, "text/html", String::new()),
    );
    r.insert(
        "/api/Members/4242/MemberIvfCenter".into(),
        (503, "text/html", "<p>down</p>".to_string()),
    );
    r
}

fn adult1() -> ProfileName {
    ProfileName::parse("adult1").unwrap()
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "embryooptions-e2e-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A token for member 4242 at clinic id 7, expiring at `exp`.
fn token(exp: i64) -> String {
    test_token(&json!({
        "user": json!({ "UserId": 4242, "IVFCenterId": 7, "MemberType": 1 }).to_string(),
        "exp": exp,
        "jti": "dummy-jti",
    }))
}

const LATER: i64 = 4_000_000_000;

/// An environment reading `server`, with adult1's session (`access`)
/// stored.
fn env_with(server: &Server, tag: &str, access: String) -> Env {
    let store = SessionStore::with_vault(Vault::new(MemoryKeys::new(), temp_dir(tag)));
    store
        .put(
            &adult1(),
            &Session::new(
                Clinic::parse("exampleclinic").unwrap(),
                Tokens {
                    access: Secret::new(access),
                    refresh: Some(Secret::new("dummy-refresh")),
                },
                Some("Mozilla/5.0 (test)".into()),
            ),
        )
        .unwrap();
    Env {
        sessions: store,
        base: server.base.clone(),
        pace: Duration::ZERO,
    }
}

fn env(server: &Server, tag: &str) -> Env {
    env_with(server, tag, token(LATER))
}

#[test]
fn the_api_fills_in_the_member_and_sends_the_bearer() {
    let server = serve(routes());
    let env = env(&server, "api");
    let site = Site::open(&env, &adult1()).unwrap();
    let path = api_path("/api/Members/{memberId}/MemberAccount/Member?ivfCenterId={ivfCenterId}").unwrap();
    let a = site.api(&path).unwrap();
    assert_eq!(a.status, 200);
    assert_eq!(String::from_utf8_lossy(&a.body), ACCOUNT);
    assert_eq!(a.what, "GET /api/Members/{memberId}/MemberAccount/Member");
    let a = site
        .api(&api_path("/api/Members/4242/MemberAccount/MemberPaymentHistoryAll").unwrap())
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&a.body), HISTORY);
    assert_eq!(a.content_type.as_deref(), Some("application/json; charset=utf-8"));

    let seen = server.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].target, "/api/Members/4242/MemberAccount/Member?ivfCenterId=7");
    assert_eq!(seen[0].authorization, Some(format!("Bearer {}", token(LATER))));
    assert_eq!(seen[0].user_agent.as_deref(), Some("Mozilla/5.0 (test)"));
    assert_eq!(seen[0].cookie, None, "the API takes the bearer, no cookie");
}

#[test]
fn a_refused_token_is_exit_3_and_the_rest_name_no_member() {
    let server = serve(routes());
    let env = env(&server, "refused");
    let site = Site::open(&env, &adult1()).unwrap();
    for (path, exit) in [
        ("/api/Members/{memberId}/Member/UpComingBills", 3),
        ("/api/Members/{memberId}/FreezeEvents/FreezeEvents", 5),
        ("/api/Members/{memberId}/MemberIvfCenter", 5),
        ("/api/Members/{memberId}/MemberIvfCenter/CollectionSites", 4),
    ] {
        let e = site.api(&api_path(path).unwrap()).err().unwrap();
        assert_eq!(e.exit_code(), exit, "{path}: {e}");
        assert!(!e.to_string().contains("4242"), "{e}");
        if exit == 3 {
            assert!(
                e.to_string()
                    .contains("auth login --browser --profile adult1 --overwrite"),
                "{e}"
            );
        }
    }
}

/// Another member's id is refused before anything is sent.
#[test]
fn another_members_path_sends_nothing() {
    let server = serve(routes());
    let env = env(&server, "other");
    let site = Site::open(&env, &adult1()).unwrap();
    let e = site
        .api(&api_path("/api/Members/4243/MemberAccount/MemberPaymentHistoryAll").unwrap())
        .err()
        .unwrap();
    assert_eq!(e.exit_code(), 2);
    assert!(server.seen.lock().unwrap().is_empty());
}

/// An expired token, or one from the site's registration step, is exit 3
/// before any request: the refresh is not built.
#[test]
fn an_expired_or_unfinished_sign_in_is_exit_3_before_a_request() {
    let server = serve(routes());
    let expired = env_with(&server, "expired", token(1_000_000_000));
    let e = Site::open(&expired, &adult1()).err().unwrap();
    assert_eq!(e.exit_code(), 3);
    assert!(e.to_string().contains("expired at 2001-09-09T01:46:40Z"), "{e}");
    assert!(e.to_string().contains("--overwrite"), "{e}");

    let intermediate = test_token(&json!({ "intermediateUser": json!({ "UserId": 4242 }).to_string() }));
    let unfinished = env_with(&server, "unfinished", intermediate);
    let e = Site::open(&unfinished, &adult1()).err().unwrap();
    assert_eq!(e.exit_code(), 3);
    assert!(e.to_string().contains("registration step"), "{e}");

    let unreadable = env_with(&server, "unreadable", "dummy_secret".into());
    let e = Site::open(&unreadable, &adult1()).err().unwrap();
    assert_eq!(e.exit_code(), 3);
    assert!(!e.to_string().contains("dummy_secret"), "{e}");
    assert!(server.seen.lock().unwrap().is_empty());
}

#[test]
fn no_session_is_exit_3_naming_the_sign_in() {
    let server = serve(routes());
    let env = Env {
        sessions: SessionStore::with_vault(Vault::new(MemoryKeys::new(), temp_dir("none"))),
        base: server.base.clone(),
        pace: Duration::ZERO,
    };
    let e = Site::open(&env, &adult1()).err().unwrap();
    assert_eq!(e.exit_code(), 3);
    assert!(
        e.remedy()
            .unwrap()
            .contains("auth login --browser --clinic <SLUG> --profile adult1"),
        "{e}"
    );
}

#[test]
fn no_answer_names_the_server_read() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let server = Server {
        base: base.clone(),
        seen: Arc::new(Mutex::new(Vec::new())),
    };
    let env = env(&server, "no-answer");
    let e = Site::open(&env, &adult1())
        .unwrap()
        .api(&api_path("/api/Members/{memberId}/MemberIvfCenter").unwrap())
        .err()
        .unwrap();
    assert_eq!(e.exit_code(), 5);
    let host = base.trim_start_matches("http://");
    assert!(e.to_string().contains(&format!("got no answer from {host}")), "{e}");
    assert!(
        e.to_string().contains("GET /api/Members/{memberId}/MemberIvfCenter"),
        "{e}"
    );
}
