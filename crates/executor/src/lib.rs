#![forbid(unsafe_code)]

//! Execution and post-trade handling.
//!
//! This crate will own:
//!
//! - balances and allowances
//! - nonce handling
//! - transaction submission
//! - receipt tracking
//! - final outcome classification
