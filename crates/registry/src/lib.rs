#![forbid(unsafe_code)]

//! Shared protocol and exchange lookup tables.
//!
//! This crate owns static ingest-time mappings such as selector and event
//! signature recognition. The lookup rules live here so they can be shared by
//! `ingest` and later by other crates.

mod log;
mod transaction;

pub use log::{exchange_from_event_signature, protocol_from_event_signature};
pub use transaction::{exchange_from_selector, protocol_from_selector};
