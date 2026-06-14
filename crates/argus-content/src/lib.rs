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
use std::collections::HashMap;
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
        links: Vec::new(),
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
                run_page_scripts(&argus_html::parse(&html));
                content.html = Some(html);
            }
            Msg::RequestFrame => {
                let fb = content.render(&channel)?;
                proto::send(&channel, Msg::FrameReady { size: viewport }, &[fb.as_fd()])?;
                content._frame = Some(fb);
            }
            Msg::InputClick { x, y } => {
                let url = content
                    .links
                    .iter()
                    .find(|l| l.contains(x as f32, y as f32))
                    .map(|l| l.href.clone())
                    .unwrap_or_default();
                if !url.is_empty() {
                    log!("link clicked at ({x}, {y}) -> {url}");
                }
                proto::send(&channel, Msg::ClickResult { url }, &[])?;
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
    /// Clickable link regions from the last render, for hit-testing input.
    links: Vec<argus_layout::LinkBox>,
    /// Keeps the last framebuffer mapped so its shared memory stays valid for the
    /// browser after `FrameReady`.
    _frame: Option<Framebuffer>,
}

impl Content {
    /// Paint the current document (or the fallback color) into a fresh framebuffer.
    /// `channel` is used to fetch image subresources from the browser.
    fn render(&mut self, channel: &Channel) -> io::Result<Framebuffer> {
        self.links.clear();
        let mut fb = Framebuffer::create(self.viewport)?;
        let (Some(font), Some(html)) = (&self.font, &self.html) else {
            fb.fill(PHASE0_PAINT);
            return Ok(fb);
        };

        let doc = argus_html::parse(html);

        // Decode every <img> (data: URLs locally, http(s) via the browser).
        let mut images: HashMap<String, argus_image::DecodedImage> = HashMap::new();
        for src in collect_img_srcs(&doc) {
            if images.contains_key(&src) {
                continue;
            }
            let decoded = if src.starts_with("data:") {
                argus_image::decode_data_url(&src)
            } else {
                let bytes = fetch_resource(channel, &src)?;
                (!bytes.is_empty())
                    .then(|| argus_image::decode(&bytes))
                    .flatten()
            };
            if let Some(img) = decoded {
                images.insert(src, img);
            }
        }
        let sizes: argus_layout::ImageSizes = images
            .iter()
            .map(|(k, v)| (k.clone(), (v.width, v.height)))
            .collect();

        fb.fill(Color::WHITE);
        let layout = argus_layout::layout(&doc, font, self.viewport.width as f32, &sizes);
        let list = argus_gfx::DisplayList {
            rects: layout.rects,
            runs: layout.runs,
        };
        let painted =
            argus_gfx::render_display_list(&list, font, self.viewport.width, self.viewport.height);
        argus_gfx::composite_over(fb.pixels_mut(), &painted.pixels);

        // Blit decoded images over the composited page.
        let (vw, vh) = (self.viewport.width, self.viewport.height);
        for ib in &layout.images {
            if let Some(img) = images.get(&ib.src) {
                argus_gfx::blit_rgba(
                    fb.pixels_mut(),
                    vw,
                    vh,
                    ib.x as i32,
                    ib.y as i32,
                    ib.w as u32,
                    ib.h as u32,
                    &img.rgba,
                    img.width,
                    img.height,
                );
            }
        }

        log!(
            "rendered page: {} rects, {} runs, {} images, {} links",
            list.rects.len(),
            list.runs.len(),
            layout.images.len(),
            layout.links.len()
        );
        self.links = layout.links;
        Ok(fb)
    }
}

/// Run every inline `<script>` (no `src`) in document order through kataan,
/// logging its console output. Phase 2 is computation + console only — there are
/// no DOM bindings yet (see `argus-script`).
fn run_page_scripts(doc: &argus_dom::Document) {
    fn collect(doc: &argus_dom::Document, id: argus_dom::NodeId, out: &mut Vec<String>) {
        if let argus_dom::NodeData::Element(e) = &doc.node(id).data {
            if e.name.is_html("script") && e.attr("src").is_none() {
                let mut src = String::new();
                for child in doc.children(id) {
                    if let argus_dom::NodeData::Text(t) = &doc.node(child).data {
                        src.push_str(t);
                    }
                }
                if !src.trim().is_empty() {
                    out.push(src);
                }
            }
        }
        for child in doc.children(id) {
            collect(doc, child, out);
        }
    }

    let mut scripts = Vec::new();
    collect(doc, doc.root(), &mut scripts);
    for src in scripts {
        match argus_script::run_script(&src) {
            Ok(result) => {
                for line in result.console.lines() {
                    log!("console.log: {line}");
                }
            }
            Err(e) => log!("script error: {e}"),
        }
    }
}

/// Collect the `src` of every `<img>` element in document order.
fn collect_img_srcs(doc: &argus_dom::Document) -> Vec<String> {
    fn walk(doc: &argus_dom::Document, id: argus_dom::NodeId, out: &mut Vec<String>) {
        if let argus_dom::NodeData::Element(e) = &doc.node(id).data {
            if e.name.is_html("img") {
                if let Some(src) = e.attr("src") {
                    out.push(src.to_string());
                }
            }
        }
        for child in doc.children(id) {
            walk(doc, child, out);
        }
    }
    let mut out = Vec::new();
    walk(doc, doc.root(), &mut out);
    out
}

/// Ask the browser to fetch a subresource; returns its bytes (empty on failure).
fn fetch_resource(channel: &Channel, url: &str) -> io::Result<Vec<u8>> {
    proto::send(
        channel,
        Msg::FetchResource {
            url: url.to_string(),
        },
        &[],
    )?;
    match proto::recv(channel)?.0 {
        Msg::ResourceData { body } => Ok(body),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected ResourceData, got {other:?}"),
        )),
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
