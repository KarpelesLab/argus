//! Child-process spawning via single-binary re-exec.
//!
//! Argus is one executable that re-executes itself in different [`Role`]s
//! (Chromium-style `--role=…`). The parent creates a socket pair, keeps one end,
//! and arranges for the child to inherit the other on a fixed fd number
//! ([`CHILD_CHANNEL_FD`]) across `exec`. The child reconstructs its [`Channel`]
//! from that fd. See `docs/PROCESS_MODEL.md`.

use argus_ipc::Channel;
use argus_util::Role;
use std::io;
use std::os::fd::RawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child as StdChild, Command, ExitStatus};

/// The fd number on which a spawned child finds its IPC channel after `exec`.
/// Chosen as the first descriptor past stdio (0/1/2).
pub const CHILD_CHANNEL_FD: RawFd = 3;

/// A spawned child process and the [`Channel`] connecting the parent to it.
pub struct Child {
    role: Role,
    proc: StdChild,
    channel: Channel,
}

impl Child {
    /// The role this child is running.
    pub fn role(&self) -> Role {
        self.role
    }

    /// The child's OS process id.
    pub fn pid(&self) -> u32 {
        self.proc.id()
    }

    /// The parent-side channel to this child.
    pub fn channel(&self) -> &Channel {
        &self.channel
    }

    /// Forcibly terminate the child (used to exercise crash isolation, and to
    /// reap a misbehaving process).
    pub fn kill(&mut self) -> io::Result<()> {
        self.proc.kill()
    }

    /// Block until the child exits, returning its status.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.proc.wait()
    }

    /// Poll whether the child has exited without blocking.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.proc.try_wait()
    }
}

/// Spawn this executable again in `role`, wiring up an IPC channel to it.
pub fn spawn_child(role: Role) -> io::Result<Child> {
    let exe = std::env::current_exe()?;
    let (parent, child) = Channel::pair()?;

    // We take raw ownership of the child end so we can place it on a fixed fd in
    // the child and then close our copy after the fork.
    let child_fd = child.into_raw_fd();

    let mut cmd = Command::new(exe);
    cmd.arg(format!("--role={role}"));

    // pre_exec runs in the child between fork and exec. It must use only
    // async-signal-safe calls. We move the inherited channel onto CHILD_CHANNEL_FD
    // and clear its close-on-exec flag so it survives the exec.
    unsafe {
        cmd.pre_exec(move || {
            if libc::dup2(child_fd, CHILD_CHANNEL_FD) < 0 {
                return Err(io::Error::last_os_error());
            }
            // Clear FD_CLOEXEC on the target (covers the case child_fd == target,
            // where dup2 is a no-op and would otherwise leave CLOEXEC set).
            if libc::fcntl(CHILD_CHANNEL_FD, libc::F_SETFD, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let spawned = cmd.spawn();

    // Close our copy of the child end regardless of spawn success: the parent
    // talks over `parent`, and leaving this open would prevent EOF detection.
    // SAFETY: child_fd is our owned raw fd, used only here.
    unsafe { libc::close(child_fd) };

    let proc = spawned?;
    Ok(Child {
        role,
        proc,
        channel: parent,
    })
}
