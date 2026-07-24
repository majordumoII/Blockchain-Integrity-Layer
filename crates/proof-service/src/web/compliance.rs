//! The compliance dashboard: `GET /compliance` (aggregate per-source
//! coverage) and `GET /compliance/{digest}` (a single proof's full audit
//! trail, with an inline re-verify action) — the README's "pre-built
//! views for auditors" feature, built as real routes over data this
//! service already produces, not new data.
//!
//! Two views, matching two different questions an auditor actually asks:
//! - **Per-record**: "show me everything about this one proof, and prove
//!   right now that it's still valid" — a human-clickable front end over
//!   the same `GET /api/v1/proofs/{digest}`/`POST /api/v1/verify` API
//!   `bil-client` already uses programmatically, so this page never
//!   duplicates verification logic, it only renders it.
//! - **Aggregate**: "has anything been silently dropped across all our
//!   sources?" — reads current values straight out of this process's own
//!   `PrometheusHandle` (the same counters `/metrics` and Grafana already
//!   scrape), rather than a second bookkeeping mechanism that could drift
//!   out of sync with what Grafana shows.

use super::AppState;
use super::api::verify;
use askama::Template;
use axum::extract::{Path, State};
use axum::response::Html;
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone)]
struct SourceCoverage {
    observed: u64,
    proved: u64,
    failed: u64,
}

impl SourceCoverage {
    /// Percentage of observed records that reached a proof, `0.0` if
    /// nothing has been observed yet (rather than dividing by zero).
    ///
    /// `as f64` precision loss is not a real concern here: a source
    /// would need to observe more than 2^52 records in one process
    /// lifetime before this ratio could actually be affected.
    #[allow(clippy::cast_precision_loss)]
    fn coverage_percent(&self) -> f64 {
        if self.observed == 0 {
            0.0
        } else {
            100.0 * self.proved as f64 / self.observed as f64
        }
    }
}

/// Parses this crate's own three counters
/// (`bil_records_observed_total`/`bil_proofs_generated_total`/`bil_records_failed_total`)
/// out of a Prometheus text-exposition string, grouped by `source_id`
/// label.
///
/// Deliberately narrow: this is not a general Prometheus text-format
/// parser (no histogram/summary handling, no escaping edge cases beyond
/// what this crate's own `metrics.rs` ever emits) — it only understands
/// the exact three counters this service defines, matched by name and
/// picking the `source_id="..."` label back out of the metric line
/// verbatim. If `metrics.rs` ever adds a differently-shaped metric, this
/// function has nothing to say about it; it silently ignores lines it
/// doesn't recognize rather than erroring, since a dashboard rendering
/// stale-but-present data is a better failure mode than a 500 page for an
/// auditor mid-review.
fn parse_source_coverage(prometheus_text: &str) -> BTreeMap<String, SourceCoverage> {
    let mut by_source: BTreeMap<String, SourceCoverage> = BTreeMap::new();

    for line in prometheus_text.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let Some((metric_and_labels, value)) = line.rsplit_once(' ') else {
            continue;
        };
        let Ok(value) = value.parse::<f64>() else {
            continue;
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let value = value.max(0.0) as u64;

        let Some(metric_name) = metric_and_labels.split('{').next() else {
            continue;
        };
        let Some(source_id) = extract_label(metric_and_labels, "source_id") else {
            continue;
        };

        let entry = by_source.entry(source_id).or_default();
        match metric_name {
            "bil_records_observed_total" => entry.observed += value,
            "bil_proofs_generated_total" => entry.proved += value,
            "bil_records_failed_total" => entry.failed += value,
            _ => {}
        }
    }

    by_source
}

/// Pulls a `label="value"` pair's value out of a Prometheus metric line's
/// `{...}` label section. Returns `None` if the metric has no labels at
/// all (a bare counter name with no `{`) or the named label isn't
/// present — both are treated the same as "doesn't belong to a source",
/// since every counter this dashboard reads is always emitted with a
/// `source_id` label by `metrics.rs`.
fn extract_label(metric_and_labels: &str, label: &str) -> Option<String> {
    let labels_start = metric_and_labels.find('{')?;
    let labels_end = metric_and_labels.rfind('}')?;
    let labels = &metric_and_labels[labels_start + 1..labels_end];

    for pair in labels.split(',') {
        let (key, value) = pair.split_once('=')?;
        if key == label {
            return Some(value.trim_matches('"').to_string());
        }
    }
    None
}

