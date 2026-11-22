//! The reader commands: what `patients`, `accounts`, `bills`, `payments`
//! and `documents` print, and the `records export` stream. Each reads
//! through a [`Site`] and returns its document, so `app` prints it and the
//! tests check it in-process against a loopback server.
//!
//! One sign-in is one member at one clinic, with one account. The
//! organization's id is the clinic's subdomain. The patient's is the
//! clinic with a hash of the profile and the member's id, so it holds
//! across sign-ins to the profile, and a profile reused for another
//! member gets another id rather than merging two people's records. The
//! member's id itself never shows.
//!
//! Every reader reads the same three answers first ([`book`]): the
//! member's account, the clinic, and the billing history, every page of
//! it. The history is one list of charges, payments, refunds, discounts
//! and credits (`reads::Kind`): a charge is a bill, a payment a payment.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::PathBuf;

use pk_cli_core::private_file;
use pk_cli_core::{CliError, RangeArgs};
use pk_cli_documents::{Document, DownloadBatch};
use pk_med_records::dto::{kind, method, relationship};
use pk_med_records::stream::skip;
use pk_med_records::{Finished, Header, Organization, PatientContext, RawResource, RecordsWriter, Skip, Source};
use serde_json::{json, Value};

use crate::bill::{self, id_of, Cents, Mapper};
use crate::cli::{DownloadArgs, BIN};
use crate::portal::{now_rfc3339, Site};
use crate::reads::{self, ClinicInfo, Kind, Member, Row};

/// `meta.source` on every resource embryooptions maps.
pub const META_SOURCE: &str = "embryooptions:site";
pub const TAG_SYSTEM: &str = "https://github.com/own-your-health/embryooptions-cli/fhir/tag";
pub const TAG_MAPPED: &str = "mapped-from-member-portal";
const TAG_DISPLAY: &str = "Mapped by embryooptions from the Embryo Options member portal";

/// The member's account.
pub const MEMBER_PATH: &str = "/api/Members/{memberId}/MemberAccount/Member?ivfCenterId={ivfCenterId}";
/// The member's clinic: its name and the currency it bills in.
pub const CLINIC_PATH: &str = "/api/Members/{memberId}/MemberIvfCenter";
/// The billing history, one page: `pageNumber` from 1.
pub const HISTORY_PATH: &str = "/api/Members/{memberId}/MemberAccount/MemberPaymentHistoryAll";
/// The next bill of a member with embryos in storage.
pub const UPCOMING_PATH: &str = "/api/Members/{memberId}/Member/UpComingBills";

/// The largest page the app asks for.
pub const PAGE_SIZE: usize = 50;
/// 10,000 rows: far beyond a storage account's history.
const MAX_PAGES: usize = 200;

/// The member type whose home page shows the upcoming bill: embryos in
/// storage (`CRY`). The app asks `UpComingBills` for no other.
const EMBRYO_CRYO: &str = "CRY";

/// What one sign-in shows, read once.
pub struct Book {
    pub member: Member,
    pub clinic: ClinicInfo,
    /// The history's rows with an id, newest first.
    pub rows: Vec<Row>,
    /// The kinds of the rows that came without an id.
    pub unidentified: Vec<Kind>,
    pub mapper: Mapper,
}

/// FNV-1a, 64-bit: a stable hash with no dependency, for ids only.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The page of the history `n` names.
pub fn history_page_path(n: usize) -> String {
    format!("{HISTORY_PATH}?pageNumber={n}&pageSize={PAGE_SIZE}")
}

