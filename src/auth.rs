//! `auth login`, `auth status`, `auth logout` and `auth reset`, and what
//! `profiles remove` removes: an Embryo Options session per profile
//! (DESIGN.md §1.7, §2.2, §2.5), kept through [`SessionStore`], which alone
//! knows where.
//!
//! A login belongs to one clinic: `auth login --clinic <SLUG>` names it, and
//! it is kept with the session, so a later sign-in of the profile
//! (`--overwrite`) finds it there. A `--browser` sign-in runs Chrome in the
//! profile's own kept browser profile (`Launch::profile_name`), so one
//! account's window never opens signed in as another; `auth logout
//! --profile <NAME>` removes the session, then that browser profile. While
//! `EMBRYOOPTIONS_KEYCHAIN_SERVICE` points the store elsewhere
//! ([`session::hermetic`]), nothing touches the real browser profiles:
//! `auth status` does not report them, `auth logout` leaves them in place,
//! and `auth login --browser` signs in through a fresh one.

use pk_cli_auth::{AuthLogin, AuthMethod, AuthStatus};
use pk_cli_browser::{BrowserError, Confirm, Launch};
use pk_cli_core::{output, CliError, Reason};
use pk_cli_secrets::Secret;
use serde_json::{json, Value};

use crate::cli::{LoginCmd, BIN};
use crate::session::{self, Session, SessionStore, KEYCHAIN_SERVICE_ENV};
use crate::site::{self, Clinic};
use crate::token::{self, Tokens};
use pk_cli_config::profile::ProfileName;

// ----------------------------------------------------------------- login

pub fn login(cmd: &LoginCmd, profile: &ProfileName, json: bool, quiet: bool) -> Result<(), CliError> {
    let LoginCmd {
        login,
        browser,
        clinic,
        user_agent,
    } = cmd;
    let manual = login.source.stdin || login.source.from_env.is_some();
    // Flags first: a refused combination reaches neither the keychain nor
    // the site.
    if browser.browser && manual {
        return Err(CliError::Usage(
            "--browser takes the session from the browser; drop --stdin and --from-env".into(),
        ));
    }
    if login.source.stdin && login.source.from_env.is_some() {
        return Err(CliError::Usage("pass one of --stdin and --from-env".into()));
    }
    if user_agent.is_some() && !manual {
        return Err(CliError::Usage(
            "--user-agent goes with --stdin or --from-env; --browser reads the browser's own".into(),
        ));
    }
    if !browser.browser && !manual {
        return Err(CliError::Usage(
            "pass --browser to sign in through a Chrome window on the clinic's web app, or --stdin with the \
             access token copied from a signed-in browser"
                .into(),
        ));
    }
    let given = clinic.as_deref().map(Clinic::parse).transpose()?;
    let base = crate::client::base()?;
    if browser.browser && base != crate::client::BASE {
        return Err(CliError::Usage(format!(
            "--browser signs in to the real site; unset {} for it",
            crate::client::BASE_URL_ENV
        )));
    }
    let verify = !login.no_verify;
    let store = SessionStore::open()?;
    // Before Chrome opens or stdin is read, so no sign-in is spent on a
    // session that would not be stored.
    let stored = match store.get(profile) {
        Ok(found) => Stored::Readable(found),
        Err(e) if e.reason() == Some(Reason::CredentialUnreadable) => Stored::Unreadable,
        Err(e) => return Err(e),
    };
    let present = !matches!(stored, Stored::Readable(None));
    if present && !login.overwrite {
        return Err(CliError::Usage(format!(
            "an Embryo Options session is already stored for profile `{profile}`; pass --overwrite to replace it \
             (`{BIN} auth logout --profile {profile}` removes it)"
        )));
    }
    let clinic = match (given, stored) {
        (Some(clinic), _) => clinic,
        (None, Stored::Readable(Some(session))) => session.clinic,
        (None, _) => {
            return Err(CliError::Usage(format!(
                "pass --clinic <SLUG>: the clinic's name in its web app's address, `<SLUG>.{}`. It is kept with \
                 the session, so profile `{profile}` needs it only on its first sign-in",
                site::DOMAIN
            )))
        }
    };
    // A stuck lock fails here, before anyone signs in (`store.put_signin`).
    store.before_signin()?;
    let session = match manual {
        false => browser_session(profile, &clinic, browser.fresh_profile, verify, quiet)?,
        true => manual_session(
            &login.source.read(None)?,
            clinic.clone(),
            user_agent.as_deref(),
            verify,
            profile,
            |tokens, agent| site::confirm_tokens_at(&base, tokens, agent),
        )?,
    };
    store.put_signin(profile, &session)?;
    if verify {
        if !quiet {
            eprintln!("Embryo Options session at {clinic} stored for profile `{profile}`");
        }
    } else {
        // Even under -q: the session is stored with nothing to show it works.
        eprintln!(
            "warning: the session was stored unverified (--no-verify): no Embryo Options read checked it. \
             `{BIN} auth login --browser --clinic {clinic} --profile {profile} --overwrite` replaces it"
        );
    }
    if json {
        output::json(&AuthLogin::stored(verify).to_json());
    }
    Ok(())
}

