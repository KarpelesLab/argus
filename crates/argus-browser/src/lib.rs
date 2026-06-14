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

/// A small decorative image embedded as a `data:` URL, for the sample page.
const SAMPLE_IMAGE: &str = include_str!("../sample_image.txt");

/// The built-in sample document rendered by the windowed shell and page dumper.
pub fn sample_html() -> String {
    format!(
        "<!DOCTYPE html><html><head><title>Argus</title><style>\
body {{ background-color: #f4f6fb; color: #1c2430 }}\
h1 {{ color: #2e86de; text-align: center }}\
h2 {{ color: #444 }}\
.card {{ background-color: #ffffff; border: 1px solid #d0d7e2; padding: 16px; margin: 12px 0 }}\
.note {{ background-color: #fff3cd; color: #5a4b00; border: 1px solid #f0d000; padding: 12px }}\
.brand {{ color: #c0392b }}\
.center {{ text-align: center }}\
</style></head><body>\
<h1>Argus</h1>\
<div class=\"card\">\
<p>A web browser written in <strong class=\"brand\">pure Rust</strong>. This page was \
fetched over the network, parsed into a DOM, run through a real CSS cascade, laid out \
with the box model, and painted with shaped, anti-aliased glyphs and decoded images — \
all inside a sandboxed content process.</p>\
<img src=\"{SAMPLE_IMAGE}\" width=\"160\" height=\"90\">\
</div>\
<h2>Box model &amp; images</h2>\
<p class=\"note\">This box has a background, a border, and padding from a class \
selector; the gradient above is a PNG decoded by argus-image. The cascade, inline \
styles, the box model, and images all work.</p>\
<p class=\"center\" style=\"color: #2e7d32\">This line is centered and colored green by \
an inline style attribute.</p>\
<h3>What works</h3>\
<p>Inline styling now works: a <strong>bold strong</strong>, a \
<span style=\"color:#c0392b\">red span</span>, and a <a href=\"https://example.com\">\
blue link</a> all flow inside this paragraph with correct spacing.</p>\
<ul>\
<li>HTML parsing, the DOM, and a real CSS cascade</li>\
<li>The box model: margins, borders, padding, width</li>\
<li>Networking over rsurl, and decoded images</li>\
</ul>\
<hr>\
<ol>\
<li>JavaScript via kataan (pending its embedding API)</li>\
<li>Navigation, tabs, and history</li>\
<li>More CSS: flexbox, grid, and the long tail</li>\
</ol>\
</body></html>"
    )
}

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

/// Ask the net service to fetch `url`, returning the raw body (empty on failure).
fn fetch_bytes(net: &Child, url: &str) -> io::Result<Vec<u8>> {
    proto::send(
        net.channel(),
        Msg::LoadUrl {
            url: url.to_string(),
        },
        &[],
    )?;
    match proto::recv(net.channel())?.0 {
        Msg::ResourceLoaded { status, body } => Ok(if status == 0 { Vec::new() } else { body }),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected ResourceLoaded, got {other:?}"),
        )),
    }
}

fn fetch_html(net: &Child, url: &str) -> io::Result<String> {
    let body = fetch_bytes(net, url)?;
    if body.is_empty() {
        Ok(error_page(
            url,
            "could not load (network error or empty response)",
        ))
    } else {
        Ok(String::from_utf8_lossy(&body).into_owned())
    }
}

fn error_page(url: &str, message: &str) -> String {
    format!(
        "<!DOCTYPE html><html><head><title>Error</title>\
         <style>body{{color:#900}} p{{color:#333}}</style></head><body>\
         <h1>Could not load page</h1><p>{url}</p><p>{message}</p></body></html>"
    )
}

/// The page to show: a fetched URL or the built-in sample.
fn resolve_html(net: &Child, url: Option<&str>) -> String {
    match url {
        Some(u) => fetch_html(net, u).unwrap_or_else(|e| error_page(u, &e.to_string())),
        None => sample_html(),
    }
}

/// Render a page (a fetched `url`, or the sample) to pixels once, off-screen.
/// Returns the framebuffer size and RGBA bytes. Used by the `--dump-page` tool.
pub fn render_once(url: Option<&str>, viewport: Size) -> io::Result<(Size, Vec<u8>)> {
    log::set_role(Role::Browser);
    let mut content = spawn_child(Role::Content)?;
    let mut net = spawn_child(Role::NetService)?;
    proto::parent_handshake(content.channel(), viewport)?;
    proto::parent_handshake(net.channel(), viewport)?;

    let html = resolve_html(&net, url);
    if let Some(bytes) = system_font_bytes() {
        proto::send(content.channel(), Msg::ProvideFont { bytes }, &[])?;
    }
    proto::send(content.channel(), Msg::LoadDocument { html }, &[])?;

    let frame = request_frame(&content, &net)?;
    let pixels = frame.pixels().to_vec();
    let size = frame.size();

    proto::send(content.channel(), Msg::Shutdown, &[])?;
    proto::send(net.channel(), Msg::Shutdown, &[])?;
    content.wait()?;
    net.wait()?;
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
    let frame = request_frame(&content, &net)?;
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

/// Ask `content` to paint, serving any subresource fetches it makes through the
/// `net` service while it renders, and map the shared framebuffer it hands back.
fn request_frame(content: &Child, net: &Child) -> io::Result<Framebuffer> {
    proto::send(content.channel(), Msg::RequestFrame, &[])?;
    let (msg, mut fds) = loop {
        let (msg, fds) = proto::recv(content.channel())?;
        match msg {
            // Content needs a subresource: fetch it and reply, then keep waiting.
            Msg::FetchResource { url } => {
                let body = fetch_bytes(net, &url).unwrap_or_default();
                proto::send(content.channel(), Msg::ResourceData { body }, &[])?;
            }
            other => break (other, fds),
        }
    };
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
/// is available, the headless verifier otherwise. `url` selects the page (the
/// built-in sample when `None`).
#[cfg(target_os = "macos")]
pub fn run_default(url: Option<String>) -> io::Result<()> {
    run_windowed(url)
}

/// See [`run_default`].
#[cfg(not(target_os = "macos"))]
pub fn run_default(_url: Option<String>) -> io::Result<()> {
    run()
}

/// Run the browser with an on-screen window (macOS). Spawns content + net, fetches
/// the page (a URL or the sample), opens a window, presents content's framebuffer,
/// forwards clicks into the sandboxed content process, and repaints — until the
/// window is closed.
#[cfg(target_os = "macos")]
pub fn run_windowed(url: Option<String>) -> io::Result<()> {
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
    let html = resolve_html(&net, url.as_deref());
    provide_page(&content, &html)?;
    log!("children handshook; page sent; opening window");

    // Present the first frame.
    let mut frame = request_frame(&content, &net)?;
    let window = Window::open("Argus", viewport);
    window.present(frame.pixels(), frame.size());
    log!("window open — click to send input to content, close to quit");

    loop {
        match window.next_event() {
            Event::MouseDown { x, y } => {
                proto::send(content.channel(), Msg::InputClick { x, y }, &[])?;
                // Repaint (Phase 0 content paints the same color each time).
                frame = request_frame(&content, &net)?;
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