/// Every row of the billing history, page by page, as the app's billing
/// tab pages it. With a `total` (the first answer's), it reads until the
/// pages hold that many rows, so a server that pages shorter than asked is
/// read whole; without one, until a page shorter than [`PAGE_SIZE`]. An
/// empty page ends it too. A history that holds fewer rows than its
/// `total` is exit 5: a reader never passes part of it off as the whole.
/// A row a later page repeats is read once, and a later page of nothing
/// but repeats is exit 5: the server is not paging.
fn history(site: &Site) -> Result<(Vec<Row>, Vec<Kind>), CliError> {
    let mut rows = Vec::new();
    let mut unidentified = Vec::new();
    let mut ids = BTreeSet::new();
    let mut total = None;
    let mut received = 0usize;
    for n in 1..=MAX_PAGES {
        let page = reads::history_page(&site.read(&history_page_path(n))?)?;
        total = total.or(page.total);
        received += page.len;
        let fresh_unidentified = page.unidentified.len();
        unidentified.extend(page.unidentified);
        let mut fresh = 0;
        for r in page.rows {
            if ids.insert(r.id.clone()) {
                rows.push(r);
                fresh += 1;
            } else {
                received -= 1;
            }
        }
        // A server that ignores `pageNumber` answers the same rows again:
        // reading on would only repeat them, up to the cap.
        if n > 1 && page.len > 0 && fresh + fresh_unidentified == 0 {
            return Err(CliError::Upstream(format!(
                "Embryo Options's billing history answered page {n} with the rows of the pages before it"
            ))
            .with_remedy(format!(
                "the API may have changed; `{BIN} api GET <PATH>` shows its pages"
            )));
        }
        let done = match total {
            Some(t) => received as u64 >= t,
            None => page.len < PAGE_SIZE,
        };
        if page.unpaged || page.len == 0 || done {
            if let Some(t) = total.filter(|t| (received as u64) < *t) {
                return Err(CliError::Upstream(format!(
                    "Embryo Options's billing history counts {t} rows and its pages held {received}"
                ))
                .with_remedy("try again; the history may have changed while it was read"));
            }
            rows.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| b.id.cmp(&a.id)));
            return Ok((rows, unidentified));
        }
    }
    Err(CliError::Upstream(format!(
        "Embryo Options's billing history ran past {MAX_PAGES} pages of {PAGE_SIZE}"
    )))
}

/// The member's account, the clinic and the billing history, with the ids
/// the resources carry.
pub fn book(site: &Site) -> Result<Book, CliError> {
    let member = reads::member(&site.read(MEMBER_PATH)?)?;
    let clinic = reads::clinic(&site.read(CLINIC_PATH)?);
    let (rows, unidentified) = history(site)?;
    let slug = site.clinic().slug().to_string();
    let organization_name = clinic
        .name
        .clone()
        .or_else(|| member.clinic_name.clone())
        .unwrap_or_else(|| slug.clone());
    let key = format!("{slug}\n{}\n{}", site.profile(), site.member_id());
    Ok(Book {
        member,
        clinic,
        rows,
        unidentified,
        mapper: Mapper {
            cli: BIN.into(),
            meta_source: META_SOURCE.into(),
            tag_system: TAG_SYSTEM.into(),
            tag_code: TAG_MAPPED.into(),
            tag_display: TAG_DISPLAY.into(),
            patient_id: id_of("patient", &format!("{slug}-{:016x}", fnv1a(key.as_bytes()))),
            organization_id: id_of("org", &slug),
            organization_name,
        },
    })
}

impl Book {
    fn provider(&self) -> bill::Provider {
        bill::Provider {
            id: self.mapper.organization_id.clone(),
            name: self.mapper.organization_name.clone(),
        }
    }

    /// The rows of `kind`, newest first.
    pub fn of(&self, kind: Kind) -> impl Iterator<Item = &Row> {
        self.rows.iter().filter(move |r| r.kind == kind)
    }

    /// What the open bills say is owed, when any says.
    pub fn owed_on_bills(&self) -> Option<Cents> {
        let owed: Vec<i64> = self
            .of(Kind::Charge)
            .filter(|r| r.is_open())
            .filter_map(|r| r.outstanding)
            .map(|c| c.0)
            .collect();
        let any = self.of(Kind::Charge).any(|r| r.outstanding.is_some());
        any.then(|| Cents(owed.iter().sum()))
    }

    /// The patient a sign-in is. `name` is never in the stream.
    pub fn context(&self) -> PatientContext {
        PatientContext {
            id: self.mapper.patient_id.clone(),
            organization: self.mapper.organization_id.clone(),
            label: None,
            relationship: Some(relationship::SELF.into()),
            kind: Some(kind::PATIENT.into()),
            member_role: None,
            fhir_user: None,
            name: None,
            session: None,
        }
    }
}