#[derive(Template)]
#[template(path = "compliance_index.html")]
struct ComplianceIndexTemplate {
    sources: Vec<SourceRow>,
}

struct SourceRow {
    source_id: String,
    observed: u64,
    proved: u64,
    failed: u64,
    coverage_percent: String,
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // exact literal percentages from integer division, not accumulated float math
mod tests {
    use super::*;

    /// A trimmed but realistic snapshot of what this crate's own
    /// `/metrics` endpoint actually emits (matching the shape observed
    /// against a real running `proof-service` in earlier verification
    /// passes), covering two sources, a failure count, and a metric this
    /// module doesn't track (the pipeline-latency histogram) to confirm
    /// unrecognized metrics are silently skipped rather than erroring.
    const SAMPLE_METRICS_TEXT: &str = "\
# TYPE bil_records_observed_total counter
bil_records_observed_total{source_id=\"postgres:default\",table=\"patients\"} 12
bil_records_observed_total{source_id=\"gcs:corporate-raw-docs\",table=\"unknown\"} 3
# TYPE bil_proofs_generated_total counter
bil_proofs_generated_total{source_id=\"postgres:default\",table=\"patients\"} 10
bil_proofs_generated_total{source_id=\"gcs:corporate-raw-docs\",table=\"unknown\"} 3
# TYPE bil_records_failed_total counter
bil_records_failed_total{source_id=\"postgres:default\",stage=\"anchor\"} 2
# TYPE bil_proof_pipeline_seconds summary
bil_proof_pipeline_seconds{source_id=\"postgres:default\",table=\"patients\",quantile=\"0.5\"} 21.7
bil_proof_pipeline_seconds_sum{source_id=\"postgres:default\",table=\"patients\"} 217.0
";

    #[test]
    fn parses_multiple_sources_from_realistic_metrics_text() {
        let by_source = parse_source_coverage(SAMPLE_METRICS_TEXT);

        assert_eq!(by_source.len(), 2);

        let postgres = &by_source["postgres:default"];
        assert_eq!(postgres.observed, 12);
        assert_eq!(postgres.proved, 10);
        assert_eq!(postgres.failed, 2);

        let gcs = &by_source["gcs:corporate-raw-docs"];
        assert_eq!(gcs.observed, 3);
        assert_eq!(gcs.proved, 3);
        assert_eq!(gcs.failed, 0);
    }

    #[test]
    fn sums_multiple_lines_for_the_same_source_and_metric() {
        // Two tables under the same source_id should accumulate, not
        // overwrite each other.
        let text = "\
bil_records_observed_total{source_id=\"postgres:default\",table=\"patients\"} 5
bil_records_observed_total{source_id=\"postgres:default\",table=\"orders\"} 7
";
        let by_source = parse_source_coverage(text);
        assert_eq!(by_source["postgres:default"].observed, 12);
    }

    #[test]
    fn empty_metrics_text_yields_no_sources() {
        assert!(parse_source_coverage("").is_empty());
    }

    #[test]
    fn coverage_percent_handles_zero_observed_without_dividing_by_zero() {
        let coverage = SourceCoverage::default();
        assert_eq!(coverage.coverage_percent(), 0.0);
    }

    #[test]
    fn coverage_percent_is_full_when_everything_observed_was_proved() {
        let coverage = SourceCoverage {
            observed: 10,
            proved: 10,
            failed: 0,
        };
        assert_eq!(coverage.coverage_percent(), 100.0);
    }

    #[test]
    fn coverage_percent_reflects_partial_coverage() {
        let coverage = SourceCoverage {
            observed: 4,
            proved: 1,
            failed: 3,
        };
        assert_eq!(coverage.coverage_percent(), 25.0);
    }

    #[test]
    fn extract_label_finds_the_named_label_among_several() {
        let value = extract_label(
            r#"bil_records_observed_total{source_id="postgres:default",table="patients"}"#,
            "source_id",
        );
        assert_eq!(value.as_deref(), Some("postgres:default"));
    }

    #[test]
    fn extract_label_returns_none_for_a_bare_metric_with_no_labels() {
        assert_eq!(extract_label("some_bare_counter", "source_id"), None);
    }

    #[test]
    fn extract_label_returns_none_when_label_is_absent() {
        let value = extract_label(
            r#"bil_records_observed_total{table="patients"}"#,
            "source_id",
        );
        assert_eq!(value, None);
    }
}

/// `GET /compliance` — aggregate per-source coverage, read from this
/// process's own live Prometheus counters.
pub async fn compliance_index(State(state): State<AppState>) -> Html<String> {
    let rendered_text = state.metrics_handle.render();
    let by_source = parse_source_coverage(&rendered_text);

    let sources = by_source
        .into_iter()
        .map(|(source_id, coverage)| SourceRow {
            source_id,
            observed: coverage.observed,
            proved: coverage.proved,
            failed: coverage.failed,
            coverage_percent: format!("{:.1}", coverage.coverage_percent()),
        })
        .collect();

    let page = ComplianceIndexTemplate { sources };
    Html(
        page.render()
            .unwrap_or_else(|e| format!("template error: {e}")),
    )
}

#[derive(Template)]
#[template(path = "compliance_record.html")]
struct ComplianceRecordTemplate {
    digest_hex: String,
    found: bool,
    source_id: String,
    source_position: String,
    table: String,
    change_kind: String,
    attestation_count: usize,
    timestamp: String,
    ledger_id: String,
    position_hex: String,
}

/// `GET /compliance/{digest}` — one proof's full audit trail: source
/// context, signer/timestamp, and anchor receipt. Renders "not found" for
/// a digest outside this service's retained history, rather than
/// erroring, since the page itself (and its Verify button) still needs to
/// render sensibly for that case.
pub async fn compliance_record(
    State(state): State<AppState>,
    Path(digest): Path<String>,
) -> Html<String> {
    let (history, _live) = state.sink.subscribe();
    let record = history
        .into_iter()
        .find(|r| r.proof.digest().to_hex() == digest);

    let page = match record {
        Some(record) => {
            let table = record
                .proof
                .metadata()
                .iter()
                .find(|(k, _)| k == "table")
                .map_or("unknown", |(_, v)| v.as_str())
                .to_string();
            let change_kind = record
                .proof
                .metadata()
                .iter()
                .find(|(k, _)| k == "change_kind")
                .map_or("unknown", |(_, v)| v.as_str())
                .to_string();

            ComplianceRecordTemplate {
                digest_hex: digest,
                found: true,
                source_id: record.source_id.to_string(),
                source_position: record.source_position,
                table,
                change_kind,
                attestation_count: record.proof.attestations().len(),
                timestamp: record.proof.timestamp_unix().to_string(),
                ledger_id: record.receipt.ledger_id,
                position_hex: record.receipt.position_hex,
            }
        }
        None => ComplianceRecordTemplate {
            digest_hex: digest,
            found: false,
            source_id: String::new(),
            source_position: String::new(),
            table: String::new(),
            change_kind: String::new(),
            attestation_count: 0,
            timestamp: String::new(),
            ledger_id: String::new(),
            position_hex: String::new(),
        },
    };

    Html(
        page.render()
            .unwrap_or_else(|e| format!("template error: {e}")),
    )
}

#[derive(Template)]
#[template(path = "verify_result.html")]
struct VerifyResultTemplate {
    valid: bool,
    digest_hex: String,
    reason: Option<String>,
}

/// `POST /compliance/{digest}/verify` — the dashboard's "Verify" button
/// target: looks the proof up the same way `compliance_record` does, then
/// calls the exact same verification path `POST /api/v1/verify` uses
/// (`Proof::verify_attestations()` + `ProofAnchor::verify()`), rendering
/// an HTML fragment (for htmx to swap in) rather than the JSON API's
/// response — this endpoint exists so a person can click a button, not so
/// a program can call it; `bil-client`/`POST /api/v1/verify` remain the
/// integration surface for programmatic callers.
pub async fn compliance_verify(
    State(state): State<AppState>,
    Path(digest): Path<String>,
) -> Html<String> {
    let (history, _live) = state.sink.subscribe();
    let Some(record) = history
        .into_iter()
        .find(|r| r.proof.digest().to_hex() == digest)
    else {
        let result = VerifyResultTemplate {
            valid: false,
            digest_hex: digest,
            reason: Some("no proof with this digest in retained history".to_string()),
        };
        return Html(
            result
                .render()
                .unwrap_or_else(|e| format!("template error: {e}")),
        );
    };

    let result = verify(&state.anchor, &record.proof, &record.receipt).await;

    let template = VerifyResultTemplate {
        valid: result.valid,
        digest_hex: result.digest_hex,
        reason: result.reason,
    };
    Html(
        template
            .render()
            .unwrap_or_else(|e| format!("template error: {e}")),
    )
}
