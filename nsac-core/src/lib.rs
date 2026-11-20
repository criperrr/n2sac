#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Pure NSAC Online (CTI/UNESP Bauru) scraping, cryptography, domain models,
//! accent-insensitive filtering, and class sentinel hashing.

pub mod client;
pub mod crypto;
pub mod error;
pub mod filters;
pub mod models;
pub mod parsers;
pub mod sentinel;
