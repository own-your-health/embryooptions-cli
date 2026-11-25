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

/// The member's account and billing history, synthetic, for the tests that
/// return an answer whole without reading its fields.
const ACCOUNT: &str = r#"{"memberAccount":{"unpaidBalance":648.42}}"#;
const HISTORY: &str = r#"{"total": 8, "data": [
{"id": 2693, "timestamp": "2022-07-19T05:19:11", "memberPaymentTypeId": "ExampleFee", "oneTimeFeeName": "Fee B", "amount": 468.77, "outstandingBalance": 0.00},
{"id": 8521, "timestamp": "2023-05-26T00:26:58", "memberPaymentTypeId": "MemberInitiatedPayment", "amount": -447.85, "memberPaymentMethodId": "CR", "creditCardSystemStatus": "DEC"},
{"id": 3970, "timestamp": "2023-12-26T22:53:24", "memberPaymentTypeId": "ExampleFee", "oneTimeFeeName": "Fee A", "amount": 297.59, "outstandingBalance": 297.59},
{"id": 4675, "timestamp": "2019-08-19T20:50:36", "memberPaymentTypeId": "CHG", "billingNote": "Note 1", "amount": 616.72, "outstandingBalance": 350.83},
{"id": 2410, "timestamp": "2021-04-20T23:38:04", "memberPaymentTypeId": "CenterInitiatedPayment", "amount": -132.44, "transactionStatus": "processing"},
{"id": 3714, "timestamp": "2024-10-07T06:29:31", "memberPaymentTypeId": "MemberInitiatedPayment", "amount": -206.23, "memberPaymentMethodId": "CR", "creditCardSystemStatus": "ACC"},
{"id": 7979, "timestamp": "2019-12-27T12:50:40", "memberPaymentTypeId": "CHG", "billingNote": "Note 2", "amount": 341.63, "outstandingBalance": 0.00},
{"id": 8176, "timestamp": "2022-02-25T05:16:47", "memberPaymentTypeId": "DirectPaymentToCenterByCheck", "amount": -49.86, "memberPaymentMethodId": "CH"}]}"#;

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
    assert_eq!(a["provider"], "Example Clinic");
    assert_eq!(a["currency"], "USD");
    assert_eq!(a["owes"].to_string(), "509.29");
    assert_eq!(a["admin_fees"].to_string(), "14.41");
    assert_eq!(
        (a["bills"].as_u64(), a["open_bills"].as_u64(), a["payments"].as_u64()),
        (Some(4), Some(2), Some(6))
    );
    assert_eq!(a["owed_on_bills"].to_string(), "509.29");
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
    assert_eq!(next["date"], "2999-04-14");
    assert_eq!(next["amount"].to_string(), "443.89");
    assert_eq!(next["freeze_date"], "2022-12-28");
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
    assert_eq!(ids(&all), ["2271", "8077", "5561", "2366"]);
    let b = &all["items"][0];
    assert_eq!(
        (b["date"].as_str(), b["end"].as_str(), b["posted"].as_str()),
        (Some("2023-11-25"), Some("2024-04-02"), Some("2023-11-10"))
    );
    assert_eq!(b["description"], "Fee B");
    assert_eq!(b["freeze_date"], "2023-02-20");
    let owed = |i: usize| {
        let b = &all["items"][i];
        (
            b["billed"].to_string(),
            b["owes"].to_string(),
            b["open"].as_bool().unwrap(),
        )
    };
    assert_eq!(owed(0), ("455.46".into(), "455.46".into(), true));
    assert_eq!(owed(1), ("765.08".into(), "0.00".into(), false));
    assert_eq!(owed(2), ("595.03".into(), "53.83".into(), true), "owed in part");
    assert_eq!(owed(3), ("105.11".into(), "0.00".into(), false));
    // .NET's empty date is no period and no freeze: the charge is dated by
    // its posting.
    let undated = &all["items"][1];
    assert_eq!(undated["date"], "2023-05-01");
    assert!(undated["end"].is_null() && undated["freeze_date"].is_null());

    let range = |since: Option<&str>, until: Option<&str>, limit: Option<u32>| RangeArgs {
        since: since.map(str::to_string),
        until: until.map(str::to_string),
        limit,
    };
    assert_eq!(
        ids(&readers::bills_list(&site, true, &RangeArgs::default()).unwrap()),
        ["2271", "5561"]
    );
    assert_eq!(
        ids(&readers::bills_list(&site, false, &range(Some("2022-3-7"), None, None)).unwrap()),
        ["2271", "8077"]
    );
    assert_eq!(
        ids(&readers::bills_list(&site, false, &range(None, Some("2022-03-07"), None)).unwrap()),
        ["5561", "2366"]
    );
    assert_eq!(
        ids(&readers::bills_list(&site, false, &range(None, None, Some(1))).unwrap()),
        ["2271"]
    );
}