/// What the vault held for the profile before a sign-in.
enum Stored {
    Readable(Option<Session>),
    Unreadable,
}

/// The session from a Chrome window: the owner signs in on the clinic's web
/// app, and the capture hands over the tokens once the app holds them and,
/// with `verify`, [`site::confirm_candidate`] accepts them.
fn browser_session(
    profile: &ProfileName,
    clinic: &Clinic,
    fresh: bool,
    verify: bool,
    quiet: bool,
) -> Result<Session, CliError> {
    let scratch = fresh || session::hermetic();
    if scratch && !fresh && !quiet {
        eprintln!(
            "{KEYCHAIN_SERVICE_ENV} is set, so this sign-in uses a fresh browser profile and leaves the kept one alone"
        );
    }
    let launch = Launch::new(BIN)
        .url(clinic.start_url())
        .profile_name(profile.as_str())
        .fresh_profile(scratch)
        .login_hint(format!(
            "{BIN} auth login --browser --clinic {clinic} --profile {profile}"
        ))
        .quiet(quiet);
    let captured = pk_cli_browser::capture(launch, &site::capture(clinic, verify)?, pk_cli_browser::DEFAULT_TIMEOUT)
        .map_err(|e| match e {
            e @ BrowserError::ChromeNotFound(_) => CliError::from(e).with_remedy(format!(
                "without Chrome, copy the access token from a signed-in browser and pipe it to `{BIN} auth login \
                 --stdin --clinic {clinic} --profile {profile}` (README.md)"
            )),
            e => e.into(),
        })?;
    let agent = captured.user_agent().map(str::to_string);
    let value = captured
        .into_value()
        .ok_or_else(|| CliError::Other("the sign-in handed over no access token".into()))?;
    let tokens = token::tokens_from(&value).map_err(|e| {
        CliError::Other(format!(
            "the clinic's web app holds an access token embryooptions cannot read ({}); the site may have changed",
            e.message()
        ))
    })?;
    Ok(Session::new(clinic.clone(), tokens, agent))
}

/// A `--stdin` (or `--from-env`) import, checked with `confirm` unless
/// `verify` is off. Unlike a browser capture, any answer short of `Ready`
/// stores nothing, because the value can be piped again.
fn manual_session(
    raw: &Secret,
    clinic: Clinic,
    user_agent: Option<&str>,
    verify: bool,
    profile: &ProfileName,
    confirm: impl FnOnce(&Tokens, Option<&str>) -> Confirm,
) -> Result<Session, CliError> {
    let tokens = token::tokens_from(raw)?;
    if verify {
        match confirm(&tokens, user_agent) {
            Confirm::Ready => {}
            Confirm::NotYet(reason) => {
                return Err(CliError::Auth(format!(
                    "Embryo Options did not accept the token ({reason}); nothing was stored"
                ))
                .with_remedy(format!(
                    "sign in with `{BIN} auth login --browser --clinic {clinic} --profile {profile}`; --no-verify \
                     stores the token unchecked"
                )))
            }
            Confirm::Undecided { reason, .. } => {
                return Err(CliError::Upstream(format!(
                    "Embryo Options's answer did not show whether the token works ({reason}); nothing was stored"
                ))
                .with_remedy("pipe it again in a few minutes"))
            }
            Confirm::Fail(e) => return Err(e),
            _ => {
                return Err(CliError::Other(
                    "the token check answered in a way this embryooptions does not know; nothing was stored".into(),
                ))
            }
        }
    }
    Ok(Session::new(clinic, tokens, user_agent.map(str::to_string)))
}

