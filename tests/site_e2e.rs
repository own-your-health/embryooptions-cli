//! The reads of Embryo Options's member API, in-process, against a loopback
//! server, the session in an in-memory vault: the requests' paths and
//! headers, the member's id filled in from the token, the token refresh,
//! and the refusals.
//! Nothing reads the real keychain or reaches the network. Every token is
//! synthetic, built here.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use embryooptions::cli::DownloadArgs;
use embryooptions::client::api_path;
use embryooptions::portal::{Env, Site, Warnings};
use embryooptions::readers;
use embryooptions::session::{Session, SessionStore};
use embryooptions::site::Clinic;
use embryooptions::token::{test_token, Tokens};
use pk_cli_auth::refresh_lock::RefreshLock;
use pk_cli_auth::vault::{MemoryKeys, Vault};
use pk_cli_config::profile::ProfileName;
use pk_cli_core::RangeArgs;
use pk_cli_secrets::Secret;
use serde_json::json;

/// One request the server saw.
#[derive(Debug, Clone)]
struct Seen {
    target: String,
    /// The `X-XSRF-TOKEN` header, which carries the token.
    token: Option<String>,
    authorization: Option<String>,
    center: Option<String>,
    user_agent: Option<String>,
    cookie: Option<String>,
}

/// A loopback server answering each `GET` by its path and query from
/// `routes` (status, content type, body); anything else is a 404.
struct Server {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

type Routes = BTreeMap<String, (u16, &'static str, String)>;
/// Runs with each request before it is answered, and answers it instead of
/// `routes` when it returns one.
type Hook = Box<dyn Fn(&Seen) -> Option<(u16, &'static str, String)> + Send>;

fn serve(routes: Routes) -> Server {
    serve_with(routes, None)
}

fn serve_with(routes: Routes, hook: Option<Hook>) -> Server {
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
            let request = Seen {
                target: target.clone(),
                token: header("x-xsrf-token"),
                authorization: header("authorization"),
                center: header("ivfcenterid"),
                user_agent: header("user-agent"),
                cookie: header("cookie"),
            };
            log.lock().unwrap().push(request.clone());
            let hooked = hook.as_ref().and_then(|hook| hook(&request));
            let (status, mime, body) = hooked.or_else(|| routes.get(&target).cloned()).unwrap_or((
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

fn routes() -> Routes {
    let mut r = Routes::new();
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

/// adult1's session at the example clinic.
fn session(access: String, refresh: Option<&str>) -> Session {
    Session::new(
        Clinic::parse("exampleclinic").unwrap(),
        Tokens {
            access: Secret::new(access),
            refresh: refresh.map(Secret::new),
        },
        Some("Mozilla/5.0 (test)".into()),
    )
}

/// An in-memory vault in a temp dir: two stores over it share its key and
/// its lock, as two embryooptions processes share the vault and the lock.
type Shared = (Arc<MemoryKeys>, std::path::PathBuf);

fn vault(tag: &str) -> Shared {
    (Arc::new(MemoryKeys::new()), temp_dir(tag))
}

/// The lock every store over `vault` shares, beside its directory.
fn lock_of(vault: &Shared) -> RefreshLock {
    RefreshLock::at(vault.1.with_extension("refresh-lock"))
}

/// A store over `vault`: another process's, when the test makes two.
fn store_of(vault: &Shared) -> SessionStore {
    SessionStore::with_vault(Vault::new(vault.0.clone(), vault.1.clone()), lock_of(vault))
}

/// An environment reading `server`, with adult1's session (`access`, and
/// `refresh` as its refresh token) stored in `vault`.
fn env_in(server: &Server, vault: &Shared, access: String, refresh: Option<&str>) -> Env {
    let store = store_of(vault);
    store.put(&adult1(), &session(access, refresh)).unwrap();
    Env {
        sessions: store,
        base: server.base.clone(),
        pace: Duration::ZERO,
        warnings: Warnings::kept(),
    }
}

fn env_with(server: &Server, tag: &str, access: String) -> Env {
    env_in(server, &vault(tag), access, Some("dummy-refresh"))
}

fn env(server: &Server, tag: &str) -> Env {
    env_with(server, tag, token(LATER))
}

const EXPIRED: i64 = 1_000_000_000;
/// A later expiry for the token a refresh hands back.
const RENEWED: i64 = 4_100_000_000;

/// The routes with `/api/Auth/RefreshToken` answering `status`, and with a
/// new pair when it is 200.
fn routes_refreshing(status: u16) -> Routes {
    let mut r = routes();
    let body = if status == 200 {
        json!({ "accessToken": token(RENEWED), "refreshToken": "dummy-refresh-2" }).to_string()
    } else {
        String::new()
    };
    r.insert("/api/Auth/RefreshToken".into(), (status, "application/json", body));
    r
}

#[test]
fn the_api_fills_in_the_member_and_sends_the_token_headers() {
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
    assert_eq!(seen[0].token, Some(format!("Bearer {}", token(LATER))));
    assert_eq!(seen[0].center.as_deref(), Some("7"));
    assert_eq!(
        seen[0].authorization, None,
        "the API answers an Authorization header with 401"
    );
    assert_eq!(seen[0].user_agent.as_deref(), Some("Mozilla/5.0 (test)"));
    assert_eq!(seen[0].cookie, None, "the API takes the token header, no cookie");
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
                    .contains("auth login --browser --clinic exampleclinic --profile adult1 --overwrite"),
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

/// An expired token with no refresh token, or one from the site's
/// registration step, is exit 3 before any request.
#[test]
fn an_expired_or_unfinished_sign_in_is_exit_3_before_a_request() {
    let server = serve(routes());
    let expired = env_in(&server, &vault("expired"), token(EXPIRED), None);
    let e = Site::open(&expired, &adult1()).err().unwrap();
    assert_eq!(e.exit_code(), 3);
    assert!(e.to_string().contains("expired at 2001-09-09T01:46:40Z"), "{e}");
    assert!(e.to_string().contains("no refresh token"), "{e}");
    assert!(
        e.to_string()
            .contains("auth login --browser --clinic exampleclinic --profile adult1 --overwrite"),
        "{e}"
    );

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

/// An expired access token is renewed once with the refresh token, the new
/// pair is stored, and the read goes out with the new access token.
#[test]
fn an_expired_token_is_refreshed_once_and_the_new_pair_stored() {
    let server = serve(routes_refreshing(200));
    let env = env_in(&server, &vault("refresh"), token(EXPIRED), Some("dummy-refresh"));
    let site = Site::open(&env, &adult1()).unwrap();
    let a = site
        .api(&api_path("/api/Members/{memberId}/MemberAccount/Member?ivfCenterId={ivfCenterId}").unwrap())
        .unwrap();
    assert_eq!(a.status, 200);

    let seen = server.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0].target, "/api/Auth/RefreshToken");
    assert_eq!(seen[0].token.as_deref(), Some("Bearer dummy-refresh"));
    assert_eq!(seen[0].center.as_deref(), Some("7"));
    assert_eq!(seen[0].authorization, None);
    assert_eq!(seen[1].target, "/api/Members/4242/MemberAccount/Member?ivfCenterId=7");
    assert_eq!(seen[1].token, Some(format!("Bearer {}", token(RENEWED))));

    let stored = env.sessions.get(&adult1()).unwrap().unwrap();
    assert_eq!(stored.access_token.expose(), token(RENEWED));
    assert_eq!(
        stored.refresh_token.as_ref().map(|r| r.expose()),
        Some("dummy-refresh-2")
    );
    assert_eq!(stored.clinic.to_string(), "exampleclinic");

    // The stored token is good now: the next open sends no refresh.
    let site = Site::open(&env, &adult1()).unwrap();
    drop(site);
    assert_eq!(server.seen.lock().unwrap().len(), 2);
}

/// A token that expires within the margin is renewed before the read.
#[test]
fn a_token_about_to_expire_is_refreshed() {
    let server = serve(routes_refreshing(200));
    let soon = embryooptions::portal::now_unix() + embryooptions::portal::REFRESH_MARGIN / 2;
    let env = env_in(&server, &vault("soon"), token(soon), Some("dummy-refresh"));
    Site::open(&env, &adult1()).unwrap();
    let seen = server.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].target, "/api/Auth/RefreshToken");
}

/// A refresh the API refuses (401 or 403) is exit 3 naming the sign-in,
/// and leaves the stored session as it was.
#[test]
fn a_refused_refresh_is_exit_3_naming_the_sign_in() {
    for status in [401, 403] {
        let server = serve(routes_refreshing(status));
        let env = env_in(
            &server,
            &vault(&format!("refused-{status}")),
            token(EXPIRED),
            Some("dummy-refresh"),
        );
        let e = Site::open(&env, &adult1()).err().unwrap();
        assert_eq!(e.exit_code(), 3, "{status}: {e}");
        assert!(
            e.to_string().contains("refresh token (GET /api/Auth/RefreshToken"),
            "{e}"
        );
        assert!(e.to_string().contains(&format!("HTTP {status}")), "{e}");
        assert!(!e.to_string().contains("dummy-refresh"), "{e}");
        assert!(
            e.remedy()
                .unwrap()
                .contains("auth login --browser --clinic exampleclinic --profile adult1 --overwrite"),
            "{e}"
        );
        let stored = env.sessions.get(&adult1()).unwrap().unwrap();
        assert_eq!(stored.access_token.expose(), token(EXPIRED));
        assert_eq!(server.seen.lock().unwrap().len(), 1);
    }
}

/// A refresh answered with a token that has expired too is exit 3, saying so.
#[test]
fn a_refresh_answering_an_expired_token_is_exit_3() {
    let mut routes = routes();
    routes.insert(
        "/api/Auth/RefreshToken".into(),
        (
            200,
            "application/json",
            json!({ "accessToken": token(EXPIRED + 600), "refreshToken": "dummy-refresh-2" }).to_string(),
        ),
    );
    let server = serve(routes);
    let env = env_in(&server, &vault("stale-pair"), token(EXPIRED), Some("dummy-refresh"));
    let e = Site::open(&env, &adult1()).err().unwrap();
    assert_eq!(e.exit_code(), 3, "{e}");
    assert!(
        e.to_string().contains("renewed it with a token that had expired too"),
        "{e}"
    );
}

/// A refresh that answers something other than a token pair is exit 5.
#[test]
fn a_refresh_without_a_pair_is_exit_5() {
    let mut routes = routes();
    routes.insert(
        "/api/Auth/RefreshToken".into(),
        (200, "application/json", r#"{"message":"ok"}"#.into()),
    );
    let server = serve(routes);
    let env = env_in(&server, &vault("no-pair"), token(EXPIRED), Some("dummy-refresh"));
    let e = Site::open(&env, &adult1()).err().unwrap();
    assert_eq!(e.exit_code(), 5, "{e}");
}

/// A refresh that fails for another reason than a refused token (here a
/// 503) leaves a token that has not expired yet (50 s left: within the
/// margin, past the 30 s `expired_at` keeps) to be read with.
#[test]
fn a_failed_refresh_of_a_valid_token_reads_with_it() {
    let server = serve(routes_refreshing(503));
    let soon = embryooptions::portal::now_unix() + embryooptions::portal::REFRESH_MARGIN - 10;
    let env = env_in(&server, &vault("transient"), token(soon), Some("dummy-refresh"));
    let site = Site::open(&env, &adult1()).unwrap();
    site.api(&api_path("/api/Members/{memberId}/MemberAccount/Member?ivfCenterId={ivfCenterId}").unwrap())
        .unwrap();
    let seen = server.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].target, "/api/Auth/RefreshToken");
    assert_eq!(seen[1].token, Some(format!("Bearer {}", token(soon))));
    let stored = env.sessions.get(&adult1()).unwrap().unwrap();
    assert_eq!(stored.access_token.expose(), token(soon));
    let warnings = env.warnings.lines();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("was not renewed") && warnings[0].contains("RefreshToken with HTTP 503"),
        "{warnings:?}"
    );

    // An expired one is not read with: the same failure is exit 5.
    let server = serve(routes_refreshing(503));
    let env = env_in(
        &server,
        &vault("transient-expired"),
        token(EXPIRED),
        Some("dummy-refresh"),
    );
    let e = Site::open(&env, &adult1()).err().unwrap();
    assert_eq!(e.exit_code(), 5, "{e}");
}

/// A sign-in another process stored while this one waited for the lock is
/// what the renewal reads: no refresh is sent, the sign-in stays stored,
/// and the read goes out with its token.
#[test]
fn a_sign_in_stored_meanwhile_is_kept_and_no_refresh_sent() {
    let shared = vault("meanwhile");
    let server = serve(routes_refreshing(200));
    let env = env_in(&server, &shared, token(EXPIRED), Some("dummy-refresh"));
    // Another process holds the lock for a sign-in, and stores it.
    let (held, holding) = std::sync::mpsc::channel();
    let other = store_of(&shared);
    let lock = lock_of(&shared);
    let signer = std::thread::spawn(move || {
        lock.hold(|| {
            held.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(300));
            other.put(&adult1(), &session(token(LATER), Some("dummy-refresh-signed-in")))
        })
        .unwrap();
    });
    holding.recv().unwrap();
    let site = Site::open(&env, &adult1()).unwrap();
    signer.join().unwrap();
    site.api(&api_path("/api/Members/{memberId}/MemberAccount/Member?ivfCenterId={ivfCenterId}").unwrap())
        .unwrap();

    let seen = server.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].token, Some(format!("Bearer {}", token(LATER))));
    let stored = env.sessions.get(&adult1()).unwrap().unwrap();
    assert_eq!(stored.access_token.expose(), token(LATER));
    assert_eq!(
        stored.refresh_token.as_ref().map(|r| r.expose()),
        Some("dummy-refresh-signed-in")
    );
}

