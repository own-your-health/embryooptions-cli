//! The Embryo Options reads, made with a profile's stored access token
//! (docs/api.md).
//!
//! - Every read is a `GET` on the member API the clinic's web app calls,
//!   [`BASE`], under `/api/Members/{memberId}/`, the signed-in member's own.
//! - A request carries the access token as `X-XSRF-TOKEN: Bearer <token>`
//!   and the clinic's id as `IvfCenterId`, as the web app sends them (it
//!   sends no `Authorization` header, and the API answers one with 401), and
//!   the browser's user agent captured with it.
//! - It follows no redirect and is bounded in time: 30 s for the whole
//!   answer, 10 s to connect.
//! - One request at a time, at least [`PACE`] after the last answer.
//! - Nothing writes to Embryo Options. `embryooptions api` sends only
//!   `GET`s, checked by [`api_path`] and [`ApiPath::resolve`] before one is
//!   sent.
//! - [`refuse`] turns what is no data into an error: a 401 is a refused
//!   session (exit 3), a 404 is exit 4, and a redirect, a 403, a 429 or a
//!   5xx is exit 5.
//! - Errors name a read by its path with `{memberId}` in the member's place
//!   and no query, so an error never carries the member's id.

use std::cell::Cell;
use std::io::Read as _;
use std::time::{Duration, Instant};

use pk_cli_config::profile::ProfileName;
use pk_cli_core::{CliError, Reason};
use pk_cli_secrets::Secret;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE, RETRY_AFTER, USER_AGENT};

use crate::cli::BIN;
use crate::session::Session;
use crate::site::Clinic;
use crate::token::Claims;

/// The header the web app sends the access token in, as `Bearer <token>`
/// (observed 2026-11-22; the API answers `Authorization` with 401).
pub const TOKEN_HEADER: HeaderName = HeaderName::from_static("x-xsrf-token");
/// The header the web app sends the clinic's id in, beside the token.
pub const CENTER_HEADER: HeaderName = HeaderName::from_static("ivfcenterid");

/// The token and clinic headers for `access_token`: the clinic's id from
/// its claims, when it has one.
pub fn token_headers(access_token: &str) -> Result<HeaderMap, CliError> {
    let mut headers = HeaderMap::new();
    let mut bearer = HeaderValue::from_str(&format!("Bearer {access_token}"))
        .map_err(|_| CliError::Other("the access token is not a valid HTTP header value".into()))?;
    bearer.set_sensitive(true);
    headers.insert(TOKEN_HEADER, bearer);
    if let Some(center) = crate::token::claims(access_token).ok().and_then(|c| c.ivf_center_id) {
        if let Ok(v) = HeaderValue::from_str(&center) {
            headers.insert(CENTER_HEADER, v);
        }
    }
    Ok(headers)
}

/// The member API's origin (the web app's `VITE_APP_API_URL`).
pub const BASE: &str = "https://member-api.prod.embryooptions.com";
/// Points the reads at a test's loopback server instead of [`BASE`]
/// ([`base`]).
pub const BASE_URL_ENV: &str = "EMBRYOOPTIONS_BASE_URL";
/// The least time between one answer and the next request.
pub const PACE: Duration = Duration::from_millis(750);
const TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The most of an answer read.
const MAX_BODY: u64 = 32 * 1024 * 1024;

/// The origin this process reads: [`BASE`], unless [`BASE_URL_ENV`] names a
/// test's server.
pub fn base() -> Result<String, CliError> {
    base_from(
        std::env::var(BASE_URL_ENV).ok().as_deref(),
        crate::session::is_test_store(),
        crate::session::hermetic(),
    )
}

