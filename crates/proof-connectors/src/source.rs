//! [`RecordSource`]: the industry-agnostic abstraction over "where records
//! to prove come from."
//!
//! A Postgres CDC listener, a webhook receiver, and a file watcher are all
//! `RecordSource`s. Nothing above this trait (proof building, sinks, the
//! UI) knows or cares which one is in play.

use crate::record::{AckToken, SourceRecord};
use std::fmt;

/// A stable identifier for a `RecordSource` instance, used to label
/// metrics and proofs (e.g. `"postgres:patients"`, `"webhook:supplier-3"`).
///
/// A newtype rather than a bare `String` so call sites can't accidentally
/// swap a source ID for some other label (org ID, table name) at a
/// function boundary — the compiler catches that mismatch instead of it
/// surfacing as a mislabeled dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceId(String);

impl SourceId {
    /// Wraps `id` as a `SourceId`.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The underlying identifier string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// An error reported by a [`RecordSource`]. Sources are expected to retry
/// their own transient failures (reconnects, backoff) internally where
/// possible; this variant set covers failures a caller (the service
/// driving the source) needs to react to.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// The source's connection to its upstream system was lost and could
    /// not be re-established. The caller should treat the source as
    /// stopped and is responsible for restart/backoff policy.
    #[error("record source '{source_id}' disconnected: {reason}")]
    Disconnected {
        /// Which source disconnected.
        source_id: SourceId,
        /// Why, if known.
        reason: String,
    },

    /// The source encountered a record it cannot represent as a
    /// [`SourceRecord`] (e.g. undecodable payload) and is skipping it
    /// rather than stalling entirely.
    #[error("record source '{source_id}' could not decode a record: {reason}")]
    UndecodableRecord {
        /// Which source reported the undecodable record.
        source_id: SourceId,
        /// Why decoding failed.
        reason: String,
    },
}

/// A source of records to be proved: watches some external system and
/// yields [`SourceRecord`]s as changes occur there.
///
/// # Contract
///
/// - `next()` yields records in the order the source observed them for a
///   given logical partition (e.g. per-table order for a database), but
///   makes no cross-source ordering guarantee.
/// - Each yielded record carries an [`AckToken`]; the caller must call
///   `ack()` on it only after the record has been successfully submitted
///   to a [`crate::sink::ProofSink`] — see [`AckToken`]'s docs for why.
/// - A source may yield the same record more than once after a restart
///   (at-least-once delivery) if it crashed before receiving an ack for
///   it. Consumers that need exactly-once semantics should de-duplicate
///   on `source_position` downstream; this trait does not guarantee it.
#[async_trait::async_trait]
pub trait RecordSource: Send + Sync {
    /// This source's stable identifier, used for metrics/proof labeling.
    fn source_id(&self) -> &SourceId;

    /// Waits for and returns the next record, or an error if the source
    /// has stopped.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError`] if the source's connection is lost or it
    /// encounters a record it cannot decode.
    async fn next(&mut self) -> Result<(SourceRecord, Box<dyn AckToken>), SourceError>;
}
