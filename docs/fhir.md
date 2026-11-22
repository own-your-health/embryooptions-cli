# The FHIR embryooptions writes

`records export` is built on the reading of the web app's code
(`docs/api.md`, *bundle*) and tested against synthetic answers; what a
signed-in read must still confirm is in `docs/api.md`, Not yet observed.

`records export --ndjson` writes `records-stream/v1` (medical-common's
`health-records/v1` profile) with `method: portal`: what the API answers,
mapped by hand to FHIR R4 in `src/bill.rs`. `src/bill.rs` is the model the
household's billing CLIs share: the same file is in peryourhealth-cli and
flywire-cli, and it needs serde_json's `arbitrary_precision`. Every
resource carries `meta.source` `embryooptions:site` and a tag saying it was
mapped. Ids are stable across runs, derived from the site's own numbers, so
a records store merges the same bill pulled twice.

| What the site shows | FHIR | Notes |
|---|---|---|
| A billing account | `Account` | `identifier` the account number (system `urn:embryooptions:<org>:account`); `subject` the Patient; `owner` the provider; `guarantor[].party.display` the responsible party; `status` `active` or `inactive`. Written before the Invoices, so `Invoice.account` resolves in the stream. R4's Account has no balance element and embryooptions writes no balance: a balance is the sum of the open (`issued`) Invoices' `totalNet` |
| A service billed (a visit or claim at the provider) | `Invoice` | `identifier[]` the site's number for it first (system `urn:embryooptions:<org>:bill`), then the insurer's claim number with `type.text` `insurer-claim` and `assigner.display` the payer where the site shows one. `status` `issued` while the patient owes on it, `balanced` at 0, `cancelled` when voided. `subject` the Patient; `issuer` the provider (`reference` and `display`); `account` the Account; `participant[]` the rendering provider; `date` the posting or statement date the site shows, never the date of service |
| A charge on it | `Invoice.contained[]` `ChargeItem`, pointed at by `lineItem[].chargeItemReference` (`#charge-<n>`) | `occurrenceDateTime` the date of service (`YYYY-MM-DD`); `code` CPT or HCPCS with their system URIs where the site shows one, else the site's description as text; `performer[].actor.display` the rendering provider; `quantity`. A bill the site shows without lines gets one ChargeItem of its own, code text the bill's name (else `Services`), holding its date of service: `occurrenceDateTime`, or `occurrencePeriod` when the service spans days |
| A charge's amounts | `lineItem[].priceComponent[]`, code system `urn:pk-med:bill-component` | `base` `billed`; `informational` `allowed`; `deduction` `insurance-payment`; `discount` `insurance-adjustment`; `deduction` `patient-payment`; `informational` `patient-responsibility` (what the patient still owes). Each coding's `display` is the profile's: `Billed charge`, `Allowed amount`, `Insurance payment`, `Insurance adjustment`, `Patient payment`, `Patient responsibility`. A bill without lines has its whole split on its one line |
| The bill's totals | `totalPriceComponent[]`, `totalNet`, `totalGross` | `totalPriceComponent` is the bill's own breakdown in the same codes, from the site's totals where it shows them, else summed from the lines: the billed amount is its `base` `billed`. `totalNet` is what the patient still owes. R4 puts both totals after the price components and `totalGross` adds tax, which a medical bill has none of, so `totalGross` equals `totalNet`; it is not the billed amount |
| A payment made | `PaymentReconciliation` | `status` `active`; `paymentDate`, `paymentAmount`, `paymentIdentifier` and `identifier` the site's transaction number; `disposition` how it was paid; `detail[]`, each `type` payment and `payee` the provider (`reference` and `display`): one per bill it paid where the site says, with `request` the Invoice and, when it paid that bill alone, `amount`; else one with the payee only. Insurance payments stay on the Invoice's lines |
| A statement | `DocumentReference` and its `Binary` | `type` `{"coding": [{"system": "urn:pk-med:doc-type", "code": "bill"}], "text": "bill"}`; `date` the statement date; `context.related` the Account, then the Invoices it lists where known. Amount due and due date have no R4 element, so none is written: a records store reads them from the file |
| A payment receipt the site serves as a file | `DocumentReference` and its `Binary` | built by `Mapper::document` with `DocType::Receipt`: typed as a statement is, code `receipt`, so a receipt's amount is never read as an amount due; `identifier` the receipt's number (system `urn:embryooptions:<org>:receipt`), ids `receipt-<account>-<number>`; `context.related` the Account and the Invoices it paid |