/// The refusal of a request under a test's keychain service
/// (`session::hermetic`) with no [`BASE_URL_ENV`]: a test run reads only a
/// server on this machine, never the member API.
fn hermetic_without_base() -> CliError {
    CliError::Usage(format!(
        "{} is set, so this is a test run, and a test run sends requests only to a server on this \
         machine: set {BASE_URL_ENV} to its loopback origin, like http://127.0.0.1:8080 (unset {} \
         to read the member API)",
        crate::session::KEYCHAIN_SERVICE_ENV,
        crate::session::KEYCHAIN_SERVICE_ENV
    ))
}

/// The check behind [`base`], on the values passed in. Under a
/// test's keychain service (`hermetic`) the override is required, so a
/// test that forgets it is refused before it sends anything. The override is for
/// a test's server: a plain `http` origin on a loopback address, and only
/// over a test's own session store (`session::is_test_store`), so no
/// token the owner stored is sent to it. An ambient variable sending a real
/// token elsewhere would be a way to steal it.
pub fn base_from(value: Option<&str>, test_store: bool, hermetic: bool) -> Result<String, CliError> {
    let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        if hermetic {
            return Err(hermetic_without_base());
        }
        return Ok(BASE.to_string());
    };
    let refuse = |why: &str| {
        CliError::Usage(format!(
            "{BASE_URL_ENV} {why}; it exists only to point tests at a server on this machine"
        ))
    };
    if !test_store {
        return Err(refuse(&format!(
            "needs a test's own session store, so no token the owner stored is sent to it: {} naming a \
             `piekstra.embryooptions.<suffix>` service, and XDG_CONFIG_HOME set away from ~/.config",
            crate::session::KEYCHAIN_SERVICE_ENV
        )));
    }
    let url = reqwest::Url::parse(value).map_err(|_| refuse("is not a URL"))?;
    let loopback = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    let plain = url.scheme() == "http"
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none();
    if !(loopback && plain) {
        return Err(refuse(
            "must be an http origin on a loopback address, like http://127.0.0.1:8080",
        ));
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

/// What a read sends and to whom: the access token and the user agent.
pub struct Client {
    http: reqwest::blocking::Client,
    base: String,
    pace: Duration,
    last: Cell<Option<Instant>>,
    profile: ProfileName,
    clinic: Clinic,
}

impl Client {
    /// A client for `profile`'s `session` against `base` ([`BASE`], or a
    /// test's loopback server), `pace` apart.
    pub fn new(session: &Session, profile: &ProfileName, base: &str, pace: Duration) -> Result<Client, CliError> {
        let agent = session
            .user_agent
            .clone()
            .unwrap_or_else(|| format!("{BIN}/{}", env!("CARGO_PKG_VERSION")));
        let mut headers = token_headers(session.access_token.expose())?;
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&agent)
                .map_err(|_| CliError::Other("the stored user agent is not a valid HTTP header value".into()))?,
        );
        let http = reqwest::blocking::Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|e| CliError::Other(format!("building the HTTP client: {}", e.without_url())))?;
        Ok(Client {
            http,
            base: base.trim_end_matches('/').to_string(),
            pace,
            last: Cell::new(None),
            profile: profile.clone(),
            clinic: session.clinic.clone(),
        })
    }

    /// `GET` of a resolved path for `embryooptions api`: the answer as it
    /// came, after [`refuse`].
    pub fn api(&self, read: &Resolved) -> Result<Answer, CliError> {
        let answer = self.get(read, "application/json")?;
        refuse(&answer, &self.profile, &self.clinic)?;
        Ok(answer)
    }

    /// Waits until [`PACE`] (the client's pace) has passed since the last
    /// answer, before a request: a read, or the token refresh between two.
    pub fn pause(&self) {
        if let Some(last) = self.last.get() {
            let since = last.elapsed();
            if since < self.pace {
                std::thread::sleep(self.pace - since);
            }
        }
    }

    /// This client for `session`'s renewed tokens, for the same profile and
    /// API, paced from now: the refresh just answered.
    pub fn replaced(&self, session: &Session) -> Result<Client, CliError> {
        let client = Client::new(session, &self.profile, &self.base, self.pace)?;
        client.last.set(Some(Instant::now()));
        Ok(client)
    }

    fn get(&self, read: &Resolved, accept: &str) -> Result<Answer, CliError> {
        let what = read.what.clone();
        self.pause();
        let result = self
            .http
            .get(format!("{}{}", self.base, read.path))
            .header(ACCEPT, accept)
            .send();
        let answer = match result {
            Ok(a) => a,
            Err(e) => {
                self.last.set(Some(Instant::now()));
                return Err(if e.is_timeout() {
                    CliError::Upstream(format!("{what} timed out after {} s", TIMEOUT.as_secs()))
                } else {
                    let host = self.base.split_once("://").map_or(self.base.as_str(), |(_, h)| h);
                    CliError::Upstream(format!("{what} got no answer from {host}"))
                });
            }
        };
        let status = answer.status().as_u16();
        let header = |name| {
            answer
                .headers()
                .get(name)
                .and_then(|v: &HeaderValue| v.to_str().ok())
                .map(str::to_string)
        };
        let content_type = header(CONTENT_TYPE);
        let retry_after = header(RETRY_AFTER);
        let mut body = Vec::new();
        let read = answer.take(MAX_BODY + 1).read_to_end(&mut body);
        self.last.set(Some(Instant::now()));
        if read.is_err() {
            return Err(CliError::Upstream(format!(
                "{what} answered HTTP {status} and its body did not arrive"
            )));
        }
        if body.len() as u64 > MAX_BODY {
            return Err(CliError::Upstream(format!(
                "{what} answered with more than {} MiB",
                MAX_BODY >> 20
            )));
        }
        Ok(Answer {
            what,
            status,
            content_type,
            retry_after,
            body,
        })
    }
}

