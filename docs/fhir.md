# The FHIR embryooptions writes

Not built yet: `records export` comes once the signed-in API is observed
(`docs/api.md`). This is the mapping it will use.

`records export --ndjson` writes `records-stream/v1` (medical-common's
`health-records/v1` profile) with `method: portal`: what the API answers,
mapped by hand to FHIR R4 in `src/bill.rs`. `src/bill.rs` is the model the
household's billing CLIs share (it is peryourhealth-cli's, unchanged). Every
resource carries `meta.source` `embryooptions:site` and a tag saying it was
mapped. Ids are stable across runs, derived from the site's own numbers, so
a records store merges the same bill pulled twice.

| What the site shows | FHIR | Notes |
|---|---|---|
| A billing account | `Account` | `identifier` the account number (system `urn:embryooptions:<org>:account`); `subject` the Patient; `owner` the provider; `guarantor[].party.display` the responsible party; `status` `active` or `inactive`. R4's Account has no balance element: a balance is the sum of the open Invoices' `totalNet` |
| A service billed (a visit or claim at the provider) | `Invoice` | `identifier[]` the site's number for it (system `urn:embryooptions:<org>:bill`), and the insurer's claim number with `type.text` `insurer-claim` and `assigner.display` the payer where the site shows one. `status` `issued` while the patient owes on it, `balanced` at 0, `cancelled` when voided. `subject` the Patient; `issuer` the provider; `account` the Account; `participant[]` the rendering provider; `date` the posting or statement date the site shows |
| A charge on it | `Invoice.contained[]` `ChargeItem`, pointed at by `lineItem[].chargeItemReference` (`#charge-<n>`) | `occurrenceDateTime` the date of service; `code` CPT or HCPCS where the site shows one, else the site's description as text; `performer[]` the rendering provider; `quantity` |
| A charge's amounts | `lineItem[].priceComponent[]`, each `amount` a `Money` in USD, code system `urn:pk-med:bill-component` | `base` `billed`; `informational` `allowed`; `deduction` `insurance-payment`; `discount` `insurance-adjustment`; `deduction` `patient-payment`; `informational` `patient-responsibility` (what the patient still owes) |
| The bill's totals | `totalGross`, `totalNet` | billed, and what the patient still owes: the site's totals, else the sum of the lines'. A bill the site shows without lines is one line of its own (`chargeItemCodeableConcept` text `Services`), with its date of service in `note` |
| A payment made | `PaymentReconciliation` | `paymentDate`, `paymentAmount`, `paymentIdentifier` the site's transaction number; `detail[].request` each Invoice it paid, where the site says; `disposition` how it was paid. Insurance payments stay on the Invoice's lines |
| A statement | `DocumentReference` (type text `bill`) and its `Binary` | `date` the statement date; `context.related` the Invoices it lists, where known |

A records store keys a bill as `bill|<first identifier>|<earliest date of
service>`, and matches it to the insurer's claim by the insurer's claim
number where present, else by person, date of service, provider and billed
amount.

## From Embryo Options

What the app shows (*bundle*, `docs/api.md`), and where it is expected to
go. It is checked against real answers before it is built.

| The app shows | Expected in the model |
|---|---|
| The account page: Service Plan, Unpaid Balance | `Account` (`balance`, `patient_balance`); the model has no field for the service plan yet |
| A billing history row of a charge (storage fee): Date, Amount, Note, Ref, Freeze Date, Lab, Provider, Tax ID, CPT Codes | a `Bill` of one `Charge`: `date` the row's date, `code` the CPT code, `description` the note, `rendering` the provider; the freeze date in the description |
| A billing history row of a payment: Date, Method, Amount, Ref | a `Payment`: `number` the reference, `method` the method |
| Next Scheduled Payment, Payment Method | not exported: they describe the payment set-up, not a bill |
