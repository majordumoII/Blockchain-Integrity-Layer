//! [`proof_connectors::RecordSource`] implementation backed by a
//! [`crate::replication::ReplicationStream`].
//!
//! `Begin`/`Commit`/`Relation`/`Truncate` messages are consumed internally
//! (they carry no row change to hash, or in `Relation`'s case exist only
//! to inform later decoding) — only `Insert`/`Update`/`Delete` are
//! surfaced to the caller as [`proof_connectors::SourceRecord`]s.

use crate::change::{ChangeKind, RowChange, columns_from_tuple};
use crate::connection::RawConnection;
use crate::pgoutput::PgOutputMessage;
use crate::replication::{ReplicationError, ReplicationStream};
use proof_connectors::{AckError, AckToken, SourceError, SourceId, SourceRecord};
use std::sync::Arc;
use tokio::sync::Mutex;

/// A [`proof_connectors::RecordSource`] that watches one Postgres logical
/// replication slot/publication and yields each row-level `Insert`,
/// `Update`, or `Delete` as a [`SourceRecord`].
pub struct PostgresSource {
    source_id: SourceId,
    stream: Arc<Mutex<ReplicationStream>>,
    /// Commit timestamp of the transaction currently being processed,
    /// captured from the most recent `Begin` message. `pgoutput` puts the
    /// timestamp on `Begin`/`Commit`, not on individual row messages, so
    /// this is threaded through rather than being available on each
    /// change directly.
    current_commit_timestamp_unix_micros: i64,
}

impl PostgresSource {
    /// Connects to Postgres and starts logical replication for
    /// `slot_name`/`publication_names`, resuming from `start_lsn` (`0` to
    /// let the server replay from the slot's own confirmed position).
    ///
    /// `source_id` labels every [`SourceRecord`] and metric this source
    /// produces — e.g. `"postgres:patients"`.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError`] if the connection or `START_REPLICATION`
    /// fails.
    pub async fn connect(
        source_id: SourceId,
        conn: RawConnection,
        slot_name: &str,
        publication_names: &str,
        start_lsn: u64,
    ) -> Result<Self, SourceError> {
        let stream = ReplicationStream::start(conn, slot_name, publication_names, start_lsn)
            .await
            .map_err(|e| SourceError::Disconnected {
                source_id: source_id.clone(),
                reason: e.to_string(),
            })?;
        Ok(Self {
            source_id,
            stream: Arc::new(Mutex::new(stream)),
            current_commit_timestamp_unix_micros: 0,
        })
    }
}

#[async_trait::async_trait]
impl proof_connectors::RecordSource for PostgresSource {
    fn source_id(&self) -> &SourceId {
        &self.source_id
    }

    async fn next(&mut self) -> Result<(SourceRecord, Box<dyn AckToken>), SourceError> {
        loop {
            let event = {
                let mut stream = self.stream.lock().await;
                stream.next_event().await.map_err(|e| match e {
                    ReplicationError::Connection(conn_err) => SourceError::Disconnected {
                        source_id: self.source_id.clone(),
                        reason: conn_err.to_string(),
                    },
                    ReplicationError::Decode(decode_err) => SourceError::UndecodableRecord {
                        source_id: self.source_id.clone(),
                        reason: decode_err.to_string(),
                    },
                })?
            };

            let (kind, relation_id, tuple) = match &event.message {
                PgOutputMessage::Begin {
                    commit_timestamp_micros,
                    ..
                } => {
                    self.current_commit_timestamp_unix_micros = *commit_timestamp_micros;
                    continue;
                }
                PgOutputMessage::Insert {
                    relation_id,
                    new_tuple,
                } => (ChangeKind::Insert, *relation_id, new_tuple),
                PgOutputMessage::Update {
                    relation_id,
                    new_tuple,
                    ..
                } => (ChangeKind::Update, *relation_id, new_tuple),
                PgOutputMessage::Delete {
                    relation_id,
                    old_tuple,
                } => (ChangeKind::Delete, *relation_id, old_tuple),
                // Commit, Relation, Truncate, and unrecognized messages
                // carry no row change of their own to surface.
                _ => continue,
            };

            let (schema, table, columns_info) = {
                let stream = self.stream.lock().await;
                let Some((schema, table, columns_info)) = stream.relation(relation_id) else {
                    // A change referencing a relation we haven't seen a
                    // Relation message for yet would be a server protocol
                    // violation (Relation always precedes changes that use
                    // it); surface it as undecodable rather than panicking,
                    // since this observation comes from network input.
                    return Err(SourceError::UndecodableRecord {
                        source_id: self.source_id.clone(),
                        reason: format!("no Relation seen yet for relation id {relation_id}"),
                    });
                };
                (
                    schema.to_string(),
                    table.to_string(),
                    columns_info
                        .iter()
                        .map(|c| c.name.clone())
                        .collect::<Vec<_>>(),
                )
            };

            let change = RowChange {
                kind,
                schema: schema.clone(),
                table: table.clone(),
                columns: columns_from_tuple(&columns_info, tuple),
                commit_timestamp_unix_micros: self.current_commit_timestamp_unix_micros,
            };
            let bytes =
                change
                    .to_canonical_bytes()
                    .map_err(|e| SourceError::UndecodableRecord {
                        source_id: self.source_id.clone(),
                        reason: format!("failed to encode row change: {e}"),
                    })?;

            let record = SourceRecord {
                bytes,
                metadata: vec![
                    ("schema".to_string(), schema),
                    ("table".to_string(), table),
                    ("change_kind".to_string(), format!("{kind:?}")),
                ],
                source_position: event.lsn.to_string(),
            };
            let ack = Box::new(LsnAckToken {
                stream: Arc::clone(&self.stream),
                lsn: event.lsn,
            });
            return Ok((record, ack));
        }
    }
}

/// Acknowledges a record by advancing the shared [`ReplicationStream`]'s
/// confirmed-flush position and immediately notifying the server, rather
/// than waiting for the next keepalive — an explicit ack is exactly the
/// moment the caller has told us it's safe for the slot to move past this
/// LSN, so there's no reason to delay reporting it.
struct LsnAckToken {
    stream: Arc<Mutex<ReplicationStream>>,
    lsn: u64,
}

impl std::fmt::Debug for LsnAckToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LsnAckToken")
            .field("lsn", &self.lsn)
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl AckToken for LsnAckToken {
    async fn ack(self: Box<Self>) -> Result<(), AckError> {
        let mut stream = self.stream.lock().await;
        stream.confirm_flush(self.lsn);
        stream
            .send_standby_status()
            .await
            .map_err(|e| AckError(e.to_string()))
    }
}
