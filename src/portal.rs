//! The commands that read Embryo Options as a profile. `app` prints what
//! these return; the tests run them in-process against a loopback server
//! with the session in an in-memory vault ([`Env`]).

use std::time::Duration;

use pk_cli_config::profile::ProfileName;
use pk_cli_core::{CliError, Reason};

use crate::client::{self, Answer, ApiPath, Client};
use crate::session::SessionStore;
use crate::token::Claims;

/// Where sessions are kept and which API is read.
pub struct Env {
    pub sessions: SessionStore,
    /// The member API's origin, or a test's loopback server.
    pub base: String,
    /// The least time between one answer and the next request.
    pub pace: Duration,
}

impl Env {
    /// The binary's: the vault, the member API, [`client::PACE`] apart.
    pub fn for_binary() -> Result<Env, CliError> {
        // The base first: a refused override reaches no vault.
        let base = client::base()?;
        Ok(Env {
            sessions: SessionStore::open()?,
            base,
            pace: client::PACE,
        })
    }

    /// Whether this reads the real API, as opposed to a test's server.
    pub fn is_real_site(&self) -> bool {
        self.base == client::BASE
    }
}

/// A profile's signed-in reads.
pub struct Site {
    client: Client,
    claims: Claims,
}

impl Site {
    /// The profile's stored session: none stored, or a token from the
    /// site's registration step, is exit 3 naming the sign-in, before any
    /// request. An access token that has expired, or expires within
    /// [`REFRESH_MARGIN`] seconds, is renewed first with the refresh token
    /// ([`client::refresh`]) and the new pair stored, unless a sign-in made
    /// meanwhile replaced the session.
    pub fn open(env: &Env, profile: &ProfileName) -> Result<Site, CliError> {
        let mut session = env.sessions.load(profile)?;
        let due = session
            .claims()
            .ok()
            .and_then(|c| c.expires_at)
            .is_some_and(|exp| exp - REFRESH_MARGIN <= now_unix());
        let renewing = due && session.refresh_token.is_some();
        if renewing {
            let tokens = client::refresh(&env.base, &session, profile)?;
            let refresh_token = match tokens.refresh {
                Some(new) => Some(new),
                None => session
                    .refresh_token
                    .as_ref()
                    .map(|old| pk_cli_secrets::Secret::new(old.expose().to_string())),
            };
            let renewed = crate::session::Session {
                clinic: session.clinic.clone(),
                access_token: tokens.access,
                refresh_token,
                user_agent: session.user_agent.clone(),
                captured_at: session.captured_at.clone(),
            };
            match env.sessions.get(profile)? {
                Some(stored) if stored.access_token.expose() == session.access_token.expose() => {
                    env.sessions.put(profile, &renewed)?;
                }
                _ => {}
            }
            session = renewed;
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
            let why = if renewing {
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
        Ok(Site { client, claims })
    }

    /// `embryooptions api GET <path>`, the path already checked by
    /// [`client::api_path`] and resolved here for the token's member.
    pub fn api(&self, path: &ApiPath) -> Result<Answer, CliError> {
        let read = path.resolve(&self.claims)?;
        self.client.api(&read)
    }
}

/// How long before its expiry an access token is renewed, in seconds: a
/// command's reads take less.
pub const REFRESH_MARGIN: i64 = 60;

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
