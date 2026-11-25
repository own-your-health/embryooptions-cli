# Changelog

## Unreleased

- A scheduled payment never reads as paid. A payment is settled only on
  positive evidence; one dated after today, pending at the processor, turned
  down or of a status or type not known is `scheduled`, `pending`, `failed`
  or `unconfirmed` in `payments list`'s new `state` field, and is counted
  in `records export`'s `not_settled` skip instead of written. A payment
  submitted for settlement, or with no status at all, used to be written
  as paid.
- While `EMBRYOOPTIONS_KEYCHAIN_SERVICE` names a test's keychain service, a command
  that would send a request without a loopback `EMBRYOOPTIONS_BASE_URL` is exit 2
  before sending it, so a test run never reaches the member API.
- The readers, built on the web app's code (`docs/api.md`, *bundle*) and
  tested against synthetic answers: `patients list`, `accounts list` (the
  account, what is owed, the open bills, and the next bill for a member
  with embryos in storage), `bills list [--open]`, `bills show <ID>`,
  `payments list` (payments, refunds, discounts and credits, with whether
  each settled) and `records export --ndjson` (records-stream/v1: the
  account, a bill per charge in the billing history and the settled
  payments, per DESIGN §1.10). Every reader reads the account, the clinic
  and every page of the billing history first; a history whose pages hold
  fewer rows than its `total`, or an amount that is not a number of two
  decimals, is exit 5 before anything is printed or streamed.
- `records export` writes no skip of an `Invoice` but for a charge it could
  not read, so a records store can tell a bill the site no longer lists;
  refunds, discounts, credits and rows that move no money are counted in
  skips of their own. A charge is a row that names its fee; a row of a type
  the app does not list that names none is no bill, an `Invoice` skip and
  a warning naming its type. A clinic billing in a currency other than USD,
  or whose currency was not read, is exit 5, and so are open bills that do
  not add up to the account's balance (`accounts list` and `bills list`
  warn). The member API takes no date, so `--since` drops nothing and the
  header names every type unbounded.
- `documents list|download`: Embryo Options serves no statement or receipt
  files, so the list is empty and an id is exit 4.
- medical-common v0.6.11 and cli-common v0.33.5: the bill's code systems,
  price components, document types, `Money` and totals come from
  `pk_med_records::bill` (DESIGN §1.10) instead of the CLI's own copy. Two
  displays change to the profile's: `insurance-adjustment` is "Insurance
  adjustment" (was "Adjustment") and `patient-responsibility` is "Patient
  responsibility" (was "Patient owes"). Codes, types, amounts and ids
  are unchanged.
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
  install --git` and the medical-common dependency follow it.
- `url` 2.5.9, for RUSTSEC-2026-0592 (a stack overflow in `Url::origin()`).
- The bill model (`src/bill.rs`, docs/fhir.md) follows medrec's bill
  mapping, as flywire's does: every `Money` value is a JSON number in the
  bill's decimal spelling (`125.00`, kept by serde_json's
  `arbitrary_precision`); a bill without lines holds its date of service
  in a ChargeItem of its own; `totalPriceComponent` is the bill's
  breakdown, its billed amount the `base` `billed` component, and
  `totalGross` equals `totalNet`, what the patient owes; a payment's
  detail names the provider paid (`payee`), with the bill's amount when
  it paid one bill alone, and `paymentIssuer` is dropped; statements and
  receipts are typed in `urn:pk-med:doc-type`, and a statement's
  `context.related` names its Account.
