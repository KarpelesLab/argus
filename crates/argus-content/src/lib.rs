//! The content process (Phase 1).
//!
//! Hosts the engine for one document behind the sandbox. It receives a font and an
//! HTML document from the (trusted) browser process — it cannot read either from
//! disk itself — then on each `RequestFrame` parses, styles, lays out, and paints
//! the page into a shared framebuffer. With no document loaded it falls back to a
//! solid color (the Phase 0 behavior, still exercised by the `phase0` test).

use argus_compositor::Framebuffer;
use argus_geometry::{Color, Size};
use argus_gfx::Font;
use argus_ipc::Channel;
use argus_protocol::{self as proto, Msg};
use argus_util::{log, Role};
use std::io;

/// Fallback color painted when no document has been loaded yet (Argus blue).
pub const PHASE0_PAINT: Color = Color::rgb(0x2E, 0x86, 0xDE);

/// Run the content process to completion over `channel`.
pub fn run(channel: Channel) -> io::Result<()> {
    log::set_role(Role::Content);
    enter_sandbox();
    let viewport = proto::child_handshake(&channel)?;
    log!("ready; viewport {}x{}", viewport.width, viewport.height);

    let mut content = Content {
        viewport,
        font: None,
        html: None,
        _frame: None,
    };

    loop {
        let (msg, _fds) = proto::recv(&channel)?;
        match msg {
            Msg::ProvideFont { bytes } => {
                let n = bytes.len();
                match Font::from_bytes(bytes) {
                    Ok(font) => {
                        content.font = Some(font);
                        log!("loaded font ({n} bytes)");
                    }
                    Err(e) => log!("WARNING: failed to load font: {e}"),
                }
            }
            Msg::LoadDocument { html } => {
                log!("loaded document ({} bytes)", html.len());
                content.html = Some(html);
            }
            Msg::RequestFrame => {
                let fb = content.render()?;
                proto::send(&channel, Msg::FrameReady { size: viewport }, &[fb.as_fd()])?;
                content._frame = Some(fb);
            }
            Msg::InputClick { x, y } => {
                log!("received click at ({x}, {y})");
            }
            Msg::Shutdown => {
                log!("shutting down");
                return Ok(());
            }
            other => log!("ignoring unexpected message {other:?}"),
        }
    }
}

struct Content {
    viewport: Size,
    font: Option<Font>,
    html: Option<String>,
    /// Keeps the last framebuffer mapped so its shared memory stays valid for the
    /// browser after `FrameReady`.
    _frame: Option<Framebuffer>,
}

impl Content {
    /// Paint the current document (or the fallback color) into a fresh framebuffer.
    fn render(&self) -> io::Result<Framebuffer> {
        let mut fb = Framebuffer::create(self.viewport)?;
        match (&self.font, &self.html) {
            (Some(font), Some(html)) => {
                fb.fill(Color::WHITE);
                let doc = argus_html::parse(html);
                let layout = argus_layout::layout(&doc, font, self.viewport.width as f32);
                let list = argus_gfx::DisplayList {
                    rects: layout.rects,
                    runs: layout.runs,
                };
                let painted = argus_gfx::render_display_list(
                    &list,
                    font,
                    self.viewport.width,
                    self.viewport.height,
                );
                argus_gfx::composite_over(fb.pixels_mut(), &painted.pixels);
                log!(
                    "rendered page: {} rects, {} text runs",
                    list.rects.len(),
                    list.runs.len()
                );
            }
            _ => fb.fill(PHASE0_PAINT),
        }
        Ok(fb)
    }
}

/// Install the OS sandbox and, when one is active, prove it took effect.
fn enter_sandbox() {
    match argus_platform::sandbox::enter() {
        Ok(true) => {
            let probe = argus_platform::sandbox::probe_denied();
            log!(
                "sandbox active (fs-write denied = {}, network denied = {})",
                probe.fs_write_denied,
                probe.network_denied
            );
            assert!(
                probe.fs_write_denied,
                "sandbox installed but filesystem writes are still permitted"
            );
        }
        Ok(false) => log!("no sandbox available on this platform (yet)"),
        Err(e) => log!("WARNING: failed to enter sandbox: {e}"),
    }
}
