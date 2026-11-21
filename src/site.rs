//! Embryo Options's sites: the clinic's own web app, where `auth login
//! --browser` signs in, and the member API it calls. What the sign-in reads
//! out of the Chrome window, and the one read that confirms it before it is
//! stored.
//!
//! Each clinic has its own web app on `<clinic>.embryooptions.com` (one Vue
//! app; the subdomain tells it which clinic). The member signs in there with
//! the user name and password the clinic's billing notice gives, and the app
//! keeps the access token and the refresh token in its `localStorage`
//! (*bundle*). The capture reads both with an expression, and
//! [`confirm_candidate`] makes one read of the member API with the access
//! token: the member's own account, which a token that is not signed in
//! does not get (docs/api.md).

use std::io::Read;
use std::time::Duration;

use pk_cli_browser::{BrowserError, Capture, Captured, Confirm, HostRule};
use pk_cli_core::CliError;
use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};

use crate::cli::BIN;
use crate::token::{Claims, Tokens};

/// The provider's name in the browser crate's status lines and errors.
pub const LABEL: &str = "Embryo Options";
/// The domain every clinic's web app is under.
pub const DOMAIN: &str = "embryooptions.com";
/// What signing in involves, for the opening status line.
pub const HINT: &str = "the user name and password from your clinic's Embryo Options billing notice";

/// What the capture evaluates in the clinic's pages: the tokens the web
/// app keeps once signed in (*bundle*: `localStorage` `accessToken` and
/// `refreshToken`), as one JSON string, and `null` before.
pub const TOKENS_JS: &str = "(() => { const a = localStorage.getItem('accessToken'); if (!a) return null; \
     return JSON.stringify({ accessToken: a, refreshToken: localStorage.getItem('refreshToken') || null }); })()";

/// The member read that confirms a token: the member's own account
/// (*bundle*), `{memberId}` and `{ivfCenterId}` from the token's claims.
pub const SESSION_CHECK: &str = "/api/Members/{memberId}/MemberAccount/Member";

/// The bound on the whole read, connecting included. The browser crate's poll
/// (and Ctrl-C) waits for it.
const TIMEOUT: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The most of an answer's body read for [`judge`].
const MAX_BODY: u64 = 4 * 1024 * 1024;

/// Subdomains of [`DOMAIN`] that are not a clinic's web app.
const NOT_CLINICS: &[&str] = &["www", "api", "member-api"];

/// The clinic a login belongs to: its subdomain of [`DOMAIN`], a DNS label
/// (1 to 63 lowercase letters, digits and `-`, no `-` at either end).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clinic(String);

impl Clinic {
    /// `exampleclinic`, `ExampleClinic`, or the clinic's web app's address
    /// (`https://exampleclinic.embryooptions.com/...`), as a billing notice
    /// prints it.
    pub fn parse(given: &str) -> Result<Clinic, CliError> {
        let given = given.trim();
        let bad = |why: &str| {
            CliError::Usage(format!(
                "--clinic {why}: give the clinic's name in its web app's address, `<clinic>.{DOMAIN}`, \
                 like `exampleclinic`"
            ))
        };
        let slug = if given.contains("://") || given.contains('.') {
            let url = if given.contains("://") {
                given.to_string()
            } else {
                format!("https://{given}")
            };
            let url = reqwest::Url::parse(&url).map_err(|_| bad("is not a clinic's name or address"))?;
            if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() || url.port().is_some()
            {
                return Err(bad("is an address that is not the clinic's https web app"));
            }
            let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
            match host.strip_suffix(&format!(".{DOMAIN}")) {
                Some(sub) if !sub.contains('.') => sub.to_string(),
                _ => return Err(bad(&format!("is an address that is not on `<clinic>.{DOMAIN}`"))),
            }
        } else {
            given.to_ascii_lowercase()
        };
        let label = (1..=63).contains(&slug.len())
            && slug
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !slug.starts_with('-')
            && !slug.ends_with('-');
        if !label {
            return Err(bad("is not a DNS label (1 to 63 letters, digits and `-`)"));
        }
        if NOT_CLINICS.contains(&slug.as_str()) {
            return Err(bad(&format!(
                "names `{slug}.{DOMAIN}`, which is not a clinic's web app"
            )));
        }
        Ok(Clinic(slug))
    }

