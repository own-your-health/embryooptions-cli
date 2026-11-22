# Security

Report a vulnerability privately through GitHub's "Report a vulnerability"
(Security tab), not in an issue.

## What embryooptions touches

- Contacts no server except GitHub, for `self-update`, and Embryo Options:
  - for `auth login --browser`, a Chrome window on the clinic's web app,
    `https://<clinic>.embryooptions.com/`, where the member signs in;
  - the member API, `https://member-api.prod.embryooptions.com`: one read
    that checks a token before it is stored, and, for a command run with
    `--profile`, the member's own reads.
- Every API request is a `GET` of a path under
  `/api/Members/{memberId}/`, the signed-in member's own, one at a time and
  spaced apart, with a 30-second limit and no redirect followed. Every URL
  is built from that fixed origin. `EMBRYOOPTIONS_BASE_URL` replaces it for
  tests only: it is refused unless it is a plain `http` origin on
  127.0.0.1, localhost or `[::1]`, `EMBRYOOPTIONS_KEYCHAIN_SERVICE` names a
  test service (`piekstra.embryooptions.<suffix>`) and `XDG_CONFIG_HOME` is
  set away from `~/.config`, so a token the owner stored is never sent
  anywhere but Embryo Options.
- `embryooptions api` is held to the same, and refuses before anything is
  sent a path outside the member's own, another member's id, and any path
  that names a payment, a card or payment method, a charge, a disposition,
  a consent, a change, a sign-in or sign-out, or the matching site. The API
  takes some actions as `GET`s, so the method alone proves nothing
  (`docs/api.md`).
- Stores one credential per profile, its Embryo Options session: the
  clinic, the access token, the refresh token, the browser's user agent and
  the capture time, as the vault entry `<profile>/session`. The vault
  (`pk_cli_auth::vault`) is encrypted owner-only files under
  `~/.config/embryooptions/vault/`, with one key in the keychain (service
  `piekstra.embryooptions`, account `vault-key`). A token never goes on a
  command line or into a log, and errors never print it or the member's id.
- Sends the access token only to the member API, as the web app does: in
  the `X-XSRF-TOKEN` header as `Bearer <token>`, with the clinic's id as
  `IvfCenterId`. It sends no `Authorization` header and no cookies, as the
  app sends none.
- Reads the token's claims (member id, clinic id, expiry) locally from its
  payload.
- Sends the refresh token only to the member API's
  `GET /api/Auth/RefreshToken`, in the same header, when the access token
  (about 10 minutes long) has expired or expires within 60 s. The new pair
  replaces the old one in the vault, unless another sign-in was stored
  meanwhile. How long a refresh token lasts is not known yet.
- Keeps one Chrome profile per embryooptions profile for `auth login --browser`:
  `~/Library/Application Support/piekstra.embryooptions/browser-profile-<profile>`
  (under `$XDG_DATA_HOME` on Linux), owner-only (`0700`; a launch is refused
  otherwise). `--fresh-profile` signs in without it.
- `auth logout --profile <NAME>` and `profiles remove <NAME>` remove that
  profile's session, then its Chrome profile. `auth reset` removes every
  session and the vault key.
- The config file holds the profile names.

## What embryooptions never does

- Writes to Embryo Options: no payment, payment method, autopay, charge,
  disposition, consent, appointment, profile or credential change, and no
  sign-out request.
- Signs in by itself, or works around the reCAPTCHA check: the member's
  user name and password are typed (or filled) on the clinic's own page in
  Chrome, never passed to embryooptions.
- Sends bills anywhere: the output goes to stdout or a file you name.
