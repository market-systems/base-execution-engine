#![forbid(unsafe_code)]

//! Application entry crate.
//!
//! This crate will own:
//!
//! - process startup
//! - config loading
//! - logging initialization
//! - dependency wiring
//! - task supervision
//! - graceful shutdown