/// Where `embryooptions api` reads: the member's own paths.
const API_PREFIX: &str = "/api/Members/";
/// The member segment's placeholder, as the bundle's path templates write it.
pub const MEMBER_ID: &str = "{memberId}";
/// The clinic id's placeholder, for a query value.
pub const IVF_CENTER_ID: &str = "{ivfCenterId}";

/// Words that, in a path segment or a query key, name something other than
/// a read: paying, a card or a payment method, the embryos' disposition, a
/// consent, a change, signing in or out. The member API takes some of its
/// actions as `GET`s (`.../MemberPreempt/ChargeExistingPaymentMethod`,
/// `.../MatchingSite/AcceptMatchRequest`, *bundle*), so a `GET` is no proof
/// of a read: `api` refuses every path that names one. `matching` is the
/// embryo donation matching site, which shows other members: out of scope.
const ACTION_WORDS: &[&str] = &[
    "payment",
    "paypal",
    "autopay",
    "card",
    "method",
    "charge",
    "vault",
    "nonce",
    "token",
    "disposition",
    "consent",
    "docusign",
    "update",
    "delete",
    "remove",
    "create",
    "save",
    "submit",
    "send",
    "login",
    "logout",
    "register",
    "enroll",
    "cancel",
    "change",
    "accept",
    "confirm",
    "rebuild",
    "generate",
    "preempt",
    "credential",
    "password",
    "needshelp",
    "capture",
    "matching",
];

/// Segments that hold an action word and are reads: the billing history.
const READS: &[&str] = &["memberpaymenthistoryall"];

/// Whether a path segment or query key holds an action word.
fn names_an_action(word: &str) -> bool {
    let w = word.to_ascii_lowercase();
    !READS.contains(&w.as_str()) && ACTION_WORDS.iter().any(|a| w.contains(a))
}

/// A path `embryooptions api` may read, checked by [`api_path`]: under
/// `/api/Members/<member>/`, where `<member>` is [`MEMBER_ID`] or an id,
/// and its query values may hold [`IVF_CENTER_ID`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiPath(String);

/// An [`ApiPath`] for one member: what is sent, and how errors name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The path and query sent.
    pub path: String,
    /// `GET /api/Members/{memberId}/...`, no query.
    pub what: String,
}

