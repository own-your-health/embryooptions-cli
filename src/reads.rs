//! What the member API's answers say, parsed into the bill model
//! (`bill.rs`).
//!
//! The shapes come from the web app's JavaScript (docs/api.md, *bundle*);
//! `MemberAccount/Member`'s keys were seen answering (*observed*):
//!
//! - Amounts are in the clinic's currency, spelled as decimals (`125.00`):
//!   the app formats them as they come, with no division by 100. A JSON
//!   number with at most two decimals is read; anything else is exit 5
//!   naming the field, never with its value.
//! - Dates are .NET date-times: the first ten characters are the day. Year
//!   1 (`0001-01-01`, .NET's empty date) is no date.
//! - A missing or `null` field is `None`.
//! - The billing history is one list of every row the member's account
//!   has: charges, payments, refunds, discounts and credits, told apart by
//!   `memberPaymentTypeId` ([`kind_of`]). Payments, refunds, discounts and
//!   credits are read as positive amounts, whatever their sign.

use pk_cli_core::CliError;
use serde_json::Value;

use crate::bill::{self, Amounts, Cents, Charge, Code};
use crate::cli::BIN;

/// The member's account (`MemberAccount/Member`, *observed* keys).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The account's id (`id`), the Account's number.
    pub id: String,
    /// What the member owes now (`billingBalance`): the app's Unpaid
    /// Balance.
    pub balance: Option<Cents>,
    /// `adminFeeBalance`, as the API gives it. Whether it is part of
    /// `billingBalance` is not known.
    pub admin_fees: Option<Cents>,
    /// `ivfCenterName`.
    pub clinic_name: Option<String>,
}

/// The member's clinic (`MemberIvfCenter`, *bundle*).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClinicInfo {
    pub name: Option<String>,
    /// `merchantAccountDetails.currencyCode`, upper case: what the clinic
    /// bills in.
    pub currency: Option<String>,
}

/// What a billing-history row is, by its `memberPaymentTypeId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// A fee billed to the member: every type the app names no other way.
    Charge,
    /// Money paid to the clinic, by the member or collected for it.
    Payment,
    /// Money paid back to the member.
    Refund,
    /// A discount the clinic gave.
    Discount,
    /// A credit to the account.
    Credit,
    /// A row that moves no money: a dispute opened, a balance carried
    /// forward, or a row with no type.
    Other,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Charge => "charge",
            Kind::Payment => "payment",
            Kind::Refund => "refund",
            Kind::Discount => "discount",
            Kind::Credit => "credit",
            Kind::Other => "other",
        }
    }
}

/// The app's payment types (`useBillingHistory`'s enum, *bundle*).
const PAYMENTS: &[&str] = &[
    "MemberInitiatedPayment",
    "AutomatedPayment",
    "ScheduledInstallmentPayment",
    "CollectedPayment",
    "CenterInitiatedPayment",
    "CenterInitiatedAdvancePayment",
    "DirectPaymentToCenterByCheck",
    "DirectAdvancePaymentToCenterByCheck",
];
const REFUNDS: &[&str] = &["RefundViaCheck", "Reimbursement"];
const DISCOUNTS: &[&str] = &[
    "Discount",
    "FlatDiscount",
    "CenterInitiatedDiscount",
    "InsuranceDiscount",
];
const CREDITS: &[&str] = &["Credit"];
/// The enum's other members: a dispute opened, and labels of a statement
/// (a balance carried forward, past due, a period's end).
const OTHERS: &[&str] = &[
    "MemberDisputedChargeResolutionProcessInitiated",
    "PastDue",
    "BalanceForward",
    "Through",
];

/// The kind of a row of type `type_id`, in any case. The bundle names
/// every type that is not a charge, and none that is: a type outside its
/// list is a fee billed (docs/api.md).
pub fn kind_of(type_id: Option<&str>) -> Kind {
    let Some(t) = type_id.map(str::trim).filter(|t| !t.is_empty()) else {
        return Kind::Other;
    };
    let is = |list: &[&str]| list.iter().any(|x| x.eq_ignore_ascii_case(t));
    if is(PAYMENTS) {
        Kind::Payment
    } else if is(REFUNDS) {
        Kind::Refund
    } else if is(DISCOUNTS) {
        Kind::Discount
    } else if is(CREDITS) {
        Kind::Credit
    } else if is(OTHERS) {
        Kind::Other
    } else {
        Kind::Charge
    }
}