/// A sign-in made while a renewal is under way waits for the renewal's
/// write and is stored after it, so the sign-in is what stays.
#[test]
fn a_sign_in_during_a_renewal_is_stored_after_it() {
    let shared = vault("during");
    let signer: Arc<Mutex<Option<std::thread::JoinHandle<()>>>> = Arc::default();
    let hook: Hook = {
        let shared = shared.clone();
        let signer = signer.clone();
        Box::new(move |request: &Seen| {
            if request.target == "/api/Auth/RefreshToken" {
                let other = store_of(&shared);
                *signer.lock().unwrap() = Some(std::thread::spawn(move || {
                    other
                        .put_signin(&adult1(), &session(token(LATER), Some("dummy-refresh-signed-in")))
                        .unwrap();
                }));
            }
            None
        })
    };
    let server = serve_with(routes_refreshing(200), Some(hook));
    let env = env_in(&server, &shared, token(EXPIRED), Some("dummy-refresh"));
    Site::open(&env, &adult1()).unwrap();
    let handle = signer.lock().unwrap().take().expect("the refresh was sent");
    handle.join().unwrap();
    let stored = env.sessions.get(&adult1()).unwrap().unwrap();
    assert_eq!(stored.access_token.expose(), token(LATER));
    assert_eq!(
        stored.refresh_token.as_ref().map(|r| r.expose()),
        Some("dummy-refresh-signed-in")
    );
}

