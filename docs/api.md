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
been seen answering; it is marked *bundle*.

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

embryooptions renews before a command's first read instead of after a 403
(`portal::Site::open`, `client::refresh`):

- An access token that has expired, or expires within 60 s, is renewed
  once, and the new pair is stored in the profile's vault entry. An answer
  without a refresh token keeps the old one.
- The new pair is stored only if the vault still holds the token that was
  renewed: a sign-in made meanwhile is kept.
- A refresh answered 401 or 403 is exit 3, naming
  `embryooptions auth login --browser --clinic <SLUG> --profile <NAME> --overwrite`.
  So is an expired token with no refresh token stored.
- Any other failure, or an answer without an access token, is exit 5.

How long a refresh token lasts is not known yet.

## The member API

Every path below is a `GET` with the [headers](#the-headers) above.
`MemberAccount/Member` has been seen answering a JSON object (*observed*);
its fields are not mapped here yet. The others have not been seen
answering (*bundle*).

| Path | What the app shows with it |
|---|---|
| `/api/Members/{memberId}/MemberAccount/Member?ivfCenterId={ivfCenterId}` | the account page: Service Plan, Next Scheduled Payment, Payment Method, Unpaid Balance |
| `/api/Members/{memberId}/MemberAccount/MemberPaymentHistoryAll` | the billing history and its print page: Date, Type, Method, Amount, Note, Ref, Freeze Date, Lab, Provider, Tax ID, CPT Codes |
| `/api/Members/{memberId}/Member/UpComingBills` | the next bill and its freeze date, on the home page of a member with embryos in storage |
| `/api/Members/{memberId}/FreezeEvents/FreezeEvents` | the specimens in storage, by freeze event |
| `/api/Members/{memberId}/MemberIvfCenter` | the member's clinic |
| `/api/Members/{memberId}/MemberIvfCenter/CollectionSites` | the clinic's collection sites |

No statement PDF and no receipt appear in the app. The billing history's
print page is the app's own rendering of `MemberPaymentHistoryAll`
(*bundle*).

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
