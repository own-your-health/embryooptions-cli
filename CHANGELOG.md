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
