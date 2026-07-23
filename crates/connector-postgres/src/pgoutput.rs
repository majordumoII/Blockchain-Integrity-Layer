//! Decoder for `pgoutput` logical replication messages (protocol version
//! 1, the version supported since Postgres 10).
//!
//! This only decodes the subset of the format this connector actually
//! needs to turn row changes into `proof_connectors::SourceRecord`s:
//! `Relation` (column names, needed to label values), `Insert`, `Update`,
//! `Delete`, `Begin`, and `Commit`. `Truncate` and message/type-only
//! variants are recognized and skipped rather than erroring, since a
//! table being truncated shouldn't take down the whole connector.
//!
//! Decoding was built and verified against real bytes captured from a
//! live Postgres 16 instance via `pg_recvlogical`, not solely from the
//! written protocol spec — see `tests/replication_live.rs`.

use bytes::{Buf, Bytes};
use std::collections::HashMap;

/// A decoded `pgoutput` message.
#[derive(Debug, Clone)]
pub enum PgOutputMessage {
    Begin {
        final_lsn: u64,
        commit_timestamp_micros: i64,
        xid: u32,
    },
    Commit {
        commit_lsn: u64,
        end_lsn: u64,
        commit_timestamp_micros: i64,
    },
    /// Table schema, sent before the first change referencing it (and
    /// again if the schema changes). Column order here defines the order
    /// values appear in in `Insert`/`Update`/`Delete`.
    Relation {
        relation_id: u32,
        namespace: String,
        name: String,
        columns: Vec<ColumnInfo>,
    },
    Insert {
        relation_id: u32,
        new_tuple: TupleData,
    },
    Update {
        relation_id: u32,
        /// Present only when the update changed a replica-identity column
        /// and the table's replica identity is `FULL` or the key columns
        /// changed; `None` under the common default (replica identity
        /// `DEFAULT`, non-key columns changed).
        old_tuple: Option<TupleData>,
        new_tuple: TupleData,
    },
    Delete {
        relation_id: u32,
        /// The key or full old row, depending on replica identity —
        /// whichever the server was configured to send.
        old_tuple: TupleData,
    },
    /// Recognized but not decoded further — truncation isn't a row-level
    /// change this connector's downstream `SourceRecord` shape models.
    Truncate,
    /// Any other/unrecognized message type. Kept rather than erroring so
    /// a future server extending the protocol doesn't stall this
    /// connector on messages it doesn't need to act on.
    Unknown { tag: u8 },
}

#[derive(Debug, Clone)]
pub struct ColumnInfo {
    pub name: String,
    /// True if this column is part of the table's replica identity (the
    /// set of columns used to identify a row for UPDATE/DELETE decoding).
    pub is_key: bool,
    pub type_oid: u32,
}

/// A decoded row's column values, in the same order as the owning
/// `Relation`'s `columns`.
#[derive(Debug, Clone)]
pub struct TupleData {
    pub values: Vec<ColumnValue>,
}

