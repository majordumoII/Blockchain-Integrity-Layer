//! Canonical, serializable representation of a single row change, and its
//! encoding into the bytes that get hashed by `proof-core`.
//!
//! Kept separate from [`crate::pgoutput`]'s types because those are
//! decode-time representations (borrow-free but tied to how pgoutput
//! frames data); this module defines the stable, owned shape that
//! actually gets hashed — the thing a `Proof`'s digest is a commitment
//! to. Changing how pgoutput is decoded must not change what gets
//! hashed for the same logical row change; routing both through this
//! module is what keeps that true.

use crate::pgoutput::{ColumnValue, TupleData};
use serde::{Deserialize, Serialize};

/// What kind of change occurred, mirroring `pgoutput`'s own operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeKind {
    Insert,
    Update,
    Delete,
}

/// An owned, serializable column value — the hashed counterpart to
/// [`crate::pgoutput::ColumnValue`], which borrows from a decode buffer
/// that doesn't outlive a single `next_event()` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnedColumnValue {
    Text(Vec<u8>),
    Binary(Vec<u8>),
    Null,
    Unchanged,
}

impl From<&ColumnValue> for OwnedColumnValue {
    fn from(value: &ColumnValue) -> Self {
        match value {
            ColumnValue::Text(bytes) => OwnedColumnValue::Text(bytes.clone()),
            ColumnValue::Binary(bytes) => OwnedColumnValue::Binary(bytes.clone()),
            ColumnValue::Null => OwnedColumnValue::Null,
            ColumnValue::Unchanged => OwnedColumnValue::Unchanged,
        }
    }
}

/// A single row change, in the canonical shape that gets bincode-encoded
/// and hashed.
///
/// Column order is preserved exactly as the source `Relation` message
/// defined it (not re-sorted) — reordering would make this crate's
/// output depend on a sort implementation rather than solely on what
/// Postgres actually sent, which is the simpler invariant to reason
/// about and matches how a human reading a schema would expect columns
/// to appear.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RowChange {
    pub kind: ChangeKind,
    pub schema: String,
    pub table: String,
    /// `(column_name, value)` pairs, in schema-definition order. For an
    /// `Update`, this is the new tuple; the old tuple (if the server
    /// sent one) is not currently included in the hashed representation
    /// — see the module docs for why: it's the *resulting* state being
    /// attested to, mirroring how the README frames this ("did this
    /// record end up in state X"), not a full diff.
    pub columns: Vec<(String, OwnedColumnValue)>,
    /// Postgres commit timestamp for the transaction this change was
    /// part of, as microseconds since the Unix epoch.
    pub commit_timestamp_unix_micros: i64,
}

impl RowChange {
    /// Encodes this change to the canonical bytes that get hashed.
    ///
    /// # Errors
    ///
    /// Returns a `bincode::Error` if encoding fails (bincode encoding of
    /// this type's fields cannot practically fail, but the signature is
    /// kept fallible to match `bincode::serialize`'s own contract rather
    /// than papering over it with an `unwrap`).
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(self)
    }
}

/// Builds the `columns` list for a tuple given its owning relation's
/// column names, in schema order.
///
/// # Panics
///
/// Panics if `tuple` has a different number of values than `column_names`
/// — this would mean the `Relation` message cached for this table is out
/// of sync with the tuple's own shape, which indicates a decoder bug
/// rather than a recoverable runtime condition (a Postgres server does
/// not send tuples inconsistent with its own preceding Relation
/// message).
#[must_use]
pub fn columns_from_tuple(
    column_names: &[String],
    tuple: &TupleData,
) -> Vec<(String, OwnedColumnValue)> {
    assert_eq!(
        column_names.len(),
        tuple.values.len(),
        "tuple column count must match its relation's column count"
    );
    column_names
        .iter()
        .zip(&tuple.values)
        .map(|(name, value)| (name.clone(), OwnedColumnValue::from(value)))
        .collect()
}
