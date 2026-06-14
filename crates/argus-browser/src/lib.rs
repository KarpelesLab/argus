//! The browser process: the trusted entry process that spawns and coordinates
//! everything else.
//!
//! Phase 0 implements the skeleton end-to-end: spawn a content process and a net
//! service, handshake both, ask content for a frame and verify the pixels arrived
//! intact over shared memory, then deliberately kill the content process to prove
//! crash isolation before shutting the rest down cleanly. The on-screen window
//! and real tab/navigation logic build on this in later phases.

use argus_compositor::Framebuffer;
use argus_geometry::{Color, Size};
use argus_platform::{spawn_child, Child};
use argus_protocol::{self as proto, Msg};
use argus_util::{log, Role};
use std::io;

/// A built-in sample document rendered by the windowed shell and the page dumper.
pub const SAMPLE_HTML: &str = "<!DOCTYPE html><html><head><title>Argus</title><style>\
body { background-color: #f4f6fb; color: #1c2430 }\
h1 { color: #2e86de }\
h2 { color: #444 }\
.note { background-color: #fff3cd; color: #5a4b00 }\
.brand { color: #c0392b }\
</style></head><body>\
<h1>Argus</h1>\
<p>A web browser written in <strong class=\"brand\">pure Rust</strong>. This page was \
fetched as HTML, parsed into a DOM, run through a real CSS cascade (user-agent + the \
author styles in this document's &lt;style&gt; element), laid out into lines, and \
painted with shaped, anti-aliased glyphs — all inside a sandboxed content process.</p>\
<h2>Phase 1</h2>\
<p class=\"note\">This paragraph has an author background-color and text color applied \
by a class selector. Specificity, the cascade, and inline styles all work.</p>\
<p style=\"color: #2e7d32\">This one is colored green by an inline style attribute, \
which beats the author rules for this element.</p>\
<h3>Next</h3>\
<p>Images, a real fragment tree, and more CSS properties come next.</p>\
</body></html>";

/// Locate a usable system font on disk (the browser process is trusted and may
/// read the filesystem; content cannot).
fn system_font_bytes() -> Option<Vec<u8>> {
    for path in [
        "/System/Library/Fonts/Geneva.ttf",
        "/System/Library/Fonts/Monaco.ttf",
        "/System/Library/Fonts/SFNS.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            return Some(bytes);
        }
    }
    None
}

/// Send the content process a font and a document to render.
fn provide_page(content: &Child, html: &str) -> io::Result<()> {
    if let Some(bytes) = system_font_bytes() {
        proto::send(content.channel(), Msg::ProvideFont { bytes }, &[])?;
    } else {
        log!("no system font found; content will render the fallback color");
    }
    proto::send(
        content.channel(),
        Msg::LoadDocument {
            html: html.to_string(),
        },
        &[],
    )
}

/// Render `html` to pixels once, off-screen, by driving a content process. Returns
/// the framebuffer size and its RGBA bytes. Used by the `--dump-page` tool.
pub fn render_page_once(html: &str, viewport: Size) -> io::Result<(Size, Vec<u8>)> {
    log::set_role(Role::Browser);
    let mut content = spawn_child(Role::Content)?;
    proto::parent_handshake(content.channel(), viewport)?;
    provide_page(&content, html)?;

    let frame = request_frame(&content)?;
    let pixels = frame.pixels().to_vec();
    let size = frame.size();

    proto::send(content.channel(), Msg::Shutdown, &[])?;
    content.wait()?;
    Ok((size, pixels))
}

