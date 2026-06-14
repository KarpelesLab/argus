//! OS-thin platform layer.
//!
//! Everything that must touch the operating system directly — process spawning,
//! the sandbox, and (later) windowing/input — lives here behind a uniform API so
//! the rest of Argus never sees per-OS differences. See `docs/ARCHITECTURE.md`.

pub mod process;
pub mod sandbox;

pub use process::{spawn_child, Child, CHILD_CHANNEL_FD};