/// One row of the billing history (`MemberPaymentHistoryAll`'s `data[]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The row's id (`id`).
    pub id: String,
    /// `memberPaymentTypeId`, as the API gives it.
    pub type_id: Option<String>,
    pub kind: Kind,
    /// The day of `timestamp`: when it was posted.
    pub date: Option<String>,
    /// `amount`, positive for every kind but a charge.
    pub amount: Option<Cents>,
    /// `outstandingBalance`: read as what is still owed on a charge.
    pub outstanding: Option<Cents>,
    /// `billingCycleStartDate` and `billingCycleEndDate`: the period a
    /// storage fee covers.
    pub period_start: Option<String>,
    pub period_end: Option<String>,
    /// `recurringFeeName`, else `oneTimeFeeName`.
    pub fee: Option<String>,
    /// `discountTypeName`.
    pub discount_type: Option<String>,
    /// `allocationDiscountAmount`, as the API gives it: not mapped.
    pub allocated_discount: Option<Cents>,
    /// `billingNote`: the app's Note.
    pub note: Option<String>,
    /// `memberFreezeEvent.freezeDate`, else `memberFreezeEventDate`: the
    /// freeze the row is for.
    pub freeze_date: Option<String>,
    /// `ivfCenterProvider.fullName`.
    pub provider: Option<String>,
    /// `ivfCenterLab.name` and `ivfCenterLab.taxId`.
    pub lab: Option<String>,
    pub tax_id: Option<String>,
    /// `cptcode1` to `cptcode3`, those given.
    pub codes: Vec<String>,
    /// How it was paid, as the app words it: the card's type (`Visa`),
    /// `Credit Card`, `PayPal` or `Check`. Never the card's or account's
    /// digits.
    pub method: Option<String>,
    /// `creditCardSystemStatus`: `ACC` accepted, `DEC`, `EXP`, `INV`, `ERR`.
    pub card_status: Option<String>,
    /// `transactionStatus`, as the payment processor words it.
    pub transaction_status: Option<String>,
}

impl Row {
    /// The first day of service: the period's start, else the posting day.
    pub fn service_start(&self) -> Option<&str> {
        self.period_start.as_deref().or(self.date.as_deref())
    }

    /// A charge still owed on.
    pub fn is_open(&self) -> bool {
        self.kind == Kind::Charge && self.outstanding.is_some_and(|c| c.0 > 0)
    }

    pub fn is_voided(&self) -> bool {
        self.transaction_status.as_deref().is_some_and(|s| {
            let s = s.to_ascii_lowercase();
            s.contains("void") || s.contains("cancel")
        })
    }

    /// Whether the money moved: not a card the processor turned down, and
    /// no processor status saying it failed.
    pub fn is_settled(&self) -> bool {
        const NOT: &[&str] = &[
            "declin",
            "fail",
            "reject",
            "void",
            "expired",
            "cancel",
            "return",
            "bounce",
            "chargeback",
            "error",
        ];
        let card_ok = self.card_status.as_deref().is_none_or(|s| {
            !["DEC", "EXP", "INV", "ERR"]
                .iter()
                .any(|bad| s.eq_ignore_ascii_case(bad))
        });
        let s = self.transaction_status.as_deref().unwrap_or("").to_ascii_lowercase();
        card_ok && !NOT.iter().any(|n| s.contains(n))
    }

    /// What it was: the fee's name, else the note, else the type.
    pub fn description(&self) -> Option<String> {
        self.fee
            .clone()
            .or_else(|| self.note.clone())
            .or_else(|| self.type_id.clone())
    }
}

/// One page of the billing history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// The rows with an id.
    pub rows: Vec<Row>,
    /// The kinds of the rows without one, which cannot be placed.
    pub unidentified: Vec<Kind>,
    /// How many rows the page held, with an id or not.
    pub len: usize,
    /// `total`: how many rows the history has across its pages.
    pub total: Option<u64>,
    /// Whether the answer was a bare list, every row at once.
    pub unpaged: bool,
}

/// The next bill (`Member/UpComingBills`, *bundle*).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upcoming {
    pub date: Option<String>,
    pub amount: Option<Cents>,
    pub freeze_date: Option<String>,
}