impl ApiPath {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The path for the token's member: [`MEMBER_ID`] and [`IVF_CENTER_ID`]
    /// filled in from `claims`, and an id in the member's place only if it
    /// is the token's own (exit 2 otherwise): `api` reads the signed-in
    /// member's paths and no other's.
    pub fn resolve(&self, claims: &Claims) -> Result<Resolved, CliError> {
        let Some(member) = claims.member_id.as_deref() else {
            return Err(CliError::Auth("the stored access token names no member".into()));
        };
        let (route, query) = self
            .0
            .split_once('?')
            .map_or((self.0.as_str(), None), |(r, q)| (r, Some(q)));
        let mut segments: Vec<&str> = route.split('/').collect();
        // ["", "api", "Members", <member>, ...]
        let given = segments[3];
        if given != MEMBER_ID && given != member {
            return Err(CliError::Usage(format!(
                "`{BIN} api` reads only the signed-in member's own paths: put {MEMBER_ID} in the member's place"
            )));
        }
        segments[3] = MEMBER_ID;
        let what = format!("GET {}", segments.join("/"));
        segments[3] = member;
        let mut path = segments.join("/");
        if let Some(query) = query {
            let query = if query.contains(IVF_CENTER_ID) {
                let Some(center) = claims.ivf_center_id.as_deref() else {
                    return Err(CliError::Usage(format!(
                        "the stored access token names no clinic id for {IVF_CENTER_ID}"
                    )));
                };
                query.replace(IVF_CENTER_ID, center)
            } else {
                query.to_string()
            };
            let query = query.replace(MEMBER_ID, member);
            path = format!("{path}?{query}");
        }
        Ok(Resolved { path, what })
    }
}

/// `embryooptions api GET <PATH>`'s path on [`BASE`], or exit 2. Nothing is
/// sent before this passes.
pub fn api_path(path: &str) -> Result<ApiPath, CliError> {
    if path.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(CliError::Usage("the path holds a space or a control character".into()));
    }
    let path = match path.strip_prefix(BASE) {
        Some(rest) => rest,
        None if path.contains("://") => {
            return Err(CliError::Usage(format!(
                "`{BIN} api` reads only {BASE}; give a path (`{API_PREFIX}{MEMBER_ID}/...`) or a URL on it"
            )))
        }
        None => path,
    };
    if !path.starts_with('/') {
        return Err(CliError::Usage("the path starts with `/`".into()));
    }
    let only_members = || {
        CliError::Usage(format!(
            "`{BIN} api` reads the member's own paths only: `{API_PREFIX}{MEMBER_ID}/...`"
        ))
    };
    let (route, query) = path.split_once('?').unwrap_or((path, ""));
    if !route.to_ascii_lowercase().starts_with(&API_PREFIX.to_ascii_lowercase())
        || route.contains("..")
        || route.contains("//")
        || path.contains('%')
        || path.contains('#')
    {
        return Err(only_members());
    }
    let segments: Vec<&str> = route.split('/').collect();
    let member = segments.get(3).copied().unwrap_or("");
    if !(member == MEMBER_ID || crate::token::is_id(member)) || segments.len() < 5 || segments[4..].contains(&"") {
        return Err(only_members());
    }
    if segments[4..].iter().any(|s| s.contains(['{', '}'])) {
        return Err(CliError::Usage(format!(
            "the path's only placeholder is {MEMBER_ID}, in the member's place"
        )));
    }
    let keys: Vec<&str> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| pair.split('=').next().unwrap_or(""))
        .collect();
    let values_ok = query
        .split('&')
        .filter_map(|pair| pair.split_once('=').map(|(_, v)| v))
        .all(|v| !v.contains(['{', '}']) || v == IVF_CENTER_ID || v == MEMBER_ID);
    if keys.iter().any(|k| k.contains(['{', '}'])) || !values_ok {
        return Err(CliError::Usage(format!(
            "a query value may be {IVF_CENTER_ID} or {MEMBER_ID}, and a key no placeholder"
        )));
    }
    if segments.iter().any(|s| names_an_action(s)) || keys.iter().any(|k| names_an_action(k)) {
        return Err(CliError::Usage(format!(
            "{BIN} is read-only: that path names a payment, a card, a disposition, a consent, a sign-in or a \
             change, which `api` never reads"
        )));
    }
    Ok(ApiPath(path.to_string()))
}

