//! Industry-agnostic connector traits linking external enterprise data
//! sources to `proof-core`.
//!
//! This crate defines two boundaries and nothing else:
//!
//! - [`RecordSource`]: where records-to-prove come from (a Postgres CDC
//!   listener, a webhook, a file watcher — see individual `connector-*`
//!   crates for concrete implementations).
//! - [`ProofSink`]: where finished proofs go (the live UI feed, metrics,
//!   eventually chain anchoring).
//!
//! Neither trait knows about the other, and neither knows about any
//! particular industry's schema — a healthcare record, a supply-chain
//! handoff event, and a loan decision all flow through the same shapes.
//! This is deliberate: it's what lets a single-process demo today become
//! a multi-service deployment later by swapping trait implementations,
//! not by redesigning the boundary.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod record;
pub mod sink;
pub mod source;

pub use record::{AckError, AckToken, SourceRecord};
pub use sink::{InProcessProofSink, ProofSink, ProvedRecord, SinkError};
pub use source::{RecordSource, SourceError, SourceId};
