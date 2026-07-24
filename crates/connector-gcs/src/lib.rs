//! Google Cloud Storage connector: watches a bucket's `OBJECT_FINALIZE`
//! events via a GCS Pub/Sub notification subscription and yields them as
//! `proof_connectors::SourceRecord`s.
//!
//! Requires the bucket to have a `JSON_API_V1`-payload-format Pub/Sub
//! notification configured (see `crate::notification`'s docs for why),
//! and requires the subscription to have exactly-once delivery enabled
//! (see `crate::source`'s docs for why plain at-least-once delivery was
//! rejected for this connector specifically).

#![forbid(unsafe_code)]

pub mod notification;
pub mod object_change;
pub mod source;

pub use source::GcsSource;
