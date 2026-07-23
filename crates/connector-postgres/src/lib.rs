//! Postgres logical replication (CDC) connector: watches a table's row
//! changes via `pgoutput` logical decoding and yields them as
//! `proof_connectors::SourceRecord`s.
//!
//! Requires the target database to have `wal_level = logical`, and a
//! publication + replication slot already created for the table(s) being
//! watched (this crate does not create schema-level objects on its own —
//! that's a deliberate operator decision, not something to happen
//! implicitly on connect).

#![forbid(unsafe_code)]

pub mod change;
pub mod connection;
pub mod pgoutput;
pub mod replication;
pub mod source;

pub use source::PostgresSource;