fn money(c: Option<Cents>) -> Value {
    c.map_or(Value::Null, |c| c.number().into())
}

/// A day given as `YYYY-MM-DD` (or unpadded, `2026-1-5`) as `YYYY-MM-DD`,
/// so it compares with the API's days as text.
fn iso_day(s: &str) -> Result<String, CliError> {
    let (y, m, d) = pk_cli_core::dates::parse_iso(s.trim())?;
    Ok(format!("{y:04}-{m:02}-{d:02}"))
}

/// The range with its days padded (`RangeArgs::validate` has checked them).
fn padded(range: &RangeArgs) -> Result<RangeArgs, CliError> {
    Ok(RangeArgs {
        since: range.since.as_deref().map(iso_day).transpose()?,
        until: range.until.as_deref().map(iso_day).transpose()?,
        ..range.clone()
    })
}

/// Whether `date` is in the range: an undated row is in it only when the
/// range is open.
fn in_range(date: Option<&str>, range: &RangeArgs) -> bool {
    match date {
        Some(d) => range.since.as_deref().is_none_or(|s| d >= s) && range.until.as_deref().is_none_or(|u| d <= u),
        None => range.since.is_none() && range.until.is_none(),
    }
}

fn limited(mut items: Vec<Value>, range: &RangeArgs) -> Vec<Value> {
    if let Some(n) = range.limit {
        items.truncate(n as usize);
    }
    items
}

fn list(record: &str, items: Vec<Value>) -> Value {
    json!({ "schema": format!("{record}-list/v1"), "items": items })
}

// ------------------------------------------------------------- patients

/// `patients list` (patient-list/v1): the one member a sign-in is.
pub fn patients(site: &Site) -> Result<Value, CliError> {
    let book = book(site)?;
    serde_json::to_value(pk_med_records::patient_list(vec![book.context()]))
        .map_err(|e| CliError::Other(format!("serializing: {e}")))
}

// ------------------------------------------------------------- accounts

/// The next bill, as the app's home page shows it to a member with embryos
/// in storage; `null` for any other member, or when there is none. The
/// app ignores a failure of this read, and so does this one but for a
/// sign-in that has ended: a warning, and `null`.
fn next_bill(site: &Site) -> Result<Value, CliError> {
    if site.member_type() != Some(EMBRYO_CRYO) {
        return Ok(Value::Null);
    }
    let read = site.read(UPCOMING_PATH).and_then(|v| reads::upcoming(&v));
    match read {
        Ok(Some(u)) => Ok(json!({ "date": u.date, "amount": money(u.amount), "freeze_date": u.freeze_date })),
        Ok(None) => Ok(Value::Null),
        Err(e) if e.exit_code() == 3 => Err(e),
        Err(e) => {
            site.warn(format!("the upcoming bill was not read: {}", e.message()));
            Ok(Value::Null)
        }
    }
}

/// `accounts list` (account-list/v1): the sign-in's one account.
pub fn accounts_list(site: &Site) -> Result<Value, CliError> {
    let book = book(site)?;
    let next = next_bill(site)?;
    let m = &book.member;
    let row = json!({
        "id": m.id,
        "provider": book.mapper.organization_name,
        "currency": book.clinic.currency,
        "owes": money(m.balance),
        "admin_fees": money(m.admin_fees),
        "bills": book.of(Kind::Charge).count(),
        "open_bills": book.of(Kind::Charge).filter(|r| r.is_open()).count(),
        "owed_on_bills": money(book.owed_on_bills()),
        "payments": book.of(Kind::Payment).count(),
        "next_bill": next,
    });
    Ok(list("account", vec![row]))
}

pub const ACCOUNT_COLUMNS: &[&str] = &["id", "provider", "owes", "bills", "open_bills"];

// ---------------------------------------------------------------- bills

