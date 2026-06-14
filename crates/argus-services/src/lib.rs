//! Trusted service processes (Phase 0 skeleton).
//!
//! The net and storage services will own the network (rsurl) and disk on the
//! trusted side of the sandbox (see `docs/PROCESS_MODEL.md`). For now they only
//! prove the spawn/handshake/lifecycle path: come up, acknowledge, idle until the
//! browser shuts them down or goes away.

use argus_ipc::Channel;
use argus_protocol::{self as proto, Msg};
use argus_util::{log, Role};
use std::io;

/// Run a service process (net or storage) to completion over `channel`.
pub fn run(role: Role, channel: Channel) -> io::Result<()> {
    log::set_role(role);
    let _viewport = proto::child_handshake(&channel)?;
    log!("ready");

    loop {
        match proto::recv(&channel) {
            Ok((Msg::LoadUrl { url }, _)) if role == Role::NetService => {
                let (status, body) = fetch(&url);
                log!("GET {url} -> {status} ({} bytes)", body.len());
                proto::send(&channel, Msg::ResourceLoaded { status, body }, &[])?;
            }
            Ok((Msg::Shutdown, _)) => {
                log!("shutting down");
                return Ok(());
            }
            Ok((other, _)) => log!("ignoring unexpected message {other:?}"),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                // The browser process is gone; exit quietly.
                log!("browser gone; exiting");
                return Ok(());
            }
            Err(e) => return Err(e),
        }
    }
}

/// Fetch `url` over rsurl. Returns `(status, body)`; `status == 0` on transport
/// error. The net service runs on the trusted side of the sandbox — content never
/// touches a socket (see `docs/PROCESS_MODEL.md`).
fn fetch(url: &str) -> (u16, Vec<u8>) {
    match rsurl::get(url) {
        Ok(resp) => (resp.status, resp.body),
        Err(e) => {
            log!("fetch error for {url}: {e}");
            (0, Vec::new())
        }
    }
}
