# Changelog

## Unreleased

- `embryooptions`: a member's storage bills from Embryo Options, read-only.
- `profiles list|add|remove`; every command that reads the site takes
  `--profile`.
- `auth login --browser --clinic <SLUG>` signs in through a Chrome window
  on the clinic's web app, in the profile's own browser profile, and reads
  the app's access and refresh tokens; `--stdin` takes them copied from a
  signed-in browser. The clinic is kept with the session.
- `auth status` shows the clinic, the capture time and the token's expiry;
  `auth logout|reset`.
- `api GET <PATH>` reads the member's own paths under
  `/api/Members/{memberId}/`, filling in `{memberId}` and `{ivfCenterId}`
  from the token; paths naming a payment, a card, a disposition, a consent,
  a change, a sign-in or the matching site are refused before a request.
- `config path|show`, `info`, `completions`, `self-update`.
- Reads send the access token as `X-XSRF-TOKEN: Bearer <token>`, with the
  clinic's id as `IvfCenterId`, as the web app does; the API answers an
  `Authorization` header with 401.
- An access token lasts about 10 minutes. One that has expired, or expires
  within 60 s, is renewed with the refresh token
  (`GET /api/Auth/RefreshToken`) before a command's first read, and the new
  pair is stored. The renewal holds a lock
  (`~/.config/embryooptions/token-refresh.lock`) from the read of the
  session to the write of the new pair, so two processes never present the
  same refresh token; a sign-in and a logout take it for their write. A
  refused refresh, or an expired token with no refresh token, is exit 3. A
  refresh that fails otherwise leaves a token not yet expired to be read
  with. A read answered 403 renews once per command and is sent again, for
  a clock that is off or a token the API stopped taking early.
- The sign-in remedies name the clinic: `auth login --browser --clinic
  <SLUG> --profile <NAME> --overwrite`.
- The repository moved to own-your-health
  (github.com/own-your-health/embryooptions-cli); `self-update`, `cargo
  install --git` and the medical-common dependency (still v0.6.9) follow it.
- `url` 2.5.9, for RUSTSEC-2026-0592 (a stack overflow in `Url::origin()`).
