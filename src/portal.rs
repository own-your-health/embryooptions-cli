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
    /// The profile's stored session: none stored, a token from the site's
    /// registration step, or one that has expired is exit 3 naming the
    /// sign-in, before any request (the token refresh is not built:
    /// docs/api.md).
    pub fn open(env: &Env, profile: &ProfileName) -> Result<Site, CliError> {
        let session = env.sessions.load(profile)?;
        let claims = session.claims().map_err(|_| {
            CliError::Auth(format!(
                "the access token stored for profile `{profile}` does not read as an Embryo Options token"
            ))
            .because(Reason::CredentialUnreadable)
            .with_remedy(client::login_remedy(profile))
        })?;
        if claims.intermediate || claims.member_id.is_none() {
            return Err(CliError::Auth(format!(
                "profile `{profile}`'s sign-in stopped at the site's registration step, before the account"
            ))
            .because(Reason::CredentialRefused)
            .with_remedy(client::login_remedy(profile)));
        }
        if claims.expired_at(now_unix()) {
            let when = claims
                .expires_at
                .map(pk_cli_core::dates::fmt_rfc3339)
                .unwrap_or_default();
            return Err(CliError::Auth(format!(
                "profile `{profile}`'s Embryo Options sign-in expired at {when}; embryooptions does not renew it yet"
            ))
            .because(Reason::CredentialRefused)
            .with_remedy(client::login_remedy(profile)));
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