/// An answer, read whole.
pub struct Answer {
    /// `GET /api/Members/{memberId}/...`, no query.
    pub what: String,
    pub status: u16,
    pub content_type: Option<String>,
    pub retry_after: Option<String>,
    pub body: Vec<u8>,
}

/// The web app's token refresh: `GET /api/Auth/RefreshToken` with the
/// refresh token as `X-XSRF-TOKEN: Bearer <refresh token>` (the bundle's
/// `updateToken`, observed 2026-11-22). The answer is a new pair,
/// `{"accessToken", "refreshToken"}`. A 401 or 403 is a sign-in that can no
/// longer be renewed (exit 3).
pub const REFRESH: &str = "/api/Auth/RefreshToken";

/// One token refresh of `session` against `base`, as [`REFRESH`] says.
pub fn refresh(base: &str, session: &Session, profile: &ProfileName) -> Result<crate::token::Tokens, CliError> {
    let what = format!("GET {REFRESH}");
    let Some(refresh) = &session.refresh_token else {
        return Err(CliError::Auth(format!(
            "no refresh token is stored with profile `{profile}`'s Embryo Options sign-in, so it cannot be renewed"
        ))
        .because(Reason::NoCredential)
        .with_remedy(login_remedy(profile, &session.clinic)));
    };
    let mut headers = token_headers(refresh.expose())?;
    if let Some(center) = session.claims().ok().and_then(|c| c.ivf_center_id) {
        if let Ok(v) = HeaderValue::from_str(&center) {
            headers.insert(CENTER_HEADER, v);
        }
    }
    let agent = session
        .user_agent
        .clone()
        .unwrap_or_else(|| format!("{BIN}/{}", env!("CARGO_PKG_VERSION")));
    let http = reqwest::blocking::Client::builder()
        .user_agent(agent)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|e| CliError::Other(format!("building the HTTP client: {}", e.without_url())))?;
    let answer = http
        .get(format!("{}{REFRESH}", base.trim_end_matches('/')))
        .headers(headers)
        .header(ACCEPT, "application/json")
        .send()
        .map_err(|e| {
            if e.is_timeout() {
                CliError::Upstream(format!("{what} timed out after {} s", TIMEOUT.as_secs()))
            } else {
                CliError::Upstream(format!("{what} got no answer"))
            }
        })?;
    let status = answer.status().as_u16();
    if status == 401 || status == 403 {
        return Err(refused_session(
            "refresh token",
            &what,
            &format!("HTTP {status}"),
            profile,
            &session.clinic,
        ));
    }
    if !(200..300).contains(&status) {
        return Err(CliError::Upstream(format!(
            "Embryo Options answered {what} with HTTP {status}"
        )));
    }
    let mut body = Vec::new();
    answer
        .take(64 * 1024)
        .read_to_end(&mut body)
        .map_err(|_| CliError::Upstream(format!("{what} answered HTTP {status} and its body did not arrive")))?;
    let tokens =
        crate::token::tokens_from(&Secret::new(String::from_utf8_lossy(&body).into_owned())).map_err(|_| {
            CliError::Upstream(format!(
                "{what} answered without a new token pair; the site may have changed"
            ))
        })?;
    Ok(tokens)
}

/// The sign-in that replaces a refused or expired session, at the stored
/// session's clinic.
pub fn login_remedy(profile: &ProfileName, clinic: &Clinic) -> String {
    format!("{BIN} auth login --browser --clinic {clinic} --profile {profile} --overwrite")
}