fn bill_row(book: &Book, r: &Row) -> Value {
    json!({
        "id": r.id,
        "account": book.member.id,
        "date": r.service_start(),
        "end": r.period_end,
        "posted": r.date,
        "type": r.type_id,
        "description": r.description(),
        "freeze_date": r.freeze_date,
        "billed": money(r.amount),
        "owes": money(r.outstanding),
        "open": r.is_open(),
    })
}

/// `bills list` (bill-list/v1): the charges, newest service first.
pub fn bills_list(site: &Site, open: bool, range: &RangeArgs) -> Result<Value, CliError> {
    let range = &padded(range)?;
    let book = book(site)?;
    let mut rows: Vec<&Row> = book
        .of(Kind::Charge)
        .filter(|r| !open || r.is_open())
        .filter(|r| in_range(r.service_start(), range))
        .collect();
    rows.sort_by(|x, y| y.service_start().cmp(&x.service_start()).then_with(|| y.id.cmp(&x.id)));
    let items = rows.into_iter().map(|r| bill_row(&book, r)).collect();
    Ok(list("bill", limited(items, range)))
}

pub const BILL_COLUMNS: &[&str] = &["id", "date", "description", "billed", "owes"];

/// `bills show <ID>` (bill/v1): one charge with all the history says of it.
pub fn bill_show(site: &Site, id: &str) -> Result<Value, CliError> {
    let book = book(site)?;
    let id = id.trim();
    let Some(r) = book.of(Kind::Charge).find(|r| r.id == id) else {
        return Err(CliError::NotFound(format!("no bill `{id}` at this sign-in"))
            .with_remedy(format!("`{BIN} bills list --profile <NAME>` lists them")));
    };
    let mut v = json!({ "schema": "bill/v1" });
    if let (Some(doc), Value::Object(row)) = (v.as_object_mut(), bill_row(&book, r)) {
        doc.extend(row);
    }
    v["fee"] = json!(r.fee);
    v["note"] = json!(r.note);
    v["provider"] = json!(r.provider);
    v["lab"] = json!(r.lab);
    v["lab_tax_id"] = json!(r.tax_id);
    v["codes"] = json!(r.codes);
    v["discount_type"] = json!(r.discount_type);
    v["allocated_discount"] = money(r.allocated_discount);
    v["charges"] = json!([{
        "date": r.service_start(),
        "code": r.codes.first(),
        "description": r.description(),
        "amount": money(r.amount),
    }]);
    Ok(v)
}

pub const CHARGE_COLUMNS: &[&str] = &["date", "code", "description", "amount"];

// ------------------------------------------------------------- payments

/// The kinds `payments list` lists: every row that pays, pays back or
/// takes off.
const PAID: &[Kind] = &[Kind::Payment, Kind::Refund, Kind::Discount, Kind::Credit];

/// `payments list` (payment-list/v1): payments, refunds, discounts and
/// credits, newest first, with whether each settled.
pub fn payments_list(site: &Site, range: &RangeArgs) -> Result<Value, CliError> {
    let range = &padded(range)?;
    let book = book(site)?;
    let items = book
        .rows
        .iter()
        .filter(|r| PAID.contains(&r.kind) && in_range(r.date.as_deref(), range))
        .map(|r| {
            json!({
                "id": r.id,
                "account": book.member.id,
                "date": r.date,
                "kind": r.kind.as_str(),
                "type": r.type_id,
                "amount": money(r.amount),
                "method": r.method,
                "note": r.note,
                "settled": r.is_settled(),
            })
        })
        .collect();
    Ok(list("payment", limited(items, range)))
}

pub const PAYMENT_COLUMNS: &[&str] = &["date", "kind", "amount", "method", "settled"];

// ------------------------------------------------------------ documents

/// Why there are no documents: the app shows no bill or receipt files
/// (docs/api.md).
pub const NO_DOCUMENTS: &str =
    "Embryo Options serves no statement or receipt files; its printable billing history is a \
     page the web app draws from the history `bills list` and `payments list` read";

/// `documents list` (document-list/v1): none. Sends no request.
pub fn documents() -> Vec<Document> {
    Vec::new()
}

