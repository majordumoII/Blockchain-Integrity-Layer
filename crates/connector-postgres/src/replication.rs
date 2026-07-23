//! Drives a Postgres logical replication stream: issues
//! `START_REPLICATION`, and decodes the `CopyBoth`-mode messages that
//! follow — `XLogData` (carrying `pgoutput` payloads) and primary
//! keepalive requests, replying with standby status updates as required
//! to keep the connection (and the replication slot) alive.

use crate::connection::{ConnectionError, RawConnection};
use crate::pgoutput::{self, ColumnInfo, PgOutputMessage, RelationCache};
use bytes::Buf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Microseconds between the Unix epoch and the Postgres epoch
/// (2000-01-01 00:00:00 UTC), used to convert the timestamps embedded in
/// replication protocol messages to/from Unix time.
const PG_EPOCH_OFFSET_MICROS: i64 = 946_684_800_000_000;

/// A logical replication session: wraps a [`RawConnection`] that has
/// already issued `START_REPLICATION` and is now receiving `CopyBoth`
/// messages.
pub struct ReplicationStream {
    conn: RawConnection,
    relations: RelationCache,
    /// The last LSN this stream has fully processed. Sent back to the
    /// server in standby status updates so it knows how far the slot can
    /// safely advance; only moves forward when the caller explicitly
    /// acknowledges via [`Self::confirm_flush`], not automatically on
    /// read, so a crash before acknowledgment causes replay rather than
    /// silent data loss (see `proof_connectors::AckToken`'s docs for why
    /// that matters).
    flushed_lsn: u64,
}

/// One decoded change, ready to be turned into a
/// `proof_connectors::SourceRecord` by the caller (this crate's
/// `RecordSource` impl, in `lib.rs`).
#[derive(Debug, Clone)]
pub struct ReplicationEvent {
    pub lsn: u64,
    pub message: PgOutputMessage,
}

impl ReplicationStream {
    /// Starts logical replication for `slot_name` using `publication_names`
    /// (a comma-separated list, matching `START_REPLICATION`'s own
    /// syntax) over `conn`, which must not have been used for anything
    /// else yet.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionError`] if the command is rejected (e.g. the
    /// slot or publication doesn't exist) or the socket fails.
    pub async fn start(
        mut conn: RawConnection,
        slot_name: &str,
        publication_names: &str,
        start_lsn: u64,
    ) -> Result<Self, ConnectionError> {
        let (hi, lo) = split_lsn(start_lsn);
        let query = format!(
            "START_REPLICATION SLOT {slot_name} LOGICAL {hi:X}/{lo:X} \
             (proto_version '1', publication_names '{publication_names}')"
        );
        conn.send_query(&query).await?;

        // The server replies with CopyBothResponse ('W') before
        // streaming begins. `postgres-protocol` doesn't recognize that
        // tag (it only knows one-directional CopyOutResponse, 'H'), so
        // this one message has to be read via the raw path.
        let (tag, _body) = conn.read_raw_message("start_replication").await?;
        if tag != b'W' {
            return Err(ConnectionError::UnexpectedMessage {
                phase: "start_replication (expected CopyBothResponse)",
            });
        }

        Ok(Self {
            conn,
            relations: RelationCache::new(),
            flushed_lsn: start_lsn,
        })
    }