#[test]
fn bills_show_has_what_the_history_says_and_only_charges_are_bills() {
    let server = serve(reader_routes());
    let env = reader_env(&server, "show");
    let site = Site::open(&env, &adult1()).unwrap();
    let v = readers::bill_show(&site, " 2271 ").unwrap();
    assert_eq!(v["schema"], "bill/v1");
    assert_eq!(v["provider"], "Example Provider");
    assert_eq!(v["lab"], "Example Lab");
    assert_eq!(v["lab_tax_id"], "00-0000000");
    assert_eq!(v["codes"], json!(["12345", "A1234"]));
    assert_eq!(v["note"], "Note 2");
    assert_eq!(v["discount_type"], "Discount A");
    assert_eq!(v["allocated_discount"].to_string(), "3.24");
    assert_eq!(v["charges"][0]["code"], "12345");
    assert_eq!(v["charges"][0]["amount"].to_string(), "455.46");
    for id in ["8545", "9999", "6379"] {
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
    assert_eq!(v["schema"], "payment-list/v2");
    assert_eq!(
        ids(&v),
        ["6379", "5995", "6792", "8670", "8148", "8545", "7024", "2435"]
    );
    let p = |i: usize| &v["items"][i];
    assert_eq!(
        (
            p(7)["kind"].as_str(),
            p(7)["method"].as_str(),
            p(7)["settled"].as_bool()
        ),
        (Some("payment"), Some("PayPal"), Some(false))
    );
    assert_eq!(
        (
            p(5)["amount"].to_string().as_str(),
            p(5)["method"].as_str(),
            p(5)["settled"].as_bool()
        ),
        ("315.31", Some("Visa"), Some(true))
    );
    for (i, amount) in [(0, "34.68"), (2, "61.27")] {
        assert_eq!(
            (p(i)["kind"].as_str(), p(i)["amount"].to_string().as_str()),
            (Some("discount"), amount)
        );
    }
    let states: Vec<&str> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["state"].as_str().unwrap())
        .collect();
    assert_eq!(
        states,
        [
            "settled",
            "settled",
            "settled",
            "unconfirmed",
            "pending",
            "settled",
            "failed",
            "failed"
        ]
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
    // The settled payments it writes carry a `transactionAuthReference`.
    assert!(HISTORY_1.contains(r#""transactionAuthReference": "dummy-auth-ref""#));
    for absent in [
        "4242",
        "1111",
        "dummy-auth-ref",
        "transactionAuthReference",
        "member-one-dummy",
    ] {
        assert!(!text.contains(absent), "{absent}");
    }
    // Amounts keep the site's spelling.
    for amount in [r#""value":455.46"#, r#""value":0.00"#] {
        assert!(text.contains(amount), "{text}");
    }
    let lines = stream_of(&out);
    let header = &lines[0];
    assert_eq!(header["schema"], "records-stream/v1");
    assert_eq!(header["method"], "portal");
    assert_eq!(header["organization"]["id"], "org-exampleclinic");
    assert_eq!(header["organization"]["name"], "Example Clinic");
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
            "Invoice",
            "PaymentReconciliation",
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
    let open = by_id("bill-1001-2271");
    assert_eq!(open["status"], "issued");
    assert_eq!(open["totalNet"]["value"].to_string(), "455.46");
    assert_eq!(open["totalGross"], open["totalNet"]);
    assert_eq!(open["account"]["reference"], "Account/account-1001");
    assert_eq!(open["date"], "2023-11-10");
    assert_eq!(open["type"]["text"], "Fee B (freeze date 2023-02-20)");
    assert_eq!(open["participant"][0]["actor"]["display"], "Example Provider");
    let charge = &open["contained"][0];
    assert_eq!(charge["occurrenceDateTime"], "2023-11-25");
    assert_eq!(charge["code"]["coding"][0]["system"], "http://www.ama-assn.org/go/cpt");
    assert_eq!(charge["code"]["coding"][0]["code"], "12345");
    let billed = open["totalPriceComponent"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["code"]["coding"][0]["code"] == "billed")
        .unwrap();
    assert_eq!(
        (billed["type"].as_str(), billed["amount"]["value"].to_string().as_str()),
        (Some("base"), "455.46")
    );
    let in_part = by_id("bill-1001-5561");
    assert_eq!(in_part["status"], "issued");
    assert_eq!(in_part["totalNet"]["value"].to_string(), "53.83");
    assert_eq!(by_id("bill-1001-2366")["status"], "balanced");
    // A charge naming no code is one line of its own.
    let uncoded = by_id("bill-1001-8077");
    assert_eq!(uncoded["status"], "balanced");
    assert_eq!(uncoded["contained"][0]["occurrenceDateTime"], "2023-05-01");
    assert_eq!(uncoded["contained"][0]["code"]["text"], "Fee A");

    let payments = of_type(&lines, "PaymentReconciliation");
    for (i, id, date, amount, method) in [
        (0, "payment-8545", "2020-03-11", "315.31", "Paid by Visa"),
        (1, "payment-5995", "2022-04-13", "201.72", "Paid by MasterCard"),
    ] {
        assert_eq!(payments[i]["id"], id);
        assert_eq!(payments[i]["paymentDate"], date);
        assert_eq!(payments[i]["paymentAmount"]["value"].to_string(), amount);
        assert_eq!(payments[i]["disposition"], method);
    }

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
    let reasons: Vec<&str> = trailer["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["reason"].as_str().unwrap())
        .collect();
    for (reason, says) in reasons.iter().zip([
        "2 payment(s) turned down (declined, failed, voided), 1 scheduled or pending and 1 not shown to have settled",
        "2 payment(s) came without an id",
        "2 discount(s)",
        "1 row(s) of the billing history that move no money",
    ]) {
        assert!(reason.starts_with(says), "{reason}");
    }
    assert!(env.warnings.lines().is_empty(), "{:?}", env.warnings.lines());
}

/// A scheduled payment, one the processor has not finished, and one with no
/// status are never written as paid.
#[test]
fn records_export_never_writes_a_payment_that_has_not_settled() {
    let mut page: serde_json::Value = serde_json::from_str(HISTORY_2).unwrap();
    let rows = page["data"].as_array_mut().unwrap();
    let row = |id: u64, kind: &str, at: &str, amount: f64, status: serde_json::Value| {
        json!({
            "id": id,
            "memberPaymentTypeId": kind,
            "timestamp": at,
            "amount": amount,
            "creditCardSystemStatus": null,
            "transactionStatus": status,
        })
    };
    rows.push(row(
        901,
        "ScheduledInstallmentPayment",
        "2999-09-02T21:26:10",
        -455.90,
        json!("settled"),
    ));
    rows.push(row(
        902,
        "MemberInitiatedPayment",
        "2023-08-10T03:25:35",
        -513.84,
        json!("submitted_for_settlement"),
    ));
    rows.push(row(
        903,
        "MemberInitiatedPayment",
        "2019-01-27T20:34:00",
        -98.41,
        json!(null),
    ));
    // A word no list of states holds.
    rows.push(row(904, "PAY", "2021-08-23T17:19:15", -856.87, json!("Example word")));
    let total = 15 + 4;
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
    assert_eq!(state("904"), ("unconfirmed".into(), false));
    assert_eq!(state("8545"), ("settled".into(), true));

    let mut out = Vec::new();
    readers::export(&site, &server.base, &mut out, None, None, None).unwrap();
    let lines = stream_of(&out);
    let ids: Vec<_> = of_type(&lines, "PaymentReconciliation")
        .iter()
        .map(|p| p["id"].clone())
        .collect();
    assert_eq!(ids, [json!("payment-8545"), json!("payment-5995")]);
    let skipped = lines.last().unwrap()["skipped"].to_string();
    assert!(
        skipped.contains("3 scheduled or pending and 3 not shown to have settled"),
        "{skipped}"
    );
}

/// The member API takes no date, so `--since` drops nothing: the header
/// names every type unbounded (DESIGN §1). A settled charge whose period
/// began before `since` still arrives, `balanced`.
#[test]
fn records_export_since_drops_nothing_and_says_so() {
    let server = serve(reader_routes());
    let env = reader_env(&server, "since");
    let site = Site::open(&env, &adult1()).unwrap();
    let mut out = Vec::new();
    readers::export(&site, &server.base, &mut out, None, None, Some("2022-6-1")).unwrap();
    let lines = stream_of(&out);
    assert_eq!(lines[0]["since"], "2022-06-01");
    assert_eq!(
        lines[0]["since_unbounded"],
        json!(["Organization", "Patient", "Account", "Invoice", "PaymentReconciliation"])
    );
    let mut whole = Vec::new();
    readers::export(&site, &server.base, &mut whole, None, None, None).unwrap();
    let resources = |l: &[serde_json::Value]| l[1..l.len() - 1].to_vec();
    let whole = stream_of(&whole);
    assert_eq!(resources(&lines).len(), resources(&whole).len());
    assert_eq!(of_type(&lines, "Invoice").len(), 4);
    assert_eq!(of_type(&lines, "PaymentReconciliation").len(), 2);
    let before = of_type(&lines, "Invoice")
        .into_iter()
        .find(|i| i["id"] == "bill-1001-2366")
        .unwrap();
    assert_eq!(before["status"], "balanced");
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
    let short = with(reader_routes(), &page_at(2), r#"{"total": 15, "data": []}"#);
    let three_places = with(
        reader_routes(),
        &page_at(1),
        &edited(
            HISTORY_1,
            r#""amount": 455.46,
      "outstandingBalance": 455.46"#,
            r#""amount": 455.465,
      "outstandingBalance": 455.46"#,
        ),
    );
    let not_json = {
        let mut r = reader_routes();
        r.insert(page_at(1), (200, "text/html", "<p>maintenance</p>".into()));
        r
    };
    for (tag, routes, says) in [
        ("short", short, "counts 15 rows and its pages held 5"),
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
        assert!(!e.to_string().contains("455.465"), "{e}");
        assert!(out.is_empty(), "{tag}");
    }
}

#[test]
fn a_page_shorter_than_asked_is_read_on_to_the_total_and_a_repeat_once() {
    // Page 2 repeats the first page's last row, a charge, as an offset page
    // does when a row arrives between two reads; its total counts that row.
    let repeated = edited(
        HISTORY_2,
        r#""data": ["#,
        r#""data": [{"id": 2366, "memberPaymentTypeId": "ExampleOtherFee", "timestamp": "2020-07-22T16:01:50", "amount": 105.11, "outstandingBalance": 0.00, "recurringFeeName": "Fee D"},"#,
    );
    let server = serve(with(
        reader_routes(),
        &page_at(2),
        &edited(&repeated, r#""total": 15"#, r#""total": 16"#),
    ));
    let env = reader_env(&server, "repeat");
    let site = Site::open(&env, &adult1()).unwrap();
    // Page 2 holds 11 rows: the repeat, and the 10 that make up the first
    // answer's total, 15.
    let e = readers::bills_list(&site, false, &RangeArgs::default());
    assert_eq!(ids(&e.unwrap()), ["2271", "8077", "5561", "2366"]);
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
            r#""billingBalance": 509.29"#,
            r#""billingBalance": 571.90"#,
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
            .filter(|w| w.contains("add to 509.29") && w.contains("571.90 owed") && w.contains("not yet been seen"))
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
        !e.to_string().contains("509.29") && !e.to_string().contains("571.90"),
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
        r#"{"id": 7, "name": "Example Clinic", "currencySymbol": "$"}"#,
    );
    for (tag, routes, warned) in [("clinic-down", down, true), ("clinic-no-currency", no_currency, false)] {
        let server = serve(routes);
        let env = reader_env(&server, tag);
        let site = Site::open(&env, &adult1()).unwrap();
        let v = readers::accounts_list(&site).unwrap();
        assert_eq!(v["items"][0]["provider"], "Example Clinic", "{tag}");
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
        ["2271", "8077", "5561", "2366"]
    );
    assert_eq!(
        ids(&readers::payments_list(&site, &RangeArgs::default()).unwrap()),
        ["6379", "5995", "6792", "8670", "8148", "8545", "7024", "2435"]
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
    assert_eq!(of_type(&lines, "Invoice").len(), 4);
}

// -------------------------------------------- the forms a field comes in

/// One page of history, synthetic: each row in a mix of forms no other row
/// has, with optional fields of its own.
const FORMS_FIXTURE: &str = include_str!("fixtures/history-forms.json");

/// The member's account owing `balance`, with `history` its one page.
fn one_page(balance: &str, history: &str) -> Routes {
    let mut r = with(
        reader_routes(),
        MEMBER_AT,
        &edited(
            MEMBER_FIXTURE,
            r#""billingBalance": 509.29"#,
            &format!(r#""billingBalance": {balance}"#),
        ),
    );
    r.insert(page_at(1), ok(history));
    r.remove(&page_at(2));
    r
}

fn forms_routes() -> Routes {
    one_page("460.35", FORMS_FIXTURE)
}

fn items(v: &serde_json::Value) -> Vec<serde_json::Value> {
    v["items"].as_array().unwrap().clone()
}

/// An amount as printed, or `null`.
fn printed(v: &serde_json::Value) -> serde_json::Value {
    if v.is_null() {
        json!(null)
    } else {
        json!(v.to_string())
    }
}

/// The form of a date: whether it is .NET's, and whether it is empty.
fn date_form(s: &str) -> &'static str {
    match (s.contains('T'), s.contains("0001")) {
        (true, false) => "date-time",
        (true, true) => "empty date-time",
        (false, false) => "written out",
        (false, true) => "empty written out",
    }
}

/// The forms a row's fields come in, as `(field, form)`.
fn forms(row: &serde_json::Value) -> Vec<(&'static str, &'static str)> {
    let text = |k: &str| row[k].as_str();
    let mut out = Vec::new();
    for k in ["amount", "outstandingBalance", "allocationDiscountAmount"] {
        if row[k].is_number() {
            out.push((k, "number"));
        } else if row[k].is_string() {
            out.push((k, "string"));
        }
    }
    for k in ["timestamp", "billingCycleStartDate", "billingCycleEndDate"] {
        out.extend(text(k).map(|s| (k, date_form(s))));
    }
    let freeze = row["memberFreezeEvent"]["freezeDate"]
        .as_str()
        .or(text("memberFreezeEventDate"));
    out.extend(freeze.map(|s| ("freezeDate", date_form(s))));
    let type_form = |s: &str| if s.len() == 3 { "type code" } else { "type word" };
    out.extend(text("memberPaymentTypeId").map(|s| ("memberPaymentTypeId", type_form(s))));
    for k in ["cptcode1", "cptcode2", "cptcode3"] {
        out.extend(text(k).map(|s| ("cptcode", if s == "N/A" { "N/A" } else { "a code" })));
    }
    out
}

/// The fixture the tests below read holds every form of every field, and no
/// two of its rows have the same set of forms or the same fields.
#[test]
fn the_forms_fixture_holds_every_form_and_no_two_rows_alike() {
    use std::collections::BTreeSet;
    let page: serde_json::Value = serde_json::from_str(FORMS_FIXTURE).unwrap();
    let rows = page["data"].as_array().unwrap();
    assert_eq!(page["total"], rows.len());
    let held: BTreeSet<(&str, &str)> = rows.iter().flat_map(forms).collect();
    let dates = ["date-time", "written out", "empty date-time", "empty written out"];
    for (field, want) in [
        ("amount", &["number", "string"][..]),
        ("outstandingBalance", &["number", "string"]),
        ("allocationDiscountAmount", &["number", "string"]),
        ("timestamp", &dates[..2]),
        ("billingCycleStartDate", &dates),
        ("billingCycleEndDate", &dates),
        ("freezeDate", &dates[..2]),
        ("memberPaymentTypeId", &["type code", "type word"]),
        ("cptcode", &["a code", "N/A"]),
    ] {
        for form in want {
            assert!(held.contains(&(field, *form)), "{field}: {form}");
        }
    }
    let sets: BTreeSet<BTreeSet<&str>> = rows
        .iter()
        .map(|r| {
            forms(r)
                .into_iter()
                .map(|(_, form)| form.trim_start_matches("empty "))
                .filter(|form| *form != "a code")
                .collect()
        })
        .collect();
    assert_eq!(sets.len(), rows.len(), "each row has its own set of forms");
    let fields: BTreeSet<BTreeSet<&String>> = rows.iter().map(|r| r.as_object().unwrap().keys().collect()).collect();
    assert_eq!(fields.len(), rows.len(), "each row has its own fields");
}

/// A charge's fields read the same in whichever form they come: amounts as
/// numbers and strings, dates as date-times and written out, types as words,
/// codes and none, and `N/A` as no code. The charges that say what is owed on
/// them account for the balance, so those that do not are owed 0.
#[test]
fn a_charge_s_fields_are_read_in_every_form() {
    let server = serve(forms_routes());
    let env = reader_env(&server, "forms-bills");
    let site = Site::open(&env, &adult1()).unwrap();
    let bills: Vec<serde_json::Value> = items(&readers::bills_list(&site, false, &RangeArgs::default()).unwrap())
        .iter()
        .map(|b| {
            let shown = readers::bill_show(&site, b["id"].as_str().unwrap()).unwrap();
            json!([
                b["id"],
                b["date"],
                b["end"],
                b["posted"],
                b["type"],
                b["description"],
                b["freeze_date"],
                printed(&b["billed"]),
                printed(&b["owes"]),
                b["open"],
                shown["codes"],
                printed(&shown["allocated_discount"]),
            ])
        })
        .collect();
    #[rustfmt::skip]
    let want = json!([
        ["7503", "2024-08-01", null, "2024-08-01", "ExampleFee", "Fee E", "2023-04-09", "367.39", "0.00", false, [], "25.30"],
        ["2612", "2023-05-11", "2023-06-23", "2023-05-24", "ExampleFee", "Fee C", null, "744.54", "0.00", false, ["12345", "A1234"], null],
        ["3772", "2022-09-27", null, "2022-09-27", "CHG", "Fee H", "2021-03-21", "860.70", "0.00", false, ["12345"], null],
        ["4982", "2022-06-14", null, "2022-06-14", "ExampleFee", "Fee F", "2024-08-26", "576.00", "0.00", false, ["A1234"], "14.51"],
        ["5900", "2022-02-09", null, "2022-02-09", "chg", "chg", "2020-12-28", "320.60", "137.57", true, ["A1234", "12345"], null],
        ["8638", "2020-12-01", null, "2020-11-08", "CHG", "Fee A", null, "392.53", "322.78", true, [], null],
        ["6491", "2020-08-02", "2020-11-30", "2020-07-28", "ExampleFee", "Fee B", null, "67.00", "0.00", false, [], null],
        ["6774", "2020-04-20", "2020-07-28", "2020-03-22", "ExampleFee", "Fee G", null, "117.00", "0.00", false, ["A1234"], "27.00"],
        ["2585", "2019-08-27", "2019-09-21", "2019-09-13", null, "Fee D", "2021-06-07", "329.00", "0.00", false, ["12345"], "33.87"],
    ]);
    assert_eq!(json!(bills), want);
    let a = &items(&readers::accounts_list(&site).unwrap())[0];
    assert_eq!(
        (a["bills"].as_u64(), a["open_bills"].as_u64(), a["payments"].as_u64()),
        (Some(9), Some(2), Some(7))
    );
    // The charges that say what is owed on them add to the balance.
    assert_eq!(
        (a["owes"].to_string(), a["owed_on_bills"].to_string()),
        ("460.35".into(), "460.35".into())
    );
    assert!(env.warnings.lines().is_empty(), "{:?}", env.warnings.lines());
}

/// The same for the payments, refunds, discounts and credits, with a
/// `transactionStatus` and without.
#[test]
fn a_payment_s_fields_are_read_in_every_form() {
    let server = serve(forms_routes());
    let env = reader_env(&server, "forms-payments");
    let site = Site::open(&env, &adult1()).unwrap();
    let v = readers::payments_list(&site, &RangeArgs::default()).unwrap();
    let payments: Vec<serde_json::Value> = items(&v)
        .iter()
        .map(|p| {
            assert_eq!(p["settled"], p["state"] == "settled");
            json!([
                p["id"],
                p["date"],
                p["kind"],
                p["type"],
                p["subtype"],
                printed(&p["amount"]),
                p["method"],
                p["note"],
                p["state"],
            ])
        })
        .collect();
    #[rustfmt::skip]
    let want = json!([
        ["4637", "2024-04-15", "credit", "Credit", null, "307.06", null, "Note 5", "settled"],
        ["2549", "2023-12-29", "payment", "PAY", null, "326.60", null, null, "unconfirmed"],
        ["6362", "2023-11-13", "payment", "PAY", null, "168.77", "PayPal", null, "failed"],
        ["8080", "2023-09-18", "discount", "DIS", null, "467.41", null, null, "settled"],
        ["5371", "2022-12-25", "payment", "MemberInitiatedPayment", null, "316.57", "MasterCard", null, "settled"],
        ["6873", "2021-08-27", "refund", "RefundViaCheck", null, "514.60", "Check", "Note 6", "settled"],
        ["8558", "2021-07-24", "discount", "InsuranceDiscount", null, "250.73", null, null, "settled"],
        ["5407", "2021-04-11", "refund", "PAY", null, "394.00", null, null, "settled"],
        ["3634", "2021-02-23", "payment", "DirectPaymentToCenterByCheck", null, "100.00", "Check", null, "settled"],
        ["5019", "2020-06-06", "credit", "CHG", null, "157.20", null, "Note 1", "unconfirmed"],
        ["4162", "2019-11-29", "payment", "CenterInitiatedPayment", null, "69.47", null, "Note 7", "failed"],
        ["5174", "2019-05-22", "payment", "PAY", null, "21.42", "Credit Card", "Note 2", "pending"],
        ["8165", "2019-05-21", "refund", "PAY", "EXAMPLE_REFUND", "121.40", null, "Note 2", "unconfirmed"],
        ["8911", "2019-03-02", "payment", "PAY", null, "101.47", "Discover", "Note 4", "settled"],
    ]);
    assert_eq!(json!(payments), want);
    for absent in ["1111", "dummy-auth-ref"] {
        assert!(
            FORMS_FIXTURE.contains(absent) && !v.to_string().contains(absent),
            "{absent}"
        );
    }
}

/// The export of the same rows: every charge an Invoice with what is owed on
/// it, said or settled by the balance, and the settled payments.
#[test]
fn records_export_writes_rows_of_every_form() {
    let server = serve(forms_routes());
    let env = reader_env(&server, "forms-export");
    let site = Site::open(&env, &adult1()).unwrap();
    let mut out = Vec::new();
    let finished = readers::export(&site, &server.base, &mut out, None, None, None).unwrap();
    assert!(finished.trailer.complete);
    let text = String::from_utf8(out.clone()).unwrap();
    for absent in ["4242", "1111", "dummy-auth-ref", "N/A"] {
        assert!(!text.contains(absent), "{absent}");
    }
    let lines = stream_of(&out);
    let totals: Vec<serde_json::Value> = of_type(&lines, "Invoice")
        .iter()
        .map(|i| json!([i["identifier"][0]["value"], printed(&i["totalNet"]["value"])]))
        .collect();
    assert_eq!(
        json!(totals),
        json!([
            ["2585", "0.00"],
            ["6774", "0.00"],
            ["6491", "0.00"],
            ["8638", "322.78"],
            ["5900", "137.57"],
            ["4982", "0.00"],
            ["3772", "0.00"],
            ["2612", "0.00"],
            ["7503", "0.00"],
        ])
    );
    // A period's start as a date-time, and its end written out.
    assert_eq!(
        of_type(&lines, "Invoice")[2]["contained"][0]["occurrencePeriod"],
        json!({"start": "2020-08-02", "end": "2020-11-30"})
    );
    let payments: Vec<serde_json::Value> = of_type(&lines, "PaymentReconciliation")
        .iter()
        .map(|p| {
            json!([
                p["id"],
                p["paymentDate"],
                printed(&p["paymentAmount"]["value"]),
                p["disposition"]
            ])
        })
        .collect();
    assert_eq!(
        json!(payments),
        json!([
            ["payment-8911", "2019-03-02", "101.47", "Paid by Discover"],
            ["payment-3634", "2021-02-23", "100.00", "Paid by Check"],
            ["payment-5371", "2022-12-25", "316.57", "Paid by MasterCard"],
        ])
    );
    let skipped = lines.last().unwrap()["skipped"].to_string();
    for says in [
        "2 payment(s) turned down (declined, failed, voided), 1 scheduled or pending and 1 not shown to have settled",
        "3 refund(s)",
        "2 discount(s)",
        "2 credit(s)",
    ] {
        assert!(skipped.contains(says), "{says}: {skipped}");
    }
}

// --------------------------------- charges that say nothing of what is owed

/// A history none of whose charges says what is owed on it: the field is
/// missing, or `null`.
fn unsaid_history() -> String {
    let charge = |id: u64, at: &str, kind: &str, fee: Option<&str>, amount: &str| {
        json!({
            "id": id,
            "memberPaymentTypeId": kind,
            "timestamp": at,
            "oneTimeFeeName": fee,
            "billingNote": "Note 1",
            "amount": amount,
        })
    };
    let payment = |id: u64, at: &str, kind: &str, amount: &str, card: Option<&str>, status: Option<&str>| {
        json!({
            "id": id,
            "memberPaymentTypeId": kind,
            "timestamp": at,
            "amount": amount,
            "creditCardSystemStatus": card,
            "transactionStatus": status,
        })
    };
    let check = "DirectPaymentToCenterByCheck";
    let member = "MemberInitiatedPayment";
    let mut null = charge(7056, "2024-12-03T20:34:00", "ExampleFee", Some("Fee C"), "266.69");
    null["outstandingBalance"] = json!(null);
    let rows = [
        payment(6540, "2024-03-02T13:41:21", "PAY", "-259.64", None, Some("settling")),
        charge(3569, "2021-02-08T03:25:35", "CHG", None, "60.31"),
        payment(8688, "2021-04-18T17:48:47", check, "-218.28", None, None),
        charge(3689, "2023-12-31T21:26:10", "ExampleFee", Some("Fee A"), "779.61"),
        payment(4444, "2022-01-14T17:11:40", "PAY", "-494.06", Some("EXP"), None),
        null,
        payment(7552, "2024-05-09T10:31:42", member, "-69.35", None, None),
        charge(3026, "2020-12-12T21:31:42", "CHG", Some("Fee B"), "90.38"),
        payment(
            7825,
            "2021-12-09T17:30:13",
            member,
            "-291.37",
            Some("ACC"),
            Some("settled"),
        ),
    ];
    json!({ "total": rows.len(), "data": rows }).to_string()
}

/// An account that owes nothing owes nothing on any charge: each is owed 0,
/// and the export writes each balanced.
#[test]
fn a_balance_of_zero_settles_the_charges_that_do_not_say() {
    let server = serve(one_page("0.00", &unsaid_history()));
    let env = reader_env(&server, "unsaid-zero");
    let site = Site::open(&env, &adult1()).unwrap();
    let a = &items(&readers::accounts_list(&site).unwrap())[0];
    assert_eq!(
        (a["bills"].as_u64(), a["open_bills"].as_u64(), a["payments"].as_u64()),
        (Some(4), Some(0), Some(5))
    );
    assert_eq!(
        (a["owes"].to_string(), a["owed_on_bills"].to_string()),
        ("0.00".into(), "0.00".into())
    );
    let bills = readers::bills_list(&site, false, &RangeArgs::default()).unwrap();
    assert_eq!(ids(&bills), ["7056", "3689", "3569", "3026"]);
    for b in items(&bills) {
        assert_eq!(
            (b["owes"].to_string(), b["open"].as_bool()),
            ("0.00".into(), Some(false))
        );
    }
    assert!(items(&readers::bills_list(&site, true, &RangeArgs::default()).unwrap()).is_empty());
    let shown = readers::bill_show(&site, "3569").unwrap();
    assert_eq!(shown["owes"].to_string(), "0.00");

    let mut out = Vec::new();
    let finished = readers::export(&site, &server.base, &mut out, None, None, None).unwrap();
    assert!(finished.trailer.complete);
    let lines = stream_of(&out);
    let invoices = of_type(&lines, "Invoice");
    assert_eq!(invoices.len(), 4);
    for i in invoices {
        assert_eq!(i["status"], "balanced", "{}", i["id"]);
        assert_eq!(i["totalNet"]["value"].to_string(), "0.00", "{}", i["id"]);
    }
    assert_eq!(of_type(&lines, "PaymentReconciliation").len(), 2);
    assert!(env.warnings.lines().is_empty(), "{:?}", env.warnings.lines());
}

/// An account that owes, when no charge says what is owed on it: which are
/// open is not known. The listings warn and leave what is owed unsaid, and
/// the export writes nothing.
#[test]
fn a_balance_above_zero_that_no_charge_says_warns_and_stops_the_export() {
    let server = serve(one_page("164.45", &unsaid_history()));
    let env = reader_env(&server, "unsaid-owed");
    let site = Site::open(&env, &adult1()).unwrap();
    let a = &items(&readers::accounts_list(&site).unwrap())[0];
    assert_eq!(a["owes"].to_string(), "164.45");
    assert!(a["owed_on_bills"].is_null());
    assert_eq!((a["bills"].as_u64(), a["open_bills"].as_u64()), (Some(4), Some(0)));
    let warned = || {
        env.warnings
            .lines()
            .iter()
            .filter(|w| w.contains("164.45 owed") && w.contains("no bill says what is owed on it"))
            .count()
    };
    assert_eq!(warned(), 1, "{:?}", env.warnings.lines());
    let bills = readers::bills_list(&site, false, &RangeArgs::default()).unwrap();
    assert_eq!(ids(&bills), ["7056", "3689", "3569", "3026"]);
    for b in items(&bills) {
        assert!(b["owes"].is_null(), "{b}");
        assert_eq!(b["open"], false);
    }
    assert_eq!(warned(), 2, "{:?}", env.warnings.lines());

    let mut out = Vec::new();
    let e = readers::export(&site, &server.base, &mut out, None, None, None)
        .err()
        .unwrap();
    assert_eq!(e.exit_code(), 5);
    assert!(e.to_string().contains("writes no stream"), "{e}");
    assert!(!e.to_string().contains("164.45"), "no figures: {e}");
    assert!(out.is_empty());
}

/// The charges that say what is owed on them do not add up to the balance, a
/// cent off included, or no balance is given: which of the others are open is
/// not known, and they stay unsaid.
#[test]
fn charges_that_do_not_say_stay_unsaid_unless_the_rest_account_for_the_balance() {
    for (balance, account) in [
        ("460.36", "the account shows 460.36 owed"),
        ("460.34", "the account shows 460.34 owed"),
        ("760.16", "the account shows 760.16 owed"),
        ("137.57", "the account shows 137.57 owed"),
        ("0.00", "the account shows 0.00 owed"),
        ("-71.48", "the account shows -71.48 owed"),
        ("null", "the account does not show what is owed"),
    ] {
        let server = serve(one_page(balance, FORMS_FIXTURE));
        let env = reader_env(&server, "forms-unsaid");
        let site = Site::open(&env, &adult1()).unwrap();
        let bills = items(&readers::bills_list(&site, false, &RangeArgs::default()).unwrap());
        let owes: Vec<serde_json::Value> = bills.iter().map(|b| json!([b["id"], printed(&b["owes"])])).collect();
        assert_eq!(
            json!(owes),
            json!([
                ["7503", "0.00"],
                ["2612", null],
                ["3772", null],
                ["4982", null],
                ["5900", "137.57"],
                ["8638", "322.78"],
                ["6491", null],
                ["6774", "0.00"],
                ["2585", null],
            ]),
            "{balance}"
        );
        let open = readers::bills_list(&site, true, &RangeArgs::default()).unwrap();
        assert_eq!(ids(&open), ["5900", "8638"]);
        let a = &items(&readers::accounts_list(&site).unwrap())[0];
        assert_eq!(a["open_bills"], 2);
        assert!(a["owed_on_bills"].is_null(), "{balance}: {a}");
        let warnings = env.warnings.lines();
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        for w in &warnings {
            let says = format!("{account}, and 5 bill(s) do not say what is owed on them");
            assert!(w.starts_with(&says), "{balance}: {w}");
            assert!(w.contains("exit 5") && !w.contains("not yet been seen"), "{w}");
        }

        let mut out = Vec::new();
        let e = readers::export(&site, &server.base, &mut out, None, None, None)
            .err()
            .unwrap();
        assert_eq!(e.exit_code(), 5);
        assert!(
            e.to_string().contains("a bill does not say what is owed on it")
                && e.to_string().contains("writes no stream"),
            "{e}"
        );
        for figure in [balance, "460.35", "137.57", "322.78"] {
            assert!(!e.to_string().contains(figure), "no figures: {e}");
        }
        assert!(out.is_empty(), "{balance}");
    }
}

/// When no charge says what is owed on it, a balance below zero, or none
/// given, leaves that as unknown as one above zero does.
#[test]
fn a_balance_below_zero_or_not_given_settles_no_charge() {
    for (balance, account) in [
        ("-266.47", "the account shows -266.47 owed"),
        ("-0.01", "the account shows -0.01 owed"),
        ("null", "the account does not show what is owed"),
    ] {
        let server = serve(one_page(balance, &unsaid_history()));
        let env = reader_env(&server, "unsaid-other");
        let site = Site::open(&env, &adult1()).unwrap();
        for b in items(&readers::bills_list(&site, false, &RangeArgs::default()).unwrap()) {
            assert_eq!((&b["owes"], &b["open"]), (&json!(null), &json!(false)), "{balance}");
        }
        let a = &items(&readers::accounts_list(&site).unwrap())[0];
        assert!(a["owed_on_bills"].is_null(), "{balance}: {a}");
        let warnings = env.warnings.lines();
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        for w in &warnings {
            let says = format!("{account}, and no bill says what is owed on it");
            assert!(w.starts_with(&says), "{balance}: {w}");
            assert!(w.contains("exit 5") && !w.contains("not yet been seen"), "{w}");
        }
        let mut out = Vec::new();
        let e = readers::export(&site, &server.base, &mut out, None, None, None)
            .err()
            .unwrap();
        assert_eq!(e.exit_code(), 5);
        assert!(e.to_string().contains("writes no stream"), "{e}");
        assert!(!e.to_string().contains(balance), "no figures: {e}");
        assert!(out.is_empty(), "{balance}");
    }
}

/// Charges that all say what is owed on them need no balance to be read, and
/// an account with no charges has none to compare with its balance.
#[test]
fn charges_that_all_say_or_none_at_all_need_no_balance() {
    let server = serve(with(
        reader_routes(),
        MEMBER_AT,
        &edited(MEMBER_FIXTURE, r#""billingBalance": 509.29,"#, ""),
    ));
    let env = reader_env(&server, "said-no-balance");
    let site = Site::open(&env, &adult1()).unwrap();
    let a = &items(&readers::accounts_list(&site).unwrap())[0];
    assert!(a["owes"].is_null());
    assert_eq!(a["owed_on_bills"].to_string(), "509.29");
    let mut out = Vec::new();
    let finished = readers::export(&site, &server.base, &mut out, None, None, None).unwrap();
    assert!(finished.trailer.complete);
    assert_eq!(of_type(&stream_of(&out), "Invoice").len(), 4);
    assert!(env.warnings.lines().is_empty(), "{:?}", env.warnings.lines());

    let rows = [
        payment_row(Some(7719), "2024-12-09T16:51:30", "-30.43", json!("settled")),
        payment_row(Some(3847), "2023-06-22T12:02:15", "-463.00", json!("failed")),
        payment_row(Some(8688), "2019-03-11T01:49:43", "-565.11", json!("completed")),
        payment_row(Some(4404), "2021-04-03T21:09:07", "-710.17", json!("processing")),
        payment_row(Some(6181), "2024-08-09T23:09:56", "-66.59", json!(null)),
        payment_row(Some(2707), "2019-04-22T10:42:57", "-834.08", json!("captured")),
    ];
    let history = json!({ "total": rows.len(), "data": rows }).to_string();
    for balance in ["164.17", "0.00", "-59.21", "null"] {
        let server = serve(one_page(balance, &history));
        let env = reader_env(&server, "no-charges");
        let site = Site::open(&env, &adult1()).unwrap();
        let a = &items(&readers::accounts_list(&site).unwrap())[0];
        assert_eq!((a["bills"].as_u64(), a["payments"].as_u64()), (Some(0), Some(6)));
        assert!(a["owed_on_bills"].is_null(), "{balance}: {a}");
        let mut out = Vec::new();
        let finished = readers::export(&site, &server.base, &mut out, None, None, None).unwrap();
        assert!(finished.trailer.complete, "{balance}");
        let lines = stream_of(&out);
        assert!(of_type(&lines, "Invoice").is_empty());
        assert_eq!(of_type(&lines, "PaymentReconciliation").len(), 3);
        assert!(env.warnings.lines().is_empty(), "{:?}", env.warnings.lines());
    }
}

/// A charge with what is owed on it and a `transactionStatus`, each `null`
/// for none.
fn charge_row(
    id: Option<u64>,
    at: &str,
    amount: &str,
    owed: serde_json::Value,
    status: serde_json::Value,
) -> serde_json::Value {
    json!({
        "id": id,
        "memberPaymentTypeId": "CHG",
        "timestamp": at,
        "oneTimeFeeName": "Fee A",
        "amount": amount,
        "outstandingBalance": owed,
        "transactionStatus": status,
    })
}

/// A payment with a `transactionStatus`, `null` for none.
fn payment_row(id: Option<u64>, at: &str, amount: &str, status: serde_json::Value) -> serde_json::Value {
    json!({
        "id": id,
        "memberPaymentTypeId": "PAY",
        "timestamp": at,
        "amount": amount,
        "transactionStatus": status,
    })
}

/// Four charges that are owed on, settled or of which `said` says what is
/// owed (`null` for one that does not say), and two voided ones: one that
/// still says 397.76 is owed on it, and one that says nothing.
fn voided_history(said: serde_json::Value) -> String {
    let rows = [
        payment_row(Some(5175), "2022-07-11T21:03:37", "-520.35", json!("settled")),
        charge_row(
            Some(4996),
            "2023-11-26T10:29:54",
            "159.96",
            json!("159.96"),
            json!(null),
        ),
        payment_row(Some(2898), "2021-06-10T08:29:28", "-71.11", json!("declined")),
        charge_row(Some(9658), "2019-10-03T11:25:18", "792.39", said, json!(null)),
        charge_row(Some(8550), "2019-02-12T21:43:20", "219.20", json!(0), json!("posted")),
        charge_row(
            Some(9835),
            "2021-11-29T06:27:23",
            "647.74",
            json!("397.76"),
            json!("Voided"),
        ),
        charge_row(Some(2580), "2023-08-22T21:18:15", "44.44", json!(27.51), json!("open")),
        payment_row(Some(9854), "2022-11-26T15:12:08", "-276.96", json!("pending")),
        payment_row(Some(4071), "2024-02-26T02:16:19", "-541.97", json!("completed")),
        charge_row(
            Some(6323),
            "2020-11-27T08:11:53",
            "550.43",
            json!(null),
            json!("CANCELLED"),
        ),
    ];
    json!({ "total": rows.len(), "data": rows }).to_string()
}

/// A voided charge is neither open nor owed, whatever it says: it is left out
/// of the count of open bills, of what the bills say is owed, and of what
/// accounts for the balance. The export writes it `cancelled`.
#[test]
fn a_voided_charge_is_neither_open_nor_owed() {
    // Said 0 and not said: the two charges owed on account for the balance.
    for said in [json!(0), json!(null)] {
        let server = serve(one_page("187.47", &voided_history(said)));
        let env = reader_env(&server, "voided");
        let site = Site::open(&env, &adult1()).unwrap();
        let a = &items(&readers::accounts_list(&site).unwrap())[0];
        assert_eq!((a["bills"].as_u64(), a["open_bills"].as_u64()), (Some(6), Some(2)));
        assert_eq!(a["owed_on_bills"].to_string(), "187.47");
        let open = readers::bills_list(&site, true, &RangeArgs::default()).unwrap();
        assert_eq!(ids(&open), ["4996", "2580"]);
        for id in ["9835", "6323"] {
            assert_eq!(readers::bill_show(&site, id).unwrap()["open"], false, "{id}");
        }
        let mut out = Vec::new();
        let finished = readers::export(&site, &server.base, &mut out, None, None, None).unwrap();
        assert!(finished.trailer.complete);
        let states: Vec<serde_json::Value> = of_type(&stream_of(&out), "Invoice")
            .iter()
            .map(|i| json!([i["identifier"][0]["value"], i["status"]]))
            .collect();
        assert_eq!(
            json!(states),
            json!([
                ["8550", "balanced"],
                ["9658", "balanced"],
                ["6323", "cancelled"],
                ["9835", "cancelled"],
                ["2580", "issued"],
                ["4996", "issued"],
            ])
        );
        assert!(env.warnings.lines().is_empty(), "{:?}", env.warnings.lines());
    }
    // No balance given: a voided charge that does not say leaves nothing
    // unknown.
    let server = serve(one_page("null", &voided_history(json!(0))));
    let env = reader_env(&server, "voided-no-balance");
    let site = Site::open(&env, &adult1()).unwrap();
    let a = &items(&readers::accounts_list(&site).unwrap())[0];
    assert_eq!(a["owed_on_bills"].to_string(), "187.47");
    let mut out = Vec::new();
    readers::export(&site, &server.base, &mut out, None, None, None).unwrap();
    let voided = stream_of(&out);
    let voided = of_type(&voided, "Invoice")
        .into_iter()
        .find(|i| i["identifier"][0]["value"] == "6323")
        .unwrap();
    assert_eq!((&voided["status"], voided.get("totalNet")), (&json!("cancelled"), None));
    assert!(env.warnings.lines().is_empty(), "{:?}", env.warnings.lines());
    // A balance that counts the voided charge is not accounted for.
    for (said, says) in [
        (json!(0), "add to 187.47, and the account shows 585.23 owed"),
        (
            json!(null),
            "the account shows 585.23 owed, and 1 bill(s) do not say what is owed on them",
        ),
    ] {
        let server = serve(one_page("585.23", &voided_history(said)));
        let env = reader_env(&server, "voided-owed");
        let site = Site::open(&env, &adult1()).unwrap();
        readers::accounts_list(&site).unwrap();
        let warnings = env.warnings.lines();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains(says), "{warnings:?}");
        let mut out = Vec::new();
        let e = readers::export(&site, &server.base, &mut out, None, None, None)
            .err()
            .unwrap();
        assert_eq!(e.exit_code(), 5);
        assert!(out.is_empty());
    }
}

// ------------------------------------------------------ rows without an id

/// A payment without an id, as two pages hold it.
fn payment_without_id(amount: &str) -> serde_json::Value {
    payment_row(None, "2022-02-11T15:50:12", amount, json!("settled"))
}

/// A charge without an id.
fn charge_without_id() -> serde_json::Value {
    charge_row(None, "2019-11-04T05:01:38", "314.76", json!("0.00"), json!(null))
}

/// A history of two pages. The first holds the payment and the charge
/// without an id after `more`; the second holds `then` before 6 rows of its
/// own, one of them a payment without an id. `total` is what both answers
/// count.
fn without_ids(total: usize, more: Vec<serde_json::Value>, then: Vec<serde_json::Value>) -> Routes {
    let mut first = more;
    first.extend([
        charge_row(Some(6244), "2019-11-29T01:04:46", "143.79", json!("0.00"), json!(null)),
        payment_without_id("-215.49"),
        charge_row(Some(9754), "2024-03-20T15:23:05", "590.40", json!(590.40), json!(null)),
        payment_row(Some(5070), "2024-12-16T11:16:44", "-874.83", json!("completed")),
        charge_without_id(),
    ]);
    let mut second = then;
    second.extend([
        charge_row(Some(9874), "2021-01-13T05:23:05", "324.30", json!(0), json!(null)),
        payment_row(Some(7059), "2019-01-21T09:34:17", "-549.69", json!("declined")),
        payment_row(None, "2021-06-11T14:52:22", "-297.27", json!("submitted")),
        charge_row(Some(6827), "2020-10-10T16:02:18", "553.78", json!(90.70), json!(null)),
        payment_row(Some(5245), "2019-07-27T14:24:59", "-853.97", json!("processing")),
        charge_row(Some(6673), "2023-12-01T04:54:48", "425.29", json!("0.00"), json!(null)),
    ]);
    let mut r = one_page("681.10", &json!({ "total": total, "data": first }).to_string());
    r.insert(page_at(2), ok(&json!({ "total": total, "data": second }).to_string()));
    r.insert(page_at(3), ok(&json!({ "total": total, "data": [] }).to_string()));
    r
}

/// The ids of the bills, and what the export says was left out for want of
/// one.
fn left_out_without_id(routes: Routes) -> (Vec<String>, String, usize) {
    let server = serve(routes);
    let env = reader_env(&server, "no-ids");
    let site = Site::open(&env, &adult1()).unwrap();
    let mut out = Vec::new();
    let finished = readers::export(&site, &server.base, &mut out, None, None, None).unwrap();
    assert!(finished.trailer.complete);
    let pages = targets(&server).iter().filter(|t| t.contains("pageNumber=")).count();
    let bills = ids(&readers::bills_list(&site, false, &RangeArgs::default()).unwrap());
    let skipped = stream_of(&out).last().unwrap()["skipped"].to_string();
    (bills, skipped, pages)
}

/// A row without an id that a later page repeats is the same row, counted
/// once; two alike on one page are two; one that differs in any field is
/// another.
#[test]
fn a_row_without_an_id_that_a_later_page_repeats_is_counted_once() {
    let bills = ["9754", "6673", "9874", "6827", "6244"];
    let repeated = || vec![payment_without_id("-215.49"), charge_without_id()];
    for (total, more, then, payments) in [
        (11, vec![], vec![], 2),
        (11, vec![], repeated(), 2),
        (12, repeated()[..1].to_vec(), vec![], 3),
        (12, repeated()[..1].to_vec(), repeated(), 3),
        (12, vec![], vec![payment_without_id("-215.48")], 3),
    ] {
        let (listed, skipped, pages) = left_out_without_id(without_ids(total, more, then));
        assert_eq!(listed, bills);
        assert_eq!(pages, 2, "read to its total of {total}");
        assert!(
            skipped.contains(&format!("{payments} payment(s) came without an id")),
            "{skipped}"
        );
        assert!(skipped.contains("1 bill(s) came without an id"), "{skipped}");
    }
}

/// Two pages that each hold a row of their own without an id, alike in every
/// field, cannot be told from a repeat: the history is short of its total,
/// exit 5, and nothing is written.
#[test]
fn rows_without_an_id_alike_on_two_pages_leave_the_history_short() {
    let server = serve(without_ids(12, vec![], vec![payment_without_id("-215.49")]));
    let env = reader_env(&server, "no-ids-short");
    let site = Site::open(&env, &adult1()).unwrap();
    let mut out = Vec::new();
    let e = readers::export(&site, &server.base, &mut out, None, None, None)
        .err()
        .unwrap();
    assert_eq!(e.exit_code(), 5);
    assert!(e.to_string().contains("counts 12 rows and its pages held 11"), "{e}");
    assert!(out.is_empty());
}

/// A later page of nothing but repeats, with an id or without, is exit 5 at
/// that page.
#[test]
fn a_page_of_only_repeated_rows_without_an_id_is_exit_5() {
    let mut routes = without_ids(13, vec![], vec![]);
    let page = json!({ "total": 13, "data": [charge_without_id(), payment_without_id("-215.49")] });
    routes.insert(page_at(2), ok(&page.to_string()));
    let server = serve(routes);
    let env = reader_env(&server, "no-ids-no-paging");
    let site = Site::open(&env, &adult1()).unwrap();
    let e = readers::bills_list(&site, false, &RangeArgs::default()).err().unwrap();
    assert_eq!(e.exit_code(), 5);
    assert!(
        e.to_string().contains("page 2 with the rows of the pages before it"),
        "{e}"
    );
    assert_eq!(targets(&server).iter().filter(|t| t.contains("pageNumber=")).count(), 2);
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
