//! Foundation utilities shared across Argus: process roles, logging, and typed IDs.
//!
//! This crate sits at the bottom of the dependency graph (Layer 0) and pulls in
//! nothing web-specific. See `docs/ARCHITECTURE.md`.

pub mod id;
pub mod log;
pub mod role;

pub use id::Id;
pub use role::Role;
