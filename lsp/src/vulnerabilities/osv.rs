//! OSV (Open Source Vulnerabilities) HTTP client. Queries
//! `https://api.osv.dev/v1/query` to learn whether a concrete
//! (ecosystem, name, version) tuple has known vulnerabilities.

use std::str::FromStr;

use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::Vulnerability;

const OSV_QUERY_URL: &str = "https://api.osv.dev/v1/query";
const OSV_VULN_URL_PREFIX: &str = "https://api.osv.dev/v1/vulns/";

#[derive(Serialize)]
struct OsvQueryRequest<'a> {
    package: OsvPackage<'a>,
    version: &'a str,
}

#[derive(Serialize)]
struct OsvPackage<'a> {
    name: &'a str,
    ecosystem: &'a str,
}

#[derive(Deserialize, Debug)]
struct OsvQueryResponse {
    #[serde(default)]
    vulns: Vec<OsvVulnRaw>,
}

#[derive(Deserialize, Debug)]
struct OsvVulnRaw {
    id: String,
    modified: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    details: Option<String>,
}

// --- Detail response types ---

#[derive(Deserialize, Debug)]
struct OsvDetailResponse {
    #[serde(default)]
    severity: Vec<OsvSeverityEntry>,
    #[serde(default)]
    database_specific: Option<OsvDetailDbSpecific>,
}

#[derive(Deserialize, Debug)]
struct OsvSeverityEntry {
    #[serde(rename = "type")]
    type_: SeverityType,
    score: String,
}

/// OSV `severity[].type` discriminant. We only parse CVSS_V3 (cvss 2.x
/// crate covers v3 and v4 vectors but the OSV-published v4 entries are
/// rare; v2 entries fall through to text-bucket fallback).
#[derive(Deserialize, Debug, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum SeverityType {
    CvssV3,
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Debug)]
struct OsvDetailDbSpecific {
    #[serde(default)]
    severity: Option<String>,
}

/// Map GHSA-style severity text to a CVSS-aligned numeric midpoint.
fn text_bucket_score(label: &str) -> Option<f32> {
    match label.to_ascii_uppercase().as_str() {
        "CRITICAL" => Some(9.5),
        "HIGH" => Some(8.0),
        "MEDIUM" | "MODERATE" => Some(5.5),
        "LOW" => Some(2.5),
        _ => None,
    }
}

fn cvss_entry_score(entry: &OsvSeverityEntry) -> Option<f32> {
    match entry.type_ {
        SeverityType::CvssV3 => cvss::v3::Base::from_str(&entry.score)
            .ok()
            .map(|b| b.score().value() as f32),
        SeverityType::Other => None,
    }
}

fn extract_score(detail: &OsvDetailResponse) -> Option<f32> {
    detail
        .severity
        .iter()
        .find(|e| e.type_ == SeverityType::CvssV3)
        .and_then(cvss_entry_score)
        .or_else(|| {
            detail
                .database_specific
                .as_ref()
                .and_then(|d| d.severity.as_deref())
                .and_then(text_bucket_score)
        })
}

/// Test-friendly wrapper: parse a JSON detail response and extract a score.
#[cfg(test)]
fn parse_detail_score(json: &str) -> Option<f32> {
    let detail: OsvDetailResponse = serde_json::from_str(json).ok()?;
    extract_score(&detail)
}

/// Fetch full detail for one OSV ID and extract its CVSS base score.
/// Returns `Ok(None)` if the advisory has no parseable severity.
pub async fn query_detail(client: &Client, id: &str) -> Result<Option<f32>> {
    let url = format!("{OSV_VULN_URL_PREFIX}{id}");
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("osv.dev detail request for {id}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow!("osv.dev detail {id}: {status}"));
    }
    let detail: OsvDetailResponse = resp
        .json()
        .await
        .with_context(|| format!("osv.dev detail response for {id}"))?;
    Ok(extract_score(&detail))
}

impl OsvQueryResponse {
    fn into_vulnerabilities(self) -> Vec<Vulnerability> {
        self.vulns
            .into_iter()
            .map(|raw| Vulnerability {
                id: raw.id,
                modified: raw.modified,
                summary: raw.summary,
                details: raw.details,
                score: None,
            })
            .collect()
    }
}