/// `documents download`: no document has an id, so one is exit 4, and
/// `--all` saves none into the directory it names.
pub fn download(args: &DownloadArgs) -> Result<DownloadBatch, CliError> {
    if !args.all {
        let id = args.id.as_deref().unwrap_or_default().trim();
        return Err(CliError::NotFound(format!(
            "no document `{id}` at this sign-in: {NO_DOCUMENTS}"
        )));
    }
    let dir = args.output.clone().unwrap_or_else(|| PathBuf::from("."));
    private_file::create_dir_all(&dir).map_err(|e| CliError::Other(format!("creating {}: {e}", dir.display())))?;
    Ok(DownloadBatch::new(dir.display().to_string(), Vec::new()))
}

// --------------------------------------------------------------- export

fn meta(last_updated: &str) -> Value {
    json!({
        "lastUpdated": last_updated,
        "source": META_SOURCE,
        "tag": [{ "system": TAG_SYSTEM, "code": TAG_MAPPED, "display": TAG_DISPLAY }],
    })
}

/// The stream header for a sign-in.
pub fn header(book: &Book, origin: &str, since: Option<&str>) -> Header {
    let mut h = Header::new(
        Source::new(BIN, env!("CARGO_PKG_VERSION")),
        Organization {
            id: book.mapper.organization_id.clone(),
            name: book.mapper.organization_name.clone(),
            fhir_base: origin.to_string(),
            vendor: Some("embryooptions".into()),
            location: None,
            aliases: Vec::new(),
        },
        book.context(),
        now_rfc3339(),
    );
    h.method = Some(method::PORTAL.into());
    if let Some(s) = since {
        h.since = Some(s.into());
        h.since_unbounded = vec!["Organization".into(), "Patient".into(), "Account".into()];
    }
    h
}

/// `--patient` and `--org` must name this sign-in's: one sign-in is one
/// member at one clinic. Errors name the id given, never another.
fn check_ids(book: &Book, patient: Option<&str>, org: Option<&str>) -> Result<(), CliError> {
    if let Some(p) = patient.map(str::trim).filter(|p| !p.is_empty()) {
        if p != book.mapper.patient_id {
            return Err(CliError::NotFound(format!(
                "no patient `{p}` at this sign-in: an Embryo Options sign-in is one member"
            ))
            .with_remedy(format!("`{BIN} patients list --profile <NAME>` shows its id")));
        }
    }
    if let Some(o) = org.map(str::trim).filter(|o| !o.is_empty()) {
        if o != book.mapper.organization_id {
            return Err(
                CliError::NotFound(format!("no organization `{o}` at this sign-in: it reads one clinic"))
                    .with_remedy(format!("`{BIN} patients list --profile <NAME>` shows its organization")),
            );
        }
    }
    Ok(())
}

/// The amounts are written in USD (DESIGN §1.10): a clinic that bills in
/// another currency is exit 5, before the stream starts.
fn check_currency(book: &Book) -> Result<(), CliError> {
    match book.clinic.currency.as_deref() {
        None | Some("USD") => Ok(()),
        Some(other) => Err(CliError::Upstream(format!(
            "the clinic bills in {other}, and the records stream carries US dollars only"
        ))
        .with_remedy(format!(
            "`{BIN} bills list` and `{BIN} payments list` show its amounts as it bills them"
        ))),
    }
}

/// `n` rows `records export` leaves out, as one skip. None of them is a
/// bill, so none is an Invoice's skip (DESIGN §1.10).
fn skip_count<W: Write>(w: &mut RecordsWriter<W>, n: usize, code: &str, reason: String) {
    if n > 0 {
        w.skip(Skip::resource("PaymentReconciliation", "", code, reason));
    }
}