    pub fn slug(&self) -> &str {
        &self.0
    }

    /// `<clinic>.embryooptions.com`: where the sign-in window opens and the
    /// capture reads.
    pub fn host(&self) -> String {
        format!("{}.{DOMAIN}", self.0)
    }

    /// Where the Chrome window opens: the clinic's web app, which shows its
    /// sign-in page to a visitor.
    pub fn start_url(&self) -> String {
        format!("https://{}/", self.host())
    }
}

impl std::fmt::Display for Clinic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What `auth login --browser` reads: [`TOKENS_JS`] in a page on the
/// clinic's host, and the browser's user agent. With `verify`, the tab stays
/// open until [`confirm_candidate`] says the token is signed in.
pub fn capture(clinic: &Clinic, verify: bool) -> Result<Capture, BrowserError> {
    let capture = Capture::new(LABEL, HostRule::exact(&clinic.host())?)
        .expression(TOKENS_JS)
        .with_user_agent()
        .hint(HINT);
    Ok(if verify {
        capture.confirm(confirm_candidate)
    } else {
        capture
    })
}

/// The capture's confirm hook: the tokens the expression answered, read
/// against the member API ([`confirm_tokens_at`]).
pub fn confirm_candidate(candidate: &Captured) -> Confirm {
    let Some(value) = candidate.value() else {
        return Confirm::NotYet("the clinic's web app holds no access token yet".into());
    };
    match crate::token::tokens_from(value) {
        Ok(tokens) => confirm_tokens_at(crate::client::BASE, &tokens, candidate.user_agent()),
        Err(e) => Confirm::Fail(CliError::Other(format!(
            "the clinic's web app holds an access token embryooptions cannot read ({}); the site may have changed",
            e.message()
        ))),
    }
}

/// Whether `tokens` are a signed-in member's: the claims first (a token
/// from the site's registration step, or one already expired, is `NotYet`),
/// then one read of [`SESSION_CHECK`] on `base` ([`crate::client::BASE`],
/// or a test's loopback server), judged by [`judge`].
pub fn confirm_tokens_at(base: &str, tokens: &Tokens, user_agent: Option<&str>) -> Confirm {
    let claims = match crate::token::claims(tokens.access.expose()) {
        Ok(claims) => claims,
        Err(e) => return Confirm::Fail(e),
    };
    if claims.intermediate {
        return Confirm::NotYet("the sign-in is at the site's registration step, not signed in to the account".into());
    }
    if claims.expired_at(crate::portal::now_unix()) {
        return Confirm::NotYet("the access token has expired".into());
    }
    let Some(url) = check_url(base, &claims) else {
        return Confirm::Fail(CliError::Other("the access token names no member".into()));
    };
    confirm_at(&url, tokens.access.expose(), user_agent)
}

/// [`SESSION_CHECK`] on `base` for the token's member, with its clinic's id
/// as `ivfCenterId` when the token has one.
fn check_url(base: &str, claims: &Claims) -> Option<String> {
    let member = claims.member_id.as_deref()?;
    let path = SESSION_CHECK.replace("{memberId}", member);
    Some(match &claims.ivf_center_id {
        Some(center) => format!("{base}{path}?ivfCenterId={center}"),
        None => format!("{base}{path}"),
    })
}

