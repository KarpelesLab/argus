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
.card {{ background-color: #ffffff; border: 1px solid #d0d7e2; padding: 16px; margin: 12px 0; border-radius: 10px }}\
.pill {{ background-color: #2e86de; color: #ffffff; padding: 6px 14px; border-radius: 14px; width: 180px; text-align: center; margin: 8px 0 }}\
.note {{ background-color: #fff3cd; color: #5a4b00; border: 1px solid #f0d000; padding: 12px }}\
.brand {{ color: #c0392b }}\
.center {{ text-align: center }}\
.tbl td, .tbl th {{ border: 1px solid #ccd3df; background-color: #ffffff }}\
.tbl th {{ background-color: #eef1f7 }}\
.row {{ display: flex }}\
.col {{ background-color: #ffffff; border: 1px solid #d0d7e2; padding: 10px; margin: 4px }}\
</style></head><body>\
<h1>Argus</h1>\
<div class=\"pill\">rounded pill</div>\
<div class=\"pill\" style=\"opacity: 0.45\">half-opacity pill</div>\
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
<h3>A table</h3>\
<table class=\"tbl\"><thead><tr><th>Subsystem</th><th>Crate</th><th>Status</th></tr></thead>\
<tbody>\
<tr><td>HTML parser</td><td>argus-html</td><td>working</td></tr>\
<tr><td>CSS cascade</td><td>argus-css</td><td>working</td></tr>\
<tr><td>Layout</td><td>argus-layout</td><td>block + inline + tables</td></tr>\
</tbody></table>\
<h3>Flexbox</h3>\
<div class=\"row\">\
<div class=\"col\">First column in a flex row.</div>\
<div class=\"col\">Second column, sharing the width equally.</div>\
<div class=\"col\">Third column of the flex container.</div>\
</div>\
<h3>Grid</h3>\
<div style=\"display:grid; grid-template-columns: repeat(2, 1fr)\">\
<div class=\"col\">Grid cell one</div>\
<div class=\"col\">Grid cell two</div>\
<div class=\"col\">Grid cell three</div>\
<div class=\"col\">Grid cell four</div>\
</div>\
<script>\
function fib(n){{ return n < 2 ? n : fib(n-1) + fib(n-2); }}\
console.log('kataan ran: fib(20) = ' + fib(20));\
</script>\
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

/// Resolve `href` against the current page `base` (minimal: absolute, protocol-
/// relative, root-relative, and same-directory relative URLs).
fn resolve_url(base: Option<&str>, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    let Some(base) = base else {
        return href.to_string();
    };
    if let Some(rest) = href.strip_prefix("//") {
        let scheme = base.split("://").next().unwrap_or("https");
        return format!("{scheme}://{rest}");
    }
    // Split base into scheme://authority and path.
    let (scheme_auth, path) = match base.find("://") {
        Some(i) => {
            let after = &base[i + 3..];
            match after.find('/') {
                Some(j) => (&base[..i + 3 + j], &after[j..]),
                None => (base, "/"),
            }
        }
        None => return href.to_string(),
    };
    if let Some(abs) = href.strip_prefix('/') {
        format!("{scheme_auth}/{abs}")
    } else {
        // Strip the last path segment (the "directory").
        let dir = &path[..path.rfind('/').map(|i| i + 1).unwrap_or(0)];
        format!("{scheme_auth}{dir}{href}")
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

/// Headless automation: fetch a page (or the sample) and return its parsed DOM
/// serialized in the html5lib `#document` format. Used by the `--dump-dom` tool.
pub fn dump_dom(url: Option<&str>) -> io::Result<String> {
    log::set_role(Role::Browser);
    let mut net = spawn_child(Role::NetService)?;
    proto::parent_handshake(net.channel(), Size::new(800, 600))?;
    let html = resolve_html(&net, url);
    proto::send(net.channel(), Msg::Shutdown, &[])?;
    net.wait()?;
    Ok(argus_html::parse(&html).serialize())
}

/// Headless automation: fetch a page and return its **accessibility tree** — the
/// ARIA role and accessible name of each semantic element (a start on the a11y
/// tree from `docs/subsystems/embedding.md`). Used by `--dump-a11y`.
pub fn dump_a11y(url: Option<&str>) -> io::Result<String> {
    use argus_dom::{Document, NodeData, NodeId};

    log::set_role(Role::Browser);
    let mut net = spawn_child(Role::NetService)?;
    proto::parent_handshake(net.channel(), Size::new(800, 600))?;
    let html = resolve_html(&net, url);
    proto::send(net.channel(), Msg::Shutdown, &[])?;
    net.wait()?;
    let doc = argus_html::parse(&html);

    /// ARIA role implied by an HTML tag (None = generic/presentational).
    fn role_for(tag: &str) -> Option<&'static str> {
        Some(match tag {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "heading",
            "a" => "link",
            "button" => "button",
            "img" => "img",
            "ul" | "ol" => "list",
            "li" => "listitem",
            "nav" => "navigation",
            "main" => "main",
            "header" => "banner",
            "footer" => "contentinfo",
            "input" | "textarea" => "textbox",
            "p" => "paragraph",
            "table" => "table",
            "tr" => "row",
            "td" => "cell",
            "th" => "columnheader",
            "form" => "form",
            _ => return None,
        })
    }

    fn text_of(doc: &Document, id: NodeId, out: &mut String) {
        match &doc.node(id).data {
            NodeData::Text(t) => out.push_str(t),
            NodeData::Element(_) => {
                for c in doc.children(id) {
                    text_of(doc, c, out);
                }
            }
            _ => {}
        }
    }

    fn walk(doc: &Document, id: NodeId, depth: usize, out: &mut String) {
        let mut next_depth = depth;
        if let NodeData::Element(e) = &doc.node(id).data {
            let tag = &*e.name.local;
            if !matches!(tag, "head" | "title" | "style" | "script" | "meta" | "link") {
                if let Some(role) = role_for(tag) {
                    let name = if tag == "img" {
                        e.attr("alt").unwrap_or("").to_string()
                    } else {
                        let mut s = String::new();
                        text_of(doc, id, &mut s);
                        s.split_whitespace().collect::<Vec<_>>().join(" ")
                    };
                    let name = if name.len() > 60 {
                        format!("{}…", &name[..60])
                    } else {
                        name
                    };
                    for _ in 0..depth {
                        out.push_str("  ");
                    }
                    out.push_str(role);
                    if !name.is_empty() {
                        out.push_str(&format!(" \"{name}\""));
                    }
                    out.push('\n');
                    next_depth = depth + 1;
                }
            }
        }
        for c in doc.children(id) {
            walk(doc, c, next_depth, out);
        }
    }

    let mut out = String::from("document\n");
    walk(&doc, doc.root(), 1, &mut out);
    Ok(out)
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

    let (frame, _) = request_frame(&content, &net, url)?;
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
    let (frame, _) = request_frame(&content, &net, None)?;
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
fn request_frame(
    content: &Child,
    net: &Child,
    base: Option<&str>,
) -> io::Result<(Framebuffer, u32)> {
    proto::send(content.channel(), Msg::RequestFrame, &[])?;
    let (msg, mut fds) = loop {
        let (msg, fds) = proto::recv(content.channel())?;
        match msg {
            // Content needs a subresource: resolve it against the page URL, fetch,
            // and reply, then keep waiting for the frame.
            Msg::FetchResource { url } => {
                let target = resolve_url(base, &url);
                let body = fetch_bytes(net, &target).unwrap_or_default();
                proto::send(content.channel(), Msg::ResourceData { body }, &[])?;
            }
            other => break (other, fds),
        }
    };
    let (size, content_height) = match msg {
        Msg::FrameReady {
            size,
            content_height,
        } => (size, content_height),
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
    Ok((Framebuffer::from_fd(fd, size)?, content_height))
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
    let mut current_url = url.clone();
    let html = resolve_html(&net, current_url.as_deref());
    provide_page(&content, &html)?;
    log!("children handshook; page sent; opening window");

    // Present the first frame.
    let (frame, mut content_height) = request_frame(&content, &net, current_url.as_deref())?;
    let window = Window::open("Argus", viewport);
    window.present(frame.pixels(), frame.size());
    log!("window open — click links to navigate, scroll the wheel, close to quit");

    let mut scroll_y: u32 = 0;
    loop {
        match window.next_event() {
            Event::MouseDown { x, y } => {
                proto::send(content.channel(), Msg::InputClick { x, y }, &[])?;
                // Content replies with the click result; navigate if a link was hit.
                if let Msg::ClickResult { url } = proto::recv(content.channel())?.0 {
                    if !url.is_empty() {
                        let target = resolve_url(current_url.as_deref(), &url);
                        log!("navigating to {target}");
                        let page = fetch_html(&net, &target)
                            .unwrap_or_else(|e| error_page(&target, &e.to_string()));
                        provide_page(&content, &page)?;
                        current_url = Some(target);
                        scroll_y = 0;
                        proto::send(content.channel(), Msg::SetScroll { y: 0 }, &[])?;
                        let (frame, h) = request_frame(&content, &net, current_url.as_deref())?;
                        content_height = h;
                        window.present(frame.pixels(), frame.size());
                    }
                }
            }
            Event::Scroll { dy } => {
                let max_scroll = content_height.saturating_sub(viewport.height);
                let next = (scroll_y as i64 - dy as i64).clamp(0, max_scroll as i64) as u32;
                if next != scroll_y {
                    scroll_y = next;
                    proto::send(content.channel(), Msg::SetScroll { y: scroll_y }, &[])?;
                    let (frame, h) = request_frame(&content, &net, current_url.as_deref())?;
                    content_height = h;
                    window.present(frame.pixels(), frame.size());
                }
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

#[cfg(test)]
mod tests {
    use super::resolve_url;

    #[test]
    fn url_resolution() {
        let base = Some("https://ex.com/a/b/page.html");
        assert_eq!(
            resolve_url(base, "https://other.com/x"),
            "https://other.com/x"
        );
        assert_eq!(resolve_url(base, "/top"), "https://ex.com/top");
        assert_eq!(
            resolve_url(base, "sibling.html"),
            "https://ex.com/a/b/sibling.html"
        );
        assert_eq!(resolve_url(base, "//cdn.com/x"), "https://cdn.com/x");
        assert_eq!(
            resolve_url(Some("https://ex.com"), "/p"),
            "https://ex.com/p"
        );
    }
}
