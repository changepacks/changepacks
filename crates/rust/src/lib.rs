//! # changepacks-rust
//!
//! Rust project support for changepacks.
//!
//! Implements project discovery and version management for Cargo.toml files. Uses `toml_edit`
//! for non-destructive parsing to preserve file formatting, comments, and whitespace. Handles
//! both single crates and Cargo workspace configurations.

pub mod finder;
pub mod package;
pub mod workspace;

pub use finder::RustProjectFinder;
