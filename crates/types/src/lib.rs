#![forbid(unsafe_code)]

//! Shared system types.
//!
//! This crate defines the common language used across the workspace. The first
//! implemented slice covers the ingest boundary: block context, input
//! channel, decode status, and the observed chain event stream produced for
//! downstream decision making.

pub mod ingest;

pub type ChainId = u64;
pub type BlockNumber = u64;
pub type UnixTimestampMillis = u64;
pub type TxHash = String;
pub type BlockHash = String;
pub type Address = String;
pub type Topic = String;
pub type Selector = String;
