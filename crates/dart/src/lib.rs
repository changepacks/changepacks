//! # changepacks-dart
//!
//! Dart project support for changepacks.
//!
//! Implements project discovery and version management for pubspec.yaml files. Parses YAML
//! using the `serde_yaml` crate while maintaining formatting. Supports both single packages
//! and workspace configurations with pub as the package manager.

pub mod finder;
pub mod package;
pub mod workspace;

pub use finder::DartProjectFinder;
