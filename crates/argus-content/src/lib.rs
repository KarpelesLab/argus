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
        checked: std::collections::HashMap::new(),
        selected: std::collections::HashMap::new(),
        details_open: std::collections::HashMap::new(),
        links: Vec::new(),
        submits: Vec::new(),
        bounds: Vec::new(),
        loaded_web_fonts: std::collections::HashSet::new(),
        scroll_y: 0,
        content_height: viewport.height,
        _frame: None,
    };

    loop {
        let (msg, _fds) = proto::recv(&channel)?;
        match msg {
            Msg::ProvideFont { bytes } => {
                let n = bytes.len();
                // The first font is the primary; subsequent ones are glyph fallbacks
                // (emoji/CJK/symbols the primary lacks).
                match content.font.take() {
                    Some(font) => {
                        content.font = Some(font.with_fallback(bytes));
                        log!("added fallback font ({n} bytes)");
                    }
                    None => match Font::from_bytes(bytes) {
                        Ok(font) => {
                            content.font = Some(font);
                            log!("loaded font ({n} bytes)");
                        }
                        Err(e) => log!("WARNING: failed to load font: {e}"),
                    },
                }
            }
            Msg::ProvideMonoFont { bytes } => {
                let n = bytes.len();
                // Attach the monospace face to the primary font (used to shape
                // monospace runs); ignored if the primary hasn't arrived yet.
                match content.font.take() {
                    Some(font) => {
                        content.font = Some(font.with_monospace(bytes));
                        log!("added monospace font ({n} bytes)");
                    }
                    None => log!("WARNING: monospace font arrived before primary"),
                }
            }
            Msg::ProvideStorage { data } => {
                // Seed persisted localStorage (survives browser restarts).
                content.storage = proto::decode_storage(&data);
                log!("seeded localStorage ({} keys)", content.storage.len());
            }
            Msg::LoadDocument { html } => {
                log!("loaded document ({} bytes)", html.len());
                // Parse once, run the page's scripts against a JS-side DOM shim, and
                // apply their mutations so layout sees the post-script tree.
                let mut doc = argus_html::parse(&html);
                // localStorage persists across navigations; the event history resets.
                content.events.clear();
                // Geometry from the previous document's layout (empty on a fresh
                // load) so getBoundingClientRect/offset* read back real boxes.
                let geom = content.geometry();
                let vp = content.window_metrics();
                let cstyle = content.computed_styles();
                if let Some(console) = argus_domscript::apply_scripts_session_geom(
                    &mut doc,
                    &[],
                    &mut content.storage,
                    &geom,
                    vp,
                    &cstyle,
                ) {
                    for line in console.lines() {
                        log!("console.log: {line}");
                    }
                }
                content.html = Some(html);
                content.focused = None;
                content.input_values.clear();
                content.checked.clear();
                content.selected.clear();
                content.details_open.clear();
                content.doc = Some(doc);
                // Report localStorage so the browser can persist it to disk.
                if !content.storage.is_empty() {
                    proto::send(
                        &channel,
                        Msg::StorageChanged {
                            data: proto::encode_storage(&content.storage),
                        },
                        &[],
                    )?;
                }
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
                // A GET link (incl. GET submit buttons) navigates by URL.
                let url = content
                    .links
                    .iter()
                    .find(|l| l.contains(x as f32, y as f32))
                    .map(|l| l.href.clone())
                    .unwrap_or_default();
                // A POST submit button navigates by POSTing the serialized body.
                let post = content
                    .submits
                    .iter()
                    .find(|s| s.contains(x as f32, y as f32))
                    .map(|s| (s.action.clone(), s.body.clone()));
                if let Some(frag) = url.strip_prefix('#') {
                    // Same-page anchor: reply with the target's absolute Y so the
                    // browser scrolls there instead of navigating/reloading.
                    let y = content.fragment_scroll_y(frag);
                    proto::send(
                        &channel,
                        Msg::ClickResult {
                            url: format!("{}{}", proto::SCROLL_TO_PREFIX, y),
                            post_body: Vec::new(),
                        },
                        &[],
                    )?;
                } else if let Some(target) = url.strip_prefix(argus_layout::LABEL_PREFIX) {
                    // Clicking a label focuses/toggles its control; re-render.
                    let target = target.to_string();
                    content.activate_label_target(&target);
                    content.apply_input_values();
                    proto::send(
                        &channel,
                        Msg::ClickResult { url: String::new(), post_body: Vec::new() },
                        &[],
                    )?;
                } else if let Some(rest) = url.strip_prefix(argus_layout::DETAILS_TOGGLE_PREFIX) {
                    // A `<summary>` toggle: flip the details' open state and re-render
                    // (no navigation). Empty ClickResult → the browser re-renders.
                    if let Ok(n) = rest.parse::<usize>() {
                        content.toggle_details(n);
                        content.apply_input_values();
                    }
                    proto::send(
                        &channel,
                        Msg::ClickResult { url: String::new(), post_body: Vec::new() },
                        &[],
                    )?;
                } else if !url.is_empty() {
                    log!("link clicked at ({x}, {y}) -> {url}");
                    proto::send(&channel, Msg::ClickResult { url, post_body: Vec::new() }, &[])?;
                } else if let Some((action, body)) = post {
                    log!("post submit at ({x}, {y}) -> {action}");
                    proto::send(
                        &channel,
                        Msg::ClickResult { url: action, post_body: body.into_bytes() },
                        &[],
                    )?;
                } else {
                    // Not navigation: a form control (checkbox/radio/select) updates,
                    // then (any element) dispatches a `click` and re-runs the page's
                    // scripts with the full interaction history.
                    content.toggle_form_control(x as f32, y as f32);
                    content.dispatch_click(x as f32, y as f32);
                    content.apply_input_values();
                    content.set_focus(x as f32, y as f32);
                    // The click's scripts may have written localStorage — persist it.
                    if !content.storage.is_empty() {
                        proto::send(
                            &channel,
                            Msg::StorageChanged {
                                data: proto::encode_storage(&content.storage),
                            },
                            &[],
                        )?;
                    }
                    proto::send(
                        &channel,
                        Msg::ClickResult { url: String::new(), post_body: Vec::new() },
                        &[],
                    )?;
                }
            }
            Msg::InputKey { ch } => {
                // Enter in a focused field may submit its form: reply with the
                // navigation (GET url, or a POST action+body), empty = just typed —
                // mirroring the InputClick → ClickResult contract.
                let (url, post_body) = content.type_key(ch).unwrap_or_default();
                proto::send(&channel, Msg::ClickResult { url, post_body }, &[])?;
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
    /// User-toggled checkbox/radio state by input id, re-applied like
    /// `input_values` so a checked box survives re-renders and feeds submission.
    checked: std::collections::HashMap<String, bool>,
    /// User-chosen `<select>` option index by select id (clicking a select cycles
    /// to the next option). Re-applied as the option's `selected` attribute.
    selected: std::collections::HashMap<String, usize>,
    /// User-toggled `<details>` open state by the details' document-order index
    /// (clicking its `<summary>` flips it). Re-applied as the `open` attribute.
    details_open: std::collections::HashMap<usize, bool>,
    /// Clickable link regions from the last render (in screen coords), for input.
    links: Vec<argus_layout::LinkBox>,
    /// `method=post` submit-button regions from the last render (screen coords) —
    /// a click here POSTs the form body instead of navigating by URL.
    submits: Vec<argus_layout::SubmitRegion>,
    /// Id'd element boxes from the last render (screen coords), for click dispatch.
    bounds: Vec<argus_layout::ElementBound>,
    /// `@font-face` family keys already fetched + registered (avoid re-fetching).
    loaded_web_fonts: std::collections::HashSet<u32>,
    /// Vertical scroll offset in pixels.
    scroll_y: u32,
    /// Full page height from the last layout (reported to the browser for clamping).
    content_height: u32,
    /// Keeps the last framebuffer mapped so its shared memory stays valid for the
    /// browser after `FrameReady`.
    _frame: Option<Framebuffer>,
}

impl Content {
    /// Per-`id` element boxes from the last layout (`self.bounds`), as the
    /// `(id, [x, y, w, h])` geometry the script shim reads for `getBoundingClientRect`
    /// / `offset*`. Empty before the first layout.
    fn geometry(&self) -> Vec<(String, [f32; 4])> {
        self.bounds
            .iter()
            .map(|b| (b.id.clone(), [b.x, b.y, b.w, b.h]))
            .collect()
    }

    /// Window metrics for the script shim: `[innerWidth, innerHeight, scrollX,
    /// scrollY]` — the content viewport and current page scroll.
    fn window_metrics(&self) -> [u32; 4] {
        [self.viewport.width, self.viewport.height, 0, self.scroll_y]
    }

    /// Resolved CSS per id'd element from the last layout, for `getComputedStyle`.
    fn computed_styles(&self) -> Vec<(String, Vec<(String, String)>)> {
        self.bounds
            .iter()
            .map(|b| (b.id.clone(), b.computed.clone()))
            .collect()
    }

    /// Paint the current document (or the fallback color) into a fresh framebuffer.
    /// `channel` is used to fetch image subresources from the browser.
    fn render(&mut self, channel: &Channel) -> io::Result<Framebuffer> {
        self.links.clear();
        // Fetch + register any not-yet-loaded `@font-face` web fonts before paint.
        self.load_web_fonts(channel)?;
        // Mark the focused field so the UA stylesheet draws its focus outline.
        self.apply_focus();
        let mut fb = Framebuffer::create(self.viewport)?;
        let (Some(font), Some(doc)) = (&self.font, &self.doc) else {
            fb.fill(PHASE0_PAINT);
            return Ok(fb);
        };

        // Decode every <img> (data: URLs locally, http(s) via the browser).
        let mut images: HashMap<String, argus_image::DecodedImage> = HashMap::new();
        for src in collect_img_srcs(doc, self.viewport.width as f32) {
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
        for s in &mut layout.submits {
            s.y -= scroll;
        }
        for b in &mut layout.bounds {
            b.y -= scroll;
        }
        self.bounds = layout.bounds;
        self.submits = layout.submits;
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
                let (cx, cy, cw, ch) = ib.crop;
                argus_gfx::blit_rgba_cropped(
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
                    (cx * img.width as f32) as u32,
                    (cy * img.height as f32) as u32,
                    (cw * img.width as f32).max(1.0) as u32,
                    (ch * img.height as f32).max(1.0) as u32,
                    ib.clip
                        .map(|[x, y, w, h]| [x as i32, y as i32, w as i32, h as i32]),
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

    /// Fetch and register any `@font-face` web fonts declared by the document that
    /// haven't been loaded yet, keyed by `argus_css::family_key` so the style engine
    /// and the font registry agree. Data-URL sources decode locally; others fetch
    /// through the browser. Each family is attempted once.
    fn load_web_fonts(&mut self, channel: &Channel) -> io::Result<()> {
        let (Some(doc), Some(_)) = (&self.doc, &self.font) else {
            return Ok(());
        };
        let faces = argus_style::author_stylesheet(doc).font_faces;
        for face in faces {
            // Key the face by family + its declared weight/style so a bold/italic
            // run selects the matching face (the style engine keys runs the same).
            let key = argus_css::style_variant(
                argus_css::family_key(&face.family),
                face.bold,
                face.italic,
            );
            if !self.loaded_web_fonts.insert(key) {
                continue; // already attempted
            }
            let bytes = if face.src_url.starts_with("data:") {
                argus_image::decode_data_url_bytes(&face.src_url).unwrap_or_default()
            } else {
                fetch_resource(channel, &face.src_url)?
            };
            if bytes.is_empty() {
                log!("web font '{}' fetch failed", face.family);
                continue;
            }
            // WOFF2 (Brotli) and WOFF (zlib) wrap an sfnt; unwrap to a bare sfnt the
            // TTF/OTF parser can read. Raw sfnt bytes pass through unchanged.
            let bytes = argus_image::woff2_to_sfnt(&bytes)
                .or_else(|| argus_image::woff_to_sfnt(&bytes))
                .unwrap_or(bytes);
            if let Some(font) = self.font.take() {
                self.font = Some(font.with_web_font(key, bytes));
                log!(
                    "loaded web font '{}' ({} bytes)",
                    face.family,
                    face.src_url.len()
                );
            }
        }
        Ok(())
    }

    /// The absolute document Y to scroll to for a `#fragment` anchor: the target
    /// id's box top in document coords (its on-screen y plus the current scroll).
    /// An empty fragment (`#`/`#top`) or unknown id scrolls to the top.
    fn fragment_scroll_y(&self, frag: &str) -> u32 {
        if frag.is_empty() {
            return 0;
        }
        self.bounds
            .iter()
            .find(|b| b.id == frag)
            .map(|b| (b.y + self.scroll_y as f32).max(0.0) as u32)
            .unwrap_or(0)
    }

    /// The id of the deepest (smallest) id'd element box under `(x, y)`.
    fn hit_id(&self, x: f32, y: f32) -> Option<String> {
        self.bounds
            .iter()
            .filter(|b| x >= b.x && x < b.x + b.w && y >= b.y && y < b.y + b.h)
            .min_by(|a, b| (a.w * a.h).partial_cmp(&(b.w * b.h)).unwrap())
            .map(|b| b.id.clone())
    }

    /// Whether the input `id` is currently checked: the user-toggled state if any,
    /// else the document's `checked` attribute.
    fn is_checked(&self, id: &str, el: &argus_dom::ElementData) -> bool {
        self.checked
            .get(id)
            .copied()
            .unwrap_or_else(|| el.attr("checked").is_some())
    }

    /// The ids of all id'd `input[type=radio]` controls with the given `name`
    /// (a radio group), so selecting one can clear the rest.
    fn radio_group_ids(&self, name: &str) -> Vec<String> {
        let Some(doc) = &self.doc else { return Vec::new() };
        fn walk(doc: &argus_dom::Document, id: argus_dom::NodeId, name: &str, out: &mut Vec<String>) {
            if let Some(e) = doc.node(id).as_element() {
                if e.name.is_html("input")
                    && e.attr("type") == Some("radio")
                    && e.attr("name") == Some(name)
                {
                    if let Some(eid) = e.attr("id") {
                        out.push(eid.to_string());
                    }
                }
            }
            for c in doc.children(id) {
                walk(doc, c, name, out);
            }
        }
        let mut out = Vec::new();
        walk(doc, doc.root(), name, &mut out);
        out
    }

    /// The `<option>` node ids of the id'd `<select>`, in document order.
    fn select_options(&self, select_id: &str) -> Vec<argus_dom::NodeId> {
        let Some(doc) = &self.doc else { return Vec::new() };
        let Some(sel) = find_element_by_id(doc, select_id) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        collect_options(doc, sel, &mut out);
        out
    }

    /// The currently-selected option index of a `<select>`: the user's choice if
    /// any, else the first option carrying `selected`, else 0.
    fn current_selected_index(&self, select_id: &str) -> usize {
        if let Some(&i) = self.selected.get(select_id) {
            return i;
        }
        let Some(doc) = &self.doc else { return 0 };
        self.select_options(select_id)
            .iter()
            .position(|&o| {
                doc.node(o)
                    .as_element()
                    .is_some_and(|e| e.attr("selected").is_some())
            })
            .unwrap_or(0)
    }

    /// Whether the `n`-th `<details>` (document order) is currently open: the user's
    /// toggle if any, else the document's `open` attribute.
    fn is_details_open(&self, n: usize) -> bool {
        if let Some(&o) = self.details_open.get(&n) {
            return o;
        }
        let Some(doc) = &self.doc else { return false };
        nth_details(doc, n)
            .and_then(|d| doc.node(d).as_element())
            .is_some_and(|e| e.attr("open").is_some())
    }

    /// Flip the open state of the `n`-th `<details>` (clicking its `<summary>`).
    fn toggle_details(&mut self, n: usize) {
        let next = !self.is_details_open(n);
        self.details_open.insert(n, next);
    }

    /// If `(x, y)` hits an id'd form control, mutate it: a checkbox flips, a radio
    /// selects within its name-group, a `<select>` advances to its next option
    /// (wrapping). Returns whether one was handled. The change persists via the
    /// `checked`/`selected` maps (re-applied each render) and feeds both the visible
    /// state and form submission.
    fn toggle_form_control(&mut self, x: f32, y: f32) -> bool {
        match self.hit_id(x, y) {
            Some(id) => self.toggle_control_by_id(&id),
            None => false,
        }
    }

    /// Mutate the id'd control: a checkbox flips, a radio selects within its
    /// name-group, a `<select>` advances its option. Returns whether it applied.
    fn toggle_control_by_id(&mut self, id: &str) -> bool {
        let Some(doc) = &self.doc else { return false };
        let Some(el) = find_element_by_id(doc, id).and_then(|n| doc.node(n).as_element()) else {
            return false;
        };
        if el.name.is_html("select") {
            let n = self.select_options(id).len();
            if n == 0 {
                return false;
            }
            let next = (self.current_selected_index(id) + 1) % n;
            self.selected.insert(id.to_string(), next);
            return true;
        }
        if !el.name.is_html("input") {
            return false;
        }
        match el.attr("type").unwrap_or("text") {
            "checkbox" => {
                let next = !self.is_checked(id, el);
                self.checked.insert(id.to_string(), next);
                true
            }
            "radio" => {
                // Clear the rest of the group, then select this one.
                if let Some(name) = el.attr("name").map(str::to_string) {
                    for gid in self.radio_group_ids(&name) {
                        self.checked.insert(gid, false);
                    }
                }
                self.checked.insert(id.to_string(), true);
                true
            }
            _ => false,
        }
    }

    /// Clicking a `<label>` activates its associated control: focus a text field,
    /// or toggle a checkbox/radio/select.
    fn activate_label_target(&mut self, id: &str) {
        if self.is_editable_input(id) {
            self.focused = Some(id.to_string());
        } else {
            self.toggle_control_by_id(id);
        }
    }

    /// Hit-test a click against id'd element boxes; if one is hit, append a `click`
    /// interaction and re-run the page's scripts with the full history (deterministic
    /// event replay), so handlers and accumulated state take effect on the next frame.
    fn dispatch_click(&mut self, x: f32, y: f32) {
        // Find the deepest (smallest) id'd box under the cursor.
        let Some(id) = self.hit_id(x, y) else {
            return;
        };
        let Some(html) = &self.html else { return };
        self.events.push(argus_domscript::Interaction {
            kind: "id".into(),
            val: id,
            event: "click".into(),
        });
        let mut doc = argus_html::parse(html);
        let geom = self.geometry();
        let vp = self.window_metrics();
        let cstyle = self.computed_styles();
        if let Some(console) = argus_domscript::apply_scripts_session_geom(
            &mut doc,
            &self.events,
            &mut self.storage,
            &geom,
            vp,
            &cstyle,
        ) {
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
        find_element_by_id(doc, id)
            .and_then(|e| doc.node(e).as_element())
            .is_some_and(is_editable_el)
    }

    /// Editable text fields (`<input>` text-likes and `<textarea>`) by id, in
    /// document order — the Tab focus ring.
    fn editable_fields_in_order(&self) -> Vec<String> {
        let Some(doc) = &self.doc else { return Vec::new() };
        fn walk(doc: &argus_dom::Document, n: argus_dom::NodeId, out: &mut Vec<String>) {
            if let Some(e) = doc.node(n).as_element() {
                if is_editable_el(e) {
                    if let Some(id) = e.attr("id") {
                        out.push(id.to_string());
                    }
                }
            }
            for c in doc.children(n) {
                walk(doc, c, out);
            }
        }
        let mut out = Vec::new();
        walk(doc, doc.root(), &mut out);
        out
    }

    /// Mark the focused element with the `__argus_focus` sentinel attribute (and
    /// strip it from any other) so the UA stylesheet draws a focus outline. Called
    /// each render before layout.
    fn apply_focus(&mut self) {
        // Collect element nodes first (immutable), then mutate (disjoint borrows).
        let Some(doc) = &self.doc else { return };
        fn collect(doc: &argus_dom::Document, n: argus_dom::NodeId, out: &mut Vec<argus_dom::NodeId>) {
            out.push(n);
            for c in doc.children(n) {
                collect(doc, c, out);
            }
        }
        let mut nodes = Vec::new();
        collect(doc, doc.root(), &mut nodes);
        let focused = self.focused.clone();
        let Some(doc) = &mut self.doc else { return };
        for n in nodes {
            if let argus_dom::NodeData::Element(e) = doc.data_mut(n) {
                let is_focused = focused.as_deref() == e.attr("id");
                let has = e.attrs.iter().any(|a| &*a.name == "__argus_focus");
                if is_focused && focused.is_some() && !has {
                    e.attrs.push(argus_dom::Attribute::new("__argus_focus", ""));
                } else if (!is_focused || focused.is_none()) && has {
                    e.attrs.retain(|a| &*a.name != "__argus_focus");
                }
            }
        }
    }

    /// Apply a typed key to the focused field: update its value and the document.
    /// On Enter inside a form, returns the submission navigation as
    /// `(url, post_body)`: a GET form gives `(action?query, [])`, a POST form gives
    /// `(action, urlencoded-body)`. Returns `None` for ordinary typing.
    fn type_key(&mut self, ch: u32) -> Option<(String, Vec<u8>)> {
        let id = self.focused.clone()?;
        // Tab moves focus to the next editable field (wrapping).
        if ch == 0x09 {
            let fields = self.editable_fields_in_order();
            if let Some(pos) = fields.iter().position(|f| *f == id) {
                self.focused = Some(fields[(pos + 1) % fields.len()].clone());
            }
            return None;
        }
        let is_textarea = self
            .doc
            .as_ref()
            .and_then(|d| find_element_by_id(d, &id).map(|n| (d, n)))
            .and_then(|(d, n)| d.node(n).as_element())
            .is_some_and(|e| e.name.is_html("textarea"));
        // Enter (CR/LF) implicitly submits a single-line field's form; in a
        // `<textarea>` it inserts a newline instead (handled below).
        if (ch == 0x0D || ch == 0x0A) && !is_textarea {
            let doc = self.doc.as_ref()?;
            let node = find_element_by_id(doc, &id)?;
            if let Some(url) = argus_layout::form_get_url_for_field(doc, node) {
                return Some((url, Vec::new()));
            }
            let (action, body) = argus_layout::form_post_data_for_field(doc, node)?;
            return Some((action, body.into_bytes()));
        }
        // Current value: the in-progress edit, else the DOM `value` attr, else (for
        // a textarea) its initial text content.
        let current = self
            .input_values
            .get(&id)
            .cloned()
            .or_else(|| {
                self.doc.as_ref().and_then(|d| {
                    let n = find_element_by_id(d, &id)?;
                    let e = d.node(n).as_element()?;
                    match e.attr("value") {
                        Some(v) => Some(v.to_string()),
                        None if e.name.is_html("textarea") => Some(node_text(d, n)),
                        None => None,
                    }
                })
            })
            .unwrap_or_default();
        // A textarea Enter appends a newline (edit_value drops control chars).
        let next = if ch == 0x0D || ch == 0x0A {
            format!("{current}\n")
        } else {
            edit_value(&current, ch)
        };
        self.input_values.insert(id, next);
        self.apply_input_values();
        None
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
        // Re-apply user-toggled checkbox/radio state as the `checked` attribute,
        // which both layout (the mark) and form submission read.
        for (id, &on) in &self.checked {
            if let Some(n) = find_element_by_id(doc, id) {
                if let argus_dom::NodeData::Element(e) = doc.data_mut(n) {
                    let has = e.attrs.iter().any(|a| &*a.name == "checked");
                    if on && !has {
                        e.attrs.push(argus_dom::Attribute::new("checked", ""));
                    } else if !on && has {
                        e.attrs.retain(|a| &*a.name != "checked");
                    }
                }
            }
        }
        // Re-apply user `<select>` choices: put `selected` on the chosen option and
        // strip it from the rest (layout + submission read `selected`).
        for (id, &idx) in &self.selected {
            let Some(sel) = find_element_by_id(doc, id) else {
                continue;
            };
            let mut opts = Vec::new();
            collect_options(doc, sel, &mut opts);
            for (i, &opt) in opts.iter().enumerate() {
                if let argus_dom::NodeData::Element(e) = doc.data_mut(opt) {
                    let has = e.attrs.iter().any(|a| &*a.name == "selected");
                    if i == idx && !has {
                        e.attrs.push(argus_dom::Attribute::new("selected", ""));
                    } else if i != idx && has {
                        e.attrs.retain(|a| &*a.name != "selected");
                    }
                }
            }
        }
        // Re-apply user `<details>` open/closed toggles as the `open` attribute,
        // which layout reads to show or hide the details body.
        for (&n, &open) in &self.details_open {
            if let Some(d) = nth_details(doc, n) {
                if let argus_dom::NodeData::Element(e) = doc.data_mut(d) {
                    let has = e.attrs.iter().any(|a| &*a.name == "open");
                    if open && !has {
                        e.attrs.push(argus_dom::Attribute::new("open", ""));
                    } else if !open && has {
                        e.attrs.retain(|a| &*a.name != "open");
                    }
                }
            }
        }
    }
}

/// The first element with `id` in document order.
/// The `n`-th `<details>` element in document (pre-order) order — matching the
/// index `argus_layout::summary_toggle_href` assigns. `None` if out of range.
fn nth_details(doc: &argus_dom::Document, n: usize) -> Option<argus_dom::NodeId> {
    fn walk(
        doc: &argus_dom::Document,
        node: argus_dom::NodeId,
        n: usize,
        count: &mut usize,
    ) -> Option<argus_dom::NodeId> {
        if doc.node(node).as_element().is_some_and(|e| e.name.is_html("details")) {
            if *count == n {
                return Some(node);
            }
            *count += 1;
        }
        for c in doc.children(node) {
            if let Some(found) = walk(doc, c, n, count) {
                return Some(found);
            }
        }
        None
    }
    walk(doc, doc.root(), n, &mut 0)
}

/// Whether `e` is a text-editable field — a text-like `<input>` or a `<textarea>`.
fn is_editable_el(e: &argus_dom::ElementData) -> bool {
    e.name.is_html("textarea")
        || (e.name.is_html("input")
            && matches!(
                e.attr("type").unwrap_or("text"),
                "text" | "search" | "email" | "url" | "tel" | "password"
            ))
}

/// The concatenated text-node content of `node`'s subtree (a `<textarea>`'s
/// initial value, whitespace and newlines preserved).
fn node_text(doc: &argus_dom::Document, node: argus_dom::NodeId) -> String {
    let mut out = String::new();
    fn walk(doc: &argus_dom::Document, n: argus_dom::NodeId, out: &mut String) {
        match &doc.node(n).data {
            argus_dom::NodeData::Text(t) => out.push_str(t),
            _ => {
                for c in doc.children(n) {
                    walk(doc, c, out);
                }
            }
        }
    }
    walk(doc, node, &mut out);
    out
}

/// Collect the `<option>` descendant node ids of `node`, in document order.
fn collect_options(doc: &argus_dom::Document, node: argus_dom::NodeId, out: &mut Vec<argus_dom::NodeId>) {
    for c in doc.children(node) {
        if doc.node(c).as_element().is_some_and(|e| e.name.is_html("option")) {
            out.push(c);
        }
        collect_options(doc, c, out);
    }
}

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
fn collect_img_srcs(doc: &argus_dom::Document, viewport_w: f32) -> Vec<String> {
    fn walk(
        doc: &argus_dom::Document,
        id: argus_dom::NodeId,
        viewport_w: f32,
        out: &mut Vec<String>,
    ) {
        if let argus_dom::NodeData::Element(e) = &doc.node(id).data {
            if e.name.is_html("img") {
                // Fetch the same URL layout resolves (`<picture>` source, else
                // the img's own `src`/`srcset`).
                if let Some(src) = argus_layout::resolve_img_url(doc, id, viewport_w) {
                    out.push(src);
                }
            } else if e.name.is_html("video") {
                // A `<video>` renders its `poster` (an image) or — once the video
                // pixel codecs land — the first frame of its own `src`. Both are
                // decoded through `argus_image::decode` (which routes container
                // bytes to the demux pipeline). Keyed by the raw attribute, same
                // as `<img>`; the browser resolves relative URLs at fetch time.
                if let Some(poster) = e.attr("poster") {
                    out.push(poster.to_string());
                }
                if let Some(src) = e.attr("src") {
                    out.push(src.to_string());
                }
            } else if e.name.is_html("source") {
                // A `<source>` inside `<video>`/`<audio>` (the `<picture>` case is
                // handled via `resolve_img_url` on the sibling `<img>`).
                if let Some(parent) = doc.node(id).parent() {
                    if let argus_dom::NodeData::Element(pe) = &doc.node(parent).data {
                        if pe.name.is_html("video") || pe.name.is_html("audio") {
                            if let Some(src) = e.attr("src") {
                                out.push(src.to_string());
                            }
                        }
                    }
                }
            }
        }
        for child in doc.children(id) {
            walk(doc, child, viewport_w, out);
        }
    }
    let mut out = Vec::new();
    walk(doc, doc.root(), viewport_w, &mut out);
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
    use super::{find_element_by_id, Content};
    use argus_geometry::Size;

    /// Build a headless `Content` over `html` with the given id'd hit-boxes (no
    /// font/channel needed for the input-state logic).
    fn headless(html: &str, bounds: Vec<argus_layout::ElementBound>) -> Content {
        Content {
            viewport: Size::new(800, 600),
            font: None,
            html: Some(html.to_string()),
            doc: Some(argus_html::parse(html)),
            events: Vec::new(),
            storage: std::collections::HashMap::new(),
            focused: None,
            input_values: std::collections::HashMap::new(),
            checked: std::collections::HashMap::new(),
            selected: std::collections::HashMap::new(),
            details_open: std::collections::HashMap::new(),
            links: Vec::new(),
            submits: Vec::new(),
            bounds,
            loaded_web_fonts: std::collections::HashSet::new(),
            scroll_y: 0,
            content_height: 600,
            _frame: None,
        }
    }

    fn box_at(id: &str, x: f32, y: f32) -> argus_layout::ElementBound {
        argus_layout::ElementBound {
            id: id.to_string(),
            x,
            y,
            w: 16.0,
            h: 16.0,
            computed: Vec::new(),
        }
    }

    fn dom_checked(c: &Content, id: &str) -> bool {
        let doc = c.doc.as_ref().unwrap();
        find_element_by_id(doc, id)
            .and_then(|n| doc.node(n).as_element())
            .is_some_and(|e| e.attr("checked").is_some())
    }

    #[test]
    fn clicking_a_checkbox_toggles_and_persists_checked_state() {
        let mut c = headless(
            "<input id=\"a\" type=\"checkbox\" name=\"a\">\
             <input id=\"b\" type=\"checkbox\" name=\"b\" checked>",
            vec![box_at("a", 10.0, 10.0), box_at("b", 40.0, 10.0)],
        );
        // Unchecked 'a' → checked after a click, and the DOM attr is applied.
        assert!(c.toggle_form_control(12.0, 12.0));
        c.apply_input_values();
        assert!(dom_checked(&c, "a"), "checkbox a now checked");
        // Click 'a' again → unchecked.
        assert!(c.toggle_form_control(12.0, 12.0));
        c.apply_input_values();
        assert!(!dom_checked(&c, "a"), "checkbox a toggled back off");
        // Pre-checked 'b' → clicking unchecks it (the attr is removed).
        assert!(c.toggle_form_control(42.0, 12.0));
        c.apply_input_values();
        assert!(!dom_checked(&c, "b"), "pre-checked b unchecked");
        // A click that hits nothing toggles nothing.
        assert!(!c.toggle_form_control(500.0, 500.0));
    }

    #[test]
    fn clicking_a_radio_selects_it_and_clears_the_group() {
        let mut c = headless(
            "<input id=\"x\" type=\"radio\" name=\"g\" checked>\
             <input id=\"y\" type=\"radio\" name=\"g\">\
             <input id=\"z\" type=\"radio\" name=\"other\">",
            vec![
                box_at("x", 10.0, 10.0),
                box_at("y", 40.0, 10.0),
                box_at("z", 70.0, 10.0),
            ],
        );
        // Select 'y' → 'x' clears (same group 'g'), 'z' (other group) untouched.
        assert!(c.toggle_form_control(42.0, 12.0));
        c.apply_input_values();
        assert!(dom_checked(&c, "y"), "y selected");
        assert!(!dom_checked(&c, "x"), "x cleared in group g");
        assert!(!dom_checked(&c, "z"), "z is a different group, stays off");
    }

    #[test]
    fn clicking_a_select_cycles_through_its_options() {
        // The selected option index (which carries the `selected` attr).
        fn selected_idx(c: &Content) -> Option<usize> {
            let doc = c.doc.as_ref().unwrap();
            let sel = find_element_by_id(doc, "s").unwrap();
            let mut opts = Vec::new();
            super::collect_options(doc, sel, &mut opts);
            opts.iter().position(|&o| {
                doc.node(o)
                    .as_element()
                    .is_some_and(|e| e.attr("selected").is_some())
            })
        }

        let mut c = headless(
            "<select id=\"s\" name=\"k\">\
               <option value=\"a\">A</option>\
               <option value=\"b\" selected>B</option>\
               <option value=\"c\">C</option>\
             </select>",
            vec![box_at("s", 10.0, 10.0)],
        );
        // Starts on B (index 1). Each click advances, wrapping 2 -> 0.
        assert_eq!(selected_idx(&c), Some(1), "initial selected option");
        assert!(c.toggle_form_control(12.0, 12.0));
        c.apply_input_values();
        assert_eq!(selected_idx(&c), Some(2), "advanced to C");
        assert!(c.toggle_form_control(12.0, 12.0));
        c.apply_input_values();
        assert_eq!(selected_idx(&c), Some(0), "wrapped to A");
        // The `selected` attr now sits on option A, so form submission (which reads
        // it, tested in argus-layout) serializes `k=a`.
    }

    #[test]
    fn toggling_details_flips_its_open_attribute() {
        fn nth_open(c: &Content, n: usize) -> bool {
            let doc = c.doc.as_ref().unwrap();
            super::nth_details(doc, n)
                .and_then(|d| doc.node(d).as_element())
                .is_some_and(|e| e.attr("open").is_some())
        }
        let mut c = headless(
            "<details><summary>A</summary><p>x</p></details>\
             <details open><summary>B</summary><p>y</p></details>",
            vec![],
        );
        // details 0 starts closed, details 1 starts open.
        assert!(!nth_open(&c, 0));
        assert!(nth_open(&c, 1));
        // Toggle 0 open and 1 closed; the `open` attr tracks each.
        c.toggle_details(0);
        c.toggle_details(1);
        c.apply_input_values();
        assert!(nth_open(&c, 0), "details 0 now open");
        assert!(!nth_open(&c, 1), "details 1 now closed");
        // Toggling 0 again closes it.
        c.toggle_details(0);
        c.apply_input_values();
        assert!(!nth_open(&c, 0), "details 0 closed again");
    }

    #[test]
    fn fragment_anchor_resolves_to_target_document_y() {
        let mut c = headless(
            "<a href=\"#sec\">jump</a><h2 id=\"sec\">Section</h2>",
            vec![box_at("sec", 0.0, 300.0)],
        );
        // With no scroll, the target's document Y is its on-screen Y.
        assert_eq!(c.fragment_scroll_y("sec"), 300);
        // Scrolled down 120px: the box's screen Y is 120 less, so document Y holds.
        c.scroll_y = 120;
        c.bounds[0].y = 180.0; // box now drawn 120px higher on screen
        assert_eq!(c.fragment_scroll_y("sec"), 300, "screen y + scroll = doc y");
        // An empty fragment or unknown id scrolls to the top.
        assert_eq!(c.fragment_scroll_y(""), 0);
        assert_eq!(c.fragment_scroll_y("missing"), 0);
    }

    #[test]
    fn typing_into_a_textarea_edits_its_value_including_newlines() {
        fn dom_value(c: &Content) -> String {
            let doc = c.doc.as_ref().unwrap();
            find_element_by_id(doc, "t")
                .and_then(|n| doc.node(n).as_element())
                .and_then(|e| e.attr("value"))
                .unwrap_or("")
                .to_string()
        }
        let mut c = headless(
            "<textarea id=\"t\">init</textarea>",
            vec![box_at("t", 0.0, 0.0)],
        );
        c.focused = Some("t".to_string());
        // A textarea is editable (unlike before), and editing builds on the initial
        // text content.
        assert!(c.is_editable_input("t"));
        assert_eq!(c.type_key('!' as u32), None);
        assert_eq!(dom_value(&c), "init!");
        // Enter inserts a newline and does NOT submit (returns None).
        assert_eq!(c.type_key(0x0A), None, "Enter in a textarea is a newline");
        assert_eq!(dom_value(&c), "init!\n");
        c.type_key('x' as u32);
        assert_eq!(dom_value(&c), "init!\nx");
        // Backspace removes the last char.
        c.type_key(0x08);
        assert_eq!(dom_value(&c), "init!\n");
    }

    #[test]
    fn tab_cycles_editable_focus_and_marks_it() {
        let mut c = headless(
            "<input id=\"a\"><input id=\"b\"><input id=\"c\" type=\"checkbox\">",
            vec![],
        );
        c.focused = Some("a".to_string());
        // Tab advances a → b (skipping the non-editable checkbox).
        c.type_key(0x09);
        assert_eq!(c.focused.as_deref(), Some("b"));
        // Tab from the last editable wraps back to the first.
        c.type_key(0x09);
        assert_eq!(c.focused.as_deref(), Some("a"));
        // apply_focus marks the focused element (and only it) for the UA outline.
        fn marked(c: &Content, id: &str) -> bool {
            let doc = c.doc.as_ref().unwrap();
            find_element_by_id(doc, id)
                .and_then(|n| doc.node(n).as_element())
                .is_some_and(|e| e.attr("__argus_focus").is_some())
        }
        c.apply_focus();
        assert!(marked(&c, "a"), "focused field marked");
        assert!(!marked(&c, "b"), "unfocused field not marked");
        // Clearing focus removes the marker.
        c.focused = None;
        c.apply_focus();
        assert!(!marked(&c, "a"), "marker cleared when focus leaves");
    }

    #[test]
    fn clicking_a_label_activates_its_control() {
        fn checked(c: &Content, id: &str) -> bool {
            let doc = c.doc.as_ref().unwrap();
            find_element_by_id(doc, id)
                .and_then(|n| doc.node(n).as_element())
                .is_some_and(|e| e.attr("checked").is_some())
        }
        // A label for a checkbox toggles it.
        let mut c = headless(
            "<label for=\"x\">Agree</label><input id=\"x\" type=\"checkbox\">",
            vec![],
        );
        c.activate_label_target("x");
        c.apply_input_values();
        assert!(checked(&c, "x"), "label toggled the checkbox on");
        c.activate_label_target("x");
        c.apply_input_values();
        assert!(!checked(&c, "x"), "label toggled it back off");

        // A label for a text field focuses it.
        let mut c2 = headless("<label for=\"t\">Name</label><input id=\"t\">", vec![]);
        c2.activate_label_target("t");
        assert_eq!(c2.focused.as_deref(), Some("t"), "label focused the text field");
    }

    #[test]
    fn woff2_decodes_to_a_parseable_font() {
        // Validate the WOFF2 decoder against any real .woff2 on disk: the
        // reconstructed sfnt must parse as a font and shape text. Skips if none
        // is present (so CI without sample fonts still passes).
        let mut dir_candidates: Vec<std::path::PathBuf> = Vec::new();
        for base in ["/private/tmp", "/tmp"] {
            if let Ok(rd) = std::fs::read_dir(base) {
                for e in rd.flatten() {
                    if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        dir_candidates.push(e.path().join("doc/static.files"));
                    }
                }
            }
        }
        let mut tested = false;
        for dir in dir_candidates {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()) != Some("woff2") {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&p) else {
                    continue;
                };
                let sfnt = argus_image::woff2_to_sfnt(&bytes)
                    .unwrap_or_else(|| panic!("woff2 decode failed for {p:?}"));
                let font = argus_gfx::Font::from_bytes(sfnt)
                    .unwrap_or_else(|e| panic!("reconstructed sfnt rejected ({p:?}): {e}"));
                assert!(font.measure("Hello", 16.0) > 0.0, "shapes text from {p:?}");
                tested = true;
            }
            if tested {
                break;
            }
        }
        if !tested {
            eprintln!("no .woff2 sample found; skipping end-to-end check");
        }
    }

    #[test]
    fn edit_value_appends_and_backspaces() {
        assert_eq!(edit_value("ab", 'c' as u32), "abc");
        assert_eq!(edit_value("abc", 0x08), "ab"); // backspace
        assert_eq!(edit_value("", 0x08), ""); // backspace on empty is a no-op
        assert_eq!(edit_value("hi", 0x0D), "hi"); // control chars (enter) ignored
        assert_eq!(edit_value("a", 'Z' as u32), "aZ");
    }
}
