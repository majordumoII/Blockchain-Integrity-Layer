//! Types shared by every [`crate::source::RecordSource`] implementation,
//! regardless of which enterprise system it's watching.

use std::fmt;

/// A single change observed at a data source, ready to be hashed and
/// signed by `proof-core`.
///
/// Deliberately holds only bytes and opaque string metadata — this crate
/// (and everything downstream of it) must stay ignorant of any particular
/// industry's schema. A Postgres row, a supply-chain handoff event, and a
/// loan decision record all arrive as the same shape.
#[derive(Debug, Clone)]
pub struct SourceRecord {
    /// The bytes to be hashed. For a database row this is typically a
    /// canonical serialization of the changed columns; for an event this
    /// is the event payload.
    pub bytes: Vec<u8>,
    /// Caller-defined key-value metadata (table name, record ID, industry
    /// tag, org ID, ...), carried through to the eventual `Proof`'s
    /// metadata so a verifier or dashboard can filter/label without
    /// decoding `bytes`.
    pub metadata: Vec<(String, String)>,
    /// Opaque token identifying this record's position in its source
    /// (e.g. a Postgres LSN, a Kafka offset). Not interpreted by this
    /// crate — only round-tripped back to the source via [`AckToken`] once
    /// the record has been durably proved.
    pub source_position: String,
}

impl SourceRecord {
    /// Looks up a metadata value by key, if present.
    #[must_use]
    pub fn metadata_value(&self, key: &str) -> Option<&str> {
        self.metadata
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Acknowledges that a [`SourceRecord`] has been durably proved (submitted
/// to a [`crate::sink::ProofSink`] and accepted), so the source may safely
/// advance past it.
///
/// Kept as a trait rather than a concrete type so each `RecordSource`
/// implementation can commit its own notion of "processed" — advancing a
/// replication slot, committing a consumer offset, deleting a queue
/// message — without this crate knowing which.
///
/// # Why acknowledgment exists at all
///
/// A crash between "hashed and signed" and "source position advanced"
/// must not silently drop the record (source advances without proving)
/// nor silently duplicate it forever (source never advances, replays
/// endlessly). Requiring an explicit ack call after a successful
/// [`crate::sink::ProofSink::submit`] makes "prove, then acknowledge" the
/// only path through the API — there is no way to advance a source without
/// having already produced a proof for what it's advancing past.
#[async_trait::async_trait]
pub trait AckToken: fmt::Debug + Send + Sync {
    /// Commits this record as processed at its source.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying source cannot durably record the
    /// acknowledgment (e.g. the replication connection dropped). Callers
    /// should treat a failed ack as "the record may be re-delivered" rather
    /// than retry the ack in a loop themselves — the source's reconnect
    /// logic owns that.
    async fn ack(self: Box<Self>) -> Result<(), AckError>;
}

/// An acknowledgment failure. Deliberately minimal — sources report their
/// own errors via [`crate::source::SourceError`]; this only covers the
/// ack step itself failing after a record was otherwise successfully
/// proved.
#[derive(Debug, thiserror::Error)]
#[error("failed to acknowledge record at source: {0}")]
pub struct AckError(pub String);