// ------------------------------------------------------------- fields

fn shape(what: &str) -> CliError {
    CliError::Upstream(format!(
        "Embryo Options's {what} is not what {BIN} reads; the API may have changed (`{BIN} api GET <PATH>` shows the answer)"
    ))
}

/// An amount at `v[key]`: a JSON number with at most two decimals, `what`
/// naming it in an error, which never quotes it.
fn money(v: &Value, key: &str, what: &str) -> Result<Option<Cents>, CliError> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => {
            let text = n.to_string();
            match Cents::parse(&text) {
                // Ten trillion: beyond any bill, and no sum overflows.
                Some(c) if c.0.unsigned_abs() < 1_000_000_000_000_000 && !text.contains(['e', 'E']) => Ok(Some(c)),
                _ => Err(shape(&format!("{what}.{key} (an amount with more than two decimals)"))),
            }
        }
        Some(_) => Err(shape(&format!("{what}.{key} (an amount that is not a number)"))),
    }
}

fn positive(v: &Value, key: &str, what: &str) -> Result<Option<Cents>, CliError> {
    Ok(money(v, key, what)?.map(|c| Cents(c.0.abs())))
}

/// Text at `v[key]`: a string, trimmed, or a number written out.
fn text(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The day of a .NET date-time at `v[key]`; .NET's empty date is none.
fn day(v: &Value, key: &str) -> Option<String> {
    let s = text(v, key)?;
    let d = s.get(..10)?;
    pk_cli_core::dates::parse_iso(d).ok()?;
    (!d.starts_with("0001-")).then(|| d.to_string())
}

// ------------------------------------------------------------ answers

/// `MemberAccount/Member`'s answer. One without an `id` is exit 5.
pub fn member(v: &Value) -> Result<Member, CliError> {
    let Some(id) = text(v, "id") else {
        return Err(shape("member account (it has no `id`)"));
    };
    Ok(Member {
        id,
        balance: money(v, "billingBalance", "MemberAccount/Member")?,
        admin_fees: money(v, "adminFeeBalance", "MemberAccount/Member")?,
        clinic_name: text(v, "ivfCenterName"),
    })
}

/// `MemberIvfCenter`'s answer.
pub fn clinic(v: &Value) -> ClinicInfo {
    ClinicInfo {
        name: text(v, "name"),
        currency: v
            .get("merchantAccountDetails")
            .and_then(|m| text(m, "currencyCode"))
            .map(|c| c.to_ascii_uppercase()),
    }
}

/// `UpComingBills`' answer: `None` when it names no freeze date, which the
/// app shows as no upcoming bill.
pub fn upcoming(v: &Value) -> Result<Option<Upcoming>, CliError> {
    let freeze_date = day(v, "freezeDate");
    if freeze_date.is_none() {
        return Ok(None);
    }
    Ok(Some(Upcoming {
        date: day(v, "nextPaymentDate"),
        amount: money(v, "nextPaymentAmount", "UpComingBills")?,
        freeze_date,
    }))
}

/// How the app words a row's payment method (`useBillingHistory`,
/// *bundle*): `memberPaymentMethodId` `CR` is a card or PayPal account, by
/// `memberPaymentMethod.type`; any other is a check.
fn method(r: &Value) -> Option<String> {
    let id = text(r, "memberPaymentMethodId")?;
    if !id.eq_ignore_ascii_case("CR") {
        return Some("Check".into());
    }
    let m = r.get("memberPaymentMethod");
    match m.and_then(|m| text(m, "type")).as_deref() {
        Some("PayPal") => Some("PayPal".into()),
        _ => m
            .and_then(|m| m.get("creditCardDetails"))
            .and_then(|c| text(c, "cardType"))
            .or_else(|| Some("Credit Card".into())),
    }
}

fn row(r: &Value) -> Result<Option<Row>, CliError> {
    let at = "MemberPaymentHistoryAll.data";
    let type_id = text(r, "memberPaymentTypeId");
    let kind = kind_of(type_id.as_deref());
    let Some(id) = text(r, "id") else {
        return Ok(None);
    };
    let amount = match kind {
        Kind::Charge | Kind::Other => money(r, "amount", at)?,
        _ => positive(r, "amount", at)?,
    };
    let codes = ["cptcode1", "cptcode2", "cptcode3"]
        .iter()
        .filter_map(|k| text(r, k))
        .collect();
    let lab = r.get("ivfCenterLab");
    Ok(Some(Row {
        id,
        type_id,
        kind,
        date: day(r, "timestamp"),
        amount,
        outstanding: money(r, "outstandingBalance", at)?,
        period_start: day(r, "billingCycleStartDate"),
        period_end: day(r, "billingCycleEndDate"),
        fee: text(r, "recurringFeeName").or_else(|| text(r, "oneTimeFeeName")),
        discount_type: text(r, "discountTypeName"),
        allocated_discount: money(r, "allocationDiscountAmount", at)?,
        note: text(r, "billingNote"),
        freeze_date: r
            .get("memberFreezeEvent")
            .and_then(|f| day(f, "freezeDate"))
            .or_else(|| day(r, "memberFreezeEventDate")),
        provider: r.get("ivfCenterProvider").and_then(|p| text(p, "fullName")),
        lab: lab.and_then(|l| text(l, "name")),
        tax_id: lab.and_then(|l| text(l, "taxId")),
        codes,
        method: method(r),
        card_status: text(r, "creditCardSystemStatus"),
        transaction_status: text(r, "transactionStatus"),
    }))
}

/// One answer of `MemberPaymentHistoryAll`: `{"data": [...], "total": N}`
/// (*bundle*), or a bare list of rows. Anything else is exit 5.
pub fn history_page(v: &Value) -> Result<Page, CliError> {
    let (list, total, unpaged) = match v {
        Value::Array(list) => (list.as_slice(), None, true),
        Value::Object(o) => {
            let Some(list) = o.get("data").and_then(Value::as_array) else {
                return Err(shape("billing history (it has no `data` list)"));
            };
            (list.as_slice(), o.get("total").and_then(Value::as_u64), false)
        }
        _ => return Err(shape("billing history")),
    };
    let mut rows = Vec::new();
    let mut unidentified = Vec::new();
    for r in list {
        match row(r)? {
            Some(row) => rows.push(row),
            None => unidentified.push(kind_of(text(r, "memberPaymentTypeId").as_deref())),
        }
    }
    Ok(Page {
        rows,
        unidentified,
        len: list.len(),
        total,
        unpaged,
    })
}

// ----------------------------------------------------------- the model

/// The bill model's account at `provider`.
pub fn account(m: &Member, provider: &bill::Provider) -> bill::Account {
    bill::Account {
        number: m.id.clone(),
        provider: provider.clone(),
        patient_name: None,
        guarantor_name: None,
        balance: m.balance,
        patient_balance: m.balance,
        insurance_pending: None,
        active: true,
    }
}

const CPT: &str = "http://www.ama-assn.org/go/cpt";
const HCPCS: &str = "https://www.cms.gov/Medicare/Coding/HCPCSReleaseCodeSets";

/// A code as the history gives it: CPT for five digits (or four and a
/// category letter, `0001F`, `0042T`), HCPCS for a letter and four digits,
/// else no system.
pub fn code_of(raw: &str) -> Code {
    let c = raw.trim().to_ascii_uppercase();
    let b = c.as_bytes();
    let digits = |s: &[u8]| s.iter().all(u8::is_ascii_digit);
    let system = match b {
        [_, _, _, _, _] if digits(b) => Some(CPT),
        [_, _, _, _, b'F' | b'T' | b'U'] if digits(&b[..4]) => Some(CPT),
        [l, ..] if b.len() == 5 && l.is_ascii_alphabetic() && digits(&b[1..]) => Some(HCPCS),
        _ => None,
    };
    Code {
        system: system.map(str::to_string),
        code: c,
        display: None,
    }
}

/// A charge's description in the stream: its own, with the freeze it is
/// for.
fn described(r: &Row) -> Option<String> {
    match (r.description(), &r.freeze_date) {
        (Some(d), Some(f)) => Some(format!("{d} (freeze date {f})")),
        (d, _) => d,
    }
}

/// The bill model's bill for a charge row on account `account`. `None`
/// for a row with no date or no amount. A row naming a code is one charge
/// line with it; one naming none is a bill without lines, so its period
/// stays on its one line (`bill.rs`).
pub fn bill(account: &str, r: &Row) -> Option<bill::Bill> {
    let start = r.service_start()?.to_string();
    let billed = r.amount?;
    let amounts = Amounts {
        billed: Some(billed),
        patient_owes: r.outstanding.map(|c| Cents(c.0.max(0))),
        ..Amounts::default()
    };
    let description = described(r);
    let lines = match r.codes.first() {
        Some(c) => vec![Charge {
            date: start.clone(),
            code: Some(code_of(c)),
            description: description.clone(),
            quantity: None,
            amounts,
        }],
        None => Vec::new(),
    };
    Some(bill::Bill {
        number: r.id.clone(),
        account: account.to_string(),
        service_end: r.period_end.clone().filter(|e| e.as_str() > start.as_str()),
        service_start: start,
        posted: r.date.clone(),
        rendering: r.provider.clone(),
        description,
        insurer: None,
        insurer_claim: None,
        lines,
        totals: amounts,
        voided: r.is_voided(),
    })
}

/// The bill model's payment for a payment row on account `account`.
/// `None` for one without a date or an amount.
pub fn payment(account: &str, r: &Row) -> Option<bill::Payment> {
    Some(bill::Payment {
        number: Some(r.id.clone()),
        account: account.to_string(),
        date: r.date.clone()?,
        amount: r.amount?,
        method: r.method.clone(),
        bills: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn number(text: &str) -> Value {
        serde_json::from_str(&format!(r#"{{"a": {text}}}"#)).unwrap()
    }

    #[test]
    fn amounts_are_decimals_of_two_places_or_an_error_naming_the_field() {
        for (text, cents) in [
            ("125.00", 12500),
            ("125", 12500),
            ("12.5", 1250),
            ("-3.10", -310),
            ("0", 0),
        ] {
            assert_eq!(money(&number(text), "a", "x").unwrap(), Some(Cents(cents)), "{text}");
        }
        assert_eq!(money(&json!({"a": null}), "a", "x").unwrap(), None);
        assert_eq!(money(&json!({}), "a", "x").unwrap(), None);
        assert_eq!(positive(&number("-40.00"), "a", "x").unwrap(), Some(Cents(4000)));
        for bad in ["12.345", "1e3", "\"12.50\"", "1e300"] {
            let e = money(&number(bad), "a", "MemberPaymentHistoryAll.data").err().unwrap();
            assert_eq!(e.exit_code(), 5, "{bad}");
            assert!(e.to_string().contains("MemberPaymentHistoryAll.data.a"), "{e}");
            assert!(!e.to_string().contains("12.3") && !e.to_string().contains("1e"), "{e}");
        }
    }

    #[test]
    fn days_are_read_from_dotnet_date_times() {
        let v = json!({"a": "2026-01-05T10:00:00", "b": "2026-01-05", "c": "0001-01-01T00:00:00",
            "d": "01/05/2026", "e": 7});
        assert_eq!(day(&v, "a").as_deref(), Some("2026-01-05"));
        assert_eq!(day(&v, "b").as_deref(), Some("2026-01-05"));
        for none in ["c", "d", "e", "missing"] {
            assert_eq!(day(&v, none), None, "{none}");
        }
    }

    #[test]
    fn rows_are_told_apart_by_their_type() {
        for (t, k) in [
            (Some("MemberInitiatedPayment"), Kind::Payment),
            (Some("automatedpayment"), Kind::Payment),
            (Some("DirectPaymentToCenterByCheck"), Kind::Payment),
            (Some("RefundViaCheck"), Kind::Refund),
            (Some("FlatDiscount"), Kind::Discount),
            (Some("Credit"), Kind::Credit),
            (Some("BalanceForward"), Kind::Other),
            (Some(" "), Kind::Other),
            (None, Kind::Other),
            (Some("AnnualStorageFee"), Kind::Charge),
        ] {
            assert_eq!(kind_of(t), k, "{t:?}");
        }
    }

    #[test]
    fn a_payment_settles_unless_the_processor_turned_it_down() {
        let r = |card: Option<&str>, status: Option<&str>| Row {
            card_status: card.map(str::to_string),
            transaction_status: status.map(str::to_string),
            ..row(&json!({"id": 1, "memberPaymentTypeId": "AutomatedPayment"}))
                .unwrap()
                .unwrap()
        };
        for (card, status) in [
            (None, None),
            (Some("ACC"), Some("settled")),
            (None, Some("submitted_for_settlement")),
        ] {
            assert!(r(card, status).is_settled(), "{card:?} {status:?}");
        }
        for (card, status) in [
            (Some("DEC"), None),
            (Some("err"), None),
            (None, Some("processor_declined")),
            (None, Some("voided")),
            (None, Some("gateway_rejected")),
        ] {
            assert!(!r(card, status).is_settled(), "{card:?} {status:?}");
        }
    }

    #[test]
    fn the_method_is_worded_as_the_app_words_it_without_digits() {
        let m = |v: Value| method(&v);
        assert_eq!(m(json!({})), None);
        assert_eq!(
            m(json!({"memberPaymentMethodId": "CH", "memberBankAccount": {"cryptAccountNumber": "x1111"}})).as_deref(),
            Some("Check")
        );
        assert_eq!(
            m(
                json!({"memberPaymentMethodId": "CR", "memberPaymentMethod": {"type": "Credit Card",
                "creditCardDetails": {"cardType": "Visa", "maskedNumber": "************1111"}}})
            )
            .as_deref(),
            Some("Visa")
        );
        assert_eq!(
            m(json!({"memberPaymentMethodId": "CR", "memberPaymentMethod": {"type": "PayPal"}})).as_deref(),
            Some("PayPal")
        );
        assert_eq!(
            m(json!({"memberPaymentMethodId": "CR"})).as_deref(),
            Some("Credit Card")
        );
    }

    #[test]
    fn codes_get_their_system_by_shape() {
        assert_eq!(code_of("89342").system.as_deref(), Some(CPT));
        assert_eq!(code_of("0042t").system.as_deref(), Some(CPT));
        assert_eq!(code_of("S4027").system.as_deref(), Some(HCPCS));
        assert_eq!(code_of("STORAGE").system, None);
        assert_eq!(code_of(" s4027 ").code, "S4027");
    }

    #[test]
    fn a_history_page_is_data_and_total_or_a_bare_list() {
        let p = history_page(&json!({"data": [{"id": 1, "memberPaymentTypeId": "Fee", "amount": 10},
            {"memberPaymentTypeId": "AutomatedPayment"}], "total": 3}))
        .unwrap();
        assert_eq!(
            (p.rows.len(), p.unidentified.clone(), p.len, p.total, p.unpaged),
            (1, vec![Kind::Payment], 2, Some(3), false)
        );
        let p = history_page(&json!([{"id": 1}])).unwrap();
        assert!(p.unpaged);
        for bad in [json!({"items": []}), json!("x")] {
            assert_eq!(history_page(&bad).err().unwrap().exit_code(), 5);
        }
    }

    #[test]
    fn a_charge_is_a_bill_with_its_period_or_its_code() {
        let r = row(
            &json!({"id": 9, "memberPaymentTypeId": "Fee", "timestamp": "2026-01-02T08:00:00",
            "amount": 600.00, "outstandingBalance": 600.00, "billingCycleStartDate": "2026-01-01T00:00:00",
            "billingCycleEndDate": "2026-12-31T00:00:00", "recurringFeeName": "Annual storage",
            "memberFreezeEvent": {"freezeDate": "2020-05-01T00:00:00"}}),
        )
        .unwrap()
        .unwrap();
        let b = bill("1001", &r).unwrap();
        assert_eq!(
            (b.service_start.as_str(), b.service_end.as_deref(), b.posted.as_deref()),
            ("2026-01-01", Some("2026-12-31"), Some("2026-01-02"))
        );
        assert!(b.lines.is_empty());
        assert_eq!(
            b.description.as_deref(),
            Some("Annual storage (freeze date 2020-05-01)")
        );
        assert_eq!(
            (b.totals.billed, b.totals.patient_owes),
            (Some(Cents(60000)), Some(Cents(60000)))
        );
        let coded = Row {
            codes: vec!["89342".into()],
            ..r.clone()
        };
        let b = bill("1001", &coded).unwrap();
        assert_eq!(b.lines.len(), 1);
        assert_eq!(b.lines[0].code.as_ref().unwrap().system.as_deref(), Some(CPT));
        assert!(bill(
            "1001",
            &Row {
                amount: None,
                ..r.clone()
            }
        )
        .is_none());
        assert!(bill(
            "1001",
            &Row {
                date: None,
                period_start: None,
                ..r
            }
        )
        .is_none());
    }
}
