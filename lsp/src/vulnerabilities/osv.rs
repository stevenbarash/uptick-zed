//! OSV (Open Source Vulnerabilities) HTTP client. Queries
//! `https://api.osv.dev/v1/query` to learn whether a concrete
//! (ecosystem, name, version) tuple has known vulnerabilities.

use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::Vulnerability;

const OSV_QUERY_URL: &str = "https://api.osv.dev/v1/query";

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

impl OsvQueryResponse {
    fn into_vulnerabilities(self) -> Vec<Vulnerability> {
        self.vulns
            .into_iter()
            .map(|raw| Vulnerability {
                id: raw.id,
                modified: raw.modified,
                summary: raw.summary,
                details: raw.details,
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
        return Err(anyhow!(
            "osv.dev {ecosystem}/{name}@{version}: {status}"
        ));
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
        assert_eq!(r.vulns[0].summary.as_deref(), Some("Prototype Pollution in lodash"));
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
        assert_eq!(v[0].summary.as_deref(), Some("Prototype Pollution in lodash"));
    }
}