/// `records export`: the stream on `out`. Every answer is read before the
/// header, so a refused session, an unreadable answer or a history read
/// short is an ordinary error and nothing reaches `out`. A charge without a
/// date or an amount is a skip for its Invoice (DESIGN §1.10).
pub fn export<W: Write>(
    site: &Site,
    origin: &str,
    out: W,
    patient: Option<&str>,
    org: Option<&str>,
    since: Option<&str>,
) -> Result<Finished, CliError> {
    let since = since.map(iso_day).transpose()?;
    let since = since.as_deref();
    let book = book(site)?;
    check_ids(&book, patient, org)?;
    check_currency(&book)?;
    if let (Some(owed), Some(balance)) = (book.owed_on_bills(), book.member.balance) {
        if owed != balance {
            site.warn(format!(
                "the bills' outstanding balances add to {}, and the account shows {} owed: {BIN} reads \
                 `outstandingBalance` as what is owed on each bill, which the API has not yet been seen to confirm",
                owed.decimal(),
                balance.decimal()
            ));
        }
    }
    let now = now_rfc3339();
    let mut w = RecordsWriter::start(out, &header(&book, origin, since))?;
    let result = (|| -> Result<(), CliError> {
        let m = &book.mapper;
        let account = &book.member.id;
        w.resource(&RawResource::from_value(&json!({
            "resourceType": "Organization",
            "id": m.organization_id,
            "meta": meta(&now),
            "name": m.organization_name,
        }))?)?;
        w.resource(&RawResource::from_value(&json!({
            "resourceType": "Patient",
            "id": m.patient_id,
            "meta": meta(&now),
            "managingOrganization": { "reference": format!("Organization/{}", m.organization_id) },
        }))?)?;
        w.resource(&RawResource::from_value(
            &m.account(&reads::account(&book.member, &book.provider()), &now),
        )?)?;
        let mut charges: Vec<&Row> = book.of(Kind::Charge).collect();
        charges.sort_by(|a, b| a.service_start().cmp(&b.service_start()).then_with(|| a.id.cmp(&b.id)));
        for r in charges {
            let Some(model) = reads::bill(account, r) else {
                w.skip(Skip::resource(
                    "Invoice",
                    m.bill_id(account, &r.id),
                    "unreadable",
                    "the bill came without a date or an amount",
                ));
                continue;
            };
            if since.is_some_and(|s| model.service_start.as_str() < s) {
                continue;
            }
            w.resource(&RawResource::from_value(&m.bill(&model, &now))?)?;
        }
        let unplaced = book.unidentified.iter().filter(|k| **k == Kind::Charge).count();
        if unplaced > 0 {
            w.skip(Skip::resource(
                "Invoice",
                "",
                "unreadable",
                format!("{unplaced} bill(s) came without an id"),
            ));
        }
        let (mut unsettled, mut undated) = (0, 0);
        let mut payments: Vec<&Row> = book.of(Kind::Payment).collect();
        payments.sort_by(|a, b| a.date.cmp(&b.date).then_with(|| a.id.cmp(&b.id)));
        for r in payments {
            if !r.is_settled() {
                unsettled += 1;
                continue;
            }
            let Some(model) = reads::payment(account, r) else {
                undated += 1;
                continue;
            };
            if since.is_some_and(|s| model.date.as_str() < s) {
                continue;
            }
            w.resource(&RawResource::from_value(&m.payment(&model, 0, |_| false))?)?;
        }
        undated += book.unidentified.iter().filter(|k| **k == Kind::Payment).count();
        skip_count(
            &mut w,
            unsettled,
            "not_settled",
            format!("{unsettled} payment(s) the processor turned down (declined, failed, voided) were left out"),
        );
        skip_count(
            &mut w,
            undated,
            "unreadable",
            format!("{undated} payment(s) came without an id, a date or an amount"),
        );
        for (k, what) in [
            (Kind::Refund, "refund(s)"),
            (Kind::Discount, "discount(s)"),
            (Kind::Credit, "credit(s)"),
            (
                Kind::Other,
                "row(s) of the billing history that move no money (a dispute opened, a balance carried forward) or name no type",
            ),
        ] {
            let n = book.of(k).count() + book.unidentified.iter().filter(|u| **u == k).count();
            skip_count(
                &mut w,
                n,
                skip::UNSUPPORTED,
                format!("{n} {what} have no resource in the bill mapping; `{BIN} payments list` and `{BIN} api` show them"),
            );
        }
        Ok(())
    })();
    w.finish(now_rfc3339(), result.as_ref().err())
}
