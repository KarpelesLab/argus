//! The Argus executable.
//!
//! A single binary that re-executes itself in different process [`Role`]s. With
//! no `--role` it is the trusted **browser** entry process; spawned children run
//! with `--role=content|net|storage` and inherit their IPC channel on
//! [`CHILD_CHANNEL_FD`]. See `docs/PROCESS_MODEL.md`.

use argus_ipc::Channel;
use argus_platform::CHILD_CHANNEL_FD;
use argus_util::Role;

fn main() {
    let role = parse_role();
    argus_util::log::set_role(role);

    let result = match role {
        // Default to an on-screen window; `--headless` runs the verifier and exits.
        Role::Browser if has_flag("--headless") => argus_browser::run(),
        Role::Browser => argus_browser::run_default(),
        Role::Content => argus_content::run(child_channel()),
        Role::NetService | Role::StorageService => argus_services::run(role, child_channel()),
    };

    if let Err(err) = result {
        eprintln!("[{role}] fatal: {err}");
        std::process::exit(1);
    }
}

/// Determine this process's role from `--role=<name>` (default: browser).
fn parse_role() -> Role {
    for arg in std::env::args().skip(1) {
        if let Some(name) = arg.strip_prefix("--role=") {
            return name.parse().unwrap_or_else(|_| {
                eprintln!("argus: unknown --role={name}");
                std::process::exit(2);
            });
        }
    }
    Role::Browser
}

/// Whether `flag` appears in the process arguments.
fn has_flag(flag: &str) -> bool {
    std::env::args().any(|a| a == flag)
}

/// Reconstruct the IPC channel a child inherited from its parent.
fn child_channel() -> Channel {
    // SAFETY: the parent placed our channel on CHILD_CHANNEL_FD before exec, and
    // nothing else in this process has touched that descriptor.
    unsafe { Channel::from_raw_fd(CHILD_CHANNEL_FD) }
}