/// Query OSV for vulnerabilities affecting `(ecosystem, name, version)`.
///
/// Returns an empty vec if OSV reports no vulns. Returns `Err` only on
/// transport errors or non-2xx HTTP responses — callers treat those as
/// cache misses, not as "clean".
pub async fn query(
    client: &Client,
    ecosystem: &str,
    name: &str,
    version: &str,
) -> Result<Vec<Vulnerability>> {
    let body = OsvQueryRequest {
        package: OsvPackage { name, ecosystem },
        version,
    };
    let resp = client
        .post(OSV_QUERY_URL)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("osv.dev request for {ecosystem}/{name}@{version}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow!("osv.dev {ecosystem}/{name}@{version}: {status}"));
    }
    let parsed: OsvQueryResponse = resp
        .json()
        .await
        .with_context(|| format!("osv.dev response for {ecosystem}/{name}@{version}"))?;
    Ok(parsed.into_vulnerabilities())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Two-vuln response as returned by OSV for a known-vulnerable package.
    const TWO_VULNS: &str = r#"{
        "vulns": [
            {
                "id": "GHSA-jf85-cpcp-j695",
                "modified": "2021-05-06T00:00:00Z",
                "summary": "Prototype Pollution in lodash",
                "details": "Versions of lodash prior to 4.17.12 ..."
            },
            {
                "id": "GHSA-p6mc-m468-83gw",
                "modified": "2020-08-14T00:00:00Z",
                "summary": "Prototype Pollution in lodash",
                "details": null
            }
        ]
    }"#;

    #[test]
    fn deserializes_two_vulns() {
        let r: OsvQueryResponse = serde_json::from_str(TWO_VULNS).unwrap();
        assert_eq!(r.vulns.len(), 2);
        assert_eq!(r.vulns[0].id, "GHSA-jf85-cpcp-j695");
        assert_eq!(
            r.vulns[0].summary.as_deref(),
            Some("Prototype Pollution in lodash")
        );
        assert!(r.vulns[0].details.is_some());
        assert_eq!(r.vulns[1].details, None);
    }

    #[test]
    fn deserializes_empty_vulns() {
        let r: OsvQueryResponse = serde_json::from_str(r#"{"vulns": []}"#).unwrap();
        assert!(r.vulns.is_empty());
    }

    #[test]
    fn deserializes_missing_vulns_field() {
        // OSV sometimes returns `{}` for clean queries. Must default to empty.
        let r: OsvQueryResponse = serde_json::from_str("{}").unwrap();
        assert!(r.vulns.is_empty());
    }

    #[test]
    fn into_vulnerabilities_maps_fields() {
        let r: OsvQueryResponse = serde_json::from_str(TWO_VULNS).unwrap();
        let v = r.into_vulnerabilities();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].id, "GHSA-jf85-cpcp-j695");
        assert_eq!(v[0].modified, "2021-05-06T00:00:00Z");
        assert_eq!(
            v[0].summary.as_deref(),
            Some("Prototype Pollution in lodash")
        );
    }

    const DETAIL_GHSA: &str = r#"{
        "id": "GHSA-jf85-cpcp-j695",
        "modified": "2021-05-06T00:00:00Z",
        "severity": [
            {
                "type": "CVSS_V3",
                "score": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:H/A:H"
            }
        ],
        "database_specific": { "severity": "CRITICAL" }
    }"#;

    const DETAIL_RUSTSEC: &str = r#"{
        "id": "RUSTSEC-2020-0071",
        "modified": "2024-01-01T00:00:00Z",
        "severity": [
            { "type": "CVSS_V3", "score": "CVSS:3.1/AV:L/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:H" }
        ]
    }"#;

    const DETAIL_PYSEC_NO_SEV: &str = r#"{
        "id": "PYSEC-2021-130",
        "modified": "2024-01-01T00:00:00Z",
        "severity": []
    }"#;

    const DETAIL_TEXT_ONLY: &str = r#"{
        "id": "GHSA-fake",
        "modified": "2024-01-01T00:00:00Z",
        "severity": [{ "type": "CVSS_V3", "score": "not-a-vector" }],
        "database_specific": { "severity": "HIGH" }
    }"#;

    const DETAIL_BARE_TEXT: &str = r#"{
        "id": "GHSA-bare",
        "modified": "2024-01-01T00:00:00Z",
        "database_specific": { "severity": "MODERATE" }
    }"#;

    #[test]
    fn detail_parses_ghsa_cvss_v3() {
        let s = parse_detail_score(DETAIL_GHSA).expect("score");
        // CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:H/A:H
        // cvss crate computes 9.1 (CRITICAL) per CVSS 3.1 spec formula.
        // (Plan said 7.5 — that value is incorrect per spec.)
        assert!((s - 9.1).abs() < 0.1, "expected ~9.1, got {s}");
    }

    #[test]
    fn detail_parses_rustsec_cvss_v3_low() {
        // CVSS:3.1/AV:L/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:H
        // cvss crate computes 6.2 (MEDIUM) per CVSS 3.1 spec formula.
        // (Plan said 6.5 — widened tolerance to ±0.1 to account for rounding.)
        let s = parse_detail_score(DETAIL_RUSTSEC).expect("score");
        assert!((s - 6.2).abs() < 0.1, "expected ~6.2, got {s}");
    }

    #[test]
    fn detail_no_severity_returns_none() {
        assert_eq!(parse_detail_score(DETAIL_PYSEC_NO_SEV), None);
    }

    #[test]
    fn detail_falls_back_to_text_bucket() {
        // Vector unparseable → fall back to "HIGH" → 8.0
        let s = parse_detail_score(DETAIL_TEXT_ONLY).expect("score");
        assert!((s - 8.0).abs() < 0.05, "expected 8.0, got {s}");
    }

    #[test]
    fn detail_uses_text_when_no_severity_array() {
        let s = parse_detail_score(DETAIL_BARE_TEXT).expect("score");
        assert!((s - 5.5).abs() < 0.05, "expected 5.5, got {s}");
    }

    #[test]
    fn text_bucket_to_score_table() {
        assert_eq!(text_bucket_score("CRITICAL"), Some(9.5));
        assert_eq!(text_bucket_score("HIGH"), Some(8.0));
        assert_eq!(text_bucket_score("MEDIUM"), Some(5.5));
        assert_eq!(text_bucket_score("MODERATE"), Some(5.5));
        assert_eq!(text_bucket_score("LOW"), Some(2.5));
        assert_eq!(text_bucket_score("UNKNOWN"), None);
        assert_eq!(text_bucket_score(""), None);
    }
}
