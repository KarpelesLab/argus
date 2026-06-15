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
        doc: None,
        events: Vec::new(),
        storage: std::collections::HashMap::new(),
        focused: None,
        input_values: std::collections::HashMap::new(),
        links: Vec::new(),
        bounds: Vec::new(),
        scroll_y: 0,
        content_height: viewport.height,
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
                // Parse once, run the page's scripts against a JS-side DOM shim, and
                // apply their mutations so layout sees the post-script tree.
                let mut doc = argus_html::parse(&html);
                // localStorage persists across navigations; the event history resets.
                content.events.clear();
                if let Some(console) =
                    argus_domscript::apply_scripts_session(&mut doc, &[], &mut content.storage)
                {
                    for line in console.lines() {
                        log!("console.log: {line}");
                    }
                }
                content.html = Some(html);
                content.focused = None;
                content.input_values.clear();
                content.doc = Some(doc);
            }
            Msg::RequestFrame => {
                let fb = content.render(&channel)?;
                let content_height = content.content_height;
                proto::send(
                    &channel,
                    Msg::FrameReady {
                        size: viewport,
                        content_height,
                    },
                    &[fb.as_fd()],
                )?;
                content._frame = Some(fb);
            }
            Msg::SetScroll { y } => {
                content.scroll_y = y;
            }
            Msg::InputClick { x, y } => {
                let url = content
                    .links
                    .iter()
                    .find(|l| l.contains(x as f32, y as f32))
                    .map(|l| l.href.clone())
                    .unwrap_or_default();
                if url.is_empty() {
                    // Not a link: dispatch a `click` to the deepest id'd element and
                    // re-run the page's scripts with the full interaction history.
                    content.dispatch_click(x as f32, y as f32);
                    content.apply_input_values();
                    content.set_focus(x as f32, y as f32);
                } else {
                    log!("link clicked at ({x}, {y}) -> {url}");
                }
                proto::send(&channel, Msg::ClickResult { url }, &[])?;
            }
            Msg::InputKey { ch } => {
                content.type_key(ch);
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
    /// The original page HTML, re-parsed for deterministic event replay.
    html: Option<String>,
    /// The parsed document, already mutated by its scripts (Phase 2 DOM bindings).
    doc: Option<argus_dom::Document>,
    /// The interaction history replayed on every script run (event sourcing).
    events: Vec<argus_domscript::Interaction>,
    /// `localStorage`, persisted across navigations in this content process.
    storage: std::collections::HashMap<String, String>,
    /// The id of the focused text field (clicked), receiving keystrokes.
    focused: Option<String>,
    /// User-typed values by input id, re-applied after script runs so typing
    /// survives re-renders and event replays.
    input_values: std::collections::HashMap<String, String>,
    /// Clickable link regions from the last render (in screen coords), for input.
    links: Vec<argus_layout::LinkBox>,
    /// Id'd element boxes from the last render (screen coords), for click dispatch.
    bounds: Vec<argus_layout::ElementBound>,
    /// Vertical scroll offset in pixels.
    scroll_y: u32,
    /// Full page height from the last layout (reported to the browser for clamping).
    content_height: u32,
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
        let (Some(font), Some(doc)) = (&self.font, &self.doc) else {
            fb.fill(PHASE0_PAINT);
            return Ok(fb);
        };

        // Decode every <img> (data: URLs locally, http(s) via the browser).
        let mut images: HashMap<String, argus_image::DecodedImage> = HashMap::new();
        for src in collect_img_srcs(doc) {
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
        let mut layout = argus_layout::layout(doc, font, self.viewport.width as f32, &sizes);

        // Apply the scroll offset: shift everything up by the clamped scroll amount
        // so the visible window of the (taller) page is rendered. Links shift too so
        // hit-testing matches what's on screen. The page height is reported back.
        self.content_height = layout.height as u32;
        let max_scroll = (layout.height - self.viewport.height as f32).max(0.0);
        let scroll = (self.scroll_y as f32).min(max_scroll);
        for r in &mut layout.rects {
            r.y -= scroll;
        }
        for r in &mut layout.runs {
            r.baseline -= scroll;
        }
        for im in &mut layout.images {
            im.y -= scroll;
        }
        for l in &mut layout.links {
            l.y -= scroll;
        }
        for b in &mut layout.bounds {
            b.y -= scroll;
        }
        self.bounds = layout.bounds;
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

    /// Hit-test a click against id'd element boxes; if one is hit, append a `click`
    /// interaction and re-run the page's scripts with the full history (deterministic
    /// event replay), so handlers and accumulated state take effect on the next frame.
    fn dispatch_click(&mut self, x: f32, y: f32) {
        // Find the deepest (smallest) id'd box under the cursor.
        let Some(id) = self
            .bounds
            .iter()
            .filter(|b| x >= b.x && x < b.x + b.w && y >= b.y && y < b.y + b.h)
            .min_by(|a, b| (a.w * a.h).partial_cmp(&(b.w * b.h)).unwrap())
            .map(|b| b.id.clone())
        else {
            return;
        };
        let Some(html) = &self.html else { return };
        self.events.push(argus_domscript::Interaction {
            kind: "id".into(),
            val: id,
            event: "click".into(),
        });
        let mut doc = argus_html::parse(html);
        if let Some(console) =
            argus_domscript::apply_scripts_session(&mut doc, &self.events, &mut self.storage)
        {
            for line in console.lines() {
                log!("console.log: {line}");
            }
        }
        self.doc = Some(doc);
    }

    /// Focus the editable text field at `(x, y)`, if any (else clear focus).
    fn set_focus(&mut self, x: f32, y: f32) {
        let id = self
            .bounds
            .iter()
            .filter(|b| x >= b.x && x < b.x + b.w && y >= b.y && y < b.y + b.h)
            .min_by(|a, b| (a.w * a.h).partial_cmp(&(b.w * b.h)).unwrap())
            .map(|b| b.id.clone());
        self.focused = id.filter(|id| self.is_editable_input(id));
    }

    /// Whether the element with `id` is a text-like `<input>` (editable by typing).
    fn is_editable_input(&self, id: &str) -> bool {
        let Some(doc) = &self.doc else { return false };
        let Some(e) = find_element_by_id(doc, id) else {
            return false;
        };
        let Some(el) = doc.node(e).as_element() else {
            return false;
        };
        el.name.is_html("input")
            && matches!(
                el.attr("type").unwrap_or("text"),
                "text" | "search" | "email" | "url" | "tel" | "password"
            )
    }

    /// Apply a typed key to the focused field: update its value and the document.
    fn type_key(&mut self, ch: u32) {
        let Some(id) = self.focused.clone() else {
            return;
        };
        let current = self
            .input_values
            .get(&id)
            .cloned()
            .or_else(|| {
                self.doc.as_ref().and_then(|d| {
                    find_element_by_id(d, &id)
                        .and_then(|n| d.node(n).as_element())
                        .and_then(|e| e.attr("value"))
                        .map(String::from)
                })
            })
            .unwrap_or_default();
        let next = edit_value(&current, ch);
        self.input_values.insert(id, next);
        self.apply_input_values();
    }

    /// Re-apply user-typed values to the document's inputs (so typing survives
    /// script re-runs and re-renders).
    fn apply_input_values(&mut self) {
        let Some(doc) = &mut self.doc else { return };
        for (id, val) in &self.input_values {
            if let Some(n) = find_element_by_id(doc, id) {
                if let argus_dom::NodeData::Element(e) = doc.data_mut(n) {
                    if let Some(a) = e.attrs.iter_mut().find(|a| &*a.name == "value") {
                        a.value = val.clone();
                    } else {
                        e.attrs
                            .push(argus_dom::Attribute::new("value", val.clone()));
                    }
                }
            }
        }
    }
}

/// The first element with `id` in document order.
fn find_element_by_id(doc: &argus_dom::Document, id: &str) -> Option<argus_dom::NodeId> {
    fn walk(
        doc: &argus_dom::Document,
        n: argus_dom::NodeId,
        id: &str,
    ) -> Option<argus_dom::NodeId> {
        if let argus_dom::NodeData::Element(e) = &doc.node(n).data {
            if e.attr("id") == Some(id) {
                return Some(n);
            }
        }
        for c in doc.children(n) {
            if let Some(found) = walk(doc, c, id) {
                return Some(found);
            }
        }
        None
    }
    walk(doc, doc.root(), id)
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

/// Apply a typed key to a text field's current value: `0x08` is backspace, other
/// non-control Unicode scalars append; everything else is ignored.
fn edit_value(current: &str, ch: u32) -> String {
    if ch == 0x08 {
        let mut s = current.to_string();
        s.pop();
        s
    } else if let Some(c) = char::from_u32(ch).filter(|c| !c.is_control()) {
        let mut s = current.to_string();
        s.push(c);
        s
    } else {
        current.to_string()
    }
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

#[cfg(test)]
mod tests {
    use super::edit_value;

    #[test]
    fn edit_value_appends_and_backspaces() {
        assert_eq!(edit_value("ab", 'c' as u32), "abc");
        assert_eq!(edit_value("abc", 0x08), "ab"); // backspace
        assert_eq!(edit_value("", 0x08), ""); // backspace on empty is a no-op
        assert_eq!(edit_value("hi", 0x0D), "hi"); // control chars (enter) ignored
        assert_eq!(edit_value("a", 'Z' as u32), "aZ");
    }
}