// ---------------------------------------------------------------- status

/// `auth status`: local facts only, no request. `authenticated` is a stored
/// session whose token has not expired by its own `exp`.
pub fn status(profile: &ProfileName, json: bool, quiet: bool) -> Result<(), CliError> {
    let store = SessionStore::open()?;
    let (stored, session) = match store.get(profile) {
        Ok(session) => (session.is_some(), session),
        Err(e) if e.reason() == Some(Reason::CredentialUnreadable) => {
            eprintln!("warning: {}", e.message());
            (true, None)
        }
        Err(e) => return Err(e),
    };
    let claims = session.as_ref().and_then(|s| s.claims().ok());
    let usable = claims
        .as_ref()
        .is_some_and(|c| !c.intermediate && c.member_id.is_some() && !c.expired_at(crate::portal::now_unix()));
    let mut status = AuthStatus::new(true, usable, AuthMethod::BrowserSession);
    status.account = Some(profile.as_str().to_string());
    status.credential_in_keychain = Some(stored);
    status.expires_at = claims
        .as_ref()
        .and_then(|c| c.expires_at)
        .map(pk_cli_core::dates::fmt_rfc3339);
    let (kept, line) = browser_profile_status(session::hermetic(), || {
        pk_cli_browser::named_profile_exists(BIN, profile.as_str())
    });
    status.browser_profile = kept;
    if let (Some(line), false) = (line, quiet) {
        eprintln!("{line}");
    }
    let clinic = session.as_ref().map(|s| s.clinic.slug().to_string());
    let captured_at = session.as_ref().map(|s| s.captured_at.clone());
    if json {
        let mut v = status.to_json();
        if let Some(clinic) = &clinic {
            v["clinic"] = json!(clinic);
        }
        if let Some(at) = &captured_at {
            v["captured_at"] = json!(at);
        }
        output::json(&v);
    } else {
        status.render();
        if let Some(clinic) = &clinic {
            println!("Clinic:        {clinic}");
        }
        if let Some(at) = &captured_at {
            println!("Captured:      {at}");
        }
    }
    Ok(())
}

/// `auth status`'s browser field: whether a `--browser` sign-in keeps a
/// profile here, not whether it is signed in, and any line for stderr. A
/// profile that needs repair is a warning and leaves the field out, and so
/// does [`KEYCHAIN_SERVICE_ENV`], under which nothing reads the real profile.
/// The check is passed in: the binary binds it to `profile_exists(BIN)`, the
/// tests to `profile_exists_in` under a temp dir.
fn browser_profile_status(
    overridden: bool,
    exists: impl FnOnce() -> Result<bool, BrowserError>,
) -> (Option<bool>, Option<String>) {
    if overridden {
        return (
            None,
            Some(format!(
                "{KEYCHAIN_SERVICE_ENV} is set, so the kept browser profile is not reported"
            )),
        );
    }
    match exists() {
        Ok(kept) => (Some(kept), None),
        Err(e) => (None, Some(format!("warning: {e}"))),
    }
}

// ---------------------------------------------------------------- logout

pub fn logout(profile: &ProfileName, json: bool, quiet: bool) -> Result<(), CliError> {
    let done = forget_login(profile)?;
    if json {
        output::json(&done.to_json());
    } else if !quiet {
        eprintln!("{}", done.line());
    }
    Ok(())
}

