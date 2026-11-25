# Changelog

## 0.1.1

- `docs/api.md` states the forms of each field on its own, and the tests cover
  each form. No change in behavior.

## 0.1.0

The first release. Licensed under AGPL-3.0-only.

- `embryooptions`: a member's storage bills from Embryo Options, read-only.
  It pays nothing and changes nothing.
- `profiles list|add|remove`; every command that reads the site takes
  `--profile`, with no default.
- `auth login --browser --clinic <SLUG>` signs in through a Chrome window
  on the clinic's web app, in the profile's own browser profile, and reads
  the app's access and refresh tokens; `--stdin` takes them copied from a
  signed-in browser. The clinic is kept with the session. `auth status`
  shows the clinic, the capture time and the token's expiry;
  `auth logout|reset`.
- An access token lasts about 10 minutes. One that has expired, or expires
  within 60 s, is renewed with the refresh token before a command's first
  read, and the new pair is stored. The renewal holds a lock
  (`~/.config/embryooptions/token-refresh.lock`), so two processes never
  present the same refresh token. A refused refresh, or an expired token
  with no refresh token, is exit 3. A read answered 403 renews once per
  command and is sent again.
- `patients list`, `accounts list` (the account, what is owed, the open
  bills, and the next bill for a member with embryos in storage),
  `bills list [--open]`, `bills show <ID>` and `payments list`
  (`payment-list/v2`: payments, refunds, discounts and credits). Every
  reader reads the account, the clinic and every page of the billing
  history first; a history whose pages hold fewer rows than its `total`, or
  an amount that is not a decimal of at most two places, is exit 5 before
  anything is printed or streamed.
- A payment is settled only on positive evidence. One dated after today,
  pending at the processor, turned down or of a status or type not known is
  `scheduled`, `pending`, `failed` or `unconfirmed` in `payments list`'s
  `state`; `settled` is true only for the `settled` state.
- The billing history's fields are each read in every form `docs/api.md`
  lists: an amount as a number or a string, a date as a .NET date-time or
  written out, a row's type as a word or a code. When no charge says what is
  owed on it, the account's balance decides.
- `records export --ndjson` (records-stream/v1): the account, a bill per
  charge and the settled payments as FHIR (`docs/fhir.md`). Payments that
  have not settled are counted by state in a `not_settled` skip, and rows
  that are no bill in skips of their own. A clinic billing in a currency
  other than USD, or whose currency was not read, is exit 5, and so are
  open bills that do not add up to the account's balance. The member API
  takes no date, so `--since` drops nothing.
- `documents list|download`: Embryo Options serves no statement or receipt
  files, so the list is empty and an id is exit 4.
- `api GET <PATH>` reads the member's own paths under
  `/api/Members/{memberId}/`, filling in `{memberId}` and `{ivfCenterId}`
  from the token; paths naming a payment, a card, a disposition, a consent,
  a change, a sign-in or the matching site are refused before a request.
- `config path|show`, `info`, `completions`, `self-update`.
- While `EMBRYOOPTIONS_KEYCHAIN_SERVICE` names a test's keychain service, a
  command that would send a request without a loopback
  `EMBRYOOPTIONS_BASE_URL` is exit 2 before sending it.