/// Exit 3: Embryo Options refused the profile's `token` (the access token or
/// the refresh token) when it was sent to `what`.
fn refused_session(token: &str, what: &str, said: &str, profile: &ProfileName, clinic: &Clinic) -> CliError {
    CliError::Auth(format!(
        "Embryo Options refused profile `{profile}`'s {token} ({what}: {said}): it has signed out or expired"
    ))
    .because(Reason::CredentialRefused)
    .with_remedy(login_remedy(profile, clinic))
}

/// The answers that are no data whatever the read: a refused token, a
/// path the API does not have, the edge's page, too many requests, the
/// API's own failures.
pub fn refuse(a: &Answer, profile: &ProfileName, clinic: &Clinic) -> Result<(), CliError> {
    let what = &a.what;
    let status = a.status;
    let html = a.content_type.as_deref().is_some_and(|t| t.contains("html"));
    match status {
        200..=299 => Ok(()),
        401 => Err(refused_session("access token", what, "HTTP 401", profile, clinic)),
        300..=399 => Err(CliError::Upstream(format!(
            "Embryo Options answered {what} with a redirect (HTTP {status}); the API may have changed"
        ))),
        403 if html => Err(CliError::Upstream(format!(
            "the site's edge refused {what} (HTTP 403 with its error page)"
        ))
        .because(Reason::ClientBlocked)
        .with_remedy(format!(
            "sign in again with `{}`, which keeps the browser's user agent",
            login_remedy(profile, clinic)
        ))),
        403 => Err(CliError::Upstream(format!("Embryo Options refused {what} (HTTP 403)"))
            .because(Reason::Forbidden)
            .with_remedy(format!(
                "the API answers 403 to a token it no longer takes: sign in again with `{}`",
                login_remedy(profile, clinic)
            ))),
        404 => Err(CliError::NotFound(format!("Embryo Options has no {what} (HTTP 404)"))),
        429 => {
            let e = CliError::Upstream(format!(
                "Embryo Options answered {what} with HTTP 429: too many requests"
            ));
            Err(match &a.retry_after {
                Some(after) => e.with_remedy(format!("try again after {} s", after.trim())),
                None => e.with_remedy("try again in a few minutes"),
            })
        }
        500..=599 => Err(
            CliError::Upstream(format!("Embryo Options answered {what} with HTTP {status}"))
                .with_remedy("try again later"),
        ),
        400 => Err(CliError::Upstream(format!(
            "Embryo Options refused {what} as a bad request (HTTP 400); the API may have changed"
        ))
        .because(Reason::BadRequest)),
        _ => Err(CliError::Upstream(format!(
            "Embryo Options answered {what} with HTTP {status}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adult1() -> ProfileName {
        ProfileName::parse("adult1").unwrap()
    }

    fn example() -> Clinic {
        Clinic::parse("exampleclinic").unwrap()
    }

    #[test]
    fn a_test_run_without_a_base_is_refused_before_any_request() {
        for test_store in [true, false] {
            let e = base_from(None, test_store, true).unwrap_err();
            assert_eq!(e.exit_code(), 2);
            let m = e.to_string();
            assert!(
                m.contains(BASE_URL_ENV) && m.contains(crate::session::KEYCHAIN_SERVICE_ENV),
                "{m}"
            );
        }
        assert_eq!(
            base_from(Some("http://127.0.0.1:8"), true, true).unwrap(),
            "http://127.0.0.1:8"
        );
    }

    #[test]
    fn the_base_is_the_api_unless_a_test_points_it_at_loopback() {
        assert_eq!(base_from(None, false, false).unwrap(), BASE);
        assert_eq!(base_from(Some("  "), false, false).unwrap(), BASE);
        for ok in [
            "http://127.0.0.1:8080",
            "http://127.0.0.1:8080/",
            "http://localhost:9",
            "http://[::1]:9",
        ] {
            let b = base_from(Some(ok), true, false).unwrap();
            assert!(!b.ends_with('/'), "{b}");
        }
        // Without a test's own store, a real token could travel.
        assert_eq!(
            base_from(Some("http://127.0.0.1:8080"), false, false)
                .unwrap_err()
                .exit_code(),
            2
        );
        for bad in [
            "https://member-api.prod.embryooptions.com.example.com",
            "http://example.com",
            "http://127.0.0.1:1@example.com",
            "http://user@localhost:8",
            "https://127.0.0.1:8",
            "http://127.0.0.1:8/api",
            "http://127.0.0.1:8/?x=1",
            "http://127.0.0.2:8",
            "127.0.0.1:8",
        ] {
            assert_eq!(base_from(Some(bad), true, false).unwrap_err().exit_code(), 2, "{bad}");
        }
    }

    fn answer(status: u16, content_type: &str) -> Answer {
        Answer {
            what: "GET /api/Members/{memberId}/MemberAccount/Member".into(),
            status,
            content_type: Some(content_type.into()),
            retry_after: None,
            body: Vec::new(),
        }
    }

    #[test]
    fn refuse_turns_what_is_no_data_into_errors() {
        assert!(refuse(&answer(200, "application/json"), &adult1(), &example()).is_ok());
        for (status, mime, exit) in [
            (401, "application/json", 3),
            (302, "text/html", 5),
            (403, "text/html", 5),
            (403, "application/json", 5),
            (404, "application/json", 4),
            (429, "application/json", 5),
            (503, "text/html", 5),
            (400, "application/json", 5),
        ] {
            let e = refuse(&answer(status, mime), &adult1(), &example()).err().unwrap();
            assert_eq!(e.exit_code(), exit, "{status}: {e}");
        }
        let e = refuse(&answer(401, "application/json"), &adult1(), &example())
            .err()
            .unwrap();
        assert!(
            e.to_string()
                .contains("auth login --browser --clinic exampleclinic --profile adult1 --overwrite"),
            "{e}"
        );
    }

    /// The reads the bundle names pass; the writes it names, and anything
    /// outside the member's paths, do not.
    #[test]
    fn the_api_reads_only_the_members_own_paths() {
        for ok in [
            "/api/Members/{memberId}/MemberAccount/Member?ivfCenterId={ivfCenterId}",
            "/api/Members/{memberId}/MemberAccount/MemberPaymentHistoryAll",
            "/api/members/{memberId}/memberaccount/memberpaymenthistoryall",
            "/api/Members/{memberId}/Member/UpComingBills",
            "/api/Members/{memberId}/FreezeEvents/FreezeEvents",
            "/api/Members/{memberId}/MemberIvfCenter",
            "/api/Members/4242/MemberIvfCenter/CollectionSites",
            "https://member-api.prod.embryooptions.com/api/Members/{memberId}/MemberAccount/Member",
        ] {
            assert!(api_path(ok).is_ok(), "{ok}");
        }
        for bad in [
            "/api/Auth/Login",
            "/api/Auth/RefreshToken",
            "/api/Configuration",
            "/api/Members/{memberId}/Payment/MemberPayment",
            "/api/Members/{memberId}/Payment/ClientToken?ivfCenterId={ivfCenterId}",
            "/api/Members/{memberId}/CardVaultingOrder",
            "/api/Members/{memberId}/PayPalVaultingOrder",
            "/api/Members/{memberId}/MemberPreempt/GetDefaultPaymentMethod",
            "/api/Members/{memberId}/MemberPreempt/UpdateDisposition",
            "/api/Members/{memberId}/MemberPreempt/ChargeNoticeFee",
            "/api/Members/{memberId}/Member/IsExploringDisposition",
            "/api/Members/{memberId}/Consents",
            "/api/Members/{memberId}/DocuSign/Details",
            "/api/Members/{memberId}/MemberAccount/UpdateProfile",
            "/api/Members/{memberId}/MemberAccount/UpdateCredentials",
            "/api/Members/{memberId}/CollectionAppointment/MemberCancelCollection",
            "/api/Members/{memberId}/CollectionAppointment/ResendEnrollmentPackage",
            "/api/Members/{memberId}/MatchingSite/AcceptMatchRequest",
            "/api/Members/{memberId}/MatchingSite/MatchingSiteMembersBySearch",
            "/api/Members/{memberId}/MemberPreempt/ChargeExistingPaymentMethod",
            "/api/Members/{memberId}/MemberIvfCenter/MatchingSiteMaintenanceModeDetails",
            "/api/Members/{memberId}/MemberAccount/Member?update=1",
            "/api/Members/{memberId}/MemberAccount/Member?token=x",
            "/api/Members/{memberId}",
            "/api/Members/{memberId}/",
            "/api/Members//MemberAccount/Member",
            "/api/Members/a.b/MemberAccount/Member",
            "/api/Members/{memberId}/{other}/Member",
            "/api/Members/{memberId}/MemberAccount/Member?{x}=1",
            "/api/Members/{memberId}/MemberAccount/Member?ivfCenterId={other}",
            "/api/Members/{memberId}/../Auth/Login",
            "/api/Members/{memberId}/%2e%2e/Auth",
            "/api/Members/{memberId}/MemberAccount/Member#x",
            "https://example.com/api/Members/{memberId}/MemberAccount/Member",
            "http://member-api.prod.embryooptions.com/api/Members/{memberId}/MemberAccount/Member",
            "api/Members/{memberId}/MemberAccount/Member",
            "/api/Members/{memberId}/MemberAccount/Member ?x=1",
            "/",
        ] {
            assert_eq!(api_path(bad).err().unwrap().exit_code(), 2, "{bad}");
        }
    }

    fn claims(member: Option<&str>, center: Option<&str>) -> Claims {
        Claims {
            member_id: member.map(str::to_string),
            ivf_center_id: center.map(str::to_string),
            member_type: None,
            intermediate: false,
            expires_at: None,
        }
    }

    #[test]
    fn a_path_is_resolved_for_the_tokens_member_only() {
        let c = claims(Some("4242"), Some("7"));
        let r = api_path("/api/Members/{memberId}/MemberAccount/Member?ivfCenterId={ivfCenterId}")
            .unwrap()
            .resolve(&c)
            .unwrap();
        assert_eq!(r.path, "/api/Members/4242/MemberAccount/Member?ivfCenterId=7");
        assert_eq!(r.what, "GET /api/Members/{memberId}/MemberAccount/Member");

        // The member's own id, spelled out, is named as the placeholder.
        let r = api_path("/api/Members/4242/MemberIvfCenter")
            .unwrap()
            .resolve(&c)
            .unwrap();
        assert_eq!(r.path, "/api/Members/4242/MemberIvfCenter");
        assert_eq!(r.what, "GET /api/Members/{memberId}/MemberIvfCenter");

        // Another member's id is refused before anything is sent.
        let e = api_path("/api/Members/4243/MemberIvfCenter")
            .unwrap()
            .resolve(&c)
            .err()
            .unwrap();
        assert_eq!(e.exit_code(), 2);
        assert!(!e.to_string().contains("4242"), "{e}");

        let no_center = claims(Some("4242"), None);
        let e = api_path("/api/Members/{memberId}/MemberAccount/Member?ivfCenterId={ivfCenterId}")
            .unwrap()
            .resolve(&no_center)
            .err()
            .unwrap();
        assert_eq!(e.exit_code(), 2);
        let e = api_path("/api/Members/{memberId}/MemberIvfCenter")
            .unwrap()
            .resolve(&claims(None, None))
            .err()
            .unwrap();
        assert_eq!(e.exit_code(), 3);
    }
}
