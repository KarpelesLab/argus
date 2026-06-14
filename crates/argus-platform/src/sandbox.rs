//! OS sandboxing.
//!
//! A content process calls [`enter`] immediately at startup, before it touches
//! any untrusted input, to drop OS capabilities it must not have (see
//! `docs/PROCESS_MODEL.md`). The mechanism is per-OS; the rest of Argus only sees
//! this uniform entry point.
//!
//! **Phase 0 posture (macOS):** allow-by-default, but deny the two capabilities
//! the security model forbids outright — network access and filesystem writes.
//! This is deliberately permissive to get a working boundary in place; it will be
//! tightened toward deny-by-default (explicit allow-lists for the few resources a
//! renderer legitimately needs) in a later phase. Linux (seccomp-bpf + Landlock)
//! and Windows arrive later; on those targets [`enter`] is currently a no-op.

use std::io;

/// Apply the sandbox for an untrusted process. Returns `Ok(true)` if a real OS
/// sandbox was installed, `Ok(false)` if this platform has no implementation yet.
pub fn enter() -> io::Result<bool> {
    imp::enter()
}

/// Best-effort verification that the sandbox is in force, used to prove the
/// boundary at startup. Returns whether a filesystem write and an outbound TCP
/// connection were each refused with a permission error.
pub fn probe_denied() -> SandboxProbe {
    // Filesystem write: under the sandbox, creating a file fails with a permission
    // error; unsandboxed it succeeds.
    let fs_write_denied = {
        let path = std::env::temp_dir().join(format!("argus-sbx-{}.probe", std::process::id()));
        match std::fs::File::create(&path) {
            Ok(_) => {
                let _ = std::fs::remove_file(&path);
                false
            }
            Err(e) => e.kind() == io::ErrorKind::PermissionDenied,
        }
    };

    // Network: a sandboxed connect attempt fails with a permission error before it
    // can even reach "connection refused".
    let network_denied = match std::net::TcpStream::connect("127.0.0.1:9") {
        Ok(_) => false,
        Err(e) => e.kind() == io::ErrorKind::PermissionDenied,
    };

    SandboxProbe {
        fs_write_denied,
        network_denied,
    }
}

/// Result of [`probe_denied`].
#[derive(Clone, Copy, Debug)]
pub struct SandboxProbe {
    /// A filesystem write was refused with a permission error.
    pub fs_write_denied: bool,
    /// An outbound TCP connection was refused with a permission error.
    pub network_denied: bool,
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{CStr, CString};
    use std::io;
    use std::os::raw::{c_char, c_int};
    use std::ptr;

    /// Seatbelt SBPL profile applied to content processes. See the module docs.
    const CONTENT_PROFILE: &str =
        "(version 1)\n(allow default)\n(deny network*)\n(deny file-write*)\n";

    extern "C" {
        fn sandbox_init(profile: *const c_char, flags: u64, errorbuf: *mut *mut c_char) -> c_int;
        fn sandbox_free_error(errorbuf: *mut c_char);
    }

    pub(super) fn enter() -> io::Result<bool> {
        let profile = CString::new(CONTENT_PROFILE).expect("profile has no interior NUL");
        let mut errbuf: *mut c_char = ptr::null_mut();
        // SAFETY: profile is a valid C string; errbuf is a valid out-pointer that
        // we free via sandbox_free_error on failure.
        let rc = unsafe { sandbox_init(profile.as_ptr(), 0, &mut errbuf) };
        if rc == 0 {
            return Ok(true);
        }
        let msg = if errbuf.is_null() {
            "sandbox_init failed".to_string()
        } else {
            // SAFETY: errbuf was populated by sandbox_init and is freed below.
            let s = unsafe { CStr::from_ptr(errbuf) }
                .to_string_lossy()
                .into_owned();
            unsafe { sandbox_free_error(errbuf) };
            s
        };
        Err(io::Error::other(msg))
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::io;

    pub(super) fn enter() -> io::Result<bool> {
        // TODO: seccomp-bpf + Landlock (Linux), AppContainer (Windows).
        Ok(false)
    }
}
