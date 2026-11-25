# Embryo Options

Embryo Options (Embryo Options, LLC; its pages carry CooperSurgical's
copyright) runs a member portal for IVF clinics. A clinic's patients who
keep embryos, eggs, sperm or testicular tissue in storage get their storage
bills through it, read their account and billing history there and pay.
The portal also offers the choices about what happens to the specimens
("disposition"), consents and an embryo donation matching site.

Embryo Options publishes no API and no developer program. Each clinic has
its own web app on `https://<clinic>.embryooptions.com`, and every clinic's
app calls one member API, `https://member-api.prod.embryooptions.com`,
with the signed-in member's access token. embryooptions reads that API with
`GET`s only.

Status: on 2026-11-20, without signing in, a clinic's web app was opened
and its `/env.js`, its JavaScript and its Terms of Use were read. On
2026-11-22 a member signed in, and the sign-in's answer, the headers the
app sends, the account read, the token's lifetime and the token refresh
were seen. What was seen is marked *observed*. The rest comes from the
JavaScript alone, the code every visitor's browser downloads, and has not
been seen answering; it is marked *bundle*. The readers (`accounts`,
`bills`, `payments`, `records export`) are built on that reading and tested
against synthetic answers of the same shape; [Not yet
observed](#not-yet-observed) lists what a signed-in read must confirm.

## Where it is

- `<clinic>.embryooptions.com` (*observed*): one Vue single-page app for
  every clinic. The subdomain tells it which clinic. It asks
  `GET /api/Auth/IvfCenterFromUrl?ivfCenterUrl=https://<clinic>.embryooptions.com`
  for the clinic's id and settings (*bundle*).
- `/env.js` on the clinic's host (*observed*) sets the app's
  `VITE_APP_API_URL`, `https://member-api.prod.embryooptions.com`, a
  reCAPTCHA site key and an error-reporting address.
- The app's routes (*bundle*) sit under four sections, `/auth`, `/member`,
  `/info` and `/matching`. They include `login`, `reset-password`, `home`,
  `account`, `print-billing-history` (the billing history's print page),
  `preempt-register` (the first sign-in's registration step), `consents`,
  `collection-appointments`, the disposition pages, `terms-and-conditions`
  and `privacy-policy`.
- `GET /api/Auth/OffsiteStorageFromUrl` (*bundle*) suggests that off-site
  storage has web apps of its own. A member with specimens in two places may
  have two logins, one profile each.
- www.embryooptions.com did not answer on 2026-11-20 (*observed*).

## Sign-in

1. The clinic's billing notice gives the web app's address, a user name and
   a password.
2. The sign-in page takes the user name and password. The app runs Google
   reCAPTCHA v3, invisibly, with the action `login`, and sends
   `POST /api/Auth/Login` with `username`, `password`, `userRole: "MEMBER"`,
   the clinic's `ivfCenterId` and `captchaToken`, as
   `application/json-patch+json` (*bundle*). A missing captcha token stops
   the sign-in in the page ("CaptchaValidationFailed").
3. The answer holds `accessToken` and `refreshToken`. The app keeps both in
   `localStorage` under those names (*observed*). It sends the access token
   with every API call as `X-XSRF-TOKEN: Bearer <accessToken>`, with the
   clinic's id as `IvfCenterId` (*observed*): see [The headers](#the-headers).
4. A first sign-in with the notice's credentials gives a token with an
   `intermediateUser` claim and no `user` claim. The app sends that member
   to `preempt-register`, where they set their own user name and password
   (`/api/Members/{memberId}/MemberPreempt/Register`). The same flow leads to
   the disposition form, the payment method and a notice fee
   (`MemberPreempt/UpdateDisposition`, `AddPaymentMethod`, `ChargeNoticeFee`)
   (*bundle*).
5. No second factor appears between the sign-in and the token (*bundle*).
6. Signing out removes both tokens from `localStorage` (*bundle*).

`auth login --browser --clinic <SLUG>` opens `https://<SLUG>.embryooptions.com/`
in Chrome, in the profile's own kept browser profile, and the member signs
in there. The capture evaluates this in the clinic's pages:

```js
(() => { const a = localStorage.getItem('accessToken'); if (!a) return null;
  return JSON.stringify({ accessToken: a, refreshToken: localStorage.getItem('refreshToken') || null }); })()
```

Once it answers, the confirm hook decodes the access token's claims. A
token with `intermediateUser` (step 4) or one already expired keeps the
window waiting. Otherwise it reads
`GET /api/Members/{memberId}/MemberAccount/Member?ivfCenterId={ivfCenterId}`
with the token. A 2xx JSON object stores the session and closes the
window. A 401 or a 403 keeps the window waiting, and anything else is
retried with a backoff (`site::judge`). embryooptions never calls
`/api/Auth/Login`, never sees the password, and never touches the
reCAPTCHA check.

`auth login --stdin` takes the same JSON, or the access token alone, as
copied from a signed-in browser's console (README).

## The clinic and the profile

A login belongs to one clinic. `--clinic` takes the clinic's subdomain
(`exampleclinic`) or the app's address
(`https://exampleclinic.embryooptions.com/...`). It must be a DNS label: 1
to 63 lowercase letters, digits and `-`, with no `-` at either end. `www`,
`api` and `member-api` are refused. The clinic is stored in the profile's
vault entry with the tokens, not in the config. A later sign-in of the
profile uses it unless `--clinic` is given, and `auth logout` forgets it.
`auth status` shows it.

## The token

The access token is a JWT. Its claims (*observed* for `user` and `exp`):

| Claim | What it holds |
|---|---|
| `user` | a JSON string: `UserId`, the `{memberId}` of every member path; `IVFCenterId`, the clinic's id and the `ivfCenterId` query; `MemberType` |
| `intermediateUser` | instead of `user`, during the first sign-in's registration step |
| `exp` | when it expires, Unix seconds |
| `aud`, `jti` | the audience and the token's id |

embryooptions reads the claims locally, from the base64url payload
(`src/token.rs`), and leaves the signature to the API. It never prints the
token or the member's id. An error names a path with `{memberId}` in the
member's place.

An access token lasts about 10 minutes (*observed*: `exp` is about 10
minutes after the sign-in). The API answers an expired one with 403
(*observed*), not 401.

## The headers

The web app sends no `Authorization` header and no cookies to the member
API (*observed*). Each request carries:

| Header | Value |
|---|---|
| `X-XSRF-TOKEN` | `Bearer <accessToken>` |
| `IvfCenterId` | the clinic's id, the token's `IVFCenterId` |
| `X-SPA-Version` | the app's version |
| `Accept-Language` | the browser's |

The API answers an `Authorization: Bearer` header with 401 (*observed*).
embryooptions sends `X-XSRF-TOKEN` and `IvfCenterId` as the app does, with
the browser's user agent captured at sign-in, and no cookie.

## Token refresh

On a 403 the app renews its tokens and retries the call once (*observed*):

1. `GET /api/Auth/RefreshToken` with the refresh token in the same header,
   `X-XSRF-TOKEN: Bearer <refreshToken>`, and `IvfCenterId`.
2. The answer is a new pair, `{"accessToken", "refreshToken"}`.

embryooptions renews before a command's first read (`portal::Site::open`,
`client::refresh`), and after a 403 as the app does (`portal::Site::api`):

- An access token that has expired, or expires within 60 s by this
  machine's clock, is renewed once, and the new pair is stored in the
  profile's vault entry. An answer without a refresh token keeps the old
  one.
- The clock can be wrong, and the API can stop taking a token before its
  `exp`. So a read the API answers 403 renews the session once per command
  and is sent again with the new token. A second 403 stands (exit 5, naming
  the sign-in).
- Each answer carries a new refresh token, so two processes must never
  present the same one. The renewal holds cli-common's `RefreshLock`
  (`~/.config/embryooptions/token-refresh.lock`) from the read of the
  stored session to the write of the new pair, and sends nothing if
  another process renewed or signed in meanwhile. A sign-in's write and a
  logout's delete take the lock too, so a renewal in flight cannot undo
  them.
- A refresh answered 401 or 403 is exit 3, naming
  `embryooptions auth login --browser --clinic <SLUG> --profile <NAME> --overwrite`.
  So is an expired token with no refresh token stored.
- Any other failure, or an answer without an access token, is exit 5 for
  a token that has expired. A token that has not yet expired is read with,
  with a warning.
- A new pair that cannot be stored is still read with, with a warning.

How long a refresh token lasts is not known yet.

## The member API

Every path below is a `GET` with the [headers](#the-headers) above.
`MemberAccount/Member` has been seen answering a JSON object (*observed*);
the others are marked *bundle*.

| Path | What the app shows with it |
|---|---|
| `/api/Members/{memberId}/MemberAccount/Member?ivfCenterId={ivfCenterId}` | the account page: Service Plan, Next Scheduled Payment, Payment Method, Unpaid Balance |
| `/api/Members/{memberId}/MemberAccount/MemberPaymentHistoryAll` | the billing history and its print page: Date, Type, Method, Amount, Note, Ref, Freeze Date, Lab, Provider, Tax ID, CPT Codes |
| `/api/Members/{memberId}/Member/UpComingBills` | the next bill and its freeze date, on the home page of a member with embryos in storage |
| `/api/Members/{memberId}/FreezeEvents/FreezeEvents` | the specimens in storage, by freeze event, with each one's service plan and next payment |
| `/api/Members/{memberId}/MemberIvfCenter` | the member's clinic: its name, billing address and the currency it bills in |
| `/api/Members/{memberId}/MemberIvfCenter/CollectionSites` | the clinic's collection sites |

### The answers

- `MemberAccount/Member` (*observed* keys) holds the account's `id`,
  `billingBalance` (the app's Unpaid Balance; it offers a payment when it is
  above 0), `adminFeeBalance`, `ivfCenterId` and `ivfCenterName`, with the
  member's own profile (names, address, user name, payment method) beside
  them.
- `MemberPaymentHistoryAll` takes `pageNumber` (from 1), `pageSize` (the
  app offers 10 to 50), `sortBy` and `sortDescending`, and answers
  `{"data": [...], "total": N}`. The print page sends none of them and
  reads `data`. A row's fields: `id`, `timestamp`, `memberPaymentTypeId`,
  `amount`, `outstandingBalance`, `billingCycleStartDate`,
  `billingCycleEndDate`, `recurringFeeName`, `oneTimeFeeName`,
  `discountTypeName`, `allocationDiscountAmount`, `billingNote`,
  `memberFreezeEvent.freezeDate`, `ivfCenterProvider.fullName`,
  `ivfCenterLab.name` and `.taxId`, `cptcode1` to `cptcode3`,
  `memberPaymentMethodId` (`CR` a card or PayPal, any other a check),
  `memberPaymentMethod.type` (`Credit Card`, `PayPal`) and
  `.creditCardDetails` (`cardType`, `maskedNumber`), `memberBankAccount`,
  `creditCardSystemStatus` (`ACC` accepted, `DEC` declined, `EXP`, `INV`,
  `ERR`), `transactionAuthReference`, `transactionStatus` and the payment
  processor's response codes.
- `memberPaymentTypeId` is a code or a word. The codes read here are `PAY`
  (a payment), `CHG` (a charge) and `DIS` (a discount); any other code is
  not known. `paymentSubType` narrows it (such as `MEMBER_INT_PMT` or
  `RECURRING_FEE`). The words the app translates, read here too, are from
  its own list: the payments (`MemberInitiatedPayment`,
  `AutomatedPayment`, `ScheduledInstallmentPayment`, `CollectedPayment`,
  `CenterInitiatedPayment`, `CenterInitiatedAdvancePayment`,
  `DirectPaymentToCenterByCheck`, `DirectAdvancePaymentToCenterByCheck`),
  the refunds (`RefundViaCheck`, `Reimbursement`), the discounts
  (`Discount`, `FlatDiscount`, `CenterInitiatedDiscount`,
  `InsuranceDiscount`), `Credit`, `MemberDisputedChargeResolutionProcessInitiated`,
  and `PastDue`, `BalanceForward` and `Through` (*bundle*).
- A coded row's `amount` can be a string of a decimal, negative for a
  payment and positive for a charge. `outstandingBalance` and
  `transactionStatus` are optional.
- `timestamp`, `billingCycleStartDate` and `billingCycleEndDate` are .NET
  date-times or written out, such as `05 March, 2024 08:15:42` (an invented
  example). A code field can hold the text `N/A`, which is read as no code.
- `UpComingBills` answers one object, `{nextPaymentAmount, freezeDate,
  nextPaymentDate}`. The app asks for it only when the token's
  `MemberType` is `CRY` (embryos in storage), ignores a failure, and shows
  nothing when `freezeDate` is empty.
- `MemberIvfCenter` holds the clinic's `name`, `billingAddressHtml`,
  `currencySymbol`, `currencyCultureInfo` and
  `merchantAccountDetails.currencyCode`, and the file names of the letters
  and invoices the clinic prints (`printInvoiceFileName`,
  `standardPatientInvoiceRegisteredFileName`, ...): templates, not a
  member's documents.
- Amounts are in the clinic's currency, as decimals: the app formats
  `amount` and `billingBalance` as they come, with no division by 100.

### No bill or receipt files

No `GET` in the app's code serves a member's statement, invoice or
receipt. The only files a member can open are consent forms, through
DocuSign (`Consents/{consentId}/GetRenewConsentForm`, `DocuSign/Details`),
which `api` refuses. The billing history's print page is the app's own
rendering of `MemberPaymentHistoryAll`. The member account's
`documentsFolder` is not read by any page. So `documents list` is empty.

## What each command reads (*bundle*)

One request at a time, 750 ms apart. Every reader reads the first three
answers, in this order, before it prints or writes anything:

| Command | Reads | Fields used |
|---|---|---|
| every reader | `MemberAccount/Member?ivfCenterId={ivfCenterId}`, `MemberIvfCenter`, then `MemberAccount/MemberPaymentHistoryAll?pageNumber=<n>&pageSize=50` from page 1 | the account's `id`, `billingBalance`, `adminFeeBalance`, `ivfCenterName`; the clinic's `name` and `merchantAccountDetails.currencyCode`; every history row as above. A `MemberIvfCenter` that fails (but for a sign-in that has ended, exit 3) is a warning and no clinic answer: the account names the clinic, and `records export`, which needs the currency, is exit 5 |
| `patients list` | as above | the clinic's subdomain and, hashed with the profile, the token's member id: the patient id. The member id never shows |
| `accounts list` | as above, then `Member/UpComingBills` for a `CRY` member | the account; how many charges and payments, the open charges and what they owe; the upcoming bill |
| `bills list [--open] [--since] [--until] [--limit]` | as above | the charges: `id`, the billing period (else the day of `timestamp`), `timestamp`, `memberPaymentTypeId`, the fee's name (else the note, else the type), the freeze date, `amount`, `outstandingBalance` |
| `bills show <ID>` | as above | one charge, with the provider, the lab and its tax id, the CPT codes, the note, the discount type and `allocationDiscountAmount` |
| `payments list [--since] [--until] [--limit]` | as above | the payments, refunds, discounts and credits: `id`, the day of `timestamp`, the kind, the type, its `paymentSubType`, `amount`, the method, the note, and where it stands (`settled`, and `state`: the settlement states below) |
| `records export --ndjson` | as above | all of the above (`docs/fhir.md`) |
| `documents list\|download` | nothing | none: Embryo Options serves no bill or receipt files |

How the answers are read (`src/reads.rs`):

- The history is read page by page, 50 rows a page, as the app's billing
  tab pages it. With a `total`, pages are read until they hold that many
  rows; without one, until a page shorter than 50. A history whose pages
  hold fewer rows than its `total` is exit 5, so no reader passes part of
  it off as the whole. A row a later page repeats is read once, and a page
  of nothing but rows already read is exit 5: the server is not paging. A
  bare list is read as every row at once.
- A row is a payment, a refund, a discount, a credit or another row by its
  `memberPaymentTypeId`, in any case, from the app's list above. A row of
  any other type, or of none, is a charge, a fee billed, when it names its
  fee (`recurringFeeName` or `oneTimeFeeName`). One that names no fee is of
  a type not known: neither a bill nor a payment, a warning naming its type
  on every reader, and an `Invoice` skip in `records export`.
- An amount is a JSON number of at most two decimals, or a string of one
  (a sign, digits, a point, one or two digits). A grouped or currency
  string, an exponent or a third decimal is exit 5 naming the field
  (`MemberPaymentHistoryAll.data.amount`), never with its value. Payments,
  refunds, discounts and credits are read as positive amounts, whatever
  their sign.
- A date is its first ten characters, when they are a day, or written out
  (`05 March, 2024 08:15:42`). .NET's empty date, `0001-01-01`, is no
  date, and `01 January, 0001 00:00:00` is the same.
- A charge's date of service is its billing period's start, else the day it
  was posted (`timestamp`); its period's end is the period's.
- `outstandingBalance` is read as what is still owed on a charge: above 0
  is an open bill (`bills list --open`). When the open charges'
  outstanding balances do not add up to the account's `billingBalance`,
  `accounts list` and `bills list` warn and `records export` is exit 5.
- A coded `PAY` row with a positive amount, or whose subtype says refund,
  reversal, chargeback or void, is a refund; a coded `CHG` row with a
  negative amount is a credit. Such a row contradicts its own code, so only
  an explicit settled status settles it. A row of any other code that names
  no fee is of a type not known (above).
- A charge is voided when its `transactionStatus` says void or cancel.
- `outstandingBalance` is optional. Without one on any charge, no row says
  whether its bill is open. When the account's `billingBalance` is 0 every charge is
  owed 0; when it is above 0 and no charge says what is owed on it,
  `accounts list` and `bills list` warn, the charges' `owes` is null and
  `records export` is exit 5, writing nothing.
- A payment is settled only on positive evidence (`Row::settlement`). The
  states are decided in this order:
  1. `failed`: its card was turned down (`DEC`, `EXP`, `INV`, `ERR`), or its
     status says so (such as declined, denied, failed, rejected, voided,
     expired, cancelled, returned, bounced, charged back, unauthorized,
     error). This outranks a date after today.
  2. `scheduled`: dated after the local day, whatever its status says. The
     app's "next scheduled payment" is never a payment made.
  3. `pending`: a status such as pending, scheduled, submitted, authorized,
     settling, processing, awaiting, hold, initiated, queued, unsettled or
     partial.
  4. `settled`: a status of settled, complete, completed, paid, captured or
     succeeded; or, with no status, an accepted card (`ACC`), or a type the
     clinic records itself (a check, a clinic-initiated or collected
     payment, a refund, a discount or a credit) with no card result.
  5. `unconfirmed`: anything else.
  `payments list` shows each with `settled` (true only for the `settled`
  state) and `state`, which is the one to read. `records export` writes
  only the settled payments and counts the rest in one `not_settled` skip,
  by state.
- The method is worded as the app words it: the card's type (`Visa`),
  `Credit Card`, `PayPal` or `Check`. The card's and the bank account's
  digits are never read.
- A row without an `id` cannot be placed: `records export` counts it in a
  skip.

### Not yet observed

Each of these is the reading of the JavaScript alone, to check against the
first signed-in answers:

- every code `memberPaymentTypeId` takes, and every value of
  `paymentSubType`;
- what a positive `PAY` row or a negative `CHG` row means;
- what `ACC` covers beyond a card the processor accepted;
- whether `outstandingBalance`, where given, is what is owed on each charge;
- whether `adminFeeBalance` is part of `billingBalance`;
- whether `allocationDiscountAmount` is taken off the charge's `amount` or
  shown beside it;
- whether `BalanceForward` and `PastDue` are rows of the history (a balance
  carried in from before is owed money, read here as no bill) or only
  labels;
- whether a declined card payment stays in the history, and the values of
  `transactionStatus`;
- whether `MemberPaymentHistoryAll` without `pageSize` answers every row,
  and whether it caps `pageSize` below 50;
- what `UpComingBills` answers for each member type.

## What `api` refuses

`embryooptions api GET <PATH>` checks the path before the session is read,
then fills in `{memberId}` and `{ivfCenterId}` from the token before
anything is sent. It refuses:

- any path not under `/api/Members/<member>/`, so the sign-in, the refresh
  and the clinic lookups are out;
- an id in the member's place that is not the token's own;
- `..`, `%`, `#`, an empty segment, or a placeholder other than
  `{memberId}` and `{ivfCenterId}`;
- any segment or query key that contains `payment`, `paypal`, `autopay`,
  `card`, `method`, `charge`, `vault`, `nonce`, `token`, `disposition`,
  `consent`, `docusign`, `update`, `delete`, `remove`, `create`, `save`,
  `submit`, `send`, `login`, `logout`, `register`, `enroll`, `cancel`,
  `change`, `accept`, `confirm`, `rebuild`, `generate`, `preempt`,
  `credential`, `password`, `needshelp`, `capture` or `matching`, in any
  case. `MemberPaymentHistoryAll` is the one exception: it is the billing
  history.

## Terms

The web app links Embryo Options's Website Terms of Use, effective April 1,
2015 (*observed*). They name no robots, scripts or automated access. In
their own words:

- §3: "we will treat anyone who uses their username and password as that
  user. We will provide this individual with all of the rights and
  privileges that we provide to that Registered User".
- §4.2: a registered user "may access and view certain other
  password-protected areas, such as the Disposition Services, for use solely
  in conjunction with the Services and as provided in these Terms, and you
  may not modify, copy, distribute or otherwise use the Embryo Options
  Content or Services available on the Website." The same section says a
  user "may not access, view or modify the User Profiles of other Registered
  Users".
- §7, the Code of Conduct, says you will not "Use the Website in a manner
  that could disable, overburden or impair the Website or Services", "Use
  the Website to test or reverse engineer the Website in order to find
  limitations, vulnerabilities or to evade filtering capabilities", or "Seek
  to obtain access to any materials or information through "hacking", "data
  harvesting" or through other means we have not intentionally made
  available to you through the Website."
- §8: Embryo Options may monitor use and suspend a user it concludes has
  broken the Code of Conduct.

No `robots.txt` was found (*observed*).

embryooptions reads only the signed-in member's own paths, which the app
itself reads for that member, one request at a time at least 750 ms apart.
It never writes, never reads the matching site (which shows other members),
never touches the reCAPTCHA check and never signs in by itself.

## Traps

- A `GET` is no proof of a read. The API takes some actions as `GET`s:
  `Payment/MemberPayment` (the account page's "Charge my account now",
  which charges the stored card the unpaid balance),
  `MemberPreempt/ChargeExistingPaymentMethod` and
  `MemberPreempt/ChargeNoticeOfFee` (charging the member),
  `MemberPreempt/ZeroChargeContinue`, and the matching site's
  `AcceptMatchRequest` and `ConfirmMatchCancellation` (*bundle*). `api`
  refuses them by name, not by method.
- The member id in every path is the `user` claim's `UserId`; the claim has
  no field named `MemberId` (*bundle*).
- The `user` claim is a JSON string inside the JWT, not an object
  (*bundle*: the app runs `JSON.parse` on it).
- A first sign-in's token is an `intermediateUser` token: it opens the
  registration step, not the account.
- `MemberPaymentHistoryAll` names a payment and is the billing history,
  the one read that holds a refused word.
- www.embryooptions.com, the marketing site, did not answer; every
  clinic's app is on its own subdomain.
