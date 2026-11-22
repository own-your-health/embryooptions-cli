# AGENTS.md — embryooptions-cli

`embryooptions` is a read-only CLI for a member's storage bills on
Embryo Options, the portal IVF clinics use to bill for keeping embryos,
eggs, sperm or testicular tissue frozen: the account, the billing history
and the payments made. Each clinic has its own web app on
`<clinic>.embryooptions.com`; all of them call one member API,
`member-api.prod.embryooptions.com`, with the member's access token.
`embryooptions` signs in per profile through the clinic's web app, reads
that API, and will emit FHIR for the household's medical-records system
(`records-stream/v1`, `medrec import --from embryooptions`). It conforms to
piekstra-cli/1 and the `health-records/v1` profile, on the `pk-cli-*` crates
from [cli-common](https://github.com/piekstra/cli-common) and
`pk-med-records` from [medical-common](https://github.com/piekstra/medical-common).
Every `pk-cli-*` stays on the cli-common tag medical-common pins.

Status: sign-in, sessions, the token refresh and `api` are built, tested
offline and seen working against the site (2026-11-22). The bill commands
wait for the API's answers to be mapped; `docs/api.md` marks what was seen
*observed* and what is read from the web app's JavaScript alone *bundle*.

## Layout

| File | Owns |
|---|---|
| `src/cli.rs` | the clap tree |
| `src/app.rs` | dispatch, argument checks, profiles, config, info, output |
| `src/auth.rs` | `auth login` (`--clinic`, `--browser`, `--stdin`), `status`, `logout` of one `--profile`, `auth reset`: the browser-profile seams |
| `src/session.rs` | `SessionStore`: get, put and delete a profile's session (clinic, tokens, user agent) in the vault; the only code that knows where it lives |
| `src/site.rs` | `Clinic`, the sign-in's browser capture (`TOKENS_JS`) and the token check (`SESSION_CHECK`, `judge`) |
| `src/token.rs` | the access token's claims, read locally (member id, clinic id, expiry), and what `--stdin` accepts |
| `src/client.rs` | the reads: the token headers (`token_headers`), the token refresh (`refresh`), pacing, `refuse` (answers to exit codes), and `api_path` / `ApiPath::resolve`, which check an `api` path is the member's own read before it is sent |
| `src/bill.rs` | the bill model (accounts, bills and their charge lines, payments, statements) and its FHIR mapping (`docs/fhir.md`); the same file as peryourhealth-cli's |
| `src/portal.rs` | the signed-in commands (`Env`: where sessions are kept and which API is read; `Site`, which renews a token due to expire and refuses one it cannot renew before a request) |
| `docs/api.md` | the site as observed or read from its code, and the traps |

## Medical data rules

These come from the household's plan and are not negotiable.

- The repository carries no personal data, in code, fixtures, docs, commits,
  pull requests or issues: no user names, member ids, tokens, the clinic the
  household uses, names, amounts, freeze dates or dates of service. Test
  data is synthetic (`exampleclinic`, member 4242); `tests/no_personal_data.rs`
  scans every tracked file for the shapes personal data takes.
- Records and their contents are never sent to an outside service: no
  health-data aggregator, cloud OCR or other company's API. A Claude session
  is not one of these (the household's decision, 2026-11-17). A session may
  read, print and summarize the household's real bills when the work needs
  it: debugging a real export, mapping the API with real answers, checking
  a pull. Real data stays out of tracked files, commits, pull requests and
  issues. A raw capture is deleted from `/tmp` once it has served.
- No FHIR or SMART libraries, no aggregators. The FHIR here is mapped by
  hand in `src/bill.rs`; general-purpose crates are fine.
- Read-only. Nothing writes to Embryo Options: every request is a `GET` of
  the member's own path, and `api` refuses by name the paths that act,
  because the API takes some actions as `GET`s (`docs/api.md`, Traps). It
  never pays, schedules or cancels a payment, stores or changes a payment
  method, chooses or changes a disposition, signs or renews a consent,
  books or cancels a collection appointment, or reads the matching site. A
  write would need SPEC §1.3's confirmation gate and a change to this
  paragraph in the same commit; a step on the site that would do any of
  these is the owner's decision, never the CLI's.

## Profiles, the clinic and the session

Every command that reads the site takes `--profile <NAME>`
(`EMBRYOOPTIONS_PROFILE`), which must be configured (`profiles add`); a
missing or unknown one is exit 2 listing the configured ones, and a lone
profile is never picked (`pk_cli_config::profile::resolve`).

A profile is one Embryo Options login, and a login belongs to one clinic.
`auth login --clinic <SLUG>` names it (a DNS label, `site::Clinic`); it is
stored with the session, so the profile's later sign-ins reuse it, and
`auth logout` forgets it. The config holds only profile names.

A session is the vault entry `<profile>/session`
(`pk_cli_auth::vault::Vault::for_binary("embryooptions")`: one keychain item,
`vault-key` in `piekstra.embryooptions`, and an encrypted file per entry
under `~/.config/embryooptions/vault/`): the clinic, the access token, the
refresh token, the browser's user agent and the capture time. A `--browser`
sign-in runs in the profile's own kept Chrome profile
(`Launch::profile_name`). `profiles remove` removes the session and that
browser profile before the profile; `auth reset` is `Vault::clear`.

The sign-in is the clinic web app's own: user name, password and an
invisible reCAPTCHA. Never work around the reCAPTCHA or drive the sign-in
headless. The window `auth login --browser` opens is a plain Chrome with
`--remote-debugging-port=0`; a session filling the sign-in for the owner
drives it through the port in `DevToolsActivePort` in the profile directory
(the `browser-explore` harness, `--port`). The capture reads `localStorage`
`accessToken` and `refreshToken` with an expression, and confirms the token
with one read of `/api/Members/{memberId}/MemberAccount/Member`.

The member id is the token's `user.UserId`, read locally (`token::claims`).
Tokens, the member id and the user name never reach argv or output; errors
name paths with `{memberId}` in the member's place.

The web app sends no `Authorization` header and no cookies; the API
answers `Authorization: Bearer` with 401. Every read sends the access token
as `X-XSRF-TOKEN: Bearer <token>` and the clinic's id as `IvfCenterId`
(`client::token_headers`), as the app does.

An access token lasts about 10 minutes, and the API answers an expired one
with 403. The app renews on a 403 with `GET /api/Auth/RefreshToken`, the
refresh token in the same header, and gets a new pair. `Site::open` renews
instead before the first read, when the access token expires within
`portal::REFRESH_MARGIN` (60 s), under `SessionStore::renew`: cli-common's
`RefreshLock` (DESIGN.md §1.7) is held from the read of the stored session
to the write of the new pair, and nothing is sent if another process
renewed or signed in meanwhile. A sign-in stores with `put_signin` (after
`before_signin`, before the browser opens), and `delete` and `clear` go
through `RefreshLock::forget`. A refresh that fails otherwise than refused
leaves a token not yet expired to be read with, with a warning. A refresh
answered 401 or 403, or an expired token with no refresh token, is exit 3
with the remedy
`auth login --browser --clinic <SLUG> --profile <NAME> --overwrite`, the
clinic filled in from the session. How long a refresh token lasts is not
known yet.

`client::Client` sends one `GET` at a time, at least `client::PACE` apart,
follows no redirect, and maps answers to exit codes: a 401 is 3; a 404 is
4; a redirect, a 403, a 429 and a 5xx are 5.

## Tests are offline, always

`cargo test` reads no real keychain item or browser profile, stores no
session in the real vault, and contacts nothing beyond loopback. Every test
that runs a credential-reading command sets
`EMBRYOOPTIONS_KEYCHAIN_SERVICE=piekstra.embryooptions.hermetic-tests`, a
service that holds nothing, with a temp `XDG_CONFIG_HOME`. Tokens in tests
are built by `token::test_token` from synthetic claims.

- `tests/cli_surface.rs`: every `--help`, exit codes, JSON errors, argument
  checks before a session is read (the clinic included), profile selection,
  `NO_COLOR`.
- `tests/site_e2e.rs`: the reads in-process against a loopback server, the
  session in an in-memory vault: the token headers, the member's id filled
  in, another member's path, the token refresh (renewed and stored once,
  refused, failed with the token still valid, no refresh token, a sign-in
  stored meanwhile or during the renewal), and the refusals.
- `tests/no_personal_data.rs`.

`make verify` = fmt-check, clippy, test, smoke.

`EMBRYOOPTIONS_BASE_URL` points every read at a test's server instead of the
member API. It is honored only for a plain `http` origin on a loopback
address, and only over a test's own store: an
`EMBRYOOPTIONS_KEYCHAIN_SERVICE` of `piekstra.embryooptions.<suffix>` and an
`XDG_CONFIG_HOME` away from `~/.config` (`session::is_test_store`), so no
token the owner stored can be sent to it; `auth login --browser` refuses
it.