The two code systems, the components' R4 types, codes and displays, the
document types, `Money` and the totals come from `pk_med_records::bill`
(medical-common `DESIGN.md` §1.10), so every billing CLI writes them
alike and a records store reads them back with `Component::amount_in`
and `DocType::of`.

Every `Money` is `{"value": <number>, "currency": "USD"}`, the value a
JSON number in the decimal spelling a bill prints (`125.00`, `-12.30`):
never a string, which R4 forbids, and never rounded to `125.0`.

A records store keys a bill as `bill|<first identifier>|<earliest date of
service>` and a payment as `payment|<first identifier>|<paymentDate>`
(`id|...` where a payment has no number), and matches a bill to the
insurer's claim by the insurer's claim number where present, else by
person, date of service, provider and billed amount.

## From Embryo Options

What `records export` writes from the member API (`src/readers.rs`,
`src/reads.rs`). Every answer is read before the stream's header, so a
refused session, an answer it cannot read, or a billing history whose
pages hold fewer rows than its `total`, is the command's error and
nothing is written.

| The API shows | In the stream |
|---|---|
| The clinic | `Organization` `org-<clinic>` (the subdomain), named by `MemberIvfCenter`'s `name`, else the account's `ivfCenterName`, else the subdomain |
| The member | `Patient` `patient-<clinic>-<hash>`: the clinic, the profile and the token's member id hashed (FNV-1a), so it holds across sign-ins and never shows the member id. `managingOrganization` the clinic |
| The member's account (`MemberAccount/Member`) | one `Account`: `identifier` the account's `id`, `status` `active`, `owner` the clinic. Its `billingBalance` is not written (R4's Account has no balance): it is the sum of the open Invoices' `totalNet` |
| A charge in the billing history | an `Invoice` `bill-<account>-<row id>`: `date` the day it was posted (`timestamp`); `type.text` the fee's name, else the note, else the type, with the freeze date it is for (`Annual embryo storage (freeze date 2020-05-01)`); `participant` the clinic's provider; `status` `issued` while `outstandingBalance` is above 0, `balanced` at 0, `cancelled` when voided; `totalPriceComponent` `base` `billed` the row's `amount`, `informational` `patient-responsibility` its `outstandingBalance`; `totalNet` and `totalGross` the `outstandingBalance` |
| Its date of service and code | a charge naming a CPT or HCPCS code (`cptcode1`) is one `ChargeItem` line with that code, dated by the billing period's start. One naming none is a bill without lines: its one line holds the period, `occurrencePeriod` from the period's start to its end (or `occurrenceDateTime` when there is no period, the posting day) |
| A settled payment | a `PaymentReconciliation` `payment-<row id>`: `paymentDate` the day posted, `paymentAmount` the amount (positive), `paymentIdentifier` the row's id, `disposition` `Paid by <method>` (`Visa`, `PayPal`, `Check`). The history does not say which bills a payment paid, so its one `detail` names the clinic as payee |

The stream leaves out, each counted in one skip of type
`PaymentReconciliation`, so no skip is an `Invoice`'s and a records store
can still tell a bill the site no longer lists (DESIGN §1.10):

| Left out | Skip code |
|---|---|
| payments whose card the processor turned down, or whose status says failed, voided or returned | `not_settled` |
| payments without an id, a date or an amount | `unreadable` |
| refunds, discounts and credits: the bill mapping has no resource for them | `unsupported` |
| rows that move no money (a dispute opened, a balance carried forward) or name no type | `unsupported` |

A charge without a date or an amount is a skip of its `Invoice`
(`unreadable`), and so are charges without an id, in one skip: a bill
that could not be read.

Not exported: the upcoming bill (`UpComingBills`), the service plans and
next scheduled payments (`FreezeEvents`) and the payment method: they
describe what will be billed or how, not a bill. `accounts list` shows the
upcoming bill.

A clinic whose `merchantAccountDetails.currencyCode` is not `USD` is exit
5 before the stream: the profile's `Money` is US dollars only.
`accounts list`, `bills list` and `payments list` show its amounts as it
bills them, with the currency.

`records export` warns on stderr when the open charges'
`outstandingBalance` does not add up to the account's `billingBalance`: a
sign that `outstandingBalance` is not what is owed on each charge, which
the API has not yet been seen to confirm.

There are no `DocumentReference`s: Embryo Options serves no statement or
receipt files (`docs/api.md`).