/// Run the Phase 0 browser-process skeleton.
pub fn run() -> io::Result<()> {
    log::set_role(Role::Browser);
    let viewport = Size::new(800, 600);
    log!("starting; viewport {}x{}", viewport.width, viewport.height);

    // Spawn the sandboxed content process and a trusted net service.
    let mut content = spawn_child(Role::Content)?;
    let mut net = spawn_child(Role::NetService)?;
    log!(
        "spawned content pid {} and net pid {}",
        content.pid(),
        net.pid()
    );

    proto::parent_handshake(content.channel(), viewport)?;
    proto::parent_handshake(net.channel(), viewport)?;
    log!(
        "both children handshook at protocol v{}",
        proto::PROTOCOL_VERSION
    );

    // Ask content to paint, then verify the framebuffer it shared back.
    let frame = request_frame(&content)?;
    let color = verify_uniform(&frame)?;
    let size = frame.size();
    log!(
        "verified {}x{} frame, uniform rgba({},{},{},{})",
        size.width,
        size.height,
        color.r,
        color.g,
        color.b,
        color.a
    );

    // Crash isolation: kill content and confirm the browser (and net) survive.
    log!("killing content to exercise crash isolation");
    content.kill()?;
    match proto::recv(content.channel()) {
        Err(_) => log!("content channel closed; browser process unaffected"),
        Ok((m, _)) => log!("unexpected message from a killed content process: {m:?}"),
    }
    let content_status = content.wait()?;
    log!("reaped content: {content_status}");

    // The net service is independent and still responsive: shut it down cleanly.
    proto::send(net.channel(), Msg::Shutdown, &[])?;
    let net_status = net.wait()?;
    log!("reaped net: {net_status}");

    println!(
        "PHASE0 OK: {}x{} frame rgba({},{},{},{}) over shared memory; crash isolation verified",
        size.width, size.height, color.r, color.g, color.b, color.a
    );
    Ok(())
}

/// Ask `content` to paint, and map the shared framebuffer it hands back.
fn request_frame(content: &Child) -> io::Result<Framebuffer> {
    proto::send(content.channel(), Msg::RequestFrame, &[])?;
    let (msg, mut fds) = proto::recv(content.channel())?;
    let size = match msg {
        Msg::FrameReady { size } => size,
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected FrameReady, got {other:?}"),
            ))
        }
    };
    let fd = fds.pop().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "FrameReady carried no framebuffer fd",
        )
    })?;
    Framebuffer::from_fd(fd, size)
}

/// Run the browser in its default mode for this platform: a real window where one
/// is available, the headless verifier otherwise.
#[cfg(target_os = "macos")]
pub fn run_default() -> io::Result<()> {
    run_windowed()
}

/// See [`run_default`].
#[cfg(not(target_os = "macos"))]
pub fn run_default() -> io::Result<()> {
    run()
}

/// Run the browser with an on-screen window (macOS). Spawns content + net, opens
/// a window, presents content's framebuffer, forwards clicks into the sandboxed
/// content process, and repaints — until the window is closed.
#[cfg(target_os = "macos")]
pub fn run_windowed() -> io::Result<()> {
    use argus_platform::window::{Event, Window};

    log::set_role(Role::Browser);
    let viewport = Size::new(800, 600);
    log!(
        "starting (windowed); viewport {}x{}",
        viewport.width,
        viewport.height
    );

    let mut content = spawn_child(Role::Content)?;
    let mut net = spawn_child(Role::NetService)?;
    proto::parent_handshake(content.channel(), viewport)?;
    proto::parent_handshake(net.channel(), viewport)?;
    provide_page(&content, SAMPLE_HTML)?;
    log!("children handshook; sample page sent; opening window");

    // Present the first frame.
    let mut frame = request_frame(&content)?;
    let window = Window::open("Argus", viewport);
    window.present(frame.pixels(), frame.size());
    log!("window open — click to send input to content, close to quit");

    loop {
        match window.next_event() {
            Event::MouseDown { x, y } => {
                proto::send(content.channel(), Msg::InputClick { x, y }, &[])?;
                // Repaint (Phase 0 content paints the same color each time).
                frame = request_frame(&content)?;
                window.present(frame.pixels(), frame.size());
            }
            Event::CloseRequested => {
                log!("window closed; shutting down");
                break;
            }
        }
    }

    proto::send(content.channel(), Msg::Shutdown, &[])?;
    proto::send(net.channel(), Msg::Shutdown, &[])?;
    content.wait()?;
    net.wait()?;
    Ok(())
}

/// Confirm every sampled pixel is identical and opaque, returning that color.
fn verify_uniform(fb: &Framebuffer) -> io::Result<Color> {
    let Size { width, height } = fb.size();
    let c0 = fb.pixel(0, 0);
    let samples = [
        (0, 0),
        (width - 1, 0),
        (0, height - 1),
        (width - 1, height - 1),
        (width / 2, height / 2),
    ];
    for (x, y) in samples {
        if fb.pixel(x, y) != c0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("framebuffer not uniform: pixel ({x},{y}) differs from (0,0)"),
            ));
        }
    }
    if c0.a != 255 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "framebuffer is not opaque",
        ));
    }
    Ok(c0)
}