/// What a login keeps, removed: the profile's session, then the browser
/// profile its `--browser` sign-in keeps (`forget_named_profile`), which
/// [`session::hermetic`] leaves alone. `auth logout` and `profiles remove`.
pub fn forget_login(profile: &ProfileName) -> Result<LoggedOut, CliError> {
    let store = SessionStore::open()?;
    let forget = (!session::hermetic()).then_some(|| pk_cli_browser::forget_named_profile(BIN, profile.as_str()));
    run_logout(|| store.delete(profile), forget, profile)
}

/// What `auth logout` removed (`auth-logout/v1`).
#[derive(Debug, PartialEq, Eq)]
pub struct LoggedOut {
    pub session_removed: bool,
    /// `None` when the profile was left alone ([`KEYCHAIN_SERVICE_ENV`]).
    pub browser_profile_removed: Option<bool>,
}

impl LoggedOut {
    fn to_json(&self) -> Value {
        let mut v = json!({ "schema": "auth-logout/v1", "session_removed": self.session_removed });
        if let Some(removed) = self.browser_profile_removed {
            v["browser_profile_removed"] = removed.into();
        }
        v
    }

    fn line(&self) -> String {
        format!(
            "logged out: {}; {}",
            session_said(self.session_removed),
            profile_said(self.browser_profile_removed)
        )
    }
}

fn session_said(removed: bool) -> &'static str {
    if removed {
        "the session was removed from the vault"
    } else {
        "no session was stored"
    }
}

fn profile_said(removed: Option<bool>) -> String {
    match removed {
        Some(true) => "the browser profile was removed".into(),
        Some(false) => "no browser profile was kept".into(),
        None => format!("{KEYCHAIN_SERVICE_ENV} is set, so the kept browser profile was left alone"),
    }
}

/// `auth logout`'s two removals: the session first, then the browser
/// profile (`None` leaves it alone). Both are attempted whatever the other
/// did, so a refusal of one leaves the other done, and the error says what
/// was. They are passed in: the binary binds them to the session store and
/// to `forget_named_profile(BIN, profile)`, the tests to fakes and to
/// `forget_named_profile_in` under a temp dir, so a test never removes the
/// owner's session or profile.
fn run_logout(
    delete_session: impl FnOnce() -> Result<bool, CliError>,
    forget_profile: Option<impl FnOnce() -> Result<bool, BrowserError>>,
    who: &ProfileName,
) -> Result<LoggedOut, CliError> {
    let session = delete_session();
    let profile = forget_profile.map(|forget| forget().map_err(CliError::from));
    match (session, profile) {
        (Ok(session_removed), None) => Ok(LoggedOut {
            session_removed,
            browser_profile_removed: None,
        }),
        (Ok(session_removed), Some(Ok(removed))) => Ok(LoggedOut {
            session_removed,
            browser_profile_removed: Some(removed),
        }),
        (Ok(session_removed), Some(Err(e))) => Err(e.with_remedy(format!(
            "{}; the browser profile is all that is left",
            session_said(session_removed)
        ))),
        (Err(e), profile) => {
            let profile = match profile {
                None => profile_said(None),
                Some(Ok(removed)) => profile_said(Some(removed)),
                Some(Err(_)) => "the browser profile was not removed either".into(),
            };
            Err(e.with_remedy(format!("{profile}; run `{BIN} auth logout --profile {who}` again")))
        }
    }
}

// ----------------------------------------------------------------- reset