    /// Waits for and returns the next decoded event, transparently
    /// handling and replying to primary keepalive messages in between.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionError`] if the connection fails, or wraps a
    /// [`pgoutput::DecodeError`] if the server sends a malformed
    /// `pgoutput` payload.
    pub async fn next_event(&mut self) -> Result<ReplicationEvent, ReplicationError> {
        loop {
            let (tag, mut body) = self.conn.read_raw_message("replication stream").await?;
            match tag {
                b'd' => {
                    // CopyData wraps either XLogData ('w') or a primary
                    // keepalive ('k'); the byte we just split via
                    // read_raw_message already dropped CopyData's own
                    // envelope, so `body` starts at that inner tag.
                    if body.remaining() < 1 {
                        return Err(ReplicationError::Connection(
                            ConnectionError::UnexpectedMessage {
                                phase: "CopyData (empty)",
                            },
                        ));
                    }
                    let inner_tag = body.get_u8();
                    match inner_tag {
                        b'w' => {
                            // XLogData: starting WAL position (8), end
                            // WAL position (8), sender clock (8), then
                            // the pgoutput payload.
                            if body.remaining() < 24 {
                                return Err(ReplicationError::Connection(
                                    ConnectionError::UnexpectedMessage {
                                        phase: "XLogData (truncated header)",
                                    },
                                ));
                            }
                            let wal_start = body.get_u64();
                            let _wal_end = body.get_u64();
                            let _sender_clock = body.get_i64();
                            let message = pgoutput::decode(&body, &mut self.relations)?;
                            return Ok(ReplicationEvent {
                                lsn: wal_start,
                                message,
                            });
                        }
                        b'k' => {
                            // Primary keepalive: end WAL (8), clock (8),
                            // reply-requested flag (1).
                            if body.remaining() < 17 {
                                return Err(ReplicationError::Connection(
                                    ConnectionError::UnexpectedMessage {
                                        phase: "primary keepalive (truncated)",
                                    },
                                ));
                            }
                            let _wal_end = body.get_u64();
                            let _clock = body.get_i64();
                            let reply_requested = body.get_u8() != 0;
                            if reply_requested {
                                self.send_standby_status().await?;
                            }
                        }
                        _ => {
                            // Unrecognized CopyData sub-message; skip
                            // rather than treating an unknown but
                            // harmless message as fatal.
                        }
                    }
                }
                b'c' => {
                    return Err(ReplicationError::Connection(
                        ConnectionError::ConnectionClosed {
                            phase: "replication stream (server sent CopyDone)",
                        },
                    ));
                }
                _ => {
                    // Any other top-level message here would be a
                    // protocol violation for a CopyBoth stream; treat it
                    // the same as an unexpected message rather than
                    // silently looping forever.
                    return Err(ReplicationError::Connection(
                        ConnectionError::UnexpectedMessage {
                            phase: "replication stream (unexpected top-level tag)",
                        },
                    ));
                }
            }
        }
    }

    /// Looks up a relation's fully-qualified name and column list, as
    /// last reported by a `Relation` message for it.
    ///
    /// Returns `None` if no `Relation` message for this ID has been seen
    /// yet on this stream — which should not happen for any relation
    /// referenced by an `Insert`/`Update`/`Delete`, since the server
    /// always sends `Relation` before the first change that needs it.
    #[must_use]
    pub fn relation(&self, relation_id: u32) -> Option<(&str, &str, &[ColumnInfo])> {
        self.relations.get(relation_id)
    }

    /// Records that everything up to and including `lsn` has been
    /// durably processed (i.e. submitted to a `ProofSink` successfully),
    /// and reports this to the server on the next status update so the
    /// replication slot can advance past it.
    ///
    /// This does not immediately send a network message — see
    /// [`Self::send_standby_status`] — it only updates local state that
    /// the next status update (sent proactively on keepalive, or
    /// callable directly) will include.
    pub fn confirm_flush(&mut self, lsn: u64) {
        if lsn > self.flushed_lsn {
            self.flushed_lsn = lsn;
        }
    }

    /// Sends a standby status update reporting `flushed_lsn` as both the
    /// written and flushed position. Written/flushed are collapsed to the
    /// same value deliberately: this connector's `AckToken` (see
    /// `proof_connectors::record`) only fires after a proof is durably
    /// submitted, at which point the record is as "flushed" as this
    /// system's guarantees extend to — there's no intermediate state
    /// worth reporting separately.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionError`] if writing to the socket fails.
    pub async fn send_standby_status(&mut self) -> Result<(), ConnectionError> {
        let mut payload = Vec::with_capacity(34);
        payload.push(b'r');
        payload.extend_from_slice(&self.flushed_lsn.to_be_bytes()); // written
        payload.extend_from_slice(&self.flushed_lsn.to_be_bytes()); // flushed
        payload.extend_from_slice(&self.flushed_lsn.to_be_bytes()); // applied
        payload.extend_from_slice(&now_pg_micros().to_be_bytes());
        payload.push(0); // reply not requested
        self.conn.send_copy_data(&payload).await
    }
}

/// Errors from driving a [`ReplicationStream`].
#[derive(Debug, thiserror::Error)]
pub enum ReplicationError {
    #[error(transparent)]
    Connection(#[from] ConnectionError),
    #[error(transparent)]
    Decode(#[from] pgoutput::DecodeError),
}

/// Splits an LSN into its high/low 32-bit halves, as `START_REPLICATION`'s
/// `%X/%X` syntax expects. Both truncating casts here are exactly the
/// intended operation (take the high half, take the low half) rather
/// than a lossy conversion to guard against.
#[allow(clippy::cast_possible_truncation)]
fn split_lsn(lsn: u64) -> (u32, u32) {
    ((lsn >> 32) as u32, lsn as u32)
}

fn now_pg_micros() -> i64 {
    let unix_micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before 1970")
        .as_micros();
    let unix_micros = i64::try_from(unix_micros)
        .expect("current time in microseconds since 1970 fits in an i64 until the year 294247");
    unix_micros - PG_EPOCH_OFFSET_MICROS
}
