//! The command tree (piekstra-cli/1, health-records/v1, documents/v1).

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use pk_cli_auth::LoginArgs;
use pk_cli_browser::BrowserArgs;
use pk_cli_core::{CommonArgs, RangeArgs};
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
    /// The member the sign-in is (patient-list/v1).
    #[command(subcommand)]
    Patients(PatientsCmd),
    /// The member's account at the clinic, with what is owed.
    #[command(subcommand)]
    Accounts(AccountsCmd),
    /// Bills: the storage and other fees on the member's billing history,
    /// with what is still owed on each.
    #[command(subcommand)]
    Bills(BillsCmd),
    /// Payments, refunds, discounts and credits on the billing history.
    #[command(subcommand)]
    Payments(PaymentsCmd),
    /// Bills and payments as FHIR in records-stream/v1, for `medrec import
    /// --from embryooptions`.
    #[command(subcommand)]
    Records(RecordsCmd),
    /// Bill and receipt files (documents/v1): Embryo Options serves none,
    /// so the list is empty.
    #[command(subcommand)]
    Documents(DocumentsCmd),
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

#[derive(Subcommand, Debug)]
pub enum PatientsCmd {
    /// The member the sign-in is, with its id for `records export
    /// --patient` (patient-list/v1).
    #[command(visible_alias = "ls")]
    List,
}

#[derive(Subcommand, Debug)]
pub enum AccountsCmd {
    /// The member's account: its id, the clinic, what is owed, how many
    /// bills and the next bill (account-list/v1).
    #[command(visible_alias = "ls")]
    List,
}

#[derive(Subcommand, Debug)]
pub enum BillsCmd {
    /// One row per bill, newest service first, open and paid (bill-list/v1).
    /// --since and --until filter on the date of service (a storage fee's
    /// billing period's start, `date`), not on the day it was posted.
    #[command(visible_alias = "ls")]
    List(BillsListArgs),
    /// One bill with all the billing history says of it (bill/v1).
    Show(BillShowArgs),
}

#[derive(Args, Debug)]
pub struct BillsListArgs {
    /// Only bills the member still owes on.
    #[arg(long)]
    pub open: bool,
    #[command(flatten)]
    pub range: RangeArgs,
}

#[derive(Args, Debug)]
pub struct BillShowArgs {
    /// The bill's id from `embryooptions bills list`.
    pub id: String,
}

#[derive(Subcommand, Debug)]
pub enum PaymentsCmd {
    /// Payments, refunds, discounts and credits, newest first, with where
    /// each stands: settled, scheduled, pending, failed or unconfirmed
    /// (payment-list/v1). --since and --until filter on the day
    /// each was posted.
    #[command(visible_alias = "ls")]
    List(PaymentsListArgs),
}

#[derive(Args, Debug)]
pub struct PaymentsListArgs {
    #[command(flatten)]
    pub range: RangeArgs,
}

#[derive(Subcommand, Debug)]
pub enum RecordsCmd {
    /// The sign-in's bills and payments as records-stream/v1.
    Export(ExportArgs),
}

#[derive(Args, Debug)]
pub struct ExportArgs {
    /// The patient: the id from `embryooptions patients list`. A sign-in is
    /// one member, so it may be omitted.
    #[arg(long, value_name = "ID")]
    pub patient: Option<String>,
    /// The clinic: the organization id from `embryooptions patients list`.
    /// A sign-in reads one clinic, so it may be omitted.
    #[arg(long, value_name = "ORG")]
    pub org: Option<String>,
    /// The day of the last pull (YYYY-MM-DD), for the stream's header. The
    /// member API takes no date, so every bill and payment is still written,
    /// and the header names every type unbounded.
    #[arg(long, value_name = "YYYY-MM-DD")]
    pub since: Option<String>,
    /// Write the stream to stdout, which `medrec import --from
    /// embryooptions` reads. The stream is the output, so --json with it is
    /// a usage error.
    #[arg(long, conflicts_with = "output")]
    pub ndjson: bool,
    /// Write the stream to DIR/records.ndjson instead (tests and synthetic
    /// data only: real records go straight into medrec).
    #[arg(short, long, value_name = "DIR")]
    pub output: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum DocumentsCmd {
    /// Bill and receipt files (document-list/v1): none, since Embryo
    /// Options serves none. Sends no request.
    #[command(visible_alias = "ls")]
    List,
    /// Save one document, or every one with --all (document-download/v1,
    /// document-download-batch/v1): an id is exit 4, and --all saves none.
    /// Sends no request.
    Download(DownloadArgs),
}

#[derive(Args, Debug)]
pub struct DownloadArgs {
    /// The document's id from `embryooptions documents list`.
    #[arg(required_unless_present = "all", conflicts_with = "all")]
    pub id: Option<String>,
    /// Save every listed document into the directory -o names.
    #[arg(long, requires = "output")]
    pub all: bool,
    /// Where to write: a file or a directory for one document, a directory
    /// for --all.
    #[arg(short, long, value_name = "PATH")]
    pub output: Option<PathBuf>,
    /// Replace a file that is already there.
    #[arg(long)]
    pub overwrite: bool,
}