const ACCOUNT_READ: &str = "/api/Members/4242/MemberAccount/Member?ivfCenterId=7";

fn account_path() -> embryooptions::client::ApiPath {
    api_path("/api/Members/{memberId}/MemberAccount/Member?ivfCenterId={ivfCenterId}").unwrap()
}

/// A read the API answers 403, whatever the clock says of the token, is
/// renewed once and sent again with the new token, and the new pair is
/// stored. A second 403 in the same command is not renewed again: it is
/// exit 5, naming the sign-in.
#[test]
fn a_read_answered_403_renews_once_and_is_sent_again() {
    let shared = vault("read-403");
    let hook: Hook = Box::new(|request: &Seen| {
        let old = request.token == Some(format!("Bearer {}", token(LATER)));
        (request.target == ACCOUNT_READ && old).then(|| (403, "application/json", String::new()))
    });
    let server = serve_with(routes_refreshing(200), Some(hook));
    let env = env_in(&server, &shared, token(LATER), Some("dummy-refresh"));
    let site = Site::open(&env, &adult1()).unwrap();
    assert_eq!(server.seen.lock().unwrap().len(), 0, "not due by the clock");
    let a = site.api(&account_path()).unwrap();
    assert_eq!(a.status, 200);

    let seen = server.seen.lock().unwrap().clone();
    let targets: Vec<_> = seen.iter().map(|s| s.target.as_str()).collect();
    assert_eq!(targets, [ACCOUNT_READ, "/api/Auth/RefreshToken", ACCOUNT_READ]);
    assert_eq!(seen[1].token.as_deref(), Some("Bearer dummy-refresh"));
    assert_eq!(seen[2].token, Some(format!("Bearer {}", token(RENEWED))));
    let stored = env.sessions.get(&adult1()).unwrap().unwrap();
    assert_eq!(stored.access_token.expose(), token(RENEWED));

    // Every token refused: one renewal, then the 403 stands.
    let hook: Hook =
        Box::new(|request: &Seen| (request.target == ACCOUNT_READ).then(|| (403, "application/json", String::new())));
    let server = serve_with(routes_refreshing(200), Some(hook));
    let env = env_in(&server, &vault("read-403-always"), token(LATER), Some("dummy-refresh"));
    let site = Site::open(&env, &adult1()).unwrap();
    let e = site.api(&account_path()).err().unwrap();
    assert_eq!(e.exit_code(), 5, "{e}");
    assert!(
        e.remedy()
            .unwrap()
            .contains("auth login --browser --clinic exampleclinic --profile adult1 --overwrite"),
        "{e}"
    );
    let e = site.api(&account_path()).err().unwrap();
    assert_eq!(e.exit_code(), 5, "{e}");
    let targets: Vec<_> = server.seen.lock().unwrap().iter().map(|s| s.target.clone()).collect();
    assert_eq!(
        targets,
        [ACCOUNT_READ, "/api/Auth/RefreshToken", ACCOUNT_READ, ACCOUNT_READ]
    );
}

/// A read's 403 renewed with a token for another member is exit 3, and the
/// read is not sent again.
#[test]
fn a_read_answered_403_renewed_for_another_member_is_exit_3() {
    let hook: Hook =
        Box::new(|request: &Seen| (request.target == ACCOUNT_READ).then(|| (403, "application/json", String::new())));
    let mut routes = routes();
    let other_member = test_token(&json!({
        "user": json!({ "UserId": 4243, "IVFCenterId": 7, "MemberType": 1 }).to_string(),
        "exp": RENEWED,
    }));
    routes.insert(
        "/api/Auth/RefreshToken".into(),
        (
            200,
            "application/json",
            json!({ "accessToken": other_member, "refreshToken": "dummy-refresh-2" }).to_string(),
        ),
    );
    let server = serve_with(routes, Some(hook));
    let env = env_in(&server, &vault("read-403-other"), token(LATER), Some("dummy-refresh"));
    let e = Site::open(&env, &adult1()).unwrap().api(&account_path()).err().unwrap();
    assert_eq!(e.exit_code(), 3, "{e}");
    assert!(e.to_string().contains("not the same member's"), "{e}");
    assert!(
        !e.to_string().contains("4243") && !e.to_string().contains("4242"),
        "{e}"
    );
    let targets: Vec<_> = server.seen.lock().unwrap().iter().map(|s| s.target.clone()).collect();
    assert_eq!(targets, [ACCOUNT_READ, "/api/Auth/RefreshToken"]);
}

/// A read answered 403 while another process stored a sign-in: the
/// renewal reads that sign-in, sends no refresh, and the read goes out
/// again with the sign-in's token.
#[test]
fn a_read_answered_403_after_a_sign_in_meanwhile_reads_with_it() {
    const SIGNED_IN: i64 = 4_200_000_000;
    let shared = vault("read-403-meanwhile");
    let hook: Hook = {
        let other = store_of(&shared);
        Box::new(move |request: &Seen| {
            let old = request.token == Some(format!("Bearer {}", token(LATER)));
            (request.target == ACCOUNT_READ && old).then(|| {
                other
                    .put_signin(&adult1(), &session(token(SIGNED_IN), Some("dummy-refresh-signed-in")))
                    .unwrap();
                (403, "application/json", String::new())
            })
        })
    };
    let server = serve_with(routes_refreshing(200), Some(hook));
    let env = env_in(&server, &shared, token(LATER), Some("dummy-refresh"));
    let a = Site::open(&env, &adult1()).unwrap().api(&account_path()).unwrap();
    assert_eq!(a.status, 200);
    let seen = server.seen.lock().unwrap().clone();
    let targets: Vec<_> = seen.iter().map(|s| s.target.as_str()).collect();
    assert_eq!(targets, [ACCOUNT_READ, ACCOUNT_READ]);
    assert_eq!(seen[1].token, Some(format!("Bearer {}", token(SIGNED_IN))));
    let stored = env.sessions.get(&adult1()).unwrap().unwrap();
    assert_eq!(stored.access_token.expose(), token(SIGNED_IN));
}

/// A read's 403 with no refresh token stored is the 403, with nothing more
/// sent.
#[test]
fn a_read_answered_403_without_a_refresh_token_sends_nothing_more() {
    let hook: Hook =
        Box::new(|request: &Seen| (request.target == ACCOUNT_READ).then(|| (403, "application/json", String::new())));
    let server = serve_with(routes_refreshing(200), Some(hook));
    let env = env_in(&server, &vault("read-403-none"), token(LATER), None);
    let e = Site::open(&env, &adult1()).unwrap().api(&account_path()).err().unwrap();
    assert_eq!(e.exit_code(), 5, "{e}");
    assert_eq!(server.seen.lock().unwrap().len(), 1);
}