#[derive(Debug, Clone)]
pub enum ColumnValue {
    /// Column value as sent by the server: text-format-encoded bytes
    /// (pgoutput's default; matches what `::text` casting would produce).
    Text(Vec<u8>),
    /// Column value sent in binary format (only when the publication
    /// requests `binary = true`; this connector doesn't request it, so
    /// this variant is decoded for completeness but not expected to be
    /// produced against this crate's own replication setup).
    Binary(Vec<u8>),
    /// Column is SQL `NULL`.
    Null,
    /// Column exists but its value was not sent (e.g. a non-key,
    /// unchanged column in an `Update`'s old-tuple, or any column in a
    /// `Delete`'s old-tuple when replica identity is `DEFAULT`).
    Unchanged,
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("pgoutput message too short: {0}")]
    Truncated(&'static str),
    #[error("pgoutput message contained invalid utf-8: {0}")]
    InvalidUtf8(#[from] std::str::Utf8Error),
    #[error("unrecognized tuple type byte: {0:#x}")]
    UnknownTupleType(u8),
    #[error("{context} reported a negative column count: {value}")]
    NegativeColumnCount { context: &'static str, value: i16 },
}

/// Reads a 16-bit column count and validates it's non-negative, returning
/// it as a `usize` so callers never need to reason about the sign again.
/// A negative count from the server would indicate a malformed message —
/// treated as a decode error rather than silently clamped to zero, which
/// would desync every subsequent read from the buffer instead of failing
/// where the actual problem was detected.
fn read_column_count(buf: &mut Bytes, context: &'static str) -> Result<usize, DecodeError> {
    let raw = read_i16(buf, context)?;
    usize::try_from(raw).map_err(|_| DecodeError::NegativeColumnCount {
        context,
        value: raw,
    })
}

/// Tracks column metadata per relation, needed because `Insert`/`Update`/
/// `Delete` messages only carry a `relation_id` and raw tuple bytes — the
/// column names/keys come from whatever `Relation` message preceded them
/// in the stream. A decoder-level cache, not a `RecordSource`-level
/// concern, so it lives here rather than being re-derived by callers.
#[derive(Debug, Default)]
pub struct RelationCache {
    relations: HashMap<u32, CachedRelation>,
}

#[derive(Debug, Clone)]
struct CachedRelation {
    namespace: String,
    name: String,
    columns: Vec<ColumnInfo>,
}

impl RelationCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn record(
        &mut self,
        relation_id: u32,
        namespace: String,
        name: String,
        columns: Vec<ColumnInfo>,
    ) {
        self.relations.insert(
            relation_id,
            CachedRelation {
                namespace,
                name,
                columns,
            },
        );
    }

    /// Looks up a previously seen relation's fully-qualified name and
    /// column list.
    #[must_use]
    pub fn get(&self, relation_id: u32) -> Option<(&str, &str, &[ColumnInfo])> {
        self.relations
            .get(&relation_id)
            .map(|r| (r.namespace.as_str(), r.name.as_str(), r.columns.as_slice()))
    }
}

/// Decodes a single `pgoutput` message from a `CopyData` payload's body
/// (i.e. everything after the `CopyData` envelope has already been
/// stripped by the caller).
///
/// # Errors
///
/// Returns [`DecodeError`] if `data` is shorter than the message type it
/// claims to be, or contains invalid UTF-8 in a string field.
pub fn decode(data: &[u8], cache: &mut RelationCache) -> Result<PgOutputMessage, DecodeError> {
    let mut buf = Bytes::copy_from_slice(data);
    let tag = read_u8(&mut buf, "message tag")?;
    match tag {
        b'B' => decode_begin(&mut buf),
        b'C' => decode_commit(&mut buf),
        b'R' => decode_relation(&mut buf, cache),
        b'I' => decode_insert(&mut buf, cache),
        b'U' => decode_update(&mut buf, cache),
        b'D' => decode_delete(&mut buf, cache),
        b'T' => Ok(PgOutputMessage::Truncate),
        other => Ok(PgOutputMessage::Unknown { tag: other }),
    }
}

fn read_u8(buf: &mut Bytes, ctx: &'static str) -> Result<u8, DecodeError> {
    if buf.remaining() < 1 {
        return Err(DecodeError::Truncated(ctx));
    }
    Ok(buf.get_u8())
}

fn read_u32(buf: &mut Bytes, ctx: &'static str) -> Result<u32, DecodeError> {
    if buf.remaining() < 4 {
        return Err(DecodeError::Truncated(ctx));
    }
    Ok(buf.get_u32())
}

fn read_u64(buf: &mut Bytes, ctx: &'static str) -> Result<u64, DecodeError> {
    if buf.remaining() < 8 {
        return Err(DecodeError::Truncated(ctx));
    }
    Ok(buf.get_u64())
}

fn read_i64(buf: &mut Bytes, ctx: &'static str) -> Result<i64, DecodeError> {
    if buf.remaining() < 8 {
        return Err(DecodeError::Truncated(ctx));
    }
    Ok(buf.get_i64())
}

fn read_i16(buf: &mut Bytes, ctx: &'static str) -> Result<i16, DecodeError> {
    if buf.remaining() < 2 {
        return Err(DecodeError::Truncated(ctx));
    }
    Ok(buf.get_i16())
}

/// Reads a null-terminated string, as used throughout the Postgres wire
/// protocol for identifiers.
fn read_cstr(buf: &mut Bytes, ctx: &'static str) -> Result<String, DecodeError> {
    let nul_pos = buf
        .iter()
        .position(|&b| b == 0)
        .ok_or(DecodeError::Truncated(ctx))?;
    let bytes = buf.split_to(nul_pos);
    buf.advance(1); // consume the NUL
    Ok(std::str::from_utf8(&bytes)?.to_string())
}

fn decode_begin(buf: &mut Bytes) -> Result<PgOutputMessage, DecodeError> {
    let final_lsn = read_u64(buf, "Begin.final_lsn")?;
    let commit_timestamp_micros = read_i64(buf, "Begin.commit_timestamp")?;
    let xid = read_u32(buf, "Begin.xid")?;
    Ok(PgOutputMessage::Begin {
        final_lsn,
        commit_timestamp_micros,
        xid,
    })
}

fn decode_commit(buf: &mut Bytes) -> Result<PgOutputMessage, DecodeError> {
    let _flags = read_u8(buf, "Commit.flags")?;
    let commit_lsn = read_u64(buf, "Commit.commit_lsn")?;
    let end_lsn = read_u64(buf, "Commit.end_lsn")?;
    let commit_timestamp_micros = read_i64(buf, "Commit.commit_timestamp")?;
    Ok(PgOutputMessage::Commit {
        commit_lsn,
        end_lsn,
        commit_timestamp_micros,
    })
}

fn decode_relation(
    buf: &mut Bytes,
    cache: &mut RelationCache,
) -> Result<PgOutputMessage, DecodeError> {
    let relation_id = read_u32(buf, "Relation.id")?;
    let namespace = read_cstr(buf, "Relation.namespace")?;
    let name = read_cstr(buf, "Relation.name")?;
    let _replica_identity = read_u8(buf, "Relation.replica_identity")?;
    let column_count = read_column_count(buf, "Relation.column_count")?;

    let mut columns = Vec::with_capacity(column_count);
    for _ in 0..column_count {
        let flags = read_u8(buf, "Relation.column.flags")?;
        let col_name = read_cstr(buf, "Relation.column.name")?;
        let type_oid = read_u32(buf, "Relation.column.type_oid")?;
        let _type_modifier = read_u32(buf, "Relation.column.type_modifier")?;
        columns.push(ColumnInfo {
            name: col_name,
            is_key: flags & 0x01 != 0,
            type_oid,
        });
    }

    cache.record(
        relation_id,
        namespace.clone(),
        name.clone(),
        columns.clone(),
    );
    Ok(PgOutputMessage::Relation {
        relation_id,
        namespace,
        name,
        columns,
    })
}

/// Reads a tuple: a column count followed by that many `ColumnValue`s,
/// each prefixed with a one-byte type tag (`t` = text, `b` = binary,
/// `n` = null, `u` = unchanged/TOASTed-and-not-sent).
fn read_tuple(buf: &mut Bytes) -> Result<TupleData, DecodeError> {
    let column_count = read_column_count(buf, "tuple.column_count")?;
    let mut values = Vec::with_capacity(column_count);
    for _ in 0..column_count {
        let kind = read_u8(buf, "tuple.column.kind")?;
        let value = match kind {
            b'n' => ColumnValue::Null,
            b'u' => ColumnValue::Unchanged,
            b't' => {
                let len = read_u32(buf, "tuple.column.text_len")? as usize;
                if buf.remaining() < len {
                    return Err(DecodeError::Truncated("tuple.column.text_data"));
                }
                ColumnValue::Text(buf.split_to(len).to_vec())
            }
            b'b' => {
                let len = read_u32(buf, "tuple.column.binary_len")? as usize;
                if buf.remaining() < len {
                    return Err(DecodeError::Truncated("tuple.column.binary_data"));
                }
                ColumnValue::Binary(buf.split_to(len).to_vec())
            }
            other => return Err(DecodeError::UnknownTupleType(other)),
        };
        values.push(value);
    }
    Ok(TupleData { values })
}

fn decode_insert(buf: &mut Bytes, _cache: &RelationCache) -> Result<PgOutputMessage, DecodeError> {
    let relation_id = read_u32(buf, "Insert.relation_id")?;
    let tuple_kind = read_u8(buf, "Insert.tuple_kind")?;
    debug_assert_eq!(
        tuple_kind, b'N',
        "Insert's tuple marker is always 'N' (new)"
    );
    let new_tuple = read_tuple(buf)?;
    Ok(PgOutputMessage::Insert {
        relation_id,
        new_tuple,
    })
}

fn decode_update(buf: &mut Bytes, _cache: &RelationCache) -> Result<PgOutputMessage, DecodeError> {
    let relation_id = read_u32(buf, "Update.relation_id")?;
    let mut marker = read_u8(buf, "Update.tuple_marker")?;

    let old_tuple = if marker == b'K' || marker == b'O' {
        let tuple = read_tuple(buf)?;
        marker = read_u8(buf, "Update.new_tuple_marker")?;
        Some(tuple)
    } else {
        None
    };
    debug_assert_eq!(marker, b'N', "Update always ends with an 'N' (new) tuple");
    let new_tuple = read_tuple(buf)?;

    Ok(PgOutputMessage::Update {
        relation_id,
        old_tuple,
        new_tuple,
    })
}

fn decode_delete(buf: &mut Bytes, _cache: &RelationCache) -> Result<PgOutputMessage, DecodeError> {
    let relation_id = read_u32(buf, "Delete.relation_id")?;
    let marker = read_u8(buf, "Delete.tuple_marker")?;
    debug_assert!(
        marker == b'K' || marker == b'O',
        "Delete's tuple marker is always 'K' (key) or 'O' (old/full)"
    );
    let old_tuple = read_tuple(buf)?;
    Ok(PgOutputMessage::Delete {
        relation_id,
        old_tuple,
    })
}