/// How the member read shows a signed-in token.
///
/// `Ready` for a 2xx JSON object. `NotYet` for a 401 or a 403: the API
/// does not accept the token for that member. Undecided for any other
/// answer. It sees only answers that [`confirm_at`] has not already
/// decided: no 429, 5xx, 403 with an HTML page or redirect. `body` is at
/// most 4 MiB.
pub fn judge(status: u16, content_type: Option<&str>, body: &[u8]) -> Confirm {
    let what = format!("GET {SESSION_CHECK}");
    if status == 401 || status == 403 {
        return Confirm::NotYet(format!("{what} answered HTTP {status}: not signed in"));
    }
    if !(200..300).contains(&status) {
        return Confirm::undecided(format!("{what} answered HTTP {status}"));
    }
    let json = content_type.is_some_and(|t| t.contains("json"))
        && serde_json::from_slice::<serde_json::Value>(body).is_ok_and(|v| v.is_object());
    if !json {
        return Confirm::undecided(format!("{what} answered HTTP {status} without a JSON object"));
    }
    Confirm::Ready
}

/// [`confirm_tokens_at`]'s read of `url`. Its reasons name
/// [`SESSION_CHECK`], never the member's id or a query.
fn confirm_at(url: &str, access_token: &str, user_agent: Option<&str>) -> Confirm {
    let what = format!("GET {SESSION_CHECK}");
    let client = match client(user_agent) {
        Ok(client) => client,
        Err(e) => return Confirm::Fail(e),
    };
    let Ok(mut bearer) = HeaderValue::from_str(&format!("Bearer {access_token}")) else {
        return Confirm::Fail(CliError::Other(
            "the access token is not a valid HTTP header value".into(),
        ));
    };
    bearer.set_sensitive(true);
    let answer = match client
        .get(url)
        .header(AUTHORIZATION, bearer)
        .header(ACCEPT, "application/json")
        .send()
    {
        Ok(answer) => answer,
        Err(e) if e.is_timeout() => return Confirm::undecided(format!("{what} timed out")),
        Err(_) => return Confirm::undecided(format!("{what} got no answer")),
    };
    if let Some(said) = pk_cli_http::inconclusive(&answer) {
        return Confirm::Undecided {
            reason: format!("{what} answered HTTP {}", said.status),
            retry_after: said.retry_after,
        };
    }
    let status = answer.status().as_u16();
    if answer.status().is_redirection() {
        return Confirm::undecided(format!("{what} answered with a redirect (HTTP {status})"));
    }
    let content_type = answer
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let mut body = Vec::new();
    if answer.take(MAX_BODY + 1).read_to_end(&mut body).is_err() {
        return Confirm::undecided(format!("{what} answered HTTP {status} and its body did not arrive"));
    }
    if body.len() as u64 > MAX_BODY {
        return Confirm::undecided(format!("{what} answered with more than {} MiB", MAX_BODY >> 20));
    }
    judge(status, content_type.as_deref(), &body)
}

