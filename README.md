# embryooptions

Unofficial; not affiliated with or endorsed by Embryo Options, LLC,
CooperSurgical, Inc. or any clinic.

`embryooptions` is a read-only command line for a member's own storage bills
on Embryo Options, the portal IVF clinics use to bill for keeping embryos,
eggs, sperm or testicular tissue frozen. It signs in as you, at your
clinic's Embryo Options web app, reads what that app shows you, and prints
it. The bill commands will also write it as FHIR R4 for a personal
medical-records store (`records-stream/v1`, the `health-records/v1` profile
of [medical-common](https://github.com/own-your-health/medical-common), read by
`medrec import --from embryooptions`). It never pays, stores or changes a
payment method, chooses or changes a disposition, or signs a consent.

Status: sign-in, `auth status` and `api` work. The bill commands
(`accounts`, `bills`, `payments`, `records export`) come once the signed-in
API is mapped; until then everything about the API is read from the web
app's code, not yet seen answering (`docs/api.md`).

## Who it is for

Anyone whose clinic sends storage bills through Embryo Options: the billing
notice gives a web address on `<clinic>.embryooptions.com`, a user name and
a password. One clinic's patients sign in at that clinic's address; the
`<clinic>` part is what `--clinic` takes.

No developer registration is needed. Embryo Options publishes no API and no
developer program, so there is no app to register and no key to request:
`embryooptions` reads the same member API your clinic's web app reads once
you are signed in (`docs/api.md`).

## Install

```sh
cargo install --git https://github.com/own-your-health/embryooptions-cli --locked
# or download a release, embryooptions-<target>.tar.gz (macOS Apple Silicon or
# Intel, Linux x86_64), put `embryooptions` on your PATH; later: embryooptions self-update
```

It needs Google Chrome for the sign-in (`EMBRYOOPTIONS_CHROME` names another
Chromium-based browser), and the macOS keychain or, on Linux, a Secret
Service keyring such as GNOME Keyring for the stored session.

## Quick start

```sh
embryooptions profiles add me                                        # one profile per user name
embryooptions auth login --browser --clinic exampleclinic --profile me   # a Chrome window opens; sign in there
embryooptions auth status --profile me
embryooptions api GET '/api/Members/{memberId}/MemberAccount/MemberPaymentHistoryAll' --profile me
```

`--clinic` takes the clinic's name from its address (`exampleclinic` for
`https://exampleclinic.embryooptions.com`) or the address itself. It is kept
with the profile's session, so a later sign-in of the same profile needs
only `--overwrite`; `auth logout` forgets it with the session.

Every command that reads the site takes `--profile <NAME>`
(`EMBRYOOPTIONS_PROFILE`); there is no default. `--json` on every command
prints one schema-tagged document. In an `api` path, `{memberId}` and
`{ivfCenterId}` are filled in from your sign-in; `api` reads only your own
member paths.

## What needs a person

`auth login --browser` opens Chrome on your clinic's Embryo Options web
app, in a browser profile kept for that `embryooptions` profile, and waits
up to 10 minutes. On the app's own page you sign in with your user name
and password. The page runs Google's invisible reCAPTCHA check, which is
one reason the sign-in happens in a real browser that you use.

The first time, with the credentials from the billing notice, the app takes
you to a registration step before your account, where you set your own
user name and password. In the app's code, the same step also leads to the
choice of what happens to the stored specimens and to a notice fee payment.
Those are yours to make in the window; `embryooptions` waits until the
account opens and does nothing on that step.

`embryooptions` never sees your password. Once the app holds your access
token, `embryooptions` reads it from the page, checks it with one read of
your account, stores it encrypted (`SECURITY.md`) and closes the window.

Without Chrome, `auth login --stdin --clinic <SLUG>` reads the tokens from
a signed-in browser instead. Copy them from the browser's developer console
on the clinic's page:
`copy(JSON.stringify({accessToken: localStorage.accessToken, refreshToken: localStorage.refreshToken}))`,
then paste them into `embryooptions auth login --stdin --clinic <SLUG> --profile <NAME>`.
An access token pasted alone works until it expires, about 10 minutes.

### How long a sign-in lasts

The sign-in gives two tokens. The access token, which every read sends,
lasts about 10 minutes; `auth status` shows when the stored one expires.
The refresh token renews it. When a command starts with an access token
that has expired or expires within 60 seconds, `embryooptions` asks Embryo
Options for a new pair with the refresh token, as the web app does, and
stores it in place of the old one.

How long a refresh token lasts is not known yet. Once Embryo Options
refuses it, or with no refresh token stored, a read exits 3 and prints the
command that signs in again:
`embryooptions auth login --browser --clinic <SLUG> --profile <NAME> --overwrite`.

## Commands

| Command | Output |
|---|---|
| `profiles list\|add\|remove` | `profile-list/v1`, … |
| `auth login --browser\|--stdin`, `auth status\|logout\|reset` | `auth-login/v1`, `auth-status/v1`, … |
| `api GET <PATH>` | a member API answer under `/api/Members/{memberId}/`, as it came |
| `config path\|show`, `info`, `completions`, `self-update` | `config/v1`, `cli-info/v1`, … |

Exit codes: 0 ok, 2 usage, 3 signed out or expired (sign in again), 4 not
found, 5 Embryo Options or the network failed, 6 confirmation needed
(`--force`).

## Embryo Options's terms and automated access

The Terms of Use the web app links (effective April 1, 2015) do not forbid
robots or scripts by name. They do say three things that bear on a tool
like this:

- "we will treat anyone who uses their username and password as that user"
  (§3). `embryooptions` acts only as you, with a sign-in you make.
- A registered user "may not modify, copy, distribute or otherwise use the
  Embryo Options Content or Services available on the Website" (§4.2).
- The Code of Conduct (§7) says you will not "test or reverse engineer the
  Website in order to find limitations, vulnerabilities or to evade
  filtering capabilities", will not "obtain access to any materials or
  information through 'hacking', 'data harvesting' or through other means
  we have not intentionally made available to you", and will not
  "overburden or impair" the site.

`embryooptions` reads only your own records, which the app shows you, one
request at a time and spaced apart. It never writes anything, never gets
around the reCAPTCHA check, and leaves the sign-in to you, in Chrome. How it
was built is in `docs/api.md`: from the code the web app sends every
visitor's browser. Whether that suits you under these terms is your call;
Embryo Options could change or close the API at any time.

## Privacy

Bills stay on your machine: `embryooptions` prints them or pipes them to
your own records store, and contacts no server but your clinic's web app
(for the sign-in), the member API and GitHub (for `self-update`). The
repository holds no personal data; its test data is synthetic.

## License

MIT or Apache-2.0.
