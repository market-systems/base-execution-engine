#![forbid(unsafe_code)]

//! Input boundary for the system.
//!
//! This crate will own everything related to getting data into the engine:
//!
//! - Flashblocks connectivity
//! - Base RPC and chain reads needed during ingest
//! - protocol decoding
//! - input normalization
//! - protocol and venue metadata used by ingest