/// `auth reset`: every profile's session and the vault's key, removed under
/// the vault's key lock (`Vault::clear`). For a key that is lost or damaged,
/// when no session reads; the profiles and the kept browser profiles stay.
/// Destructive, so it asks on a terminal and needs `--force` otherwise
/// (exit 6), decided before the vault is opened.
pub fn reset(force: bool, interactive: bool, json: bool, quiet: bool) -> Result<(), CliError> {
    let what = "remove every profile's Embryo Options session and the vault's key";
    pk_cli_core::confirm::require_confirmable(force, interactive, what)?;
    pk_cli_core::confirm::confirm(
        force,
        "Remove every profile's stored Embryo Options session and the vault's key? Each profile signs in again after",
    )?;
    let removed = SessionStore::open()?.clear()?;
    if json {
        output::json(&json!({ "schema": "auth-reset/v1", "sessions_removed": removed }));
    } else if !quiet {
        eprintln!(
            "reset: {removed} stored session(s) and the vault's key removed; sign in again with `{BIN} auth login \
             --browser --clinic <SLUG> --profile <NAME>`"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adult1() -> ProfileName {
        ProfileName::parse("adult1").unwrap()
    }

    fn clinic() -> Clinic {
        Clinic::parse("exampleclinic").unwrap()
    }

    fn access() -> String {
        crate::token::test_token(&json!({ "user": json!({ "UserId": 4242, "IVFCenterId": 7 }).to_string() }))
    }

    /// A `--stdin` import stores only what the read accepted, unless
    /// --no-verify skips the read; what does not read as a token is exit 2
    /// before any read.
    #[test]
    fn a_manual_import_stores_only_an_accepted_session() {
        let raw = Secret::new(format!(
            "{{\"accessToken\":\"{}\",\"refreshToken\":\"dummy-refresh\"}}",
            access()
        ));
        let agent = Some("Mozilla/5.0 (test)");
        let ok = manual_session(&raw, clinic(), agent, true, &adult1(), |t, a| {
            assert_eq!(t.access.expose(), access());
            assert_eq!(a, Some("Mozilla/5.0 (test)"));
            Confirm::Ready
        })
        .unwrap();
        assert_eq!(ok.access_token.expose(), access());
        assert_eq!(ok.refresh_token.as_ref().map(|r| r.expose()), Some("dummy-refresh"));
        assert_eq!(ok.clinic, clinic());
        assert_eq!(ok.user_agent.as_deref(), Some("Mozilla/5.0 (test)"));

        let not_signed_in = "GET /x answered HTTP 401: not signed in";
        let refused = manual_session(&raw, clinic(), agent, true, &adult1(), |_, _| {
            Confirm::NotYet(not_signed_in.into())
        })
        .err()
        .unwrap();
        assert_eq!(refused.exit_code(), 3);
        assert!(refused.message().contains(not_signed_in), "{}", refused.message());
        assert!(refused.to_string().contains("--no-verify"));
        assert!(refused.to_string().contains("--clinic exampleclinic"));

        let undecided = manual_session(&raw, clinic(), agent, true, &adult1(), |_, _| {
            Confirm::undecided("GET /x answered HTTP 503")
        })
        .err()
        .unwrap();
        assert_eq!(undecided.exit_code(), 5);

        let failed = manual_session(&raw, clinic(), agent, true, &adult1(), |_, _| {
            Confirm::Fail(CliError::Other("x".into()))
        })
        .err()
        .unwrap();
        assert_eq!(failed.exit_code(), 1);

        // The token alone, no user agent, unchecked.
        let unchecked = manual_session(&Secret::new(access()), clinic(), None, false, &adult1(), |_, _| {
            panic!("--no-verify makes no read")
        })
        .unwrap();
        assert_eq!(unchecked.access_token.expose(), access());
        assert!(unchecked.refresh_token.is_none());
        assert_eq!(unchecked.user_agent, None);

        // What is not a token is checked before any read.
        let bad = manual_session(
            &Secret::new("dummy_secret"),
            clinic(),
            agent,
            true,
            &adult1(),
            |_, _| panic!("no read for what is not a token"),
        );
        let e = bad.err().unwrap();
        assert_eq!(e.exit_code(), 2);
        assert!(!e.to_string().contains("dummy_secret"), "{e}");
    }

    fn temp_root(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("embryooptions-auth-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let profile = root.join(format!("piekstra.{BIN}")).join("browser-profile");
        (root, profile)
    }

    /// The status's browser field on a temp dir through `profile_exists_in`,
    /// so the test never reads the owner's real data dir.
    #[test]
    fn auth_status_reports_the_kept_browser_profile() {
        let (root, _) = temp_root("status");
        let profile = root.join(format!("piekstra.{BIN}")).join("browser-profile-adult1");
        let exists = || pk_cli_browser::named_profile_exists_in(&root, BIN, "adult1");
        assert_eq!(browser_profile_status(false, exists), (Some(false), None));
        assert!(!root.exists(), "asking creates nothing");

        std::fs::create_dir_all(&profile).unwrap();
        assert_eq!(browser_profile_status(false, exists), (Some(true), None));
        let mut status = AuthStatus::new(false, false, AuthMethod::BrowserSession);
        status.browser_profile = browser_profile_status(false, exists).0;
        assert_eq!(status.to_json()["browser_profile"], true);
        assert_eq!(status.to_json()["method"], "browser-session");

        // Under the test service the real profile is never asked about.
        let (field, line) = browser_profile_status(true, || panic!("the real profile was read"));
        assert_eq!(field, None);
        assert!(line.unwrap().contains(KEYCHAIN_SERVICE_ENV));

        // A profile that needs repair leaves the field out with a warning.
        std::fs::remove_dir(&profile).unwrap();
        std::fs::write(&profile, "x").unwrap();
        let (field, line) = browser_profile_status(false, exists);
        assert_eq!(field, None);
        assert!(line.unwrap().starts_with("warning: "));
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The logout's removals on fakes and a temp dir through
    /// `forget_profile_in`: the keychain item first, then the profile, both
    /// reported.
    #[test]
    fn logout_removes_the_session_then_the_kept_profile() {
        let (root, _) = temp_root("logout");
        let profile = root.join(format!("piekstra.{BIN}")).join("browser-profile-adult1");
        std::fs::create_dir_all(profile.join("Default")).unwrap();
        let order = std::cell::RefCell::new(Vec::new());
        let done = run_logout(
            || {
                order.borrow_mut().push("keychain");
                Ok(true)
            },
            Some(|| {
                order.borrow_mut().push("profile");
                pk_cli_browser::forget_named_profile_in(&root, BIN, "adult1")
            }),
            &adult1(),
        )
        .unwrap();
        assert_eq!(*order.borrow(), ["keychain", "profile"]);
        assert_eq!(
            done,
            LoggedOut {
                session_removed: true,
                browser_profile_removed: Some(true)
            }
        );
        assert!(!profile.exists());
        assert_eq!(
            done.to_json(),
            json!({ "schema": "auth-logout/v1", "session_removed": true, "browser_profile_removed": true })
        );
        assert_eq!(
            done.line(),
            "logged out: the session was removed from the vault; the browser profile was removed"
        );

        let nothing = run_logout(
            || Ok(false),
            Some(|| pk_cli_browser::forget_named_profile_in(&root, BIN, "adult1")),
            &adult1(),
        )
        .unwrap();
        assert_eq!(
            nothing.line(),
            "logged out: no session was stored; no browser profile was kept"
        );

        let left = run_logout(|| Ok(false), None::<fn() -> Result<bool, BrowserError>>, &adult1()).unwrap();
        assert!(left.to_json().get("browser_profile_removed").is_none());
        assert!(left.line().contains("left alone"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Each removal runs whatever the other did, and the error says what was
    /// done.
    #[test]
    fn a_logout_that_fails_halfway_says_what_was_done() {
        let e = run_logout(
            || Ok(true),
            Some(|| Err(BrowserError::ProfileInUse("a Chrome has it".into()))),
            &adult1(),
        )
        .err()
        .unwrap();
        assert_eq!(e.exit_code(), 1);
        assert!(e.to_string().contains("the session was removed"), "{e}");

        let profile_ran = std::cell::Cell::new(false);
        let e = run_logout(
            || Err(CliError::Keychain("denied".into())),
            Some(|| {
                profile_ran.set(true);
                Ok(true)
            }),
            &adult1(),
        )
        .err()
        .unwrap();
        assert!(profile_ran.get());
        assert!(e.to_string().contains("the browser profile was removed"), "{e}");
        assert!(e.to_string().contains("auth logout --profile adult1"), "{e}");
    }
}
