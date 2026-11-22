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

use embryooptions::client::api_path;
use embryooptions::portal::{Env, Site, Warnings};
use embryooptions::session::{Session, SessionStore};
use embryooptions::site::Clinic;
use embryooptions::token::{test_token, Tokens};
use pk_cli_auth::refresh_lock::RefreshLock;
use pk_cli_auth::vault::{MemoryKeys, Vault};
use pk_cli_config::profile::ProfileName;
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