/// A renewed pair the vault cannot store is still read with, and the
/// warning names the sign-in: the refresh has already spent the old pair.
/// While the refresh is answered, the vault's directory is moved aside and
/// a file put in its place, so the write fails.
#[test]
fn a_renewed_pair_that_cannot_be_stored_is_read_with() {
    let shared = vault("unstorable");
    let dir = shared.1.clone();
    let aside = dir.with_extension("aside");
    let hook: Hook = {
        let (dir, aside) = (dir.clone(), aside.clone());
        Box::new(move |request: &Seen| {
            if request.target == "/api/Auth/RefreshToken" {
                std::fs::rename(&dir, &aside).unwrap();
                std::fs::write(&dir, b"").unwrap();
            }
            None
        })
    };
    let server = serve_with(routes_refreshing(200), Some(hook));
    let env = env_in(&server, &shared, token(EXPIRED), Some("dummy-refresh"));
    let site = Site::open(&env, &adult1()).unwrap();
    let read = site.api(&account_path()).map(|a| a.status);
    std::fs::remove_file(&dir).unwrap();
    std::fs::rename(&aside, &dir).unwrap();
    assert_eq!(read.unwrap(), 200);

    let seen = server.seen.lock().unwrap().clone();
    assert_eq!(seen[1].token, Some(format!("Bearer {}", token(RENEWED))));
    let stored = env.sessions.get(&adult1()).unwrap().unwrap();
    assert_eq!(stored.access_token.expose(), token(EXPIRED), "nothing was stored");
    let warnings = env.warnings.lines();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("renewed Embryo Options sign-in was not stored"),
        "{warnings:?}"
    );
    assert!(
        warnings[0].contains("auth login --browser --clinic exampleclinic --profile adult1 --overwrite"),
        "{warnings:?}"
    );
    assert!(!warnings[0].contains("dummy-refresh"), "{warnings:?}");
}

