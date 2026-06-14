//! Minimal logging.
//!
//! Phase 0 logging is deliberately tiny: a single line to stderr tagged with the
//! process role and PID, so multi-process output is legible when several Argus
//! processes share a terminal. This will grow into a real structured logger.

use crate::role::Role;
use std::sync::OnceLock;

static ROLE: OnceLock<Role> = OnceLock::new();

/// Record the role of the current process, used as a log prefix.
///
/// Idempotent: the first call wins. Each process calls this once at startup.
pub fn set_role(role: Role) {
    let _ = ROLE.set(role);
}

/// The current process role, or [`Role::Browser`] if not yet set.
pub fn role() -> Role {
    *ROLE.get().unwrap_or(&Role::Browser)
}

/// Write a log line tagged with `[role pid]`.
pub fn line(args: std::fmt::Arguments<'_>) {
    let pid = std::process::id();
    eprintln!("[{} {pid}] {args}", role());
}

/// Log a formatted message, e.g. `log!("spawned {} children", n)`.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::log::line(format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_browser_role() {
        // Without set_role in this test process, role() is Browser.
        assert_eq!(role(), Role::Browser);
    }
}
