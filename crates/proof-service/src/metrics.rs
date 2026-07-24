//! Prometheus metric definitions and recording helpers.
//!
//! Metrics are labeled by `source_id` (e.g. `"postgres:patients"`) so a
//! Grafana dashboard can facet proof activity by data source/industry —
//! the actual goal being real signal per connector, not one undifferentiated
//! global counter.

use metrics::{counter, histogram};
use metrics_exporter_prometheus::PrometheusHandle;
use proof_connectors::SourceId;
use std::time::Instant;

/// Installs the Prometheus recorder and starts serving `/metrics` on
/// `addr`, returning a [`PrometheusHandle`] the caller can use to render
/// current metric values in-process (e.g. for the compliance dashboard's
/// aggregate coverage view) without a second HTTP round trip to
/// `/metrics`. Must be called once, before any of the `record_*`
/// functions in this module are used.
///
/// # Panics
///
/// Panics if a metrics recorder has already been installed globally, or
/// if binding the metrics HTTP listener fails.
#[must_use]
pub fn install(addr: std::net::SocketAddr) -> PrometheusHandle {
    metrics_exporter_prometheus::PrometheusBuilder::new()
        .with_http_listener(addr)
        .install_recorder()
        .expect("failed to install Prometheus recorder/listener")
}

/// Records that a record was observed from a source, before any
/// hashing/signing has happened.
pub fn record_observed(source_id: &SourceId, table: &str) {
    counter!(
        "bil_records_observed_total",
        "source_id" => source_id.to_string(),
        "table" => table.to_string()
    )
    .increment(1);
}

/// Records that a proof was successfully built and submitted for a
/// record, and how long the hash+sign+build+submit pipeline took.
pub fn record_proved(source_id: &SourceId, table: &str, started_at: Instant) {
    counter!(
        "bil_proofs_generated_total",
        "source_id" => source_id.to_string(),
        "table" => table.to_string()
    )
    .increment(1);
    histogram!(
        "bil_proof_pipeline_seconds",
        "source_id" => source_id.to_string(),
        "table" => table.to_string()
    )
    .record(started_at.elapsed().as_secs_f64());
}

/// Records that processing a record failed (source error, sink error, or
/// ack failure) and did not result in a proof.
pub fn record_failed(source_id: &SourceId, stage: &'static str) {
    counter!(
        "bil_records_failed_total",
        "source_id" => source_id.to_string(),
        "stage" => stage
    )
    .increment(1);
}