#[test]
fn no_session_is_exit_3_naming_the_sign_in() {
    let server = serve(routes());
    let env = Env {
        sessions: store_of(&vault("none")),
        base: server.base.clone(),
        pace: Duration::ZERO,
        warnings: Warnings::kept(),
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

// ------------------------------------------------------------- the readers

const MEMBER_FIXTURE: &str = include_str!("fixtures/member.json");
const CLINIC_FIXTURE: &str = include_str!("fixtures/clinic.json");
const HISTORY_1: &str = include_str!("fixtures/history-page-1.json");
const HISTORY_2: &str = include_str!("fixtures/history-page-2.json");
/// The shape the live API answers (*observed*), with invented values: string
/// amounts, written-out dates, codes for types, no `outstandingBalance` and
/// no `transactionStatus`.
const HISTORY_LIVE: &str = include_str!("fixtures/history-live-shape.json");
const UPCOMING_FIXTURE: &str = include_str!("fixtures/upcoming.json");

const MEMBER_AT: &str = "/api/Members/4242/MemberAccount/Member?ivfCenterId=7";
const CLINIC_AT: &str = "/api/Members/4242/MemberIvfCenter";
const UPCOMING_AT: &str = "/api/Members/4242/Member/UpComingBills";

fn page_at(n: usize) -> String {
    format!("/api/Members/4242/MemberAccount/MemberPaymentHistoryAll?pageNumber={n}&pageSize=50")
}

fn ok(body: &str) -> (u16, &'static str, String) {
    (200, "application/json; charset=utf-8", body.to_string())
}

/// The member's account, the clinic and two pages of history, synthetic
/// (tests/fixtures).
fn reader_routes() -> Routes {
    let mut r = Routes::new();
    r.insert(MEMBER_AT.into(), ok(MEMBER_FIXTURE));
    r.insert(CLINIC_AT.into(), ok(CLINIC_FIXTURE));
    r.insert(page_at(1), ok(HISTORY_1));
    r.insert(page_at(2), ok(HISTORY_2));
    r.insert(UPCOMING_AT.into(), ok(UPCOMING_FIXTURE));
    r
}

/// `routes` with `path` answering `body`.
fn with(mut routes: Routes, path: &str, body: &str) -> Routes {
    routes.insert(path.into(), ok(body));
    routes
}

/// A fixture with `from` replaced by `to`, which must be in it.
fn edited(fixture: &str, from: &str, to: &str) -> String {
    assert!(fixture.contains(from), "{from}");
    fixture.replace(from, to)
}

fn reader_env(server: &Server, tag: &str) -> Env {
    env(server, tag)
}

/// A token for a member with embryos in storage (`MemberType` `CRY`).
fn embryo_token() -> String {
    test_token(&json!({
        "user": json!({ "UserId": 4242, "IVFCenterId": 7, "MemberType": "CRY" }).to_string(),
        "exp": LATER,
    }))
}

fn ids(v: &serde_json::Value) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

fn targets(server: &Server) -> Vec<String> {
    server.seen.lock().unwrap().iter().map(|s| s.target.clone()).collect()
}

#[test]
fn accounts_list_reads_the_account_the_clinic_and_every_page_of_history() {
    let server = serve(reader_routes());
    let env = reader_env(&server, "accounts");
    let site = Site::open(&env, &adult1()).unwrap();
    let v = readers::accounts_list(&site).unwrap();
    assert_eq!(v["schema"], "account-list/v1");
    let a = &v["items"][0];
    assert_eq!(a["id"], "1001");
    assert_eq!(a["provider"], "Example Fertility Clinic");
    assert_eq!(a["currency"], "USD");
    assert_eq!(a["owes"].to_string(), "600.00");
    assert_eq!(a["admin_fees"].to_string(), "0.00");
    assert_eq!(
        (a["bills"].as_u64(), a["open_bills"].as_u64(), a["payments"].as_u64()),
        (Some(3), Some(1), Some(2))
    );
    assert_eq!(a["owed_on_bills"].to_string(), "600.00");
    assert!(a["next_bill"].is_null(), "member type 1 is not embryos in storage");
    // The app's own reads, in order, and nothing else: no upcoming bill
    // for this member type.
    assert_eq!(
        targets(&server),
        vec![MEMBER_AT.to_string(), CLINIC_AT.into(), page_at(1), page_at(2)]
    );
    let text = v.to_string();
    for absent in ["4242", "member-one-dummy", "dummy-folder"] {
        assert!(!text.contains(absent), "{absent}");
    }
}

#[test]
fn a_member_with_embryos_in_storage_sees_the_next_bill() {
    let server = serve(reader_routes());
    let env = env_with(&server, "upcoming", embryo_token());
    let site = Site::open(&env, &adult1()).unwrap();
    let v = readers::accounts_list(&site).unwrap();
    let next = &v["items"][0]["next_bill"];
    assert_eq!(next["date"], "2027-01-01");
    assert_eq!(next["amount"].to_string(), "600.00");
    assert_eq!(next["freeze_date"], "2020-05-01");
    assert_eq!(targets(&server).last().map(String::as_str), Some(UPCOMING_AT));

    // A failure of that read is a warning, as the app ignores it; a
    // sign-in that has ended is not.
    let mut down = reader_routes();
    down.insert(UPCOMING_AT.into(), (503, "text/html", "<p>down</p>".into()));
    let server = serve(down);
    let env = env_with(&server, "upcoming-down", embryo_token());
    let site = Site::open(&env, &adult1()).unwrap();
    let v = readers::accounts_list(&site).unwrap();
    assert!(v["items"][0]["next_bill"].is_null());
    assert!(
        env.warnings
            .lines()
            .iter()
            .any(|l| l.contains("upcoming bill was not read")),
        "{:?}",
        env.warnings.lines()
    );

    let mut refused = reader_routes();
    refused.insert(UPCOMING_AT.into(), (401, "application/json", String::new()));
    let server = serve(refused);
    let env = env_with(&server, "upcoming-refused", embryo_token());
    let site = Site::open(&env, &adult1()).unwrap();
    assert_eq!(readers::accounts_list(&site).err().unwrap().exit_code(), 3);

    // No freeze date is no upcoming bill, as the app shows it.
    let server = serve(with(
        reader_routes(),
        UPCOMING_AT,
        r#"{"nextPaymentAmount":0,"freezeDate":null}"#,
    ));
    let env = env_with(&server, "upcoming-none", embryo_token());
    let site = Site::open(&env, &adult1()).unwrap();
    assert!(readers::accounts_list(&site).unwrap()["items"][0]["next_bill"].is_null());
}

#[test]
fn bills_list_is_the_charges_newest_service_first() {
    let server = serve(reader_routes());
    let env = reader_env(&server, "bills");
    let site = Site::open(&env, &adult1()).unwrap();
    let all = readers::bills_list(&site, false, &RangeArgs::default()).unwrap();
    assert_eq!(all["schema"], "bill-list/v1");
    assert_eq!(ids(&all), ["501", "503", "506"]);
    let b = &all["items"][0];
    assert_eq!(
        (b["date"].as_str(), b["end"].as_str(), b["posted"].as_str()),
        (Some("2026-01-01"), Some("2026-12-31"), Some("2026-01-02"))
    );
    assert_eq!(b["description"], "Annual embryo storage");
    assert_eq!(b["freeze_date"], "2020-05-01");
    assert_eq!(
        (
            b["billed"].to_string().as_str(),
            b["owes"].to_string().as_str(),
            b["open"].as_bool()
        ),
        ("600.00", "600.00", Some(true))
    );
    // .NET's empty date is no period: the notice fee is dated by its posting.
    assert_eq!(all["items"][2]["date"], "2024-12-01");

    let range = |since: Option<&str>, until: Option<&str>, limit: Option<u32>| RangeArgs {
        since: since.map(str::to_string),
        until: until.map(str::to_string),
        limit,
    };
    assert_eq!(
        ids(&readers::bills_list(&site, true, &RangeArgs::default()).unwrap()),
        ["501"]
    );
    assert_eq!(
        ids(&readers::bills_list(&site, false, &range(Some("2025-6-1"), None, None)).unwrap()),
        ["501"]
    );
    assert_eq!(
        ids(&readers::bills_list(&site, false, &range(None, Some("2025-06-01"), None)).unwrap()),
        ["503", "506"]
    );
    assert_eq!(
        ids(&readers::bills_list(&site, false, &range(None, None, Some(1))).unwrap()),
        ["501"]
    );
}

#[test]
fn bills_show_has_what_the_history_says_and_only_charges_are_bills() {
    let server = serve(reader_routes());
    let env = reader_env(&server, "show");
    let site = Site::open(&env, &adult1()).unwrap();
    let v = readers::bill_show(&site, " 501 ").unwrap();
    assert_eq!(v["schema"], "bill/v1");
    assert_eq!(v["provider"], "Dr. Example Provider");
    assert_eq!(v["lab"], "Example Lab");
    assert_eq!(v["codes"], json!(["89342"]));
    assert_eq!(v["note"], "Storage, year 7");
    assert_eq!(v["charges"][0]["code"], "89342");
    assert_eq!(v["charges"][0]["amount"].to_string(), "600.00");
    for id in ["502", "9999", "505"] {
        let e = readers::bill_show(&site, id).err().unwrap();
        assert_eq!(e.exit_code(), 4, "{id}");
    }
}

#[test]
fn payments_list_tells_payments_discounts_and_failures_apart() {
    let server = serve(reader_routes());
    let env = reader_env(&server, "payments");
    let site = Site::open(&env, &adult1()).unwrap();
    let v = readers::payments_list(&site, &RangeArgs::default()).unwrap();
    assert_eq!(v["schema"], "payment-list/v1");
    assert_eq!(ids(&v), ["504", "502", "505"]);
    let p = |i: usize| &v["items"][i];
    assert_eq!(
        (
            p(0)["kind"].as_str(),
            p(0)["method"].as_str(),
            p(0)["settled"].as_bool()
        ),
        (Some("payment"), Some("PayPal"), Some(false))
    );
    assert_eq!(
        (
            p(1)["amount"].to_string().as_str(),
            p(1)["method"].as_str(),
            p(1)["settled"].as_bool()
        ),
        ("600.00", Some("Visa"), Some(true))
    );
    assert_eq!(
        (p(2)["kind"].as_str(), p(2)["amount"].to_string().as_str()),
        (Some("discount"), "25.00")
    );
    let text = v.to_string();
    for absent in ["1111", "dummy-auth-ref"] {
        assert!(!text.contains(absent), "{absent}");
    }
}

/// The stream's lines: header, resources, trailer.
fn stream_of(bytes: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8(bytes.to_vec())
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn of_type<'a>(lines: &'a [serde_json::Value], t: &str) -> Vec<&'a serde_json::Value> {
    lines.iter().filter(|l| l["resourceType"] == t).collect()
}

#[test]
fn records_export_writes_the_account_its_bills_and_settled_payments() {
    let server = serve(reader_routes());
    let env = reader_env(&server, "export");
    let site = Site::open(&env, &adult1()).unwrap();
    let mut out = Vec::new();
    let finished = readers::export(&site, &server.base, &mut out, None, None, None).unwrap();
    assert!(finished.trailer.complete);
    let text = String::from_utf8(out.clone()).unwrap();
    for absent in ["4242", "1111", "dummy-auth-ref", "member-one-dummy"] {
        assert!(!text.contains(absent), "{absent}");
    }
    // Amounts keep the site's spelling.
    assert!(text.contains(r#""value":600.00"#), "{text}");
    let lines = stream_of(&out);
    let header = &lines[0];
    assert_eq!(header["schema"], "records-stream/v1");
    assert_eq!(header["method"], "portal");
    assert_eq!(header["organization"]["id"], "org-exampleclinic");
    assert_eq!(header["organization"]["name"], "Example Fertility Clinic");
    let patient = header["patient"]["id"].as_str().unwrap();
    assert!(patient.starts_with("patient-exampleclinic-"), "{patient}");
    let types: Vec<&str> = lines[1..lines.len() - 1]
        .iter()
        .map(|l| l["resourceType"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        [
            "Organization",
            "Patient",
            "Account",
            "Invoice",
            "Invoice",
            "Invoice",
            "PaymentReconciliation"
        ]
    );
    let account = of_type(&lines, "Account")[0];
    assert_eq!(account["id"], "account-1001");
    assert_eq!(account["identifier"][0]["value"], "1001");

    let invoices = of_type(&lines, "Invoice");
    let by_id = |id: &str| {
        *invoices
            .iter()
            .find(|i| i["id"] == id)
            .unwrap_or_else(|| panic!("{id}"))
    };
    let open = by_id("bill-1001-501");
    assert_eq!(open["status"], "issued");
    assert_eq!(open["totalNet"]["value"].to_string(), "600.00");
    assert_eq!(open["totalGross"], open["totalNet"]);
    assert_eq!(open["account"]["reference"], "Account/account-1001");
    assert_eq!(open["date"], "2026-01-02");
    assert_eq!(open["type"]["text"], "Annual embryo storage (freeze date 2020-05-01)");
    assert_eq!(open["participant"][0]["actor"]["display"], "Dr. Example Provider");
    let charge = &open["contained"][0];
    assert_eq!(charge["occurrenceDateTime"], "2026-01-01");
    assert_eq!(charge["code"]["coding"][0]["system"], "http://www.ama-assn.org/go/cpt");
    assert_eq!(charge["code"]["coding"][0]["code"], "89342");
    let billed = open["totalPriceComponent"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["code"]["coding"][0]["code"] == "billed")
        .unwrap();
    assert_eq!(
        (billed["type"].as_str(), billed["amount"]["value"].to_string().as_str()),
        (Some("base"), "600.00")
    );
    assert_eq!(by_id("bill-1001-503")["status"], "balanced");
    // A charge naming no code is one line of its own.
    let notice = by_id("bill-1001-506");
    assert_eq!(notice["status"], "balanced");
    assert_eq!(notice["contained"][0]["occurrenceDateTime"], "2024-12-01");
    assert_eq!(notice["contained"][0]["code"]["text"], "Notice fee");

    let payments = of_type(&lines, "PaymentReconciliation");
    assert_eq!(payments[0]["id"], "payment-502");
    assert_eq!(payments[0]["paymentDate"], "2025-01-03");
    assert_eq!(payments[0]["paymentAmount"]["value"].to_string(), "600.00");
    assert_eq!(payments[0]["disposition"], "Paid by Visa");

    let trailer = lines.last().unwrap();
    assert_eq!(trailer["schema"], "records-stream-end/v1");
    let skips: Vec<(String, String)> = trailer["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["resource_type"].as_str().unwrap().to_string(),
                s["code"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let skip = |t: &str, c: &str| (t.to_string(), c.to_string());
    assert_eq!(
        skips,
        [
            skip("PaymentReconciliation", "not_settled"),
            skip("PaymentReconciliation", "unreadable"),
            skip("PaymentReconciliation", "unsupported"),
            skip("PaymentReconciliation", "unsupported"),
        ],
        "no skip is an Invoice's, so a store may tell a bill the site dropped"
    );
    assert!(env.warnings.lines().is_empty(), "{:?}", env.warnings.lines());
}

/// A scheduled payment, one the processor has not finished, and one with no
/// status are never written as paid.
#[test]
fn records_export_never_writes_a_payment_that_has_not_settled() {
    let mut page: serde_json::Value = serde_json::from_str(HISTORY_2).unwrap();
    let rows = page["data"].as_array_mut().unwrap();
    let row = |id: u64, kind: &str, at: &str, status: serde_json::Value| {
        json!({
            "id": id,
            "memberPaymentTypeId": kind,
            "timestamp": format!("{at}T09:00:00"),
            "amount": -600.00,
            "creditCardSystemStatus": null,
            "transactionStatus": status,
        })
    };
    rows.push(row(901, "ScheduledInstallmentPayment", "2999-01-03", json!("settled")));
    rows.push(row(
        902,
        "AutomatedPayment",
        "2025-05-01",
        json!("submitted_for_settlement"),
    ));
    rows.push(row(903, "AutomatedPayment", "2025-05-02", json!(null)));
    let total = 8 + 3;
    page["total"] = json!(total);
    let mut page1: serde_json::Value = serde_json::from_str(HISTORY_1).unwrap();
    page1["total"] = json!(total);
    let mut routes = reader_routes();
    routes.insert(page_at(1), ok(&page1.to_string()));
    routes.insert(page_at(2), ok(&page.to_string()));
    let server = serve(routes);
    let env = reader_env(&server, "unsettled");
    let site = Site::open(&env, &adult1()).unwrap();

    let v = readers::payments_list(&site, &RangeArgs::default()).unwrap();
    let state = |id: &str| {
        let r = v["items"].as_array().unwrap().iter().find(|r| r["id"] == id).unwrap();
        (
            r["state"].as_str().unwrap().to_string(),
            r["settled"].as_bool().unwrap(),
        )
    };
    assert_eq!(state("901"), ("scheduled".into(), false));
    assert_eq!(state("902"), ("pending".into(), false));
    assert_eq!(state("903"), ("unconfirmed".into(), false));
    assert_eq!(state("502"), ("settled".into(), true));

    let mut out = Vec::new();
    readers::export(&site, &server.base, &mut out, None, None, None).unwrap();
    let lines = stream_of(&out);
    let ids: Vec<_> = of_type(&lines, "PaymentReconciliation")
        .iter()
        .map(|p| p["id"].clone())
        .collect();
    assert_eq!(ids, [json!("payment-502")]);
    let skipped = lines.last().unwrap()["skipped"].to_string();
    assert!(
        skipped.contains("2 scheduled or pending and 1 not shown to have settled"),
        "{skipped}"
    );
}

/// The routes for the live shape, with the account owing `balance`.
fn live_routes(balance: &str) -> Routes {
    let mut r = reader_routes();
    r.insert(page_at(1), ok(HISTORY_LIVE));
    r.insert(
        MEMBER_AT.into(),
        ok(&edited(
            MEMBER_FIXTURE,
            "\"billingBalance\": 600.00",
            &format!("\"billingBalance\": {balance}"),
        )),
    );
    r
}

#[test]
fn the_live_shape_reads_as_a_bill_paid_by_a_card_payment() {
    let server = serve(live_routes("0.00"));
    let env = reader_env(&server, "live-paid");
    let site = Site::open(&env, &adult1()).unwrap();

    let a = readers::accounts_list(&site).unwrap()["items"][0].clone();
    assert_eq!(a["owes"].to_string(), "0.00");
    assert_eq!(
        (a["bills"].as_u64(), a["open_bills"].as_u64(), a["payments"].as_u64()),
        (Some(1), Some(0), Some(1))
    );
    assert_eq!(a["owed_on_bills"].to_string(), "0.00");

    let b = readers::bills_list(&site, false, &RangeArgs::default()).unwrap();
    assert_eq!(b["items"].as_array().unwrap().len(), 1);
    let row = &b["items"][0];
    assert_eq!(
        (row["date"].as_str(), row["end"].as_str()),
        (Some("2025-12-01"), Some("2026-11-30"))
    );
    assert_eq!(row["posted"], "2024-03-05");
    assert_eq!(
        (row["open"].as_bool(), row["owes"].to_string()),
        (Some(false), "0.00".into())
    );
    assert_eq!(row["billed"].to_string(), "75.00");

    let p = readers::payments_list(&site, &RangeArgs::default()).unwrap();
    let p = &p["items"][0];
    assert_eq!(
        (
            p["kind"].as_str(),
            p["state"].as_str(),
            p["settled"].as_bool(),
            p["date"].as_str()
        ),
        (Some("payment"), Some("settled"), Some(true), Some("2024-03-05"))
    );
    assert_eq!(
        (p["subtype"].as_str(), p["method"].as_str()),
        (Some("MEMBER_INT_PMT"), Some("Mastercard"))
    );
    assert_eq!(p["amount"].to_string(), "75.00");

    let mut out = Vec::new();
    readers::export(&site, &server.base, &mut out, None, None, None).unwrap();
    let text = String::from_utf8(out.clone()).unwrap();
    assert!(text.contains(r#""value":75.00"#), "{text}");
    for absent in ["1111", "dummy-auth-ref"] {
        assert!(!text.contains(absent), "{absent}");
    }
    let lines = stream_of(&out);
    let invoices = of_type(&lines, "Invoice");
    assert_eq!(invoices.len(), 1);
    assert_eq!(invoices[0]["status"], "balanced");
    assert_eq!(invoices[0]["totalNet"]["value"].to_string(), "0.00");
    assert_eq!(invoices[0]["contained"][0]["code"]["coding"][0]["code"], "89346");
    let payments = of_type(&lines, "PaymentReconciliation");
    assert_eq!(payments.len(), 1);
    assert_eq!(payments[0]["paymentDate"], "2024-03-05");
    assert_eq!(payments[0]["paymentAmount"]["value"].to_string(), "75.00");
    assert_eq!(lines.last().unwrap()["skipped"], json!([]));
    assert!(env.warnings.lines().is_empty(), "{:?}", env.warnings.lines());
}

#[test]
fn a_live_account_that_owes_and_says_not_which_bill_is_refused() {
    let server = serve(live_routes("75.00"));
    let env = reader_env(&server, "live-owes");
    let site = Site::open(&env, &adult1()).unwrap();
    let a = readers::accounts_list(&site).unwrap()["items"][0].clone();
    assert_eq!(a["owes"].to_string(), "75.00");
    assert!(a["owed_on_bills"].is_null());
    assert!(
        env.warnings
            .lines()
            .iter()
            .any(|w| w.contains("no bill says what is owed on it")),
        "{:?}",
        env.warnings.lines()
    );
    let mut out = Vec::new();
    let e = readers::export(&site, &server.base, &mut out, None, None, None)
        .err()
        .unwrap();
    assert_eq!(e.exit_code(), 5);
    assert!(out.is_empty(), "nothing is streamed");
    let bills = readers::bills_list(&site, false, &RangeArgs::default()).unwrap();
    assert!(bills["items"][0]["owes"].is_null(), "{bills}");
}

/// The member API takes no date, so `--since` drops nothing: the header
/// names every type unbounded (DESIGN §1). An annual fee whose period
/// began before `since` and was paid after it still arrives `balanced`.
#[test]
fn records_export_since_drops_nothing_and_says_so() {
    let server = serve(reader_routes());
    let env = reader_env(&server, "since");
    let site = Site::open(&env, &adult1()).unwrap();
    let mut out = Vec::new();
    readers::export(&site, &server.base, &mut out, None, None, Some("2025-6-1")).unwrap();
    let lines = stream_of(&out);
    assert_eq!(lines[0]["since"], "2025-06-01");
    assert_eq!(
        lines[0]["since_unbounded"],
        json!(["Organization", "Patient", "Account", "Invoice", "PaymentReconciliation"])
    );
    let mut whole = Vec::new();
    readers::export(&site, &server.base, &mut whole, None, None, None).unwrap();
    let resources = |l: &[serde_json::Value]| l[1..l.len() - 1].to_vec();
    let whole = stream_of(&whole);
    assert_eq!(resources(&lines).len(), resources(&whole).len());
    assert_eq!(of_type(&lines, "Invoice").len(), 3);
    assert_eq!(of_type(&lines, "PaymentReconciliation").len(), 1);
    let warnings = env.warnings.lines();
    assert_eq!(
        warnings
            .iter()
            .filter(|w| w.contains("--since only goes in the stream's header"))
            .count(),
        1,
        "only the bounded export warns: {warnings:?}"
    );
}

#[test]
fn records_export_takes_only_this_sign_ins_patient_and_clinic() {
    let server = serve(reader_routes());
    let env = reader_env(&server, "ids");
    let site = Site::open(&env, &adult1()).unwrap();
    let patient = readers::patients(&site).unwrap()["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut out = Vec::new();
    readers::export(
        &site,
        &server.base,
        &mut out,
        Some(&patient),
        Some("org-exampleclinic"),
        None,
    )
    .unwrap();
    for (p, o) in [(Some("patient-other"), None), (None, Some("org-other"))] {
        let mut out = Vec::new();
        let e = readers::export(&site, &server.base, &mut out, p, o, None)
            .err()
            .unwrap();
        assert_eq!(e.exit_code(), 4, "{e}");
        assert!(out.is_empty());
    }
}

/// Every failure of a read comes before the header: nothing reaches the
/// stream, and the error is the command's.
#[test]
fn a_history_short_of_its_total_or_unreadable_is_exit_5_before_the_stream() {
    let short = with(reader_routes(), &page_at(2), r#"{"total": 8, "data": []}"#);
    let three_places = with(
        reader_routes(),
        &page_at(1),
        &edited(
            HISTORY_1,
            r#""amount": 600.00,
      "outstandingBalance": 600.00"#,
            r#""amount": 600.005,
      "outstandingBalance": 600.00"#,
        ),
    );
    let not_json = {
        let mut r = reader_routes();
        r.insert(page_at(1), (200, "text/html", "<p>maintenance</p>".into()));
        r
    };
    for (tag, routes, says) in [
        ("short", short, "counts 8 rows and its pages held 3"),
        ("places", three_places, "MemberPaymentHistoryAll.data.amount"),
        ("html", not_json, "with no JSON"),
    ] {
        let server = serve(routes);
        let env = reader_env(&server, tag);
        let site = Site::open(&env, &adult1()).unwrap();
        let mut out = Vec::new();
        let e = readers::export(&site, &server.base, &mut out, None, None, None)
            .err()
            .unwrap();
        assert_eq!(e.exit_code(), 5, "{tag}: {e}");
        assert!(e.to_string().contains(says), "{tag}: {e}");
        assert!(!e.to_string().contains("600.005"), "{e}");
        assert!(out.is_empty(), "{tag}");
    }
}

#[test]
fn a_page_shorter_than_asked_is_read_on_to_the_total_and_a_repeat_once() {
    // Page 2 repeats the first page's last row, as an offset page does when
    // a row arrives between two reads; the total counts it.
    let repeated = edited(
        HISTORY_2,
        r#""data": ["#,
        r#""data": [{"id": 503, "memberPaymentTypeId": "AnnualStorageFee", "timestamp": "2025-01-02T08:00:00", "amount": 600.00, "outstandingBalance": 0.00},"#,
    );
    let server = serve(with(
        reader_routes(),
        &page_at(2),
        &edited(&repeated, r#""total": 8"#, r#""total": 9"#),
    ));
    let env = reader_env(&server, "repeat");
    let site = Site::open(&env, &adult1()).unwrap();
    // The total, 9, is the 8 rows and the repeat: page 2 holds 6.
    let e = readers::bills_list(&site, false, &RangeArgs::default());
    assert_eq!(ids(&e.unwrap()), ["501", "503", "506"]);
}

/// A server that ignores `pageNumber` answers page 1 again: exit 5 at page
/// 2, not a read on to the page cap.
#[test]
fn a_history_that_does_not_page_is_exit_5_at_its_second_page() {
    let server = serve(with(reader_routes(), &page_at(2), HISTORY_1));
    let env = reader_env(&server, "no-paging");
    let site = Site::open(&env, &adult1()).unwrap();
    let e = readers::bills_list(&site, false, &RangeArgs::default()).err().unwrap();
    assert_eq!(e.exit_code(), 5);
    assert!(
        e.to_string().contains("page 2 with the rows of the pages before it"),
        "{e}"
    );
    assert_eq!(targets(&server).len(), 4);
}

#[test]
fn a_clinic_billing_in_another_currency_lists_but_does_not_export() {
    let server = serve(with(
        reader_routes(),
        CLINIC_AT,
        &edited(CLINIC_FIXTURE, r#""USD""#, r#""cad""#),
    ));
    let env = reader_env(&server, "currency");
    let site = Site::open(&env, &adult1()).unwrap();
    assert_eq!(readers::accounts_list(&site).unwrap()["items"][0]["currency"], "CAD");
    let mut out = Vec::new();
    let e = readers::export(&site, &server.base, &mut out, None, None, None)
        .err()
        .unwrap();
    assert_eq!(e.exit_code(), 5);
    assert!(e.to_string().contains("bills in CAD"), "{e}");
    assert!(out.is_empty());
}

/// Open bills that do not add up to the account's balance: the listings
/// warn, and the export writes nothing, since a store would sum the open
/// Invoices into a wrong balance.
#[test]
fn a_balance_the_open_bills_do_not_add_to_stops_the_export() {
    let server = serve(with(
        reader_routes(),
        MEMBER_AT,
        &edited(
            MEMBER_FIXTURE,
            r#""billingBalance": 600.00"#,
            r#""billingBalance": 650.00"#,
        ),
    ));
    let env = reader_env(&server, "balance");
    let site = Site::open(&env, &adult1()).unwrap();
    readers::accounts_list(&site).unwrap();
    readers::bills_list(&site, true, &RangeArgs::default()).unwrap();
    let warnings = env.warnings.lines();
    assert_eq!(
        warnings
            .iter()
            .filter(|w| w.contains("add to 600.00") && w.contains("650.00 owed"))
            .count(),
        2,
        "{warnings:?}"
    );
    let mut out = Vec::new();
    let e = readers::export(&site, &server.base, &mut out, None, None, None)
        .err()
        .unwrap();
    assert_eq!(e.exit_code(), 5);
    assert!(e.to_string().contains("writes no stream"), "{e}");
    assert!(
        !e.to_string().contains("600.00") && !e.to_string().contains("650.00"),
        "no figures: {e}"
    );
    assert!(out.is_empty());
}

/// The clinic's answer gives only its name and currency: a failure is a
/// warning for the listings, which name the clinic from the account, and
/// stops the export, which needs the currency. A sign-in that has ended is
/// exit 3 for all.
#[test]
fn a_clinic_not_read_is_a_warning_for_the_listings_and_stops_the_export() {
    let mut down = reader_routes();
    down.insert(
        CLINIC_AT.into(),
        (404, "application/json", r#"{"message":"not found"}"#.into()),
    );
    let no_currency = with(
        reader_routes(),
        CLINIC_AT,
        r#"{"id": 7, "name": "Example Fertility Clinic", "currencySymbol": "$"}"#,
    );
    for (tag, routes, warned) in [("clinic-down", down, true), ("clinic-no-currency", no_currency, false)] {
        let server = serve(routes);
        let env = reader_env(&server, tag);
        let site = Site::open(&env, &adult1()).unwrap();
        let v = readers::accounts_list(&site).unwrap();
        assert_eq!(v["items"][0]["provider"], "Example Fertility Clinic", "{tag}");
        assert!(v["items"][0]["currency"].is_null(), "{tag}");
        assert_eq!(
            env.warnings
                .lines()
                .iter()
                .any(|w| w.contains("the clinic was not read")),
            warned,
            "{tag}: {:?}",
            env.warnings.lines()
        );
        let mut out = Vec::new();
        let e = readers::export(&site, &server.base, &mut out, None, None, None)
            .err()
            .unwrap();
        assert_eq!(e.exit_code(), 5, "{tag}");
        assert!(e.to_string().contains("currency was not read"), "{tag}: {e}");
        assert!(out.is_empty());
    }
    let mut refused = reader_routes();
    refused.insert(CLINIC_AT.into(), (401, "application/json", String::new()));
    let server = serve(refused);
    let env = reader_env(&server, "clinic-refused");
    let site = Site::open(&env, &adult1()).unwrap();
    assert_eq!(
        readers::bills_list(&site, false, &RangeArgs::default())
            .err()
            .unwrap()
            .exit_code(),
        3
    );
}

/// A type the app's list does not name, with no fee: neither a bill nor a
/// payment, a warning naming the type, and an Invoice skip, so a store
/// infers no dropped bill from a stream that may lack one.
#[test]
fn a_row_of_an_unknown_type_naming_no_fee_is_no_bill_and_an_invoice_skip() {
    let page = edited(
        HISTORY_2,
        r#""memberPaymentTypeId": "MemberDisputedChargeResolutionProcessInitiated""#,
        r#""memberPaymentTypeId": "WriteOff""#,
    );
    let server = serve(with(reader_routes(), &page_at(2), &page));
    let env = reader_env(&server, "unknown");
    let site = Site::open(&env, &adult1()).unwrap();
    assert_eq!(
        ids(&readers::bills_list(&site, false, &RangeArgs::default()).unwrap()),
        ["501", "503", "506"]
    );
    assert_eq!(
        ids(&readers::payments_list(&site, &RangeArgs::default()).unwrap()),
        ["504", "502", "505"]
    );
    let warnings = env.warnings.lines();
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("1 billing-history row(s) of a type") && w.contains("(WriteOff)")),
        "{warnings:?}"
    );
    let mut out = Vec::new();
    assert!(
        readers::export(&site, &server.base, &mut out, None, None, None)
            .unwrap()
            .trailer
            .complete
    );
    let lines = stream_of(&out);
    let skipped = lines.last().unwrap()["skipped"].as_array().unwrap().clone();
    let invoice_skips: Vec<&serde_json::Value> = skipped.iter().filter(|s| s["resource_type"] == "Invoice").collect();
    assert_eq!(invoice_skips.len(), 1, "{skipped:?}");
    assert_eq!(invoice_skips[0]["code"], "unsupported");
    assert!(
        !lines.last().unwrap().to_string().contains("WriteOff"),
        "the type stays out of the stream"
    );
    assert_eq!(of_type(&lines, "Invoice").len(), 3);
}

#[test]
fn documents_are_none_and_send_nothing() {
    assert!(readers::documents().is_empty());
    let one = DownloadArgs {
        id: Some("statement-1".into()),
        all: false,
        output: None,
        overwrite: false,
    };
    let e = readers::download(&one).err().unwrap();
    assert_eq!(e.exit_code(), 4);
    assert!(e.to_string().contains("serves no statement or receipt files"), "{e}");
    let dir = temp_dir("documents");
    let all = DownloadArgs {
        id: None,
        all: true,
        output: Some(dir.clone()),
        overwrite: false,
    };
    let batch = readers::download(&all).unwrap();
    assert_eq!(batch.count, 0);
    assert!(dir.is_dir());
    let _ = std::fs::remove_dir_all(dir);
}
