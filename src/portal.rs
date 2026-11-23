//! The commands that read Embryo Options as a profile. `app` prints what
//! these return; the tests run them in-process against a loopback server
//! with the session in an in-memory vault ([`Env`]).

use std::cell::{Cell, RefCell};
use std::time::Duration;

use pk_cli_config::profile::ProfileName;
use pk_cli_core::{CliError, Reason};

use crate::client::{self, Answer, ApiPath, Client};
use crate::session::{Session, SessionStore};
use crate::site::Clinic;
use crate::token::Claims;

/// Where sessions are kept and which API is read.
pub struct Env {
    pub sessions: SessionStore,
    /// The member API's origin, or a test's loopback server.
    pub base: String,
    /// The least time between one answer and the next request.
    pub pace: Duration,
    pub warnings: Warnings,
}

impl Env {
    /// The binary's: the vault, the member API, [`client::PACE`] apart,
    /// warnings on stderr.
    pub fn for_binary() -> Result<Env, CliError> {
        // The base first: a refused override reaches no vault.
        let base = client::base()?;
        Ok(Env {
            sessions: SessionStore::open()?,
            base,
            pace: client::PACE,
            warnings: Warnings::stderr(),
        })
    }

    /// Whether this reads the real API, as opposed to a test's server.
    pub fn is_real_site(&self) -> bool {
        self.base == client::BASE
    }
}

/// Where a warning goes: stderr for the binary, a list for a test.
pub struct Warnings(Option<RefCell<Vec<String>>>);

impl Warnings {
    pub fn stderr() -> Warnings {
        Warnings(None)
    }

    /// Kept for [`lines`](Self::lines) instead of printed.
    pub fn kept() -> Warnings {
        Warnings(Some(RefCell::default()))
    }

    pub fn warn(&self, line: String) {
        match &self.0 {
            None => eprintln!("warning: {line}"),
            Some(kept) => kept.borrow_mut().push(line),
        }
    }

    /// The warnings kept so far.
    pub fn lines(&self) -> Vec<String> {
        self.0.as_ref().map(|k| k.borrow().clone()).unwrap_or_default()
    }
}

/// A profile's signed-in reads.
pub struct Site<'e> {
    env: &'e Env,
    profile: ProfileName,
    session: RefCell<Session>,
    client: RefCell<Client>,
    claims: Claims,
    /// Whether a read's 403 has renewed the session already: once per
    /// command.
    renewed_after_refusal: Cell<bool>,
}

