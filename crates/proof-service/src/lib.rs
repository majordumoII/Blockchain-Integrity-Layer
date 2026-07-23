//! Library surface for `proof-service`, split out from `main.rs` so the
//! wiring logic is unit-testable and reusable without spawning a real
//! process (e.g. from an integration test that boots the router directly).

pub mod metrics;
pub mod pipeline;
pub mod web;
