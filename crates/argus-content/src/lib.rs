//! The content process (Phase 0 slice).
//!
//! Untrusted web content will eventually live here behind the sandbox. For now it
//! does just enough to prove the pipeline: handshake, and on request paint a
//! solid-color framebuffer into shared memory and hand its fd to the browser.

use argus_compositor::Framebuffer;
use argus_geometry::Color;
use argus_ipc::Channel;
use argus_protocol::{self as proto, Msg};
use argus_util::{log, Role};
use std::io;

/// The placeholder color the content process paints in Phase 0 (Argus blue).
/// Real painting (DOM → style → layout → paint) arrives in Phase 1.
pub const PHASE0_PAINT: Color = Color::rgb(0x2E, 0x86, 0xDE);

/// Run the content process to completion over `channel`.
pub fn run(channel: Channel) -> io::Result<()> {
    log::set_role(Role::Content);
    let viewport = proto::child_handshake(&channel)?;
    log!("ready; viewport {}x{}", viewport.width, viewport.height);

    // Hold the most recent frame so its shared memory stays mapped (and thus the
    // object stays alive) after we reply to the browser.
    let mut _frame: Option<Framebuffer> = None;

    loop {
        let (msg, _fds) = proto::recv(&channel)?;
        match msg {
            Msg::RequestFrame => {
                let mut fb = Framebuffer::create(viewport)?;
                fb.fill(PHASE0_PAINT);
                proto::send(&channel, Msg::FrameReady { size: viewport }, &[fb.as_fd()])?;
                log!(
                    "painted and sent a {}x{} frame",
                    viewport.width,
                    viewport.height
                );
                _frame = Some(fb);
            }
            Msg::Shutdown => {
                log!("shutting down");
                return Ok(());
            }
            other => log!("ignoring unexpected message {other:?}"),
        }
    }
}