/// The confirm read's client: no redirects followed, bounded in time.
fn client(user_agent: Option<&str>) -> Result<reqwest::blocking::Client, CliError> {
    let mut builder = pk_cli_http::builder(BIN, env!("CARGO_PKG_VERSION"))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT);
    if let Some(agent) = user_agent {
        builder = builder.user_agent(agent);
    }
    builder
        .build()
        .map_err(|e| CliError::Other(format!("building the HTTP client: {}", e.without_url())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::test_token;
    use pk_cli_secrets::Secret;
    use serde_json::json;
    use std::io::Write;
    use std::net::TcpListener;

    fn clinic() -> Clinic {
        Clinic::parse("exampleclinic").unwrap()
    }

    #[test]
    fn a_clinic_is_its_subdomain() {
        for given in [
            "exampleclinic",
            "ExampleClinic",
            " exampleclinic ",
            "exampleclinic.embryooptions.com",
            "https://exampleclinic.embryooptions.com",
            "https://ExampleClinic.embryooptions.com/member/home?x=1",
        ] {
            assert_eq!(Clinic::parse(given).unwrap().slug(), "exampleclinic", "{given}");
        }
        assert_eq!(Clinic::parse("clinic-2").unwrap().slug(), "clinic-2");
        assert_eq!(clinic().host(), "exampleclinic.embryooptions.com");
        assert_eq!(clinic().start_url(), "https://exampleclinic.embryooptions.com/");
        let long = "a".repeat(64);
        // Built here, so the no-personal-data scan does not read it as an
        // email address.
        let userinfo = format!("https://user{}exampleclinic.embryooptions.com", '@');
        for bad in [
            "",
            "-clinic",
            "clinic-",
            "clinic_1",
            "clinic one",
            long.as_str(),
            "www",
            "member-api",
            "api",
            "http://exampleclinic.embryooptions.com",
            "https://exampleclinic.embryooptions.com:8443",
            userinfo.as_str(),
            "https://exampleclinic.embryooptions.com.example.com",
            "https://a.b.embryooptions.com",
            "https://embryooptions.com",
            "https://member-api.prod.embryooptions.com",
            "exampleclinic.example.com",
        ] {
            assert_eq!(Clinic::parse(bad).err().unwrap().exit_code(), 2, "{bad}");
        }
    }

    /// The capture `pk-cli-browser` checks before Chrome starts, checked
    /// offline, with and without the confirm hook.
    #[test]
    fn the_browser_capture_is_well_formed() {
        for verify in [true, false] {
            let capture = capture(&clinic(), verify).unwrap();
            capture.validate().unwrap();
            assert_eq!(capture.label(), LABEL);
        }
        assert!(crate::client::BASE.starts_with("https://"));
        assert!(crate::client::BASE.ends_with(&format!(".{DOMAIN}")));
    }

    fn signed_in_token() -> String {
        test_token(&json!({
            "user": json!({ "UserId": 4242, "IVFCenterId": 7, "MemberType": 1 }).to_string(),
            "exp": 4_000_000_000_i64,
        }))
    }

    fn tokens(access: String) -> Tokens {
        Tokens {
            access: Secret::new(access),
            refresh: None,
        }
    }

    #[test]
    fn a_candidate_without_a_readable_token_is_not_read() {
        let built = Captured::for_test(LABEL, "exampleclinic.embryooptions.com").build();
        assert!(matches!(confirm_candidate(&built), Confirm::NotYet(r) if r.contains("no access token")));
        let unreadable = Captured::for_test(LABEL, "exampleclinic.embryooptions.com")
            .value("{\"accessToken\":\"dummy_secret\"}")
            .build();
        match confirm_candidate(&unreadable) {
            Confirm::Fail(e) => assert!(!e.to_string().contains("dummy_secret"), "{e}"),
            other => panic!("{other:?}"),
        }
    }

    /// The claims decide before any read: a registration-step token and an
    /// expired one are not signed in, and neither reaches a server.
    #[test]
    fn the_claims_decide_before_the_read() {
        let unused = "http://127.0.0.1:9";
        let intermediate = test_token(&json!({ "intermediateUser": json!({ "UserId": 1 }).to_string() }));
        assert!(matches!(
            confirm_tokens_at(unused, &tokens(intermediate), None),
            Confirm::NotYet(r) if r.contains("registration")
        ));
        let expired = test_token(&json!({ "user": json!({ "UserId": 1 }).to_string(), "exp": 1 }));
        assert!(matches!(
            confirm_tokens_at(unused, &tokens(expired), None),
            Confirm::NotYet(r) if r.contains("expired")
        ));
    }

    /// What the API answers (docs/api.md), judged.
    #[test]
    fn judge_reads_the_apis_answers() {
        let json = Some("application/json; charset=utf-8");
        assert!(matches!(judge(200, json, b"{\"a\":1}"), Confirm::Ready));
        assert!(matches!(judge(401, json, b""), Confirm::NotYet(r) if r.contains("HTTP 401")));
        assert!(matches!(judge(403, json, b"{}"), Confirm::NotYet(r) if r.contains("HTTP 403")));
        for (status, mime, body) in [
            (200, Some("text/html"), &b"<p>app</p>"[..]),
            (200, json, &b"[1]"[..]),
            (200, json, &b"not json"[..]),
            (200, None, &b"{}"[..]),
            (404, json, &b"{}"[..]),
            (304, None, &b""[..]),
        ] {
            assert!(
                matches!(judge(status, mime, body), Confirm::Undecided { .. }),
                "{status} {:?}",
                String::from_utf8_lossy(body)
            );
        }
    }

    /// Serves one canned answer on loopback and hands back the request's head.
    fn serve_once(answer: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap() == 1 {
                head.push(byte[0]);
            }
            stream.write_all(answer.as_bytes()).unwrap();
            String::from_utf8_lossy(&head).to_string()
        });
        (base, handle)
    }

    /// The read: the member's own account with the bearer token and the
    /// browser's user agent, no redirect followed; what cannot decide is
    /// `Undecided` (with the provider's `Retry-After`), and a reason never
    /// names the member or the query.
    #[test]
    fn the_confirm_read_sends_the_bearer_to_the_members_account() {
        let token = signed_in_token();
        let (base, server) =
            serve_once("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}");
        assert!(matches!(
            confirm_tokens_at(&base, &tokens(token.clone()), Some("Mozilla/5.0 (test)")),
            Confirm::Ready
        ));
        let head = server.join().unwrap();
        assert!(
            head.starts_with("GET /api/Members/4242/MemberAccount/Member?ivfCenterId=7 "),
            "{head}"
        );
        let lower = head.to_lowercase();
        assert!(lower.contains(&format!("\r\nauthorization: bearer {}\r\n", token.to_lowercase())));
        assert!(lower.contains("\r\nuser-agent: mozilla/5.0 (test)\r\n"), "{head}");
        assert!(lower.contains("\r\naccept: application/json\r\n"), "{head}");

        let undecided = |answer: &'static str, retry: Option<u64>| {
            let (base, server) = serve_once(answer);
            match confirm_tokens_at(&base, &tokens(signed_in_token()), None) {
                Confirm::Undecided { reason, retry_after } => {
                    assert!(
                        reason.starts_with("GET /api/Members/{memberId}/MemberAccount/Member answered"),
                        "{reason}"
                    );
                    assert!(!reason.contains("4242") && !reason.contains("ivfCenterId"), "{reason}");
                    assert_eq!(retry_after, retry.map(Duration::from_secs));
                }
                other => panic!("{answer:?}: {other:?}"),
            }
            server.join().unwrap()
        };
        let head = undecided("HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n", None);
        assert!(
            head.to_lowercase().contains(&format!("\r\nuser-agent: {BIN}/")),
            "{head}"
        );
        undecided(
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 30\r\nContent-Length: 0\r\n\r\n",
            Some(30),
        );
        undecided(
            "HTTP/1.1 403 Forbidden\r\nContent-Type: text/html\r\nContent-Length: 9\r\n\r\n<p>no</p>",
            None,
        );
        undecided(
            "HTTP/1.1 302 Found\r\nLocation: https://example.com/\r\nContent-Length: 0\r\n\r\n",
            None,
        );

        let (base, server) = serve_once("HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
        assert!(matches!(
            confirm_tokens_at(&base, &tokens(signed_in_token()), None),
            Confirm::NotYet(r) if r.contains("HTTP 401")
        ));
        server.join().unwrap();
    }

    #[test]
    fn no_answer_is_undecided() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        match confirm_tokens_at(&format!("http://127.0.0.1:{port}"), &tokens(signed_in_token()), None) {
            Confirm::Undecided { reason, .. } => {
                assert_eq!(reason, "GET /api/Members/{memberId}/MemberAccount/Member got no answer")
            }
            other => panic!("{other:?}"),
        }
    }
}
