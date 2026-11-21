//! The command tree (piekstra-cli/1).

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use pk_cli_auth::LoginArgs;
use pk_cli_browser::BrowserArgs;
use pk_cli_core::CommonArgs;
use pk_cli_selfupdate::SelfUpdateArgs;

pub const BIN: &str = "embryooptions";

/// Unofficial read-only CLI for a member's storage bills on Embryo Options,
/// the portal IVF clinics use to bill for keeping embryos and eggs frozen:
/// the account, the billing history and the payments made. It never pays,
/// changes a payment method or touches a disposition or a consent.
#[derive(Parser, Debug)]
#[command(name = BIN, version, about, long_about = None)]
pub struct Cli {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Override the config file location.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Read Embryo Options as this profile: one configured with `embryooptions
    /// profiles add <NAME>` and signed in with `embryooptions auth login
    /// --browser --clinic <SLUG> --profile <NAME>`. There is no default
    /// profile.
    #[arg(long, global = true, env = "EMBRYOOPTIONS_PROFILE", value_name = "NAME")]
    pub profile: Option<String>,

    /// Whether --profile was given as a flag, not through EMBRYOOPTIONS_PROFILE.
    #[arg(skip)]
    pub profile_flag: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// The Embryo Options sign-in, one per --profile, each at its clinic.
    #[command(subcommand)]
    Auth(AuthCmd),
    /// The logins: one profile per Embryo Options user name (profile-list/v1).
    #[command(subcommand)]
    Profiles(ProfilesCmd),
    /// One read of the member API as --profile <NAME>: `GET <PATH>` on
    /// member-api.prod.embryooptions.com, under /api/Members/{memberId}/,
    /// printed as it answers. Read-only: a path naming a payment, a card, a
    /// disposition, a consent, a change or a sign-in is refused before a
    /// request is sent.
    Api(ApiArgs),
    /// Non-secret settings: the config file and the profiles in it.
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Update to the latest release from GitHub.
    SelfUpdate(SelfUpdateArgs),
    /// Print a shell completion script.
    Completions { shell: Shell },
    /// Machine-readable capability discovery (cli-info/v1).
    Info,
}

/// `embryooptions api GET <PATH>`: cli-common's `ApiArgs` offers writes, and
/// embryooptions only reads.
#[derive(Args, Debug, Clone)]
pub struct ApiArgs {
    /// GET (any case): the only method sent.
    #[arg(value_enum, ignore_case = true)]
    pub method: ApiMethod,
    /// A path on https://member-api.prod.embryooptions.com, like
    /// `/api/Members/{memberId}/MemberAccount/MemberPaymentHistoryAll`, or a
    /// URL on it. {memberId} is filled in from the signed-in token, and so is
    /// {ivfCenterId} in the query; an id in the member's place must be the
    /// signed-in member's own.
    pub path: String,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiMethod {
    #[value(name = "GET")]
    Get,
}

#[derive(Subcommand, Debug)]
pub enum AuthCmd {
    /// Sign in to Embryo Options as --profile <NAME> at --clinic <SLUG> and
    /// keep the session in embryooptions's vault: encrypted files under
    /// ~/.config/embryooptions/vault/, sealed with a key held in the OS
    /// keychain (auth-login/v1).
    ///
    /// --browser opens a Chrome window on the clinic's web app,
    /// <SLUG>.embryooptions.com, in the profile's own browser profile. There
    /// the member signs in with the user name and password from the clinic's
    /// billing notice, and embryooptions reads the access token the web app
    /// keeps. --stdin reads that token instead, as the JSON
    /// {"accessToken": ..., "refreshToken": ...} or the access token alone,
    /// copied from a signed-in browser. One read of the member's account
    /// checks it before it is stored, unless --no-verify.
    Login(LoginCmd),
    /// Whether the profile's session is stored, its clinic, when it was
    /// captured and when its token expires (auth-status/v1). Sends no
    /// request.
    Status,
    /// Remove the profile's stored session, then the browser profile its
    /// --browser sign-in keeps (auth-logout/v1).
    Logout,
    /// Remove every profile's stored session from the vault and the vault's
    /// key from the keychain (auth-reset/v1): the way out when the key is
    /// lost or damaged. Asks first; without a terminal it needs --force
    /// (exit 6).
    Reset {
        /// Do not ask.
        #[arg(long)]
        force: bool,
    },
}

// The family's login flags with `--browser` beside them (DESIGN.md §2.2).
// A plain comment: a doc comment here would replace `auth login`'s about.
#[derive(Args, Debug)]
pub struct LoginCmd {
    #[command(flatten)]
    pub login: LoginArgs,
    #[command(flatten)]
    pub browser: BrowserArgs,
    /// The clinic's name in its web app's address, <SLUG>.embryooptions.com
    /// (or that address). Kept with the session; a later sign-in of the same
    /// profile uses the stored one unless this is given.
    #[arg(long, value_name = "SLUG")]
    pub clinic: Option<String>,
    /// With --stdin or --from-env: the User-Agent of the browser the token
    /// was copied from, sent with the check and every later read.
    #[arg(long, value_name = "UA")]
    pub user_agent: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum ProfilesCmd {
    /// The configured profiles, by name (profile-list/v1).
    #[command(visible_alias = "ls")]
    List,
    /// Add a profile: 1 to 32 lowercase letters, digits, `-` or `_`,
    /// starting with a letter or a digit (profile-add/v1).
    Add { name: String },
    /// Remove a profile with its stored session and the browser profile its
    /// --browser sign-in keeps (profile-remove/v1). Asks first; without a
    /// terminal it needs --force (exit 6).
    Remove {
        name: String,
        /// Do not ask.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConfigCmd {
    /// Print the config file path.
    Path,
    /// Show the effective configuration.
    Show,
}
