//! Process roles.
//!
//! Argus is multi-process (see `docs/PROCESS_MODEL.md`). A single executable
//! re-executes itself in different roles, dispatched by the `--role=<name>`
//! argument. The trusted **browser** role is the default (the entry process);
//! every other role is a child it spawns.

use std::fmt;
use std::str::FromStr;

/// The role a given process is playing.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Role {
    /// Trusted entry process: owns the window, UI, and process management.
    Browser,
    /// Sandboxed, untrusted: hosts one site instance's engine.
    Content,
    /// Trusted service: networking (rsurl).
    NetService,
    /// Trusted service: storage (disk).
    StorageService,
}

impl Role {
    /// The stable wire/CLI name for this role.
    pub const fn as_str(self) -> &'static str {
        match self {
            Role::Browser => "browser",
            Role::Content => "content",
            Role::NetService => "net",
            Role::StorageService => "storage",
        }
    }

    /// Whether this role runs untrusted web content and must be sandboxed.
    pub const fn is_sandboxed(self) -> bool {
        matches!(self, Role::Content)
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when a string does not name a known [`Role`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct UnknownRole;

impl fmt::Display for UnknownRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unknown process role")
    }
}

impl std::error::Error for UnknownRole {}

impl FromStr for Role {
    type Err = UnknownRole;

    fn from_str(s: &str) -> Result<Role, UnknownRole> {
        Ok(match s {
            "browser" => Role::Browser,
            "content" => Role::Content,
            "net" => Role::NetService,
            "storage" => Role::StorageService,
            _ => return Err(UnknownRole),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_str() {
        for role in [
            Role::Browser,
            Role::Content,
            Role::NetService,
            Role::StorageService,
        ] {
            assert_eq!(role.as_str().parse(), Ok(role));
        }
    }

    #[test]
    fn unknown_role_is_err() {
        assert_eq!("gpu".parse::<Role>(), Err(UnknownRole));
    }

    #[test]
    fn only_content_is_sandboxed() {
        assert!(Role::Content.is_sandboxed());
        assert!(!Role::Browser.is_sandboxed());
        assert!(!Role::NetService.is_sandboxed());
    }
}
