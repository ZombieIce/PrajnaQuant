use super::{Result, classify, hash};
use chrono::NaiveDate;
use regex::Regex;
use serde_json::{Value, json};
use std::{collections::BTreeMap, io::Write, process::Command};

pub const EXTRACTOR_VERSION: &str = "cninfo_pdf_fields_v1";

pub trait PdfText {
    fn text(&mut self, pdf: &[u8]) -> Result<String>;
}

/// Poppler is a local optional runtime dependency. Scanned/encrypted PDFs or
/// unavailable converters yield unknown fields; no OCR or date guessing.
pub struct Poppler;
impl PdfText for Poppler {
    fn text(&mut self, pdf: &[u8]) -> Result<String> {
        if !pdf.starts_with(b"%PDF-") {
            return Err("not a PDF".into());
        }
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(pdf)?;
        let output = Command::new("pdftotext")
            .args(["-layout", "-enc", "UTF-8"])
            .arg(file.path())
            .arg("-")
            .output()?;
        if !output.status.success() {
            return Err("PDF text conversion failed".into());
        }
        Ok(String::from_utf8(output.stdout)?)
    }
}

fn unique_capture(text: &str, pattern: &str) -> Option<String> {
    let re = Regex::new(pattern).expect("static extraction regex");
    let values: std::collections::BTreeSet<_> =
        re.captures_iter(text).map(|c| c[1].to_owned()).collect();
    (values.len() == 1).then(|| values.into_iter().next().unwrap())
}

fn date(text: &str, label: &str) -> Option<String> {
    let pattern =
        format!(r"(?:{label})[：:]?(\d{{4}}年\d{{1,2}}月\d{{1,2}}日|\d{{4}}-\d{{2}}-\d{{2}})");
    let value = unique_capture(text, &pattern)?;
    NaiveDate::parse_from_str(&value, "%Y年%m月%d日")
        .or_else(|_| value.parse())
        .ok()
        .map(|d| d.to_string())
}

/// Only unambiguous, labelled facts are accepted. `corporateActions` retains the
/// existing v1 keys; incomplete facts remain null and cannot enter the ledger.
/// Every non-null extracted field has the original PDF SHA in `fieldSources`.
pub fn extract_fields(title: &str, pdf: &[u8], converter: &mut impl PdfText) -> Value {
    let categories = classify(title);
    let text = pdf
        .starts_with(b"%PDF-")
        .then(|| converter.text(pdf).ok())
        .flatten()
        .map(|t| t.chars().filter(|c| !c.is_whitespace()).collect::<String>());
    let text = text.as_deref().unwrap_or("");
    let mut fields = BTreeMap::<String, Value>::new();
    if categories.iter().any(|c| c == "dividend") {
        fields.insert(
            "payDate".into(),
            json!(date(text, "现金红利发放日|红利发放日")),
        );
    }
    if categories.iter().any(|c| c == "liquidation") {
        fields.insert(
            "payDate".into(),
            json!(date(text, "剩余财产分配日|剩余财产发放日")),
        );
        fields.insert(
            "liquidationCashPerShare".into(),
            json!(unique_capture(
                text,
                r"每份基金份额(?:可获|分配|剩余财产分配金额为)[：:]?([0-9]+(?:\.[0-9]+)?)元"
            )),
        );
    }
    if categories
        .iter()
        .any(|c| c == "suspension" || c == "resumption")
    {
        fields.insert(
            "suspensionStart".into(),
            json!(date(text, "停牌起始日|停牌日期|停牌时间")),
        );
        fields.insert(
            "resumptionDate".into(),
            json!(date(text, "复牌日期|复牌时间")),
        );
    }
    let pdf_hash = hash(pdf);
    let sources: BTreeMap<_, _> = fields
        .iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, _)| (k.clone(), pdf_hash.clone()))
        .collect();
    let unknown: Vec<_> = fields
        .iter()
        .filter(|(_, v)| v.is_null())
        .map(|(k, _)| k.clone())
        .collect();
    let mut actions = Vec::new();
    if categories.iter().any(|c| c == "dividend") {
        actions.push(json!({"type":"dividend", "recordDate":null, "exDate":null, "payDate":fields.get("payDate"), "cashPerShare":null}));
    }
    if categories.iter().any(|c| c == "liquidation") {
        actions.push(json!({"type":"liquidation", "effectiveDate":null, "payDate":fields.get("payDate"), "liquidationCashPerShare":fields.get("liquidationCashPerShare")}));
    }
    json!({"schema":"cninfo_corporateActions_structured_v1", "extractorVersion":EXTRACTOR_VERSION, "pdfRawSha256":pdf_hash, "corporateActions":actions, "fields":fields, "fieldSources":sources, "unknown":unknown})
}