impl<'e> Site<'e> {
    /// The profile's stored session: none stored, or a token from the
    /// site's registration step, is exit 3 naming the sign-in, before any
    /// request.
    ///
    /// An access token that has expired, or expires within
    /// [`REFRESH_MARGIN`] seconds by this machine's clock, is renewed first
    /// ([`renew`]). A renewal that fails for any reason but a refused refresh
    /// token (exit 3) leaves a token that has not yet expired to be read
    /// with, with a warning.
    pub fn open(env: &'e Env, profile: &ProfileName) -> Result<Site<'e>, CliError> {
        let mut session = env.sessions.load(profile)?;
        let mut renewed = false;
        if due(&session) {
            match renew(env, profile, due) {
                Ok((latest, granted)) => {
                    session = latest;
                    renewed = granted;
                }
                Err(e) if e.exit_code() != 3 && !expired(&session) => env.warnings.warn(format!(
                    "profile `{profile}`'s Embryo Options sign-in was not renewed ({e}); this command reads \
                     with the current token, which expires within {REFRESH_MARGIN} s"
                )),
                Err(e) => return Err(e),
            }
        }
        let claims = session.claims().map_err(|_| {
            CliError::Auth(format!(
                "the access token stored for profile `{profile}` does not read as an Embryo Options token"
            ))
            .because(Reason::CredentialUnreadable)
            .with_remedy(client::login_remedy(profile, &session.clinic))
        })?;
        if claims.intermediate || claims.member_id.is_none() {
            return Err(CliError::Auth(format!(
                "profile `{profile}`'s sign-in stopped at the site's registration step, before the account"
            ))
            .because(Reason::CredentialRefused)
            .with_remedy(client::login_remedy(profile, &session.clinic)));
        }
        if claims.expired_at(now_unix()) {
            let when = claims
                .expires_at
                .map(pk_cli_core::dates::fmt_rfc3339)
                .unwrap_or_default();
            let why = if renewed {
                "and Embryo Options renewed it with a token that had expired too"
            } else {
                "and no refresh token was stored to renew it"
            };
            return Err(CliError::Auth(format!(
                "profile `{profile}`'s Embryo Options sign-in expired at {when}, {why}"
            ))
            .because(Reason::CredentialRefused)
            .with_remedy(client::login_remedy(profile, &session.clinic)));
        }
        let client = Client::new(&session, profile, &env.base, env.pace)?;
        Ok(Site {
            env,
            profile: profile.clone(),
            session: RefCell::new(session),
            client: RefCell::new(client),
            claims,
            renewed_after_refusal: Cell::new(false),
        })
    }

    pub fn profile(&self) -> &ProfileName {
        &self.profile
    }

    /// The clinic the session was signed in at.
    pub fn clinic(&self) -> Clinic {
        self.session.borrow().clinic.clone()
    }

    /// The token's member id: [`Site::open`] refuses a token without one.
    /// Never printed: it is hashed into the patient's id.
    pub fn member_id(&self) -> &str {
        self.claims.member_id.as_deref().unwrap_or_default()
    }

    /// The token's `MemberType` (`CRY` for embryos in storage).
    pub fn member_type(&self) -> Option<&str> {
        self.claims.member_type.as_deref()
    }

    pub fn warn(&self, line: String) {
        self.env.warnings.warn(line);
    }

    /// One read the readers make: `path` checked as `api` checks it
    /// ([`client::api_path`]), then its answer as JSON. An answer that is
    /// not JSON is exit 5.
    pub fn read(&self, path: &str) -> Result<serde_json::Value, CliError> {
        let answer = self.api(&client::api_path(path)?)?;
        serde_json::from_slice(&answer.body).map_err(|_| {
            CliError::Upstream(format!(
                "Embryo Options answered {} with no JSON (HTTP {})",
                answer.what, answer.status
            ))
            .with_remedy("the API may have changed; `embryooptions api GET <PATH>` shows the answer")
        })
    }

    /// `embryooptions api GET <path>`, the path already checked by
    /// [`client::api_path`] and resolved here for the token's member.
    ///
    /// A read the API answers 403 (its answer to an access token it no
    /// longer takes, whatever this machine's clock says) renews the session
    /// once per command, as the web app does, and is sent again with the
    /// new token. The renewal is skipped when no refresh token is stored,
    /// and sends nothing when another process has renewed meanwhile: its
    /// pair is read with.
    pub fn api(&self, path: &ApiPath) -> Result<Answer, CliError> {
        let read = path.resolve(&self.claims)?;
        let refused = match self.client.borrow().api(&read) {
            Err(e) if e.reason() == Some(Reason::Forbidden) => e,
            answer => return answer,
        };
        if self.renewed_after_refusal.replace(true) || self.session.borrow().refresh_token.is_none() {
            return Err(refused);
        }
        let presented = self.session.borrow().access_token.expose().to_string();
        self.client.borrow().pause();
        let (latest, _) = renew(self.env, &self.profile, |stored| {
            stored.refresh_token.is_some() && stored.access_token.expose() == presented
        })?;
        if latest.access_token.expose() == presented {
            return Err(refused);
        }
        let member = latest.claims().ok().and_then(|c| c.member_id);
        if member.is_none() || member != self.claims.member_id {
            return Err(CliError::Auth(format!(
                "profile `{}`'s renewed Embryo Options sign-in is not the same member's",
                self.profile
            ))
            .because(Reason::CredentialRefused)
            .with_remedy(client::login_remedy(&self.profile, &latest.clinic)));
        }
        let client = self.client.borrow().replaced(&latest)?;
        *self.client.borrow_mut() = client;
        *self.session.borrow_mut() = latest;
        self.client.borrow().api(&read)
    }
}

/// One renewal of `profile`'s session under the session store's lock
/// ([`SessionStore::renew`]): the stored session is read again and, if
/// `due` still holds for it, [`client::refresh`] asks for a new pair, which
/// is stored. Otherwise another process renewed or signed in meanwhile, and
/// its session is the answer, with nothing sent. A new pair that cannot be
/// stored is still the answer, with a warning: the refresh has already
/// spent the old one. The flag says whether a pair was granted.
fn renew(env: &Env, profile: &ProfileName, due: impl FnOnce(&Session) -> bool) -> Result<(Session, bool), CliError> {
    let mut granted = false;
    let session = env.sessions.renew(profile, due, |latest| {
        let tokens = client::refresh(&env.base, &latest, profile)?;
        let fresh = latest.renewed(tokens);
        granted = true;
        if let Err(e) = env.sessions.put(profile, &fresh) {
            env.warnings.warn(format!(
                "profile `{profile}`'s renewed Embryo Options sign-in was not stored ({e}); this command reads \
                 with it, and the next may need `{}`",
                client::login_remedy(profile, &latest.clinic)
            ));
        }
        Ok(fresh)
    })?;
    Ok((session, granted))
}

/// How long before its expiry an access token is renewed, in seconds: a
/// command's reads take less.
pub const REFRESH_MARGIN: i64 = 60;

/// Whether `session` is due a renewal: it has a refresh token, and its
/// access token has expired or expires within [`REFRESH_MARGIN`].
fn due(session: &Session) -> bool {
    session.refresh_token.is_some()
        && session
            .claims()
            .ok()
            .and_then(|c| c.expires_at)
            .is_some_and(|exp| exp <= now_unix().saturating_add(REFRESH_MARGIN))
}

/// Whether `session`'s access token has expired ([`Claims::expired_at`]).
fn expired(session: &Session) -> bool {
    session.claims().is_ok_and(|c| c.expired_at(now_unix()))
}

/// Now, Unix seconds.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn now_rfc3339() -> String {
    pk_cli_core::dates::fmt_rfc3339(now_unix())
}
