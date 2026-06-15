//! Layout engine (Phase 1 slice).
//!
//! Block-and-inline layout producing a display list: filled background and border
//! rects for block boxes plus positioned, colored, aligned text runs. Block boxes
//! stack vertically with their cascaded margins; each box honors width, padding,
//! and borders (the standard content/padding/border box geometry). Inline content
//! is greedily broken into lines that fit the content width, measured with the real
//! font, and aligned per `text-align`. Styles come from the `argus-style` cascade.
//!
//! Covers block + inline formatting, the box model (with `box-sizing`,
//! `min/max-width`, `height`, and `line-height`), `position: relative` offsets,
//! lists (`list-style-type`), form controls (`<input>`/`<textarea>`/`<button>`/
//! `<select>`), `<br>`, `<hr>`, tables, and basic flex/grid (with `gap`).
//! Inline runs keep their own font size, color, and background (so spans, `<mark>`,
//! emphasis, and links style correctly). Still a subset of
//! `docs/subsystems/layout.md`: no floats, no absolute/fixed positioning, no margin
//! collapsing, no `flex-grow`/`justify`/`align` or grid spans, and no inline-block
//! geometry (inline padding/borders/width don't reserve space).

use argus_dom::{Document, ElementData, NodeData, NodeId};
use argus_gfx::{Font, RectFill, TextRun};
use argus_style::{
    author_stylesheet, computed_style, AlignItems, AuthorStylesheet, BoxSizing, Clear,
    ComputedStyle, Display, FlexDirection, Float, GridTrack, JustifyContent, Length, ListStyle,
    Position, PseudoElement, TextAlign, TextTransform, VerticalAlign, GRID_MAX_TRACKS,
};
use std::collections::HashMap;
use std::rc::Rc;

const PAGE_MARGIN: f32 = 8.0;

/// A list-item marker: either a glyph string (numbers/letters) or a geometric
/// bullet drawn as a shape (font-independent, like real browsers).
enum Marker {
    Text(String),
    Disc,
    Circle,
    Square,
}

/// The marker for a list item, given its `list-style-type` and 1-based index
/// among siblings. Returns `None` for `list-style-type: none`.
fn list_marker(style: ListStyle, index: u32) -> Option<Marker> {
    Some(match style {
        ListStyle::Disc => Marker::Disc,
        ListStyle::Circle => Marker::Circle,
        ListStyle::Square => Marker::Square,
        ListStyle::Decimal => Marker::Text(format!("{index}.")),
        ListStyle::LowerAlpha => Marker::Text(format!("{}.", alpha_marker(index, false))),
        ListStyle::UpperAlpha => Marker::Text(format!("{}.", alpha_marker(index, true))),
        ListStyle::LowerRoman => Marker::Text(format!("{}.", roman_marker(index))),
        ListStyle::None => return None,
    })
}

/// Bijective base-26 alphabetic counter: 1→a, 26→z, 27→aa.
fn alpha_marker(mut n: u32, upper: bool) -> String {
    let base = if upper { b'A' } else { b'a' };
    let mut out = Vec::new();
    while n > 0 {
        n -= 1;
        out.push(base + (n % 26) as u8);
        n /= 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Lowercase Roman numeral for `n` (falls back to decimal outside 1..=3999).
fn roman_marker(n: u32) -> String {
    if n == 0 || n > 3999 {
        return n.to_string();
    }
    const VALS: [(u32, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut n = n;
    let mut out = String::new();
    for (v, s) in VALS {
        while n >= v {
            out.push_str(s);
            n -= v;
        }
    }
    out
}

/// Resolve a `position: relative` element's net `(dx, dy)` shift from its inset
/// offsets. `left`/`top` win over `right`/`bottom`; lengths resolve against the
/// containing block width `avail`.
fn relative_offset(style: &ComputedStyle, avail: f32) -> (f32, f32) {
    let fs = style.font_size;
    let axis = |a: Option<Length>, b: Option<Length>| -> f32 {
        if let Some(l) = a {
            l.to_px(fs, avail)
        } else if let Some(r) = b {
            -r.to_px(fs, avail)
        } else {
            0.0
        }
    };
    let dx = axis(style.inset_left, style.inset_right);
    let dy = axis(style.inset_top, style.inset_bottom);
    (dx, dy)
}

/// Clamp a content-box `width` to the `min-width`/`max-width` constraints,
/// resolving each against the containing block `avail` (and `box-sizing`).
fn clamp_content_width(style: &ComputedStyle, width: f32, avail: f32) -> f32 {
    let mut w = width;
    if let Some(max) = style.max_width {
        let m = border_box_to_content(style, max.to_px(style.font_size, avail));
        w = w.min(m);
    }
    if let Some(min) = style.min_width {
        let m = border_box_to_content(style, min.to_px(style.font_size, avail));
        w = w.max(m);
    }
    w.max(0.0)
}

/// A snapshot of the display-list lengths (rects, runs, images, links, bounds),
/// used to shift everything appended after the mark by a delta.
type DisplayListMark = (usize, usize, usize, usize, usize);

/// A placed float: the vertical band it occupies and the inner edge that inline
/// content flows up to (a left float's right edge; a right float's left edge).
#[derive(Clone, Copy)]
struct FloatBox {
    top: f32,
    bottom: f32,
    /// Inner edge x: content stays right of a left float / left of a right float.
    edge: f32,
    side: Float,
}

/// Convert a specified `width` into a content-box width, honoring `box-sizing`.
/// For `border-box`, the horizontal padding and border are subtracted.
fn border_box_to_content(style: &ComputedStyle, width: f32) -> f32 {
    match style.box_sizing {
        BoxSizing::ContentBox => width,
        BoxSizing::BorderBox => {
            let chrome =
                style.padding.left + style.padding.right + style.border.left + style.border.right;
            (width - chrome).max(0.0)
        }
    }
}

/// A word in an inline formatting context, carrying its own style so spans, links,
/// and emphasis keep their color/size within a paragraph.
#[derive(Clone)]
struct InlineWord {
    text: String,
    font_size: f32,
    color: argus_geometry::Color,
    /// Background paint behind this word (`a == 0` = none), for inline highlights.
    background: argus_geometry::Color,
    /// Whether whitespace precedes this word (a break opportunity + a space glyph).
    space_before: bool,
    /// Whether this word is underlined (`text-decoration: underline`).
    underline: bool,
    /// Whether this word has a strike-through (`text-decoration: line-through`).
    strike: bool,
    /// Whether this word has an overline (`text-decoration: overline`).
    overline: bool,
    /// Color of the decoration lines (`text-decoration-color`, else the text color).
    decoration_color: argus_geometry::Color,
    /// The hyperlink target, if this word is inside an `<a href>`.
    href: Option<Rc<str>>,
    /// Force a line break before this word (an `<br>` element).
    hard_break: bool,
    /// Vertical baseline offset in pixels (negative = up), for sub/superscript.
    baseline_shift: f32,
}

/// A clickable hyperlink region in canvas pixels.
#[derive(Clone, Debug)]
pub struct LinkBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub href: String,
}

impl LinkBox {
    /// Whether `(px, py)` falls inside this link region.
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// A placed image: its box in canvas pixels and the source URL (key into the
/// content process's decoded-image map).
#[derive(Clone, Debug)]
pub struct ImageBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub src: String,
}

/// The border-box of an element that carries an `id`, for click hit-testing.
#[derive(Clone, Debug)]
pub struct ElementBound {
    pub id: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// The result of laying a document out at a given viewport width.
pub struct Layout {
    /// Background + border rectangles, painted behind text (ancestors first).
    pub rects: Vec<RectFill>,
    /// Positioned, colored text runs, top-to-bottom.
    pub runs: Vec<TextRun>,
    /// Placed images (blitted by the content process from decoded bytes).
    pub images: Vec<ImageBox>,
    /// Clickable hyperlink regions.
    pub links: Vec<LinkBox>,
    /// Border-boxes of id'd elements (deepest last), for click hit-testing.
    pub bounds: Vec<ElementBound>,
    /// Total content height in pixels.
    pub height: f32,
}

impl Layout {
    /// The `id` of the most specific id'd element whose box contains `(x, y)` —
    /// the smallest-area containing box wins, so a nested child beats its ancestor.
    pub fn element_at(&self, x: f32, y: f32) -> Option<&str> {
        self.bounds
            .iter()
            .filter(|b| x >= b.x && x < b.x + b.w && y >= b.y && y < b.y + b.h)
            .min_by(|a, b| (a.w * a.h).partial_cmp(&(b.w * b.h)).unwrap())
            .map(|b| b.id.as_str())
    }
}

/// Intrinsic `(width, height)` of each image by source URL, for sizing boxes.
pub type ImageSizes = HashMap<String, (u32, u32)>;

/// Lay `doc` out into a display list for a viewport `viewport_width` pixels wide,
/// given the intrinsic sizes of any images.
pub fn layout(doc: &Document, font: &Font, viewport_width: f32, images: &ImageSizes) -> Layout {
    let content_x = PAGE_MARGIN;
    let content_width = (viewport_width - 2.0 * PAGE_MARGIN).max(0.0);
    // Apply `@media` rules that match this viewport width.
    let author = author_stylesheet(doc).matching_media(viewport_width);

    let mut ctx = Ctx {
        doc,
        font,
        author: &author,
        image_sizes: images,
        rects: Vec::new(),
        runs: Vec::new(),
        images: Vec::new(),
        links: Vec::new(),
        bounds: Vec::new(),
        cursor_y: PAGE_MARGIN,
        floats: Vec::new(),
        cb_x: 0.0,
        cb_y: 0.0,
        cb_w: viewport_width,
        cb_h: None,
        viewport_w: viewport_width,
    };

    let start = body_or_root(doc);
    let start_style = match &doc.node(start).data {
        NodeData::Element(_) => computed_style(doc, start, &ComputedStyle::initial(), &author),
        _ => ComputedStyle::initial(),
    };
    ctx.layout_block(start, start_style, content_x, content_width, None);

    Layout {
        rects: ctx.rects,
        runs: ctx.runs,
        images: ctx.images,
        links: ctx.links,
        bounds: ctx.bounds,
        height: ctx.cursor_y + PAGE_MARGIN,
    }
}

fn body_or_root(doc: &Document) -> NodeId {
    let root = doc.root();
    let html = doc
        .children(root)
        .find(|&c| is_element(doc, c, "html"))
        .unwrap_or(root);
    doc.children(html)
        .find(|&c| is_element(doc, c, "body"))
        .unwrap_or(root)
}

fn is_element(doc: &Document, id: NodeId, name: &str) -> bool {
    matches!(&doc.node(id).data, NodeData::Element(e) if e.name.is_html(name))
}

struct Ctx<'a> {
    doc: &'a Document,
    font: &'a Font,
    author: &'a AuthorStylesheet,
    image_sizes: &'a ImageSizes,
    rects: Vec<RectFill>,
    runs: Vec<TextRun>,
    images: Vec<ImageBox>,
    links: Vec<LinkBox>,
    bounds: Vec<ElementBound>,
    cursor_y: f32,
    /// Active floats (absolute coords) narrowing the inline region of later lines.
    floats: Vec<FloatBox>,
    /// Containing block for absolutely-positioned descendants: the padding box of
    /// the nearest positioned ancestor (origin x/y, width; height if definite).
    /// Defaults to the initial containing block (the viewport).
    cb_x: f32,
    cb_y: f32,
    cb_w: f32,
    cb_h: Option<f32>,
    /// Viewport width — the containing block used by `position: fixed`.
    viewport_w: f32,
}

impl Ctx<'_> {
    /// Lay out block `id` within the containing block `[x, x + avail)` (content box
    /// of the parent). `x`/`avail` are the parent's content origin and width.
    /// `marker`, if set, is a list-item marker drawn to the left of the content.
    fn layout_block(
        &mut self,
        id: NodeId,
        style: ComputedStyle,
        x: f32,
        avail: f32,
        marker: Option<Marker>,
    ) {
        // For `position: relative`, remember where this subtree's display-list
        // items begin so they can all be shifted by the inset offset at the end.
        let ds_start = (
            self.rects.len(),
            self.runs.len(),
            self.images.len(),
            self.links.len(),
            self.bounds.len(),
        );

        let border_box_top = self.cursor_y;

        let h_extra = style.margin.left
            + style.margin.right
            + style.border.left
            + style.border.right
            + style.padding.left
            + style.padding.right;
        let content_w = match style.width {
            Some(len) => border_box_to_content(&style, len.to_px(style.font_size, avail)),
            None => (avail - h_extra).max(0.0),
        };
        let content_w = clamp_content_width(&style, content_w, avail);
        let border_box_w = content_w
            + style.padding.left
            + style.padding.right
            + style.border.left
            + style.border.right;
        // Horizontal placement: a block with a definite width and `auto` left+right
        // margins centers itself in the available width; otherwise it sits at the
        // left margin.
        let border_box_left = if style.width.is_some() && style.margin_auto_lr {
            x + (avail - border_box_w).max(0.0) / 2.0
        } else {
            x + style.margin.left
        };
        let content_left = border_box_left + style.border.left + style.padding.left;

        // The containing block this box is positioned within: the nearest positioned
        // ancestor's padding box (tracked in `self.cb_*`), except `fixed`, which is
        // anchored to the viewport.
        let cb_self = if style.position == Position::Fixed {
            (0.0, 0.0, self.viewport_w, None)
        } else {
            (self.cb_x, self.cb_y, self.cb_w, self.cb_h)
        };
        // If this box is itself positioned, it establishes the containing block for
        // its absolutely-positioned descendants. Save the parent's CB and install
        // this box's padding box (its definite height, if any) for the subtree.
        let saved_cb = (self.cb_x, self.cb_y, self.cb_w, self.cb_h);
        if style.position != Position::Static {
            self.cb_x = border_box_left + style.border.left;
            self.cb_y = border_box_top + style.border.top;
            self.cb_w = content_w + style.padding.left + style.padding.right;
            self.cb_h = style.height.map(|len| {
                len.to_px(style.font_size, avail) + style.padding.top + style.padding.bottom
            });
        }

        // Reserve background + border rect slots up front so ancestors paint first.
        // `visibility: hidden` keeps the box's geometry but paints no ink.
        let bg_idx = (style.background_color.a > 0 && !style.hidden).then(|| {
            self.rects.push(RectFill {
                x: border_box_left,
                y: border_box_top,
                w: border_box_w,
                h: 0.0,
                color: style.fade(style.background_color),
                radius: style.border_radius,
            });
            self.rects.len() - 1
        });
        let has_border = style.border_color.a > 0
            && !style.hidden
            && (style.border.top + style.border.right + style.border.bottom + style.border.left)
                > 0.0;
        let border_idx = has_border.then(|| {
            let i = self.rects.len();
            for _ in 0..4 {
                self.rects.push(RectFill {
                    x: 0.0,
                    y: 0.0,
                    w: 0.0,
                    h: 0.0,
                    color: style.border_color,
                    radius: 0.0,
                });
            }
            i
        });

        self.cursor_y += style.border.top + style.padding.top;

        // A list-item marker sits on the first line, just left of the content.
        if let Some(marker) = marker.as_ref().filter(|_| !style.hidden) {
            let fs = style.font_size;
            match marker {
                Marker::Text(s) => {
                    let baseline = self.cursor_y + self.font.ascent_px(fs);
                    let mw = self.font.measure(s, fs);
                    self.runs.push(TextRun {
                        x: content_left - mw - 8.0,
                        baseline,
                        text: s.clone(),
                        size_px: fs,
                        color: style.color,
                    });
                }
                bullet => {
                    // Geometric bullets are drawn as shapes (font-independent).
                    let d = (fs * 0.42).max(3.0);
                    let bx = content_left - d - 10.0;
                    let by = self.cursor_y + (fs - d) * 0.5;
                    let round = matches!(bullet, Marker::Disc | Marker::Circle);
                    self.rects.push(RectFill {
                        x: bx,
                        y: by,
                        w: d,
                        h: d,
                        color: style.color,
                        radius: if round { d * 0.5 } else { 0.0 },
                    });
                    if matches!(bullet, Marker::Circle) {
                        // Punch out the centre so the ring reads as hollow.
                        let t = (d * 0.22).max(1.0);
                        self.rects.push(RectFill {
                            x: bx + t,
                            y: by + t,
                            w: d - 2.0 * t,
                            h: d - 2.0 * t,
                            color: argus_geometry::Color::WHITE,
                            radius: (d - 2.0 * t) * 0.5,
                        });
                    }
                }
            }
        }

        // List items get a marker from their own `list-style-type`, counted 1-based.
        let mut item_index = 0u32;

        // Preformatted (`white-space: pre`): emit raw lines, preserving whitespace
        // and breaking only on newlines (no collapsing, no wrapping).
        if style.white_space_pre {
            let mut raw = String::new();
            self.gather_raw_text(id, &mut raw);
            // Expand tabs to `tab-size` spaces (pre-line later collapses them).
            if raw.contains('\t') {
                raw = raw.replace('\t', &" ".repeat(style.tab_size.max(1) as usize));
            }
            let color = if style.hidden {
                argus_geometry::Color::TRANSPARENT
            } else {
                style.fade(style.color)
            };
            let fs = style.font_size;
            for line in raw.trim_end_matches('\n').split('\n') {
                // `pre-line` collapses runs of whitespace to single spaces and wraps
                // long lines; `pre`/`pre-wrap` keep each newline-delimited line whole.
                let visual: Vec<String> = if style.pre_line {
                    self.wrap_collapsed(line, fs, content_w)
                } else if style.pre_wrap {
                    self.wrap_preserving(line, fs, content_w)
                } else {
                    vec![line.to_string()]
                };
                for vline in visual {
                    let baseline = self.cursor_y + self.font.ascent_px(fs);
                    self.runs.push(TextRun {
                        x: content_left,
                        baseline,
                        text: vline,
                        size_px: fs,
                        color,
                    });
                    self.cursor_y += fs * style.line_height;
                }
            }
        } else {
            // Children. Inline-level content accumulates into `words` (each with its own
            // style); block-level children flush the line box and lay out separately.
            let mut words: Vec<InlineWord> = Vec::new();
            let mut pending_space = false;
            // Floats introduced while laying out this block's content are contained
            // by it: at the end we extend the cursor past them and drop them so they
            // don't leak to siblings.
            let float_base = self.floats.len();
            // `::before` generated content is the element's first inline content.
            if let Some(text) =
                argus_style::pseudo_content(self.doc, id, self.author, PseudoElement::Before)
            {
                self.gather_generated(&text, &style, &mut words, &mut pending_space);
            }
            // Form controls render synthesized text: a text `<input>`'s value (or
            // grey placeholder) or a `<select>`'s selected option. Checkbox/radio
            // render no text (a checked mark is drawn after the box).
            if let Some(e) = self.doc.node(id).as_element() {
                let (text, placeholder) = if e.name.is_html("input") {
                    let ty = e.attr("type").unwrap_or("text");
                    if matches!(ty, "checkbox" | "radio") {
                        (String::new(), false)
                    } else {
                        match e.attr("value").filter(|v| !v.is_empty()) {
                            Some(v) => (v.to_string(), false),
                            None => (e.attr("placeholder").unwrap_or("").to_string(), true),
                        }
                    }
                } else if e.name.is_html("select") {
                    (self.selected_option_text(id), false)
                } else {
                    (String::new(), false)
                };
                let color = if placeholder {
                    argus_geometry::Color::rgb(0x80, 0x80, 0x80)
                } else {
                    style.fade(style.color)
                };
                for (i, word) in text.split_whitespace().enumerate() {
                    words.push(InlineWord {
                        text: word.to_string(),
                        font_size: style.font_size,
                        color,
                        background: argus_geometry::Color::TRANSPARENT,
                        space_before: i > 0,
                        underline: false,
                        strike: false,
                        overline: false,
                        decoration_color: argus_geometry::Color::TRANSPARENT,
                        href: None,
                        hard_break: false,
                        baseline_shift: 0.0,
                    });
                }
            }
            // A closed `<details>` shows only its `<summary>`; other children hide.
            let details_closed = self
                .doc
                .node(id)
                .as_element()
                .is_some_and(|e| e.name.is_html("details") && e.attr("open").is_none());
            for child in self.doc.children(id) {
                if details_closed
                    && !matches!(&self.doc.node(child).data,
                        NodeData::Element(e) if e.name.is_html("summary"))
                {
                    continue;
                }
                // A floated child is taken out of flow and placed at the side; the
                // text gathered before it is flushed above, and following inline
                // content flows around it.
                if matches!(&self.doc.node(child).data, NodeData::Element(_)) {
                    let cf = computed_style(self.doc, child, &style, self.author);
                    if cf.float != Float::None
                        && cf.position == Position::Static
                        && cf.display != Display::None
                    {
                        self.flush_words(&mut words, &style, content_left, content_w);
                        pending_space = false;
                        self.place_float(child, cf, content_left, content_w);
                        continue;
                    }
                }
                match &self.doc.node(child).data {
                    NodeData::Text(_) => {
                        self.gather_inline(child, &style, None, &mut words, &mut pending_space);
                    }
                    NodeData::Element(e) if e.name.is_html("img") => {
                        self.flush_words(&mut words, &style, content_left, content_w);
                        pending_space = false;
                        let istyle = computed_style(self.doc, child, &style, self.author);
                        self.place_image(e, &istyle, content_left, content_w);
                    }
                    NodeData::Element(e)
                        if e.name.is_html("progress") || e.name.is_html("meter") =>
                    {
                        self.flush_words(&mut words, &style, content_left, content_w);
                        pending_space = false;
                        let cstyle = computed_style(self.doc, child, &style, self.author);
                        self.place_bar(e, &cstyle, content_left, content_w);
                    }
                    NodeData::Element(e) if e.name.is_html("hr") => {
                        self.flush_words(&mut words, &style, content_left, content_w);
                        pending_space = false;
                        let hr = computed_style(self.doc, child, &style, self.author);
                        self.cursor_y += hr.margin.top;
                        let h = hr.border.top.max(1.0);
                        self.rects.push(rect(
                            content_left,
                            self.cursor_y,
                            content_w,
                            h,
                            hr.border_color,
                        ));
                        self.cursor_y += h + hr.margin.bottom;
                    }
                    NodeData::Element(e) if e.name.is_html("table") => {
                        self.flush_words(&mut words, &style, content_left, content_w);
                        pending_space = false;
                        let tstyle = computed_style(self.doc, child, &style, self.author);
                        self.cursor_y += tstyle.margin.top;
                        self.layout_table(child, tstyle, content_left, content_w);
                        self.cursor_y += tstyle.margin.bottom;
                    }
                    NodeData::Element(_) => {
                        let cstyle = computed_style(self.doc, child, &style, self.author);
                        match cstyle.display {
                            Display::None => {}
                            Display::Inline => {
                                self.gather_inline(
                                    child,
                                    &cstyle,
                                    None,
                                    &mut words,
                                    &mut pending_space,
                                );
                            }
                            Display::Block => {
                                self.flush_words(&mut words, &style, content_left, content_w);
                                pending_space = false;
                                // `clear` drops this block below the relevant floats.
                                if cstyle.clear != Clear::None {
                                    let cb = self.clear_bottom(cstyle.clear);
                                    if cb > self.cursor_y {
                                        self.cursor_y = cb;
                                    }
                                }
                                let child_marker = if self.is_li(child) {
                                    item_index += 1;
                                    list_marker(cstyle.list_style, item_index)
                                } else {
                                    None
                                };
                                self.cursor_y += cstyle.margin.top;
                                self.layout_block(
                                    child,
                                    cstyle,
                                    content_left,
                                    content_w,
                                    child_marker,
                                );
                                self.cursor_y += cstyle.margin.bottom;
                            }
                            Display::Flex => {
                                self.flush_words(&mut words, &style, content_left, content_w);
                                pending_space = false;
                                self.cursor_y += cstyle.margin.top;
                                self.layout_flex(child, cstyle, content_left, content_w);
                                self.cursor_y += cstyle.margin.bottom;
                            }
                            Display::Grid => {
                                self.flush_words(&mut words, &style, content_left, content_w);
                                pending_space = false;
                                self.cursor_y += cstyle.margin.top;
                                self.layout_grid(child, cstyle, content_left, content_w);
                                self.cursor_y += cstyle.margin.bottom;
                            }
                        }
                    }
                    _ => {}
                }
            }
            // `::after` generated content is the element's last inline content.
            if let Some(text) =
                argus_style::pseudo_content(self.doc, id, self.author, PseudoElement::After)
            {
                self.gather_generated(&text, &style, &mut words, &mut pending_space);
            }
            self.flush_words(&mut words, &style, content_left, content_w);
            // Contain this block's floats: grow to enclose them, then drop them.
            if self.floats.len() > float_base {
                let max_bottom = self.floats[float_base..]
                    .iter()
                    .map(|f| f.bottom)
                    .fold(self.cursor_y, f32::max);
                self.cursor_y = max_bottom;
                self.floats.truncate(float_base);
            }
        } // end !white_space_pre

        // Honor a specified `height` / `min-height`: extend the content box down to
        // it (we don't clip overflow, so taller content still grows the box). Both
        // only extend, so the larger target wins.
        let content_top = border_box_top + style.border.top + style.padding.top;
        // The natural content bottom (before any height/aspect extension) — `max-height`
        // never shrinks below this, since overflow isn't clipped.
        let natural_bottom = self.cursor_y;
        for len in [style.height, style.min_height].into_iter().flatten() {
            let target = content_top + len.to_px(style.font_size, content_w);
            if self.cursor_y < target {
                self.cursor_y = target;
            }
        }
        // `aspect-ratio` with a definite width and auto height derives the height.
        if style.height.is_none() {
            if let (Some(ratio), Some(_)) = (style.aspect_ratio, style.width) {
                let target = content_top + (content_w / ratio).max(0.0);
                if self.cursor_y < target {
                    self.cursor_y = target;
                }
            }
        }
        // `max-height` caps the extended height, but never below the actual content.
        if let Some(mh) = style.max_height {
            let cap = content_top + mh.to_px(style.font_size, content_w);
            self.cursor_y = self.cursor_y.min(cap).max(natural_bottom);
        }

        self.cursor_y += style.padding.bottom + style.border.bottom;
        let border_box_h = self.cursor_y - border_box_top;

        if let Some(i) = bg_idx {
            self.rects[i].h = border_box_h;
        }
        if let Some(i) = border_idx {
            let b = &style.border;
            self.rects[i] = rect(
                border_box_left,
                border_box_top,
                border_box_w,
                b.top,
                style.border_top_color,
            );
            self.rects[i + 1] = rect(
                border_box_left,
                border_box_top + border_box_h - b.bottom,
                border_box_w,
                b.bottom,
                style.border_bottom_color,
            );
            self.rects[i + 2] = rect(
                border_box_left,
                border_box_top,
                b.left,
                border_box_h,
                style.border_left_color,
            );
            self.rects[i + 3] = rect(
                border_box_left + border_box_w - b.right,
                border_box_top,
                b.right,
                border_box_h,
                style.border_right_color,
            );
        }

        // A checked checkbox/radio: fill the inner box with the text color.
        if let Some(e) = self.doc.node(id).as_element() {
            let ty = e.attr("type").unwrap_or("");
            if e.name.is_html("input")
                && matches!(ty, "checkbox" | "radio")
                && e.attr("checked").is_some()
                && !style.hidden
            {
                let inset = 3.0;
                let radius = if ty == "radio" {
                    (border_box_w - 2.0 * inset) * 0.5
                } else {
                    0.0
                };
                self.rects.push(RectFill {
                    x: border_box_left + inset,
                    y: border_box_top + inset,
                    w: (border_box_w - 2.0 * inset).max(0.0),
                    h: (border_box_h - 2.0 * inset).max(0.0),
                    color: style.color,
                    radius,
                });
            }
            // `<input type=color>`: fill the inner box with the value's color swatch.
            if e.name.is_html("input") && ty == "color" && !style.hidden {
                let swatch = e
                    .attr("value")
                    .and_then(argus_style::parse_color)
                    .unwrap_or(argus_geometry::Color::BLACK);
                let inset = 2.0;
                self.rects.push(RectFill {
                    x: border_box_left + inset,
                    y: border_box_top + inset,
                    w: (border_box_w - 2.0 * inset).max(0.0),
                    h: (border_box_h - 2.0 * inset).max(0.0),
                    color: swatch,
                    radius: 0.0,
                });
            }
        }

        // `outline`: four rects just outside the border box (no layout effect).
        if style.outline_width > 0.0 && style.outline_color.a > 0 && !style.hidden {
            let ow = style.outline_width;
            let (ol, ot) = (border_box_left - ow, border_box_top - ow);
            let (ow_full, oh_full) = (border_box_w + 2.0 * ow, border_box_h + 2.0 * ow);
            let oc = style.outline_color;
            self.rects.push(rect(ol, ot, ow_full, ow, oc)); // top
            self.rects
                .push(rect(ol, border_box_top + border_box_h, ow_full, ow, oc)); // bottom
            self.rects.push(rect(ol, ot, ow, oh_full, oc)); // left
            self.rects
                .push(rect(border_box_left + border_box_w, ot, ow, oh_full, oc));
            // right
        }

        // Record this element's border-box for click hit-testing, if it has an id.
        if let Some(eid) = self.doc.node(id).as_element().and_then(|e| e.attr("id")) {
            self.bounds.push(ElementBound {
                id: eid.to_string(),
                x: border_box_left,
                y: border_box_top,
                w: border_box_w,
                h: self.cursor_y - border_box_top,
            });
        }

        // Restore the parent's containing block now that the subtree is laid out.
        self.cb_x = saved_cb.0;
        self.cb_y = saved_cb.1;
        self.cb_w = saved_cb.2;
        self.cb_h = saved_cb.3;

        // Positioning. `relative` paints the box (and subtree) shifted by its inset
        // without affecting following siblings. `absolute`/`fixed` additionally take
        // the box out of normal flow (the parent's cursor is reset) and are anchored
        // to their containing block by `top`/`left`/`right`/`bottom`.
        match style.position {
            Position::Relative => {
                let (dx, dy) = relative_offset(&style, avail);
                if dx != 0.0 || dy != 0.0 {
                    self.shift_display_list(ds_start, dx, dy);
                }
            }
            Position::Absolute | Position::Fixed => {
                let (cbx, cby, cbw, cbh) = cb_self;
                let fs = style.font_size;
                // Horizontal: anchor to the CB's left or right edge; else keep the
                // static position. Right anchoring needs the box width.
                let target_left = if let Some(l) = style.inset_left {
                    cbx + l.to_px(fs, cbw)
                } else if let Some(r) = style.inset_right {
                    cbx + cbw - r.to_px(fs, cbw) - border_box_w
                } else {
                    border_box_left
                };
                // Vertical: anchor to the CB's top or bottom edge. Bottom anchoring
                // needs a definite CB height; without one, fall back to static.
                let target_top = if let Some(t) = style.inset_top {
                    cby + t.to_px(fs, cbh.unwrap_or(0.0))
                } else if let (Some(b), Some(h)) = (style.inset_bottom, cbh) {
                    cby + h - b.to_px(fs, h) - border_box_h
                } else {
                    border_box_top
                };
                let dx = target_left - border_box_left;
                let dy = target_top - border_box_top;
                if dx != 0.0 || dy != 0.0 {
                    self.shift_display_list(ds_start, dx, dy);
                }
                // Out of flow: following siblings ignore this box's height.
                self.cursor_y = border_box_top;
            }
            Position::Static => {}
        }

        // `transform`: paint the subtree scaled (about its center) and/or shifted,
        // with no effect on flow. `%` translate resolves against the border box.
        if let Some((sx, sy)) = style.transform_scale {
            if sx != 1.0 || sy != 1.0 {
                let cx = border_box_left + border_box_w / 2.0;
                let cy = border_box_top + border_box_h / 2.0;
                self.scale_display_list(ds_start, sx, sy, cx, cy);
            }
        }
        if let Some((tx, ty)) = style.transform_translate {
            let dx = tx.to_px(style.font_size, border_box_w);
            let dy = ty.to_px(style.font_size, border_box_h);
            if dx != 0.0 || dy != 0.0 {
                self.shift_display_list(ds_start, dx, dy);
            }
        }
    }

    /// Shift every display-list item appended since `start` by `(dx, dy)`.
    fn shift_display_list(&mut self, start: DisplayListMark, dx: f32, dy: f32) {
        for r in &mut self.rects[start.0..] {
            r.x += dx;
            r.y += dy;
        }
        for r in &mut self.runs[start.1..] {
            r.x += dx;
            r.baseline += dy;
        }
        for im in &mut self.images[start.2..] {
            im.x += dx;
            im.y += dy;
        }
        for l in &mut self.links[start.3..] {
            l.x += dx;
            l.y += dy;
        }
        for b in &mut self.bounds[start.4..] {
            b.x += dx;
            b.y += dy;
        }
    }

    /// Scale every display-list item appended since `start` by `(sx, sy)` about the
    /// point `(ox, oy)` — positions, sizes, and text size all scale (for
    /// `transform: scale`). Text size uses the horizontal factor.
    fn scale_display_list(&mut self, start: DisplayListMark, sx: f32, sy: f32, ox: f32, oy: f32) {
        for r in &mut self.rects[start.0..] {
            r.x = ox + (r.x - ox) * sx;
            r.y = oy + (r.y - oy) * sy;
            r.w *= sx;
            r.h *= sy;
        }
        for r in &mut self.runs[start.1..] {
            r.x = ox + (r.x - ox) * sx;
            r.baseline = oy + (r.baseline - oy) * sy;
            r.size_px *= sx;
        }
        for im in &mut self.images[start.2..] {
            im.x = ox + (im.x - ox) * sx;
            im.y = oy + (im.y - oy) * sy;
            im.w *= sx;
            im.h *= sy;
        }
        for l in &mut self.links[start.3..] {
            l.x = ox + (l.x - ox) * sx;
            l.y = oy + (l.y - oy) * sy;
            l.w *= sx;
            l.h *= sy;
        }
        for b in &mut self.bounds[start.4..] {
            b.x = ox + (b.x - ox) * sx;
            b.y = oy + (b.y - oy) * sy;
            b.w *= sx;
            b.h *= sy;
        }
    }

    /// Render a `<progress>`/`<meter>` as a horizontal bar: a light track with a
    /// colored portion filled to `value / max` (meter offsets by `min`). A
    /// `<progress>` with no `value` is indeterminate and shows an empty track.
    fn place_bar(&mut self, e: &ElementData, istyle: &ComputedStyle, x: f32, avail: f32) {
        if istyle.hidden {
            return;
        }
        let attr = |name: &str| e.attr(name).and_then(|v| v.trim().parse::<f32>().ok());
        let is_meter = e.name.is_html("meter");
        let min = if is_meter { attr("min").unwrap_or(0.0) } else { 0.0 };
        let max = attr("max").unwrap_or(1.0).max(min + f32::EPSILON);
        let frac = match attr("value") {
            Some(v) => ((v - min) / (max - min)).clamp(0.0, 1.0),
            None => 0.0, // indeterminate progress → empty track
        };
        // Default size ~ a typical UA control; honor explicit width/height.
        let w = istyle
            .width
            .map(|l| l.to_px(istyle.font_size, avail))
            .unwrap_or(160.0)
            .min(avail);
        let h = istyle
            .height
            .map(|l| l.to_px(istyle.font_size, avail))
            .unwrap_or((istyle.font_size * 0.9).max(10.0));
        let top = self.cursor_y + istyle.margin.top;
        let radius = (h / 2.0).min(6.0);
        // Track.
        self.rects.push(RectFill {
            x,
            y: top,
            w,
            h,
            color: argus_geometry::Color::rgb(0xd0, 0xd0, 0xd0),
            radius,
        });
        // Filled portion (blue for progress, green for meter).
        if frac > 0.0 {
            let fill = if is_meter {
                argus_geometry::Color::rgb(0x3c, 0xb0, 0x37)
            } else {
                argus_geometry::Color::rgb(0x2b, 0x6c, 0xde)
            };
            self.rects.push(RectFill {
                x,
                y: top,
                w: w * frac,
                h,
                color: fill,
                radius,
            });
        }
        self.cursor_y = top + h + istyle.margin.bottom;
    }

    /// Place an `<img>` as a block-level replaced box on its own line. A broken or
    /// unresolved image with non-empty `alt` text renders that text instead.
    fn place_image(&mut self, e: &ElementData, istyle: &ComputedStyle, x: f32, avail: f32) {
        let hidden = istyle.hidden;
        let Some(src) = e.attr("src") else { return };
        let (iw, ih) = self.image_sizes.get(src).copied().unwrap_or((0, 0));

        // Width: the `width` attribute, else intrinsic, capped to the content box.
        let attr_w = e.attr("width").and_then(|v| v.parse::<f32>().ok());
        let attr_h = e.attr("height").and_then(|v| v.parse::<f32>().ok());
        let mut w = attr_w.unwrap_or(iw as f32).min(avail);
        let mut h = match (attr_w, attr_h) {
            (_, Some(h)) => h,
            (Some(_), None) if iw > 0 => w * ih as f32 / iw as f32, // keep aspect
            _ => ih as f32,
        };
        if w <= 0.0 || h <= 0.0 {
            // Unresolved/broken image: reserve a small placeholder line.
            w = 0.0;
            h = if iw == 0 { 0.0 } else { ih as f32 };
        }
        if w > 0.0 && h > 0.0 {
            // `visibility: hidden` reserves the image's box but paints nothing.
            if !hidden {
                self.images.push(ImageBox {
                    x,
                    y: self.cursor_y,
                    w,
                    h,
                    src: src.to_string(),
                });
            }
            self.cursor_y += h;
        } else if !hidden {
            // Broken/unresolved image: render its `alt` text on its own line(s).
            if let Some(alt) = e.attr("alt").filter(|a| !a.trim().is_empty()) {
                let fs = istyle.font_size;
                let color = istyle.fade(istyle.color);
                for line in self.wrap_collapsed(alt, fs, avail) {
                    let baseline = self.cursor_y + self.font.ascent_px(fs);
                    self.runs.push(TextRun {
                        x,
                        baseline,
                        text: line,
                        size_px: fs,
                        color,
                    });
                    self.cursor_y += fs * istyle.line_height;
                }
            }
        }
    }

    fn is_li(&self, id: NodeId) -> bool {
        matches!(&self.doc.node(id).data, NodeData::Element(e) if e.name.is_html("li"))
    }

    /// Greedily wrap a single line into sub-lines that fit `width`, collapsing runs
    /// of whitespace to single spaces (for `white-space: pre-line`). Always returns
    /// at least one (possibly empty) sub-line, and never splits a single word.
    fn wrap_collapsed(&self, line: &str, fs: f32, width: f32) -> Vec<String> {
        let space_w = self.font.measure(" ", fs);
        let mut out: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut cur_w = 0.0f32;
        for word in line.split_whitespace() {
            let ww = self.font.measure(word, fs);
            if cur.is_empty() {
                cur.push_str(word);
                cur_w = ww;
            } else if cur_w + space_w + ww > width {
                out.push(std::mem::take(&mut cur));
                cur.push_str(word);
                cur_w = ww;
            } else {
                cur.push(' ');
                cur.push_str(word);
                cur_w += space_w + ww;
            }
        }
        out.push(cur); // keep a trailing/only line even when empty
        out
    }

    /// Greedily wrap a line into sub-lines that fit `width` while **preserving**
    /// whitespace (for `white-space: pre-wrap`). Breaks at the last space before
    /// overflow; a token wider than `width` overflows rather than being split.
    fn wrap_preserving(&self, line: &str, fs: f32, width: f32) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut last_break: Option<usize> = None; // byte index just past a space
        for ch in line.chars() {
            cur.push(ch);
            if ch == ' ' {
                last_break = Some(cur.len());
            }
            if self.font.measure(&cur, fs) > width && cur.chars().count() > 1 {
                match last_break.filter(|&b| b < cur.len()) {
                    Some(bp) => {
                        let rest = cur.split_off(bp);
                        out.push(std::mem::take(&mut cur));
                        cur = rest;
                    }
                    None => {
                        // No break opportunity yet: break before the current char.
                        let last = cur.pop().unwrap();
                        out.push(std::mem::take(&mut cur));
                        cur.push(last);
                    }
                }
                last_break = None;
            }
        }
        out.push(cur);
        out
    }

    /// Truncate `text` to the largest prefix whose width plus an `…` fits `width`,
    /// returning the prefix with `…` appended (for `text-overflow: ellipsis`).
    fn truncate_ellipsis(&self, text: &str, fs: f32, width: f32) -> String {
        let ell = "…";
        let ell_w = self.font.measure(ell, fs);
        let budget = (width - ell_w).max(0.0);
        let mut cur = String::new();
        for ch in text.chars() {
            cur.push(ch);
            if self.font.measure(&cur, fs) > budget {
                cur.pop();
                break;
            }
        }
        cur.push_str(ell);
        cur
    }

    /// Split a word into the largest character chunks that each fit `width` (for
    /// `overflow-wrap: break-word`). Always keeps at least one char per chunk.
    fn split_word(&self, word: &str, fs: f32, width: f32) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut cur = String::new();
        for ch in word.chars() {
            cur.push(ch);
            if self.font.measure(&cur, fs) > width && cur.chars().count() > 1 {
                cur.pop();
                chunks.push(std::mem::take(&mut cur));
                cur.push(ch);
            }
        }
        if !cur.is_empty() {
            chunks.push(cur);
        }
        chunks
    }

    /// The inline region `[lx, rx]` left after subtracting any floats overlapping
    /// the vertical band `[top, bottom)` from the content box `[x, right]`.
    fn float_band(&self, x: f32, right: f32, top: f32, bottom: f32) -> (f32, f32) {
        let mut lx = x;
        let mut rx = right;
        for f in &self.floats {
            if f.bottom > top && f.top < bottom {
                match f.side {
                    Float::Left => lx = lx.max(f.edge),
                    Float::Right => rx = rx.min(f.edge),
                    Float::None => {}
                }
            }
        }
        (lx, rx.max(lx))
    }

    /// The lowest bottom edge among active floats on the given side(s) — used by
    /// `clear` to drop a block below them.
    fn clear_bottom(&self, clear: Clear) -> f32 {
        let mut y = f32::MIN;
        for f in &self.floats {
            let matches = match clear {
                Clear::Both => true,
                Clear::Left => f.side == Float::Left,
                Clear::Right => f.side == Float::Right,
                Clear::None => false,
            };
            if matches {
                y = y.max(f.bottom);
            }
        }
        y
    }

    /// Place a floated child at the current cursor: lay its box out at the left or
    /// right edge of the content box (past any existing floats on that side), then
    /// register the occupied band so later inline content flows around it. Does not
    /// advance the block's cursor (floats are out of normal vertical flow).
    fn place_float(&mut self, id: NodeId, fstyle: ComputedStyle, x: f32, avail: f32) {
        let content_right = x + avail;
        // Float width: explicit, else shrink-to-content; capped to the content box.
        let fw = fstyle
            .width
            .map(|len| {
                border_box_to_content(&fstyle, len.to_px(fstyle.font_size, avail))
                    + fstyle.padding.left
                    + fstyle.padding.right
                    + fstyle.border.left
                    + fstyle.border.right
            })
            .unwrap_or_else(|| self.intrinsic_border_width(id, &fstyle))
            .min(avail);
        // Find the highest band at/below the cursor where the float fits in the
        // remaining width; if it doesn't fit beside existing floats, drop below the
        // nearest one and retry (so floats stack).
        let mut top = self.cursor_y + fstyle.margin.top;
        let (mut lx, mut rx) = self.float_band(x, content_right, top, top + 1.0);
        loop {
            if rx - lx >= fw {
                break;
            }
            let next = self
                .floats
                .iter()
                .filter(|f| f.bottom > top && f.top <= top)
                .map(|f| f.bottom)
                .fold(f32::MAX, f32::min);
            if next == f32::MAX || next <= top {
                break; // nothing to drop past (float just overflows the line)
            }
            top = next;
            let band = self.float_band(x, content_right, top, top + 1.0);
            lx = band.0;
            rx = band.1;
        }
        let left = match fstyle.float {
            Float::Right => (rx - fw).max(lx),
            _ => lx,
        };
        // Lay the float's own box out at the chosen origin (in normal block mode).
        let saved_y = self.cursor_y;
        self.cursor_y = top;
        self.layout_block(id, fstyle, left - fstyle.margin.left, fw + fstyle.margin.left, None);
        let bottom = self.cursor_y + fstyle.margin.bottom;
        self.cursor_y = saved_y; // floats don't push the block's cursor down
        self.floats.push(FloatBox {
            top,
            bottom,
            edge: match fstyle.float {
                Float::Right => left - fstyle.margin.left,
                _ => left + fw + fstyle.margin.right,
            },
            side: fstyle.float,
        });
    }

    /// Approximate the max-content border-box width of an element: the widest line
    /// its inline text would occupy if never wrapped (segments split on hard
    /// `<br>` breaks), plus its own horizontal padding and border. Used to give a
    /// shrink-to-content base size to flex items that have no explicit `width`.
    /// Block descendants are treated as inline here (an over-estimate that is fine
    /// for the typical flex item — a label or button).
    fn intrinsic_border_width(&self, id: NodeId, style: &ComputedStyle) -> f32 {
        let mut words: Vec<InlineWord> = Vec::new();
        let mut pending_space = false;
        for child in self.doc.children(id) {
            self.gather_inline(child, style, None, &mut words, &mut pending_space);
        }
        let mut max_line = 0.0f32;
        let mut cur = 0.0f32;
        for w in &words {
            if w.hard_break {
                max_line = max_line.max(cur);
                cur = 0.0;
                continue;
            }
            let space = if w.space_before {
                self.font.measure(" ", w.font_size)
            } else {
                0.0
            };
            cur += space + self.font.measure(&w.text, w.font_size);
        }
        max_line = max_line.max(cur);
        max_line + style.padding.left + style.padding.right + style.border.left + style.border.right
    }

    /// The text of a `<select>`'s selected `<option>` (the one with a `selected`
    /// attribute, else the first option).
    fn selected_option_text(&self, select: NodeId) -> String {
        let mut first: Option<NodeId> = None;
        fn walk(doc: &Document, id: NodeId, first: &mut Option<NodeId>) -> Option<NodeId> {
            for c in doc.children(id) {
                if let NodeData::Element(e) = &doc.node(c).data {
                    if e.name.is_html("option") {
                        if first.is_none() {
                            *first = Some(c);
                        }
                        if e.attr("selected").is_some() {
                            return Some(c);
                        }
                    }
                }
                if let Some(found) = walk(doc, c, first) {
                    return Some(found);
                }
            }
            None
        }
        let chosen = walk(self.doc, select, &mut first).or(first);
        let mut text = String::new();
        if let Some(opt) = chosen {
            fn collect(doc: &Document, id: NodeId, out: &mut String) {
                match &doc.node(id).data {
                    NodeData::Text(t) => out.push_str(t),
                    _ => {
                        for c in doc.children(id) {
                            collect(doc, c, out);
                        }
                    }
                }
            }
            collect(self.doc, opt, &mut text);
        }
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Lay out a `display: flex` container. In the default `row` direction the
    /// children share the content width equally on a single line (height = tallest
    /// item); in `flex-direction: column` they stack vertically at full width, `gap`
    /// apart. Honors fixed item widths, `justify-content` (when all items are
    /// fixed-width), and `align-items` cross-axis placement. No wrapping or
    /// `flex-grow` yet.
    fn layout_flex(&mut self, id: NodeId, style: ComputedStyle, x: f32, avail: f32) {
        let mut items: Vec<NodeId> = self
            .doc
            .children(id)
            .filter(|&c| match &self.doc.node(c).data {
                NodeData::Element(_) => {
                    computed_style(self.doc, c, &style, self.author).display != Display::None
                }
                _ => false,
            })
            .collect();
        if items.is_empty() {
            return;
        }
        // `order` reorders items visually; a stable sort keeps document order among
        // equal-order items (the default, order:0, leaves them in source order).
        if items
            .iter()
            .any(|&c| computed_style(self.doc, c, &style, self.author).order != 0)
        {
            items.sort_by_key(|&c| computed_style(self.doc, c, &style, self.author).order);
        }

        let border_box_top = self.cursor_y;
        let border_box_left = x + style.margin.left;
        let h_extra = style.margin.left
            + style.margin.right
            + style.border.left
            + style.border.right
            + style.padding.left
            + style.padding.right;
        let content_w = match style.width {
            Some(len) => border_box_to_content(&style, len.to_px(style.font_size, avail)),
            None => (avail - h_extra).max(0.0),
        };
        let content_w = clamp_content_width(&style, content_w, avail);
        let content_left = border_box_left + style.border.left + style.padding.left;
        let border_box_w = content_w
            + style.padding.left
            + style.padding.right
            + style.border.left
            + style.border.right;

        let bg_idx = (style.background_color.a > 0).then(|| {
            self.rects.push(RectFill {
                x: border_box_left,
                y: border_box_top,
                w: border_box_w,
                h: 0.0,
                color: style.fade(style.background_color),
                radius: style.border_radius,
            });
            self.rects.len() - 1
        });

        self.cursor_y += style.border.top + style.padding.top;
        let row_top = self.cursor_y;
        let n = items.len() as f32;

        if style.flex_direction == FlexDirection::Column {
            // Column: stack items vertically, each at full content width, with `gap`
            // between them. `align-items` controls cross-axis (horizontal) placement;
            // an item with an explicit width can be flush-left (default/stretch),
            // centered, or flush-right within the content box via a post-layout
            // shift. When the container has an explicit `height` that exceeds the
            // items' stacked extent, the leftover space is distributed on the main
            // (vertical) axis per `justify-content`.
            let mut snaps: Vec<DisplayListMark> = Vec::new();
            for (i, &item) in items.iter().enumerate() {
                if i > 0 {
                    self.cursor_y += style.row_gap; // column main-axis = vertical
                }
                let istyle = computed_style(self.doc, item, &style, self.author);
                let ds = (
                    self.rects.len(),
                    self.runs.len(),
                    self.images.len(),
                    self.links.len(),
                    self.bounds.len(),
                );
                self.layout_block(item, istyle, content_left, content_w, None);
                // Cross-axis offset only applies to fixed-width items (a stretched
                // item already fills the content box, leaving nothing to distribute).
                if let Some(len) = istyle.width {
                    let outer = border_box_to_content(&istyle, len.to_px(istyle.font_size, content_w))
                        + istyle.padding.left
                        + istyle.padding.right
                        + istyle.border.left
                        + istyle.border.right
                        + istyle.margin.left
                        + istyle.margin.right;
                    let dx = match style.align_items {
                        AlignItems::FlexStart | AlignItems::Stretch => 0.0,
                        AlignItems::Center => ((content_w - outer) / 2.0).max(0.0),
                        AlignItems::FlexEnd => (content_w - outer).max(0.0),
                    };
                    if dx != 0.0 {
                        self.shift_display_list(ds, dx, 0.0);
                    }
                }
                snaps.push(ds);
            }
            // Vertical justify-content: distribute free space when an explicit height
            // leaves room below the stacked items.
            let items_total = self.cursor_y - row_top;
            let explicit_h = [style.height, style.min_height]
                .into_iter()
                .flatten()
                .map(|len| len.to_px(style.font_size, content_w))
                .fold(None, |acc: Option<f32>, v| Some(acc.map_or(v, |a| a.max(v))));
            if let Some(target_h) = explicit_h {
                let free = (target_h - items_total).max(0.0);
                if free > 0.0 {
                    let (lead, between_extra) = match style.justify_content {
                        JustifyContent::FlexStart => (0.0, 0.0),
                        JustifyContent::FlexEnd => (free, 0.0),
                        JustifyContent::Center => (free / 2.0, 0.0),
                        JustifyContent::SpaceBetween => {
                            (0.0, if n > 1.0 { free / (n - 1.0) } else { 0.0 })
                        }
                        JustifyContent::SpaceAround => {
                            let unit = free / n;
                            (unit / 2.0, unit)
                        }
                        JustifyContent::SpaceEvenly => {
                            let unit = free / (n + 1.0);
                            (unit, unit)
                        }
                    };
                    for (idx, ds) in snaps.iter().enumerate() {
                        let dy = lead + idx as f32 * between_extra;
                        if dy != 0.0 {
                            self.shift_display_list(*ds, 0.0, dy);
                        }
                    }
                    self.cursor_y = row_top + target_h;
                }
            }
            self.cursor_y += style.padding.bottom + style.border.bottom;
        } else {
            // Row: items lay out along the main axis. Items with an explicit `width`
            // take that as a fixed slot; the rest (flexible) share the remaining
            // width equally. If every item is fixed, the leftover free space is
            // distributed per `justify-content`; cross-axis placement within the
            // line height follows `align-items`.
            let total_gap = style.gap * (n - 1.0);
            let istyles: Vec<ComputedStyle> = items
                .iter()
                .map(|&it| computed_style(self.doc, it, &style, self.author))
                .collect();
            // Fixed main-axis footprint (margin box) for items with explicit width.
            let fixed: Vec<Option<f32>> = istyles
                .iter()
                .map(|s| {
                    s.width.map(|len| {
                        let c = border_box_to_content(s, len.to_px(s.font_size, content_w));
                        c + s.padding.left
                            + s.padding.right
                            + s.border.left
                            + s.border.right
                            + s.margin.left
                            + s.margin.right
                    })
                })
                .collect();
            if style.flex_wrap {
                // Multi-line flex: pack items (at their base size — explicit width or
                // shrink-to-content, capped to the line) onto lines that fit the
                // content width, breaking when the next item would overflow. Lines
                // stack vertically `gap` apart; `align-items` applies within each
                // line. (Per-line `justify-content` and line stretching are not yet
                // modeled — lines are left-packed.)
                let bases: Vec<f32> = istyles
                    .iter()
                    .enumerate()
                    .map(|(i, s)| {
                        fixed[i]
                            .unwrap_or_else(|| {
                                self.intrinsic_border_width(items[i], s)
                                    + s.margin.left
                                    + s.margin.right
                            })
                            .min(content_w)
                    })
                    .collect();
                let mut line_top = row_top;
                let mut i = 0usize;
                while i < items.len() {
                    // Greedily fill one line.
                    let mut line: Vec<usize> = Vec::new();
                    let mut line_w = 0.0f32;
                    while i < items.len() {
                        let add = if line.is_empty() {
                            bases[i]
                        } else {
                            style.gap + bases[i]
                        };
                        if !line.is_empty() && line_w + add > content_w {
                            break;
                        }
                        line_w += add;
                        line.push(i);
                        i += 1;
                    }
                    // Distribute this line's leftover width per `justify-content`.
                    let ln = line.len() as f32;
                    let used: f32 = line.iter().map(|&idx| bases[idx]).sum::<f32>()
                        + style.gap * (ln - 1.0);
                    let free = (content_w - used).max(0.0);
                    let (lead, between_extra) = match style.justify_content {
                        JustifyContent::FlexStart => (0.0, 0.0),
                        JustifyContent::FlexEnd => (free, 0.0),
                        JustifyContent::Center => (free / 2.0, 0.0),
                        JustifyContent::SpaceBetween => {
                            (0.0, if ln > 1.0 { free / (ln - 1.0) } else { 0.0 })
                        }
                        JustifyContent::SpaceAround => {
                            let unit = free / ln;
                            (unit / 2.0, unit)
                        }
                        JustifyContent::SpaceEvenly => {
                            let unit = free / (ln + 1.0);
                            (unit, unit)
                        }
                    };
                    // Lay out the line, recording snapshots for the align shift.
                    let mut cx = content_left + lead;
                    let mut max_h = 0.0f32;
                    let mut snaps: Vec<(DisplayListMark, f32)> = Vec::new();
                    for &idx in &line {
                        self.cursor_y = line_top;
                        let ds = (
                            self.rects.len(),
                            self.runs.len(),
                            self.images.len(),
                            self.links.len(),
                            self.bounds.len(),
                        );
                        self.layout_block(items[idx], istyles[idx], cx, bases[idx], None);
                        let h = self.cursor_y - line_top;
                        max_h = max_h.max(h);
                        snaps.push((ds, h));
                        cx += bases[idx] + style.gap + between_extra;
                    }
                    for (ds, h) in &snaps {
                        let dy = match style.align_items {
                            AlignItems::FlexStart | AlignItems::Stretch => 0.0,
                            AlignItems::Center => (max_h - h) / 2.0,
                            AlignItems::FlexEnd => max_h - h,
                        };
                        if dy != 0.0 {
                            self.shift_display_list(*ds, 0.0, dy);
                        }
                    }
                    line_top += max_h;
                    if i < items.len() {
                        line_top += style.row_gap; // between wrapped flex lines
                    }
                }
                self.cursor_y = line_top + style.padding.bottom + style.border.bottom;
                if let Some(idx) = bg_idx {
                    self.rects[idx].h = self.cursor_y - border_box_top;
                }
                return;
            }

            // When any item declares `flex-grow`, use the proper grow model: each
            // item starts at its base size (explicit-width footprint, else
            // shrink-to-content) and positive free space is split in proportion to
            // the grow factors. Otherwise keep the equal-share model with
            // `justify-content` distributing any leftover among fixed-width items.
            let any_grow = istyles.iter().any(|s| s.flex_grow > 0.0);
            // Base size of each item: explicit-width footprint, else shrink-to-content.
            let base: Vec<f32> = istyles
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    fixed[i].unwrap_or_else(|| {
                        self.intrinsic_border_width(items[i], s) + s.margin.left + s.margin.right
                    })
                })
                .collect();
            let base_sum: f32 = base.iter().sum();
            let overflow = base_sum + total_gap - content_w;
            let any_shrink = istyles.iter().any(|s| s.flex_shrink > 0.0);

            let (sizes, lead, between_extra): (Vec<f32>, f32, f32) = if overflow > 0.0 && any_shrink
            {
                // Items overflow the line: shrink each in proportion to
                // `flex-shrink × base size` until they fit (clamped at zero).
                let scaled: Vec<f32> = istyles
                    .iter()
                    .enumerate()
                    .map(|(i, s)| s.flex_shrink * base[i])
                    .collect();
                let total_scaled: f32 = scaled.iter().sum();
                let sizes: Vec<f32> = base
                    .iter()
                    .enumerate()
                    .map(|(i, &b)| {
                        if total_scaled > 0.0 {
                            (b - overflow * scaled[i] / total_scaled).max(0.0)
                        } else {
                            b
                        }
                    })
                    .collect();
                (sizes, 0.0, 0.0)
            } else if any_grow {
                let free = (content_w - total_gap - base_sum).max(0.0);
                let total_grow: f32 = istyles.iter().map(|s| s.flex_grow).sum();
                let sizes: Vec<f32> = base
                    .iter()
                    .enumerate()
                    .map(|(i, &b)| {
                        if total_grow > 0.0 {
                            b + free * istyles[i].flex_grow / total_grow
                        } else {
                            b
                        }
                    })
                    .collect();
                (sizes, 0.0, 0.0)
            } else {
                let flex_count = fixed.iter().filter(|f| f.is_none()).count();
                let fixed_sum: f32 = fixed.iter().filter_map(|f| *f).sum();
                let flex_w = if flex_count > 0 {
                    ((content_w - total_gap - fixed_sum) / flex_count as f32).max(0.0)
                } else {
                    0.0
                };
                let sizes: Vec<f32> = fixed.iter().map(|f| f.unwrap_or(flex_w)).collect();
                let used: f32 = sizes.iter().sum::<f32>() + total_gap;
                let free = (content_w - used).max(0.0);

                // Justify-content only has free space to distribute when no item is
                // flexible (flexible items already absorb it).
                let (lead, between_extra) = if flex_count > 0 {
                    (0.0, 0.0)
                } else {
                    match style.justify_content {
                        JustifyContent::FlexStart => (0.0, 0.0),
                        JustifyContent::FlexEnd => (free, 0.0),
                        JustifyContent::Center => (free / 2.0, 0.0),
                        JustifyContent::SpaceBetween => {
                            (0.0, if n > 1.0 { free / (n - 1.0) } else { 0.0 })
                        }
                        JustifyContent::SpaceAround => {
                            let unit = free / n;
                            (unit / 2.0, unit)
                        }
                        JustifyContent::SpaceEvenly => {
                            let unit = free / (n + 1.0);
                            (unit, unit)
                        }
                    }
                };
                (sizes, lead, between_extra)
            };

            let mut cx = content_left + lead;
            let mut max_h = 0.0f32;
            // Per-item display-list snapshot + height, for the cross-axis shift.
            let mut snaps: Vec<(DisplayListMark, f32)> = Vec::new();
            for (i, &item) in items.iter().enumerate() {
                self.cursor_y = row_top;
                let ds = (
                    self.rects.len(),
                    self.runs.len(),
                    self.images.len(),
                    self.links.len(),
                    self.bounds.len(),
                );
                self.layout_block(item, istyles[i], cx, sizes[i], None);
                let h = self.cursor_y - row_top;
                max_h = max_h.max(h);
                snaps.push((ds, h));
                cx += sizes[i] + style.gap + between_extra;
            }
            // align-items: offset each item vertically within the line box.
            for (ds, h) in &snaps {
                let dy = match style.align_items {
                    AlignItems::FlexStart | AlignItems::Stretch => 0.0,
                    AlignItems::Center => (max_h - h) / 2.0,
                    AlignItems::FlexEnd => max_h - h,
                };
                if dy != 0.0 {
                    self.shift_display_list(*ds, 0.0, dy);
                }
            }
            self.cursor_y = row_top + max_h + style.padding.bottom + style.border.bottom;
        }

        if let Some(idx) = bg_idx {
            self.rects[idx].h = self.cursor_y - border_box_top;
        }
    }

    /// Lay out a `display: grid` container: items flow row-major into
    /// `grid-template-columns` equal columns; each row's height is its tallest item.
    fn layout_grid(&mut self, id: NodeId, style: ComputedStyle, x: f32, avail: f32) {
        let items: Vec<NodeId> = self
            .doc
            .children(id)
            .filter(|&c| match &self.doc.node(c).data {
                NodeData::Element(_) => {
                    computed_style(self.doc, c, &style, self.author).display != Display::None
                }
                _ => false,
            })
            .collect();
        if items.is_empty() {
            return;
        }
        let cols = style.grid_columns.max(1) as usize;

        let border_box_top = self.cursor_y;
        let border_box_left = x + style.margin.left;
        let h_extra = style.margin.left
            + style.margin.right
            + style.border.left
            + style.border.right
            + style.padding.left
            + style.padding.right;
        let content_w = match style.width {
            Some(len) => border_box_to_content(&style, len.to_px(style.font_size, avail)),
            None => (avail - h_extra).max(0.0),
        };
        let content_w = clamp_content_width(&style, content_w, avail);
        let content_left = border_box_left + style.border.left + style.padding.left;
        let border_box_w = content_w
            + style.padding.left
            + style.padding.right
            + style.border.left
            + style.border.right;

        let bg_idx = (style.background_color.a > 0).then(|| {
            self.rects.push(RectFill {
                x: border_box_left,
                y: border_box_top,
                w: border_box_w,
                h: 0.0,
                color: style.fade(style.background_color),
                radius: style.border_radius,
            });
            self.rects.len() - 1
        });

        self.cursor_y += style.border.top + style.padding.top;
        // Resolve each column's width from its track size: fixed lengths take their
        // value; `fr` (and `auto`, treated as `1fr`) share the leftover space by
        // factor. Then compute each column's left edge (cumulative + gaps).
        let tracks = &style.grid_tracks[..cols];
        let fs = style.font_size;
        let fixed_sum: f32 = tracks
            .iter()
            .map(|t| match t {
                GridTrack::Len(l) => l.to_px(fs, content_w).max(0.0),
                _ => 0.0,
            })
            .sum();
        let fr_total: f32 = tracks
            .iter()
            .map(|t| match t {
                GridTrack::Fr(f) => *f,
                GridTrack::Auto => 1.0,
                GridTrack::Len(_) => 0.0,
            })
            .sum();
        let gaps = style.gap * (cols.saturating_sub(1)) as f32;
        let free = (content_w - fixed_sum - gaps).max(0.0);
        let fr_unit = if fr_total > 0.0 { free / fr_total } else { 0.0 };
        let col_w: Vec<f32> = tracks
            .iter()
            .map(|t| match t {
                GridTrack::Len(l) => l.to_px(fs, content_w).max(0.0),
                GridTrack::Fr(f) => f * fr_unit,
                GridTrack::Auto => fr_unit,
            })
            .collect();
        let mut col_x = Vec::with_capacity(cols);
        let mut acc = content_left;
        for w in &col_w {
            col_x.push(acc);
            acc += w + style.gap;
        }
        let grid_top = self.cursor_y;
        // Placement: scan the occupancy grid row-major, putting each item in the next
        // free cell where its column-span fits, marking the cells its col×row span
        // covers. Items may span columns and rows; spans are clamped to the grid.
        struct Placed {
            item: NodeId,
            style: ComputedStyle,
            row: usize,
            col: usize,
            rspan: usize,
            width: f32,
        }
        let mut occ: Vec<[bool; GRID_MAX_TRACKS]> = Vec::new();
        let free_at = |occ: &Vec<[bool; GRID_MAX_TRACKS]>, r: usize, c: usize, cs: usize| {
            (0..cs).all(|k| c + k < cols && (r >= occ.len() || !occ[r][c + k]))
        };
        let mut placed: Vec<Placed> = Vec::new();
        let mut cursor = (0usize, 0usize); // (row, col) search start
        for &item in &items {
            let istyle = computed_style(self.doc, item, &style, self.author);
            let cspan = (istyle.grid_column_span.max(1) as usize).min(cols);
            let rspan = istyle.grid_row_span.max(1) as usize;
            // Find the next free slot at or after the cursor.
            let (mut r, mut c) = cursor;
            loop {
                if c + cspan > cols {
                    r += 1;
                    c = 0;
                    continue;
                }
                if free_at(&occ, r, c, cspan) {
                    break;
                }
                c += 1;
            }
            // Mark the span's cells occupied (growing the occupancy grid as needed).
            while occ.len() < r + rspan {
                occ.push([false; GRID_MAX_TRACKS]);
            }
            for dr in 0..rspan {
                for dc in 0..cspan {
                    occ[r + dr][c + dc] = true;
                }
            }
            let width = col_w[c..c + cspan].iter().sum::<f32>() + style.gap * (cspan - 1) as f32;
            placed.push(Placed {
                item,
                style: istyle,
                row: r,
                col: c,
                rspan,
                width,
            });
            cursor = (r, c + cspan);
        }
        let nrows = occ.len();

        // Measure each item's natural height by laying it out at a throwaway origin,
        // then truncating the display list (it's re-laid for real once row heights
        // are known).
        let mut heights = vec![0.0f32; placed.len()];
        for (i, p) in placed.iter().enumerate() {
            let mark = (
                self.rects.len(),
                self.runs.len(),
                self.images.len(),
                self.links.len(),
                self.bounds.len(),
            );
            self.cursor_y = 0.0;
            self.layout_block(p.item, p.style, col_x[p.col], p.width, None);
            heights[i] = self.cursor_y;
            self.rects.truncate(mark.0);
            self.runs.truncate(mark.1);
            self.images.truncate(mark.2);
            self.links.truncate(mark.3);
            self.bounds.truncate(mark.4);
        }

        // Row heights: single-row items set their row's height directly; multi-row
        // items push any height deficit onto their last spanned row.
        let mut row_h = vec![0.0f32; nrows];
        for (i, p) in placed.iter().enumerate() {
            if p.rspan == 1 {
                row_h[p.row] = row_h[p.row].max(heights[i]);
            }
        }
        for (i, p) in placed.iter().enumerate() {
            if p.rspan > 1 {
                let last = (p.row + p.rspan - 1).min(nrows - 1);
                let spanned: f32 = row_h[p.row..=last].iter().sum::<f32>()
                    + style.row_gap * (last - p.row) as f32;
                if heights[i] > spanned {
                    row_h[last] += heights[i] - spanned;
                }
            }
        }
        // Cumulative y for each row top (grid content origin + heights + row gaps).
        let mut row_y = vec![grid_top; nrows];
        for r in 1..nrows {
            row_y[r] = row_y[r - 1] + row_h[r - 1] + style.row_gap;
        }

        // Real layout: place each item at its cell's (x, y).
        for p in &placed {
            self.cursor_y = row_y[p.row];
            self.layout_block(p.item, p.style, col_x[p.col], p.width, None);
        }
        self.cursor_y = grid_top
            + row_h.iter().sum::<f32>()
            + style.row_gap * (nrows.saturating_sub(1)) as f32;
        self.cursor_y += style.padding.bottom + style.border.bottom;
        if let Some(idx) = bg_idx {
            self.rects[idx].h = self.cursor_y - border_box_top;
        }
    }

    /// Lay out a `<table>` as a simple equal-column grid: columns share the table
    /// width equally; each cell is a block box; row height is the tallest cell.
    fn layout_table(&mut self, id: NodeId, style: ComputedStyle, x: f32, avail: f32) {
        let rows = self.collect_rows(id);
        if rows.is_empty() {
            return;
        }
        // Column count is the widest row once each cell's `colspan` is counted.
        let num_cols = rows
            .iter()
            .map(|r| r.iter().map(|&c| self.cell_colspan(c)).sum::<u32>())
            .max()
            .unwrap_or(1)
            .max(1);
        let table_left = x + style.margin.left;
        let table_w = match style.width {
            Some(len) => border_box_to_content(&style, len.to_px(style.font_size, avail)),
            None => (avail - style.margin.left - style.margin.right).max(0.0),
        };
        let table_w = clamp_content_width(&style, table_w, avail);
        let col_w = table_w / num_cols as f32;

        // A `<caption>` renders as a block spanning the table width, above the rows.
        if let Some(cap) = self.doc.children(id).find(|&c| {
            matches!(&self.doc.node(c).data, NodeData::Element(e) if e.name.is_html("caption"))
        }) {
            let cap_style = computed_style(self.doc, cap, &style, self.author);
            self.layout_block(cap, cap_style, table_left, table_w, None);
        }

        for row in &rows {
            let row_top = self.cursor_y;
            let mut max_h = 0.0f32;
            let mut col = 0u32;
            for &cell in row {
                let span = self
                    .cell_colspan(cell)
                    .min(num_cols - col.min(num_cols - 1));
                let cell_x = table_left + col as f32 * col_w;
                let cell_w = span as f32 * col_w;
                self.cursor_y = row_top;
                let cell_style = computed_style(self.doc, cell, &style, self.author);
                self.layout_block(cell, cell_style, cell_x, cell_w, None);
                max_h = max_h.max(self.cursor_y - row_top);
                col += span.max(1);
            }
            self.cursor_y = row_top + max_h;
        }
    }

    /// The `colspan` of a table cell (defaults to 1, clamped to `>= 1`).
    fn cell_colspan(&self, cell: NodeId) -> u32 {
        self.doc
            .node(cell)
            .as_element()
            .and_then(|e| e.attr("colspan"))
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(1)
            .max(1)
    }

    /// Collect a table's rows (flattening `thead`/`tbody`/`tfoot`); each row is the
    /// list of its `td`/`th` cells.
    fn collect_rows(&self, table: NodeId) -> Vec<Vec<NodeId>> {
        let mut rows = Vec::new();
        let push_row = |this: &Self, tr: NodeId, rows: &mut Vec<Vec<NodeId>>| {
            let cells: Vec<NodeId> = this
                .doc
                .children(tr)
                .filter(|&c| {
                    matches!(&this.doc.node(c).data, NodeData::Element(e)
                        if e.name.is_html("td") || e.name.is_html("th"))
                })
                .collect();
            if !cells.is_empty() {
                rows.push(cells);
            }
        };
        for child in self.doc.children(table) {
            match &self.doc.node(child).data {
                NodeData::Element(e) if e.name.is_html("tr") => push_row(self, child, &mut rows),
                NodeData::Element(e)
                    if e.name.is_html("thead")
                        || e.name.is_html("tbody")
                        || e.name.is_html("tfoot") =>
                {
                    for tr in self.doc.children(child) {
                        if matches!(&self.doc.node(tr).data, NodeData::Element(e) if e.name.is_html("tr"))
                        {
                            push_row(self, tr, &mut rows);
                        }
                    }
                }
                _ => {}
            }
        }
        rows
    }

    /// Flatten an inline subtree into styled words, collapsing whitespace and
    /// tracking break opportunities via `space_before`.
    /// Concatenate all descendant text verbatim (for `white-space: pre`), with no
    /// whitespace collapsing. Element boundaries contribute no spacing.
    fn gather_raw_text(&self, id: NodeId, out: &mut String) {
        match &self.doc.node(id).data {
            NodeData::Text(t) => out.push_str(t),
            _ => {
                for child in self.doc.children(id) {
                    self.gather_raw_text(child, out);
                }
            }
        }
    }

    /// Gather a generated-content string (`::before`/`::after`) into inline words,
    /// styled like the element itself.
    fn gather_generated(
        &self,
        text: &str,
        style: &ComputedStyle,
        words: &mut Vec<InlineWord>,
        pending_space: &mut bool,
    ) {
        let (color, background) = if style.hidden {
            (
                argus_geometry::Color::TRANSPARENT,
                argus_geometry::Color::TRANSPARENT,
            )
        } else {
            (style.fade(style.color), style.fade(style.background_color))
        };
        let mut first = true;
        for word in text.split_whitespace() {
            words.push(InlineWord {
                text: transform_text(word, style.text_transform),
                font_size: style.font_size,
                color,
                background,
                space_before: *pending_space || !first,
                underline: style.underline && !style.hidden,
                strike: style.strike && !style.hidden,
                overline: style.overline && !style.hidden,
                decoration_color: style.fade(style.decoration_color.unwrap_or(style.color)),
                href: None,
                hard_break: false,
                baseline_shift: 0.0,
            });
            *pending_space = false;
            first = false;
        }
        if text.ends_with(char::is_whitespace) {
            *pending_space = true;
        }
    }

    fn gather_inline(
        &self,
        id: NodeId,
        style: &ComputedStyle,
        link: Option<Rc<str>>,
        words: &mut Vec<InlineWord>,
        pending_space: &mut bool,
    ) {
        match &self.doc.node(id).data {
            NodeData::Text(t) => {
                if t.starts_with(char::is_whitespace) {
                    *pending_space = true;
                }
                let shift = match style.vertical_align {
                    VerticalAlign::Sub => style.font_size * 0.2,
                    VerticalAlign::Super => -style.font_size * 0.4,
                    VerticalAlign::Baseline => 0.0,
                };
                // `visibility: hidden` keeps the words' space but paints nothing.
                let (color, background) = if style.hidden {
                    (
                        argus_geometry::Color::TRANSPARENT,
                        argus_geometry::Color::TRANSPARENT,
                    )
                } else {
                    (style.fade(style.color), style.fade(style.background_color))
                };
                let mut first = true;
                for word in t.split_whitespace() {
                    words.push(InlineWord {
                        text: transform_text(word, style.text_transform),
                        font_size: style.font_size,
                        color,
                        background,
                        // Words within a text node are separated by whitespace.
                        space_before: *pending_space || !first,
                        underline: style.underline && !style.hidden,
                        strike: style.strike && !style.hidden,
                        overline: style.overline && !style.hidden,
                        decoration_color: style.fade(style.decoration_color.unwrap_or(style.color)),
                        href: if style.hidden { None } else { link.clone() },
                        hard_break: false,
                        baseline_shift: shift,
                    });
                    *pending_space = false;
                    first = false;
                }
                if t.ends_with(char::is_whitespace) {
                    *pending_space = true;
                }
            }
            NodeData::Element(e) => {
                let cstyle = computed_style(self.doc, id, style, self.author);
                if cstyle.display == Display::None {
                    return;
                }
                // A <br> forces a line break in the inline flow.
                if e.name.is_html("br") {
                    words.push(InlineWord {
                        text: String::new(),
                        font_size: style.font_size,
                        color: style.fade(style.color),
                        background: argus_geometry::Color::TRANSPARENT,
                        space_before: false,
                        underline: false,
                        strike: false,
                        overline: false,
                        decoration_color: argus_geometry::Color::TRANSPARENT,
                        href: link.clone(),
                        hard_break: true,
                        baseline_shift: 0.0,
                    });
                    *pending_space = false;
                    return;
                }
                // An <a href> sets the link target for its descendants.
                let child_link = if e.name.is_html("a") {
                    e.attr("href").map(Rc::from).or(link)
                } else {
                    link
                };
                for child in self.doc.children(id) {
                    self.gather_inline(child, &cstyle, child_link.clone(), words, pending_space);
                }
            }
            _ => {}
        }
    }

    /// Break `words` into lines that fit `width`, aligned per the block's
    /// `text-align`, emitting one [`TextRun`] per word (each in its own style).
    fn flush_words(
        &mut self,
        words: &mut Vec<InlineWord>,
        block: &ComputedStyle,
        x: f32,
        width: f32,
    ) {
        if words.is_empty() {
            return;
        }
        let mut taken = std::mem::take(words);
        // `overflow-wrap: break-word`: pre-split any word wider than the content box
        // into chunks that fit, so it wraps instead of overflowing. Each later chunk
        // loses its leading space so it can break onto its own line.
        if block.break_word && width > 0.0 {
            let mut expanded: Vec<InlineWord> = Vec::with_capacity(taken.len());
            for w in taken {
                if !w.text.is_empty() && self.font.measure(&w.text, w.font_size) > width {
                    for (k, chunk) in self.split_word(&w.text, w.font_size, width).into_iter().enumerate() {
                        let mut nw = w.clone();
                        nw.text = chunk;
                        if k > 0 {
                            nw.space_before = false;
                        }
                        expanded.push(nw);
                    }
                } else {
                    expanded.push(w);
                }
            }
            taken = expanded;
        }

        // `text-overflow: ellipsis` on a non-wrapping line: if the joined text
        // overflows the content box, render a single truncated run ending with `…`.
        if block.ellipsis && block.nowrap && width > 0.0 {
            let mut text = String::new();
            let mut max_size = 0.0f32;
            for (i, w) in taken.iter().enumerate() {
                if w.text.is_empty() {
                    continue;
                }
                if i > 0 && w.space_before {
                    text.push(' ');
                }
                text.push_str(&w.text);
                max_size = max_size.max(w.font_size);
            }
            if self.font.measure(&text, max_size) > width {
                let color = taken
                    .iter()
                    .find(|w| !w.text.is_empty())
                    .map(|w| w.color)
                    .unwrap_or(block.fade(block.color));
                let clipped = self.truncate_ellipsis(&text, max_size, width);
                let baseline = self.cursor_y + self.font.ascent_px(max_size);
                self.runs.push(TextRun {
                    x,
                    baseline,
                    text: clipped,
                    size_px: max_size,
                    color,
                });
                self.cursor_y += max_size * block.line_height;
                return;
            }
        }
        let content_right = x + width;

        // Greedily assign words to lines. Each line's inline region is narrowed by
        // any floats overlapping its vertical band, so the available width (and the
        // left edge) can change line to line. Records `(range, left x, region width)`.
        let mut lines: Vec<(std::ops::Range<usize>, f32, f32)> = Vec::new();
        let mut y = self.cursor_y;
        let mut i = 0usize;
        while i < taken.len() {
            let line_start = i;
            let first_size = taken[line_start].font_size;
            let probe_h = (first_size * block.line_height).max(1.0);
            let (lx, rx) = self.float_band(x, content_right, y, y + probe_h);
            let region_w = (rx - lx).max(0.0);
            // The first line has less room when `text-indent` is set.
            let indent = if line_start == 0 { block.text_indent } else { 0.0 };
            let avail = (region_w - indent).max(0.0);
            let mut pen = 0.0f32;
            let mut line_max = 0.0f32;
            while i < taken.len() {
                let w = &taken[i];
                // A <br> forces a break before it (it begins the next line).
                if w.hard_break && i > line_start {
                    break;
                }
                let space = if i > line_start && w.space_before {
                    self.font.measure(" ", w.font_size) + block.word_spacing
                } else {
                    0.0
                };
                let ww = self.font.measure(&w.text, w.font_size);
                if !block.nowrap && i > line_start && pen + space + ww > avail {
                    break;
                }
                pen += space + ww;
                line_max = line_max.max(w.font_size);
                i += 1;
            }
            lines.push((line_start..i, lx, region_w));
            y += line_max.max(first_size) * block.line_height;
        }

        let line_count = lines.len();
        for (line_idx, (range, lx, region_w)) in lines.into_iter().enumerate() {
            let line = &taken[range.clone()];
            // Line width, gap count, and tallest font for baseline/height.
            let mut line_w = 0.0f32;
            let mut max_size = 0.0f32;
            let mut gaps = 0u32;
            for (j, w) in line.iter().enumerate() {
                let has_space = j > 0 && w.space_before;
                let space = if has_space {
                    self.font.measure(" ", w.font_size) + block.word_spacing
                } else {
                    0.0
                };
                if has_space && !w.text.is_empty() {
                    gaps += 1;
                }
                line_w += space + self.font.measure(&w.text, w.font_size);
                max_size = max_size.max(w.font_size);
            }
            // `justify` stretches inter-word gaps on every line but the last
            // (and not the line just before a forced `<br>` break).
            let is_last =
                line_idx + 1 == line_count || taken.get(range.end).is_some_and(|w| w.hard_break);
            let justify_extra = if block.text_align == TextAlign::Justify && !is_last && gaps > 0 {
                ((region_w - line_w) / gaps as f32).max(0.0)
            } else {
                0.0
            };
            let offset = match block.text_align {
                TextAlign::Center => ((region_w - line_w) / 2.0).max(0.0),
                TextAlign::Right => (region_w - line_w).max(0.0),
                _ => 0.0,
            };
            let baseline = self.cursor_y + self.font.ascent_px(max_size);

            // `text-indent` shifts the block's first line only.
            let indent = if line_idx == 0 {
                block.text_indent
            } else {
                0.0
            };
            let line_top = self.cursor_y;
            let line_h = max_size * block.line_height;
            let mut pen_x = lx + offset + indent;
            for (j, w) in line.iter().enumerate() {
                // The <br> sentinel only contributes line height, no glyphs.
                if w.text.is_empty() {
                    continue;
                }
                if j > 0 && w.space_before {
                    pen_x +=
                        self.font.measure(" ", w.font_size) + block.word_spacing + justify_extra;
                }
                let word_w = self.font.measure(&w.text, w.font_size);
                let wb = baseline + w.baseline_shift;
                // Inline background (e.g. <mark>, highlighted span) paints behind the
                // glyphs, covering the line box around this word.
                if w.background.a > 0 {
                    self.rects.push(rect(
                        pen_x,
                        line_top + w.baseline_shift,
                        word_w,
                        line_h,
                        w.background,
                    ));
                }
                self.runs.push(TextRun {
                    x: pen_x,
                    baseline: wb,
                    text: w.text.clone(),
                    size_px: w.font_size,
                    color: w.color,
                });
                if w.underline {
                    let uy = wb + (w.font_size * 0.08).max(1.0);
                    let uh = (w.font_size / 16.0).max(1.0);
                    self.rects.push(rect(pen_x, uy, word_w, uh, w.decoration_color));
                }
                if w.strike {
                    let sy = wb - self.font.ascent_px(w.font_size) * 0.32;
                    let sh = (w.font_size / 16.0).max(1.0);
                    self.rects.push(rect(pen_x, sy, word_w, sh, w.decoration_color));
                }
                if w.overline {
                    // A line at the top of the glyph box (just above the ascent).
                    let oy = wb - self.font.ascent_px(w.font_size);
                    let oh = (w.font_size / 16.0).max(1.0);
                    self.rects.push(rect(pen_x, oy, word_w, oh, w.decoration_color));
                }
                if let Some(href) = &w.href {
                    self.links.push(LinkBox {
                        x: pen_x,
                        y: line_top,
                        w: word_w,
                        h: line_h,
                        href: href.to_string(),
                    });
                }
                pen_x += word_w;
            }
            self.cursor_y += line_h;
        }
    }
}

/// Apply a `text-transform` to one whitespace-delimited word.
fn transform_text(word: &str, transform: TextTransform) -> String {
    match transform {
        TextTransform::None => word.to_string(),
        TextTransform::Uppercase => word.to_uppercase(),
        TextTransform::Lowercase => word.to_lowercase(),
        TextTransform::Capitalize => {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        }
    }
}

fn rect(x: f32, y: f32, w: f32, h: f32, color: argus_geometry::Color) -> RectFill {
    RectFill {
        x,
        y,
        w,
        h,
        color,
        radius: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argus_html::parse;

    fn system_font() -> Option<Font> {
        for path in [
            "/System/Library/Fonts/Geneva.ttf",
            "/System/Library/Fonts/Monaco.ttf",
            "/System/Library/Fonts/SFNS.ttf",
        ] {
            if let Ok(bytes) = std::fs::read(path) {
                if let Ok(font) = Font::from_bytes(bytes) {
                    return Some(font);
                }
            }
        }
        None
    }

    #[test]
    fn boxes_borders_align_and_wrap() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let html = "<style>\
            .card { border: 3px solid #333; padding: 10px; background-color: #eee }\
            .c { text-align: center }\
            </style>\
            <div class=\"card\"><p class=\"c\">centered</p>\
            <p>one two three four five six seven eight nine ten eleven twelve thirteen \
            fourteen fifteen sixteen seventeen eighteen</p></div>";
        let doc = parse(html);
        let layout = layout(&doc, &font, 200.0, &ImageSizes::new());

        // The .card div has a background rect + 4 border rects.
        assert!(
            layout.rects.len() >= 5,
            "expected bg + 4 borders, got {}",
            layout.rects.len()
        );
        // The centered paragraph's run is offset from the content's left edge.
        let p_runs: Vec<_> = layout
            .runs
            .iter()
            .filter(|r| r.text.contains("centered"))
            .collect();
        assert_eq!(p_runs.len(), 1);
        assert!(
            p_runs[0].x > 8.0 + 3.0 + 10.0,
            "centered text should be indented past padding/border"
        );
        // The long paragraph still wraps.
        assert!(
            layout
                .runs
                .iter()
                .filter(|r| r.text.contains("eighteen"))
                .count()
                >= 1
        );
    }

    #[test]
    fn sub_and_super_shift_the_baseline() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let doc = parse("<p>x<sub>down</sub><sup>up</sup>base</p>");
        let l = layout(&doc, &font, 400.0, &ImageSizes::new());
        let y = |t: &str| l.runs.iter().find(|r| r.text == t).map(|r| r.baseline);
        let base = y("base").expect("base run");
        let down = y("down").expect("sub run");
        let up = y("up").expect("super run");
        assert!(
            down > base,
            "subscript {down} should sit below baseline {base}"
        );
        assert!(
            up < base,
            "superscript {up} should sit above baseline {base}"
        );
    }

    #[test]
    fn inline_background_paints_behind_words() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // A <mark> (UA yellow background) paints a rect behind its word; the
        // surrounding plain text does not.
        let doc = parse("<p>plain <mark>highlighted</mark> plain</p>");
        let l = layout(&doc, &font, 400.0, &ImageSizes::new());
        let yellow = l
            .rects
            .iter()
            .filter(|r| r.color.r == 0xFE && r.color.g == 0xF0 && r.color.b == 0x8A)
            .count();
        assert_eq!(yellow, 1, "exactly the marked word gets a highlight rect");
    }

    #[test]
    fn details_hides_content_unless_open() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let render = |open: &str| -> Vec<String> {
            let html = format!(
                "<details{open}><summary>More info</summary><p>Hidden body text</p></details>"
            );
            let doc = parse(&html);
            let l = layout(&doc, &font, 400.0, &ImageSizes::new());
            l.runs.iter().map(|r| r.text.clone()).collect()
        };
        // Closed: only the summary renders.
        let closed = render("");
        assert!(
            closed.contains(&"More".to_string()),
            "summary shows: {closed:?}"
        );
        assert!(
            !closed.contains(&"Hidden".to_string()),
            "body hidden: {closed:?}"
        );
        // Open: the body renders too.
        let open = render(" open");
        assert!(
            open.contains(&"Hidden".to_string()),
            "open body shows: {open:?}"
        );
    }

    #[test]
    fn input_renders_value_in_a_box() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let doc = parse(
            "<input value=\"hello\"><input placeholder=\"type here\"><button>Submit</button>",
        );
        let l = layout(&doc, &font, 400.0, &ImageSizes::new());
        let texts: Vec<&str> = l.runs.iter().map(|r| r.text.as_str()).collect();
        // The value, the placeholder words, and the button label all render.
        assert!(texts.contains(&"hello"), "input value: {texts:?}");
        assert!(
            texts.contains(&"type") && texts.contains(&"here"),
            "placeholder: {texts:?}"
        );
        assert!(texts.contains(&"Submit"), "button label: {texts:?}");
        // Each field is a bordered box (border rects exist).
        assert!(
            l.rects.iter().filter(|r| r.w > 0.0 && r.h > 0.0).count() >= 3,
            "expected boxes for the fields"
        );
    }

    #[test]
    fn select_shows_selected_option_and_checkbox_checks() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let doc = parse(
            "<select><option>Alpha</option><option selected>Beta</option><option>Gamma</option></select>\
             <input type=checkbox checked><input type=checkbox>",
        );
        let l = layout(&doc, &font, 400.0, &ImageSizes::new());
        let texts: Vec<&str> = l.runs.iter().map(|r| r.text.as_str()).collect();
        // Only the selected option renders; the others (display:none) do not.
        assert!(texts.contains(&"Beta"), "selected option: {texts:?}");
        assert!(
            !texts.contains(&"Alpha") && !texts.contains(&"Gamma"),
            "others hidden: {texts:?}"
        );
        // A checked checkbox adds an inner fill rect; the unchecked one does not —
        // so there is at least one small filled mark.
        assert!(
            l.rects
                .iter()
                .any(|r| r.w > 2.0 && r.w < 16.0 && r.h > 2.0 && r.h < 16.0),
            "checked checkbox should draw a small mark"
        );
    }

    #[test]
    fn element_at_hit_tests_id_boxes() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let doc = parse(
            "<div id=\"outer\" style=\"padding:20px\">\
               <p id=\"inner\">hello</p>\
             </div>",
        );
        let l = layout(&doc, &font, 400.0, &ImageSizes::new());
        // A point inside the inner paragraph resolves to the deepest id (inner).
        let inner = l
            .bounds
            .iter()
            .find(|b| b.id == "inner")
            .expect("inner bound");
        assert_eq!(l.element_at(inner.x + 2.0, inner.y + 2.0), Some("inner"));
        // A point in the outer's top padding (above the inner box) resolves to outer.
        let outer = l
            .bounds
            .iter()
            .find(|b| b.id == "outer")
            .expect("outer bound");
        assert!(outer.y + 5.0 < inner.y, "padding should sit above inner");
        assert_eq!(l.element_at(outer.x + 5.0, outer.y + 5.0), Some("outer"));
        // Far outside hits nothing.
        assert_eq!(l.element_at(5000.0, 5000.0), None);
    }

    #[test]
    fn word_spacing_widens_gaps() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let second_word_x = |css: &str| -> f32 {
            let doc = parse(&format!("<p style=\"{css}\">alpha beta</p>"));
            let l = layout(&doc, &font, 400.0, &ImageSizes::new());
            l.runs
                .iter()
                .find(|r| r.text == "beta")
                .map(|r| r.x)
                .unwrap()
        };
        assert!(
            second_word_x("word-spacing: 20px") > second_word_x("") + 15.0,
            "word-spacing should push later words right"
        );
    }

    #[test]
    fn text_indent_shifts_first_line_only() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let doc = parse(
            "<p style=\"text-indent: 40px\">one two three four five six seven eight \
             nine ten eleven twelve thirteen fourteen fifteen</p>",
        );
        let l = layout(&doc, &font, 200.0, &ImageSizes::new());
        let mut by_line: std::collections::BTreeMap<i32, f32> = std::collections::BTreeMap::new();
        for r in &l.runs {
            let y = r.baseline as i32;
            by_line.entry(y).or_insert(f32::INFINITY);
            let e = by_line.get_mut(&y).unwrap();
            *e = e.min(r.x);
        }
        let xs: Vec<f32> = by_line.values().copied().collect();
        assert!(xs.len() >= 2, "paragraph should wrap to multiple lines");
        // First line starts ~40px further right than the second line.
        assert!(
            (xs[0] - xs[1] - 40.0).abs() < 1.0,
            "indent: {} vs {}",
            xs[0],
            xs[1]
        );
    }

    #[test]
    fn nowrap_keeps_text_on_one_line() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let line_count = |css: &str| -> usize {
            let html = format!(
                "<p style=\"{css}\">one two three four five six seven eight nine ten \
                 eleven twelve thirteen fourteen fifteen sixteen seventeen</p>"
            );
            let doc = parse(&html);
            let l = layout(&doc, &font, 160.0, &ImageSizes::new());
            let mut ys: Vec<i32> = l.runs.iter().map(|r| r.baseline as i32).collect();
            ys.sort_unstable();
            ys.dedup();
            ys.len()
        };
        assert!(line_count("") > 1, "default text should wrap");
        assert_eq!(
            line_count("white-space: nowrap"),
            1,
            "nowrap stays on one line"
        );
    }

    #[test]
    fn text_decoration_overline_draws_a_line_above_text() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let has_line_above = |css: &str| -> bool {
            let html = format!("<p style=\"{css}\">hi</p>");
            let doc = parse(&html);
            let l = layout(&doc, &font, 400.0, &ImageSizes::new());
            let baseline = l.runs.iter().find(|r| r.text == "hi").unwrap().baseline;
            // A thin rect well above the baseline = the overline.
            l.rects
                .iter()
                .any(|r| r.w > 0.0 && r.h < 3.0 && r.y < baseline - 4.0)
        };
        assert!(has_line_above("text-decoration: overline"), "overline drawn above text");
        assert!(!has_line_above(""), "no overline without the decoration");
    }

    #[test]
    fn text_decoration_color_differs_from_text() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // Black text with a red underline: the underline rect is red, the text run
        // black.
        let html = "<p style=\"color:#000; text-decoration: underline; text-decoration-color:#ff0000\">hi</p>";
        let doc = parse(html);
        let l = layout(&doc, &font, 400.0, &ImageSizes::new());
        let run = l.runs.iter().find(|r| r.text == "hi").unwrap();
        // The underline rect (thin, below the baseline) is red, not the text color.
        let underline = l
            .rects
            .iter()
            .find(|r| r.w > 0.0 && r.h < 3.0 && r.y > run.baseline)
            .expect("underline rect");
        assert!(underline.color.r > 200 && underline.color.g < 60, "underline is red, got {:?}", underline.color);
        assert!(run.color.r < 40, "text stays black, got {:?}", run.color);
    }

    #[test]
    fn text_overflow_ellipsis_truncates_nowrap_line() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // A long nowrap line in a narrow box with text-overflow:ellipsis collapses to
        // a single run ending in `…`; without it the text stays full.
        let render = |css: &str| -> Vec<String> {
            let html = format!(
                "<div style=\"width:80px; white-space:nowrap; overflow:hidden; {css}\">the quick brown fox jumps</div>"
            );
            let doc = parse(&html);
            let l = layout(&doc, &font, 400.0, &ImageSizes::new());
            l.runs.iter().map(|r| r.text.clone()).collect()
        };
        let with = render("text-overflow: ellipsis");
        assert_eq!(with.len(), 1, "ellipsis collapses to one run: {with:?}");
        assert!(with[0].ends_with('…'), "ends with ellipsis: {:?}", with[0]);
        assert!(!with[0].contains("jumps"), "tail truncated: {:?}", with[0]);
        // Without ellipsis, the full text is present (multiple word runs, no …).
        let without = render("");
        assert!(without.iter().any(|t| t == "jumps"), "full text kept: {without:?}");
        assert!(without.iter().all(|t| !t.ends_with('…')), "no ellipsis: {without:?}");
    }

    #[test]
    fn overflow_wrap_break_word_splits_long_words() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // A single very long word in a narrow box. Without break-word it stays one
        // run (overflows); with break-word it splits across multiple lines.
        let line_count = |css: &str| -> usize {
            let html = format!(
                "<p style=\"width:80px; {css}\">supercalifragilisticexpialidocioussupercalifragilistic</p>"
            );
            let doc = parse(&html);
            let l = layout(&doc, &font, 400.0, &ImageSizes::new());
            let mut ys: Vec<i32> = l.runs.iter().map(|r| r.baseline as i32).collect();
            ys.sort_unstable();
            ys.dedup();
            ys.len()
        };
        assert_eq!(line_count(""), 1, "default: the long word stays on one line");
        assert!(
            line_count("overflow-wrap: break-word") > 1,
            "break-word splits the long word across lines"
        );
    }

    #[test]
    fn input_type_color_renders_a_swatch() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // <input type=color value=#ff0000> paints a red swatch rect.
        let html = "<input type=\"color\" value=\"#ff0000\" style=\"width:30px; height:20px\">";
        let doc = parse(html);
        let l = layout(&doc, &font, 400.0, &ImageSizes::new());
        let red = l
            .rects
            .iter()
            .any(|r| r.color.r > 200 && r.color.g < 60 && r.color.b < 60 && r.w > 10.0);
        assert!(red, "expected a red color swatch");
    }

    #[test]
    fn per_side_border_colors() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // A box with a uniform border but a red top and blue left override. The four
        // border rects should carry the per-side colors.
        let html = "<div style=\"border:4px solid black; border-top-color:#ff0000; border-left-color:#0000ff; width:50px; height:30px\"></div>";
        let doc = parse(html);
        let l = layout(&doc, &font, 400.0, &ImageSizes::new());
        let red = l.rects.iter().any(|r| r.color.r > 200 && r.color.g < 60 && r.color.b < 60 && r.h < 6.0);
        let blue = l.rects.iter().any(|r| r.color.b > 200 && r.color.r < 60 && r.w < 6.0);
        assert!(red, "red top border present");
        assert!(blue, "blue left border present");
    }

    #[test]
    fn progress_renders_a_filled_bar() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // <progress value=0.25 max=1> → a 160px track with a ~40px blue fill.
        let html = "<progress value=\"0.25\" max=\"1\"></progress>";
        let doc = parse(html);
        let l = layout(&doc, &font, 400.0, &ImageSizes::new());
        // Track is the wider light-gray rect; fill is the narrower colored rect.
        let track = l.rects.iter().find(|r| r.w > 100.0).expect("track rect");
        let fill = l
            .rects
            .iter()
            .find(|r| r.color.b > 150 && r.w < track.w * 0.5)
            .expect("fill rect");
        assert!((fill.w - track.w * 0.25).abs() < 2.0, "fill ~25% of track: {} vs {}", fill.w, track.w);
        // An indeterminate progress (no value) draws only the track, no blue fill.
        let doc2 = parse("<progress></progress>");
        let l2 = layout(&doc2, &font, 400.0, &ImageSizes::new());
        assert!(l2.rects.iter().all(|r| !(r.color.b > 150 && r.color.r < 100)), "no fill when indeterminate");
    }

    #[test]
    fn tab_size_expands_tabs_in_preformatted_text() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // In a <pre>, a leading tab expands to `tab-size` spaces; a smaller tab-size
        // indents the text less, so "x" starts further left.
        let x_of = |ts: u32| -> f32 {
            let html = format!("<pre style=\"tab-size:{ts}\">\tx</pre>");
            let doc = parse(&html);
            let l = layout(&doc, &font, 400.0, &ImageSizes::new());
            // The run text begins with the expanded spaces; find the run containing x.
            let run = l.runs.iter().find(|r| r.text.contains('x')).unwrap();
            // Measure where the 'x' glyph lands within the run.
            let lead = run.text.split('x').next().unwrap_or("");
            run.x + font.measure(lead, run.size_px)
        };
        let narrow = x_of(2);
        let wide = x_of(8);
        assert!(wide > narrow + 10.0, "larger tab-size indents more: {narrow} -> {wide}");
    }

    #[test]
    fn pre_wrap_preserves_spaces_and_wraps() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // pre-wrap keeps the multiple spaces (unlike pre-line) and still wraps the
        // long content within the narrow box.
        let html = "<p style=\"white-space: pre-wrap; width:90px\">a    b the quick brown fox jumps over</p>";
        let doc = parse(html);
        let l = layout(&doc, &font, 400.0, &ImageSizes::new());
        // The first visual run preserves the 4-space gap between a and b.
        assert!(l.runs[0].text.starts_with("a    b") || l.runs[0].text == "a    b ", "spaces preserved, got {:?}", l.runs[0].text);
        // Long content wraps to multiple visual lines.
        let mut ys: Vec<i32> = l.runs.iter().map(|r| r.baseline as i32).collect();
        ys.sort_unstable();
        ys.dedup();
        assert!(ys.len() >= 2, "pre-wrap wraps long lines, got {} lines", ys.len());
    }

    #[test]
    fn pre_line_preserves_newlines_collapses_spaces_and_wraps() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // pre-line keeps the explicit newline (two paragraphs), collapses the runs of
        // spaces, and wraps the long second line within the narrow content box.
        let html = "<p style=\"white-space: pre-line\">a    b\nthe quick brown fox jumps over the lazy dog again and again</p>";
        let doc = parse(html);
        let l = layout(&doc, &font, 160.0, &ImageSizes::new());
        // First visual run is "a b" — the 4-space run collapsed to one space.
        assert_eq!(l.runs[0].text, "a b", "spaces collapsed, newline kept");
        // The second source line wraps into multiple visual lines (distinct baselines).
        let mut ys: Vec<i32> = l.runs.iter().map(|r| r.baseline as i32).collect();
        ys.sort_unstable();
        ys.dedup();
        assert!(ys.len() >= 3, "newline + wrapping → ≥3 visual lines, got {}", ys.len());
    }

    #[test]
    fn justify_stretches_non_last_lines() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // First-line right edge: justified text should reach further right than
        // left-aligned text (it fills the content width).
        let first_line_right = |align: &str| -> f32 {
            let html = format!(
                "<p style=\"text-align:{align}\">one two three four five six seven eight \
                 nine ten eleven twelve thirteen fourteen fifteen sixteen</p>"
            );
            let doc = parse(&html);
            let l = layout(&doc, &font, 200.0, &ImageSizes::new());
            let min_baseline = l
                .runs
                .iter()
                .map(|r| r.baseline)
                .fold(f32::INFINITY, f32::min);
            l.runs
                .iter()
                .filter(|r| (r.baseline - min_baseline).abs() < 0.5)
                .map(|r| r.x + font.measure(&r.text, r.size_px))
                .fold(0.0, f32::max)
        };
        let left = first_line_right("left");
        let just = first_line_right("justify");
        // The justified first line fills to the content's right edge
        // (PAGE_MARGIN + content width = 8 + (200 - 16) = 192); left-aligned does not.
        let right_edge = PAGE_MARGIN + (200.0 - 2.0 * PAGE_MARGIN);
        assert!(
            (just - right_edge).abs() < 1.5,
            "justified right {just} vs edge {right_edge}"
        );
        assert!(
            just > left + 1.0,
            "justify {just} should exceed left {left}"
        );
    }

    #[test]
    fn line_height_scales_line_spacing() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // A two-line paragraph: doubling line-height roughly doubles the gap
        // between the two lines' baselines.
        let body = "<p style=\"line-height: {LH}\">one two three four five six seven \
                    eight nine ten eleven twelve thirteen fourteen fifteen</p>";
        let gap = |lh: &str| -> f32 {
            let doc = parse(&body.replace("{LH}", lh));
            let l = layout(&doc, &font, 160.0, &ImageSizes::new());
            let mut ys: Vec<f32> = l.runs.iter().map(|r| r.baseline).collect();
            ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
            ys.dedup_by(|a, b| (*a - *b).abs() < 0.5);
            assert!(ys.len() >= 2, "expected the paragraph to wrap");
            ys[1] - ys[0]
        };
        let single = gap("1.0");
        let double = gap("2.0");
        assert!(
            double > single * 1.6,
            "line-height:2 gap {double} should far exceed line-height:1 gap {single}"
        );
    }

    #[test]
    fn media_query_applies_at_layout_width() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // A @media rule narrows the box background only below 600px wide.
        let html = "<style>\
            div { background-color: #0000ff }\
            @media (max-width: 600px) { div { background-color: #ff0000 } }\
            </style><div>x</div>";
        let red_bg = |vw: u32| -> bool {
            let doc = parse(html);
            let l = layout(&doc, &font, vw as f32, &ImageSizes::new());
            l.rects
                .iter()
                .any(|r| r.color.r == 255 && r.color.g == 0 && r.color.b == 0 && r.color.a > 0)
        };
        assert!(red_bg(400), "narrow viewport should apply the @media rule");
        assert!(!red_bg(800), "wide viewport should keep the base rule");
    }

    #[test]
    fn visibility_hidden_keeps_space_but_no_ink() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // A hidden paragraph with a background paints no rect and no visible runs,
        // but a visible child still shows and the following block keeps its position.
        let html = "<p style=\"visibility:hidden; background-color:#ff0000\">hidden \
                    <span style=\"visibility:visible\">shown</span></p><p>after</p>";
        let doc = parse(html);
        let l = layout(&doc, &font, 400.0, &ImageSizes::new());
        // No red background rect from the hidden paragraph.
        assert!(
            !l.rects
                .iter()
                .any(|r| r.color.r == 255 && r.color.g == 0 && r.color.a > 0),
            "hidden bg should not paint"
        );
        // The hidden words are transparent; the visible child and "after" are opaque.
        let opaque: Vec<&str> = l
            .runs
            .iter()
            .filter(|r| r.color.a > 0)
            .map(|r| r.text.as_str())
            .collect();
        assert!(opaque.contains(&"shown"), "visible child shows: {opaque:?}");
        assert!(
            opaque.contains(&"after"),
            "following block shows: {opaque:?}"
        );
        assert!(
            !opaque.contains(&"hidden"),
            "hidden word painted: {opaque:?}"
        );
        // "after" still sits below the (space-reserving) hidden paragraph.
        let y_after = l.runs.iter().find(|r| r.text == "after").unwrap().baseline;
        let y_shown = l.runs.iter().find(|r| r.text == "shown").unwrap().baseline;
        assert!(y_after > y_shown, "after should be below the hidden block");
    }

    #[test]
    fn outline_paints_outside_border_box() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // A 50px-wide box at the page margin (8px) with a 3px outline paints
        // outline rects extending to the left of the border box (x < 8).
        let doc = parse("<div style=\"width:50px; outline: 3px solid #ff0000\">x</div>");
        let l = layout(&doc, &font, 200.0, &ImageSizes::new());
        let red: Vec<&RectFill> = l
            .rects
            .iter()
            .filter(|r| r.color.r == 255 && r.color.g == 0 && r.color.b == 0)
            .collect();
        assert_eq!(red.len(), 4, "outline should be 4 rects");
        // The left outline rect sits just outside the page margin.
        assert!(
            red.iter().any(|r| r.x < PAGE_MARGIN),
            "outline extends left of border box"
        );
    }

    #[test]
    fn position_relative_shifts_the_subtree() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let bg = |css: &str| -> (f32, f32) {
            let html = format!("<div style=\"background-color:#ff0000; {css}\">hi</div>");
            let doc = parse(&html);
            let l = layout(&doc, &font, 400.0, &ImageSizes::new());
            let r = l
                .rects
                .iter()
                .find(|r| r.color.r == 255 && r.color.g == 0)
                .expect("bg rect");
            (r.x, r.y)
        };
        let (sx, sy) = bg("");
        let (rx, ry) = bg("position: relative; left: 30px; top: 10px");
        assert!((rx - sx - 30.0).abs() < 0.5, "dx: {} vs {}", rx, sx);
        assert!((ry - sy - 10.0).abs() < 0.5, "dy: {} vs {}", ry, sy);
        // `right` shifts left (negative dx).
        let (rx2, _) = bg("position: relative; right: 20px");
        assert!((rx2 - sx + 20.0).abs() < 0.5, "right dx: {} vs {}", rx2, sx);
    }

    #[test]
    fn height_extends_the_box() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // A 120px-tall box renders a background rect at least that tall.
        let doc = parse("<div style=\"height: 120px; background-color: #ff0000\">x</div>");
        let l = layout(&doc, &font, 400.0, &ImageSizes::new());
        let bg = l
            .rects
            .iter()
            .find(|r| r.color.r == 255 && r.color.g == 0)
            .expect("bg rect");
        assert!(bg.h >= 120.0, "box height {} should be >= 120", bg.h);
    }

    #[test]
    fn max_width_caps_and_min_width_floors() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let bg_w = |css: &str| -> f32 {
            let html = format!("<div style=\"background-color:#ff0000; {css}\">x</div>");
            let doc = parse(&html);
            let l = layout(&doc, &font, 1000.0, &ImageSizes::new());
            l.rects
                .iter()
                .find(|r| r.color.r == 255 && r.color.g == 0)
                .map(|r| r.w)
                .expect("bg rect")
        };
        // max-width caps an otherwise-full-width block.
        assert!((bg_w("max-width: 300px") - 300.0).abs() < 0.5);
        // min-width floors an explicitly narrow block.
        assert!((bg_w("width: 50px; min-width: 200px") - 200.0).abs() < 0.5);
    }

    #[test]
    fn box_sizing_border_box_subtracts_padding_and_border() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // Same specified width (200px), padding 20, border 5; the content-box div's
        // border box is wider (250) than the border-box div's (200).
        let html = "<style>\
            div { width: 200px; padding: 20px; border: 5px solid #000 }\
            .cb { box-sizing: content-box; background-color: #ff0000 }\
            .bb { box-sizing: border-box; background-color: #00ff00 }\
            </style>\
            <div class=\"cb\">content box</div>\
            <div class=\"bb\">border box</div>";
        let doc = parse(html);
        let layout = layout(&doc, &font, 600.0, &ImageSizes::new());

        let bg_w = |r: u8, g: u8| -> f32 {
            layout
                .rects
                .iter()
                .find(|rect| rect.color.r == r && rect.color.g == g && rect.color.b == 0)
                .map(|rect| rect.w)
                .expect("background rect")
        };
        let content_box = bg_w(255, 0);
        let border_box = bg_w(0, 255);
        // content-box: 200 + 2*20 padding + 2*5 border = 250.
        assert!(
            (content_box - 250.0).abs() < 0.5,
            "content-box {content_box}"
        );
        // border-box: the 200 includes padding + border.
        assert!((border_box - 200.0).abs() < 0.5, "border-box {border_box}");
    }

    #[test]
    fn table_caption_renders_above_rows() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let html = "<table><caption>My Caption</caption>\
            <tr><td>cell</td></tr></table>";
        let doc = parse(html);
        let l = layout(&doc, &font, 300.0, &ImageSizes::new());
        let cap_y = l
            .runs
            .iter()
            .find(|r| r.text == "Caption")
            .map(|r| r.baseline);
        let cell_y = l.runs.iter().find(|r| r.text == "cell").map(|r| r.baseline);
        let (cap_y, cell_y) = (cap_y.expect("caption"), cell_y.expect("cell"));
        assert!(
            cap_y < cell_y,
            "caption {cap_y} should sit above cell {cell_y}"
        );
    }

    #[test]
    fn table_colspan_spans_columns() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // 3-column table; second row's first cell spans 2 columns, so the second
        // cell ("y") starts in column 3 — aligned with "c" from the first row.
        let html = "<table>\
            <tr><td>a</td><td>b</td><td>c</td></tr>\
            <tr><td colspan=2>x</td><td>y</td></tr></table>";
        let doc = parse(html);
        let l = layout(&doc, &font, 300.0, &ImageSizes::new());
        let x_of = |t: &str| l.runs.iter().find(|r| r.text == t).map(|r| r.x).unwrap();
        // "y" aligns with the third column ("c"); "x" starts at the first ("a").
        assert!(
            (x_of("y") - x_of("c")).abs() < 1.0,
            "y {} vs c {}",
            x_of("y"),
            x_of("c")
        );
        assert!(
            (x_of("x") - x_of("a")).abs() < 1.0,
            "x {} vs a {}",
            x_of("x"),
            x_of("a")
        );
        assert!(x_of("y") > x_of("b"), "y should be past column 2");
    }

    #[test]
    fn table_lays_cells_in_columns() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let html = "<table><tr><td>a</td><td>b</td><td>c</td></tr>\
                    <tr><td>d</td><td>e</td><td>f</td></tr></table>";
        let doc = parse(html);
        let layout = layout(&doc, &font, 300.0, &ImageSizes::new());

        let cell_runs: Vec<_> = layout
            .runs
            .iter()
            .filter(|r| ["a", "b", "c", "d", "e", "f"].contains(&r.text.as_str()))
            .collect();
        assert_eq!(cell_runs.len(), 6, "expected 6 cell texts");
        // Three distinct column x-positions.
        let xs: std::collections::BTreeSet<i32> = cell_runs.iter().map(|r| r.x as i32).collect();
        assert_eq!(xs.len(), 3, "expected 3 columns, got {xs:?}");
    }

    #[test]
    fn layout_survives_arbitrary_input() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let mut seed = 0xD1B54A32D192ED03u64;
        let mut byte = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed & 0xff) as u8
        };
        // Bias toward markup plus CSS property/value bytes so styled, nested boxes
        // (flex/grid/tables/lists, floats, positioning, fr tracks) are exercised,
        // not just text.
        const BIAS: &[u8] = b"<>/=\"' ;:{}().%#-\nstyledivpaulitbflexgridcolorwidthpaddingmargin\
borderdisplay0123floatleftrightclearbothfrgrowshrinkwrapspanabsolutefixedrelativtopbottomgaprepeat";
        for _ in 0..2000 {
            let len = (byte() as usize) * 4;
            let bytes: Vec<u8> = (0..len)
                .map(|_| {
                    if byte() < 150 {
                        BIAS[byte() as usize % BIAS.len()]
                    } else {
                        byte()
                    }
                })
                .collect();
            let s = String::from_utf8_lossy(&bytes);
            let doc = parse(&s);
            // The full pipeline (parse → cascade → layout) must never panic, and
            // every emitted geometry must be finite (no NaN/inf from fr/flex math).
            let l = layout(&doc, &font, 400.0, &ImageSizes::new());
            assert!(l.height.is_finite());
            for r in &l.rects {
                assert!(r.w.is_finite() && r.h.is_finite() && r.x.is_finite() && r.y.is_finite());
            }
            for run in &l.runs {
                assert!(run.x.is_finite() && run.baseline.is_finite());
            }
            for im in &l.images {
                assert!(im.x.is_finite() && im.y.is_finite() && im.w.is_finite() && im.h.is_finite());
            }
        }
    }

    #[test]
    fn gap_spaces_flex_items() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // Two flex items: with a gap, the second item starts further right than
        // it would with no gap (item width shrinks and a gap is inserted).
        let second_x = |gap: &str| -> f32 {
            let html = format!(
                "<div style=\"display:flex; gap:{gap}\">\
                 <div>aaa</div><div id=second>bbb</div></div>"
            );
            let doc = parse(&html);
            let l = layout(&doc, &font, 400.0, &ImageSizes::new());
            l.runs
                .iter()
                .find(|r| r.text == "bbb")
                .map(|r| r.x)
                .expect("second item run")
        };
        assert!(
            second_x("40px") > second_x("0px") + 10.0,
            "gap should push the second item rightward"
        );
    }

    #[test]
    fn flex_row_places_items_side_by_side() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let html = "<div style=\"display:flex\"><div>one</div><div>two</div></div>";
        let doc = parse(html);
        let layout = layout(&doc, &font, 400.0, &ImageSizes::new());
        let one = layout.runs.iter().find(|r| r.text == "one").unwrap();
        let two = layout.runs.iter().find(|r| r.text == "two").unwrap();
        // Items sit on the same line (≈ same baseline), in two columns.
        assert!(
            (one.baseline - two.baseline).abs() < 1.0,
            "items not on one row"
        );
        assert!(
            two.x > one.x + 100.0,
            "second item should be in the next column"
        );
    }

    #[test]
    fn justify_content_center_offsets_fixed_items() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // Two 40px-wide items in a 400px row. Default (flex-start) puts the first at
        // the left edge; `center` shifts the whole group right by half the free space.
        let first_x = |jc: &str| -> f32 {
            let html = format!(
                "<div style=\"display:flex; width:400px; justify-content:{jc}\">\
                   <div style=\"width:40px\">aaa</div>\
                   <div style=\"width:40px\">bbb</div>\
                 </div>"
            );
            let doc = parse(&html);
            let l = layout(&doc, &font, 800.0, &ImageSizes::new());
            l.runs.iter().find(|r| r.text == "aaa").unwrap().x
        };
        let start = first_x("flex-start");
        let center = first_x("center");
        let end = first_x("flex-end");
        assert!(center > start + 100.0, "center should shift right: {start} -> {center}");
        assert!(end > center + 100.0, "flex-end further right: {center} -> {end}");
    }

    #[test]
    fn justify_space_between_pushes_items_to_edges() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // space-between: first item at the left edge, last item flush to the right.
        let html = "<div style=\"display:flex; width:400px; justify-content:space-between\">\
                      <div style=\"width:40px\">aaa</div>\
                      <div style=\"width:40px\">bbb</div>\
                    </div>";
        let doc = parse(html);
        let l = layout(&doc, &font, 800.0, &ImageSizes::new());
        let a = l.runs.iter().find(|r| r.text == "aaa").unwrap();
        let b = l.runs.iter().find(|r| r.text == "bbb").unwrap();
        // First sits near the left (page margin ~8); the second is pushed far right.
        assert!(a.x < 20.0, "first item near left, got {}", a.x);
        assert!(b.x > 300.0, "second item near right edge, got {}", b.x);
    }

    #[test]
    fn align_items_center_centers_items_vertically() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // A tall item (forced height) sets the line height; a short item is centered
        // vertically against it. Compared with the default (flex-start) it sits lower.
        let baseline = |ai: &str| -> f32 {
            let html = format!(
                "<div style=\"display:flex; align-items:{ai}\">\
                   <div style=\"height:100px; width:40px\">tall</div>\
                   <div style=\"width:40px\">x</div>\
                 </div>"
            );
            let doc = parse(&html);
            let l = layout(&doc, &font, 400.0, &ImageSizes::new());
            l.runs.iter().find(|r| r.text == "x").unwrap().baseline
        };
        let start = baseline("flex-start");
        let center = baseline("center");
        let end = baseline("flex-end");
        assert!(center > start + 20.0, "center lower than start: {start} -> {center}");
        assert!(end > center + 20.0, "end lower than center: {center} -> {end}");
    }

    #[test]
    fn column_align_items_center_centers_horizontally() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // A fixed-width item in a column. Default (stretch/flex-start) keeps it at the
        // left; `center` and `flex-end` push it rightward within the content box.
        let item_x = |ai: &str| -> f32 {
            let html = format!(
                "<div style=\"display:flex; flex-direction:column; width:400px; align-items:{ai}\">\
                   <div style=\"width:40px\">it</div>\
                 </div>"
            );
            let doc = parse(&html);
            let l = layout(&doc, &font, 800.0, &ImageSizes::new());
            l.runs.iter().find(|r| r.text == "it").unwrap().x
        };
        let start = item_x("flex-start");
        let center = item_x("center");
        let end = item_x("flex-end");
        assert!(center > start + 100.0, "center shifts right: {start} -> {center}");
        assert!(end > center + 100.0, "flex-end further right: {center} -> {end}");
    }

    #[test]
    fn column_justify_content_distributes_vertical_space() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // A 300px-tall column holding two short items. flex-start keeps the first at
        // the top; center/flex-end push it down into the free vertical space.
        let first_baseline = |jc: &str| -> f32 {
            let html = format!(
                "<div style=\"display:flex; flex-direction:column; height:300px; justify-content:{jc}\">\
                   <div>aaa</div>\
                   <div>bbb</div>\
                 </div>"
            );
            let doc = parse(&html);
            let l = layout(&doc, &font, 400.0, &ImageSizes::new());
            l.runs.iter().find(|r| r.text == "aaa").unwrap().baseline
        };
        let start = first_baseline("flex-start");
        let center = first_baseline("center");
        let end = first_baseline("flex-end");
        assert!(center > start + 80.0, "center pushes down: {start} -> {center}");
        assert!(end > center + 80.0, "flex-end further down: {center} -> {end}");
    }

    #[test]
    fn broken_image_renders_alt_text() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // No intrinsic size is provided for the src (unresolved image), so the alt
        // text is rendered in its place.
        let html = "<img src=\"missing.png\" alt=\"a red apple\">";
        let doc = parse(html);
        let l = layout(&doc, &font, 400.0, &ImageSizes::new());
        let texts: Vec<&str> = l.runs.iter().map(|r| r.text.as_str()).collect();
        assert!(texts.contains(&"a red apple"), "alt text rendered, got {texts:?}");
        assert!(l.images.is_empty(), "no image box for an unresolved image");
    }

    #[test]
    fn flex_order_reorders_items() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // Source order a,b,c but a has order:2, so visually it moves to the end:
        // b, c, a left-to-right.
        let html = "<div style=\"display:flex; width:300px\">\
                      <div style=\"order:2\">a</div>\
                      <div>b</div>\
                      <div>c</div>\
                    </div>";
        let doc = parse(html);
        let l = layout(&doc, &font, 400.0, &ImageSizes::new());
        let x = |t: &str| l.runs.iter().find(|r| r.text == t).unwrap().x;
        let (ax, bx, cx) = (x("a"), x("b"), x("c"));
        // b and c come before a horizontally.
        assert!(bx < ax && cx < ax, "a (order:2) moves last: a={ax} b={bx} c={cx}");
        assert!(bx < cx, "b before c (equal order keeps source order)");
    }

    #[test]
    fn flex_grow_distributes_free_space_by_weight() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // Two short items in a 400px row; the second has flex-grow:2 so it should get
        // a larger share of the free space and therefore start further left (it grows
        // rightward) while the first stays narrow. We measure the gap between their
        // start positions: with grow only on the second, the second item begins right
        // after the first's (small) content width.
        let html = "<div style=\"display:flex; width:400px\">\
                      <div style=\"flex-grow:1\">aa</div>\
                      <div style=\"flex-grow:2\">bb</div>\
                    </div>";
        let doc = parse(html);
        let l = layout(&doc, &font, 800.0, &ImageSizes::new());
        let a = l.runs.iter().find(|r| r.text == "aa").unwrap();
        let b = l.runs.iter().find(|r| r.text == "bb").unwrap();
        // First item gets 1/3 of free space, second gets 2/3. The second item's start
        // (a.base + 1/3 free) is well past the first's start, and its left edge should
        // be more than a third of the way across the 400px container.
        assert!(b.x > a.x + 100.0, "grow:2 item pushed right of grow:1 item: a={} b={}", a.x, b.x);
        assert!(b.x > 130.0, "second item starts past the first third, got {}", b.x);
    }

    #[test]
    fn flex_grow_zero_keeps_items_at_content_width() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // With flex-grow on one item only, the non-growing item stays at its content
        // width; the growing item absorbs all the free space. The first (grow:0,
        // explicit-width) item keeps its 50px slot, so the grower starts at ~50px.
        let html = "<div style=\"display:flex; width:400px\">\
                      <div style=\"width:50px\">a</div>\
                      <div style=\"flex-grow:1\">grow</div>\
                    </div>";
        let doc = parse(html);
        let l = layout(&doc, &font, 800.0, &ImageSizes::new());
        let grow = l.runs.iter().find(|r| r.text == "grow").unwrap();
        // The grower starts just after the fixed 50px slot (plus page margin ~8).
        assert!(grow.x > 50.0 && grow.x < 80.0, "grower starts after fixed slot, got {}", grow.x);
    }

    #[test]
    fn absolute_anchors_to_positioned_ancestor_corner() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // A relatively-positioned 300×200 box establishes the containing block; an
        // absolute child with right:0/top:0 pins to the box's top-right corner, so
        // its left edge ≈ box_left + 300 - child_width (not the page origin).
        let html = "<div style=\"position:relative; width:300px; height:200px; margin-left:50px\">\
                      <div id=\"pin\" style=\"position:absolute; top:0; right:0; width:40px\">x</div>\
                    </div>";
        let doc = parse(html);
        let l = layout(&doc, &font, 600.0, &ImageSizes::new());
        let pin = l.bounds.iter().find(|b| b.id == "pin").expect("pin bounds");
        // Container border-box left = page-margin(8) + margin-left(50) = 58; its
        // padding box right edge = 58 + 300 = 358; child (40px) left ≈ 318.
        assert!((pin.x - 318.0).abs() < 4.0, "pinned to right edge, got {}", pin.x);
        // top:0 → child top aligns with the container's content top (~8).
        assert!((pin.y - 8.0).abs() < 4.0, "pinned to top edge, got {}", pin.y);
    }

    #[test]
    fn absolute_bottom_anchors_to_container_height() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // bottom:0 against a 200px-tall positioned container puts the child's bottom
        // at the container bottom: child top ≈ container_top(8) + 200 - child_height.
        let html = "<div style=\"position:relative; height:200px\">\
                      <div id=\"b\" style=\"position:absolute; bottom:0; height:30px; width:20px\">y</div>\
                    </div>";
        let doc = parse(html);
        let l = layout(&doc, &font, 600.0, &ImageSizes::new());
        let b = l.bounds.iter().find(|b| b.id == "b").expect("b bounds");
        // container content top ≈ 8; bottom edge ≈ 208; child top ≈ 208 - 30 = 178.
        assert!((b.y - 178.0).abs() < 5.0, "pinned to bottom, got {}", b.y);
    }

    #[test]
    fn flex_wrap_applies_justify_content_per_line() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // A single 120px item on its own line in a 300px wrapping container. With
        // justify-content:center the item is centered (free space ~180 → lead ~90),
        // so its left edge sits well right of the content origin.
        let html = "<div style=\"display:flex; flex-wrap:wrap; width:300px; justify-content:center\">\
                      <div style=\"width:120px\">solo</div>\
                    </div>";
        let doc = parse(html);
        let l = layout(&doc, &font, 600.0, &ImageSizes::new());
        let solo = l.runs.iter().find(|r| r.text == "solo").unwrap();
        // Centered: ~ page-margin(8) + lead(90) ≈ 98, far from the left edge.
        assert!(solo.x > 70.0, "wrapped line item centered, got {}", solo.x);
    }

    #[test]
    fn flex_shrink_compresses_overflowing_items_to_fit() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // Two 250px items in a 300px no-wrap row overflow by 200px. With the default
        // flex-shrink:1 they shrink proportionally to fit, so the second item's left
        // edge lands well inside the container rather than at ~250px.
        let html = "<div style=\"display:flex; width:300px\">\
                      <div style=\"width:250px\">aa</div>\
                      <div style=\"width:250px\">bb</div>\
                    </div>";
        let doc = parse(html);
        let l = layout(&doc, &font, 600.0, &ImageSizes::new());
        let b = l.runs.iter().find(|r| r.text == "bb").unwrap();
        // Each shrinks from 250 to ~150, so the second starts near 150 (+page margin),
        // not at its unshrunk 250px position.
        assert!(b.x > 120.0 && b.x < 200.0, "second item shrunk to fit, got {}", b.x);
    }

    #[test]
    fn flex_shrink_zero_prevents_shrinking() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // flex-shrink:0 on both items keeps them at full width even though they
        // overflow: the second still starts at ~250px (its unshrunk position).
        let html = "<div style=\"display:flex; width:300px\">\
                      <div style=\"width:250px; flex-shrink:0\">aa</div>\
                      <div style=\"width:250px; flex-shrink:0\">bb</div>\
                    </div>";
        let doc = parse(html);
        let l = layout(&doc, &font, 600.0, &ImageSizes::new());
        let b = l.runs.iter().find(|r| r.text == "bb").unwrap();
        assert!(b.x > 240.0, "no-shrink item keeps full width, got {}", b.x);
    }

    #[test]
    fn flex_wrap_breaks_items_onto_multiple_lines() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // Three 120px items in a 300px container with flex-wrap: only two fit per
        // line, so the third drops to a second line (lower baseline, back at the
        // left). Without wrap they'd all share one row.
        let html = "<div style=\"display:flex; flex-wrap:wrap; width:300px\">\
                      <div style=\"width:120px\">one</div>\
                      <div style=\"width:120px\">two</div>\
                      <div style=\"width:120px\">three</div>\
                    </div>";
        let doc = parse(html);
        let l = layout(&doc, &font, 600.0, &ImageSizes::new());
        let one = l.runs.iter().find(|r| r.text == "one").unwrap();
        let two = l.runs.iter().find(|r| r.text == "two").unwrap();
        let three = l.runs.iter().find(|r| r.text == "three").unwrap();
        // First two on the same line; third wraps below and starts at the left.
        assert!((one.baseline - two.baseline).abs() < 1.0, "one/two share a line");
        assert!(three.baseline > one.baseline + 10.0, "three wrapped to a new line");
        assert!((three.x - one.x).abs() < 1.0, "wrapped item back at the left edge");
    }

    #[test]
    fn generated_content_before_and_after() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let html = "<style>.x::before{content:\"PRE\"} .x::after{content:\"POST\"}</style>\
                    <p class=\"x\">mid</p>";
        let doc = parse(html);
        let lay = layout(&doc, &font, 400.0, &ImageSizes::new());
        let texts: Vec<&str> = lay.runs.iter().map(|r| r.text.as_str()).collect();
        // PRE comes before "mid" which comes before POST, all on the line.
        let pre = texts.iter().position(|t| *t == "PRE");
        let mid = texts.iter().position(|t| *t == "mid");
        let post = texts.iter().position(|t| *t == "POST");
        assert!(pre.is_some() && mid.is_some() && post.is_some(), "runs: {texts:?}");
        assert!(pre < mid && mid < post, "order PRE<mid<POST: {texts:?}");
    }

    #[test]
    fn transform_scale_grows_box_about_its_center() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // A 100px box with a background, scaled 2x about its center. The background
        // rect should double in size and keep the same center.
        let html = "<div style=\"width:100px; height:40px; background:#f00; transform: scale(2)\"></div>";
        let doc = parse(html);
        let lay = layout(&doc, &font, 400.0, &ImageSizes::new());
        let bg = lay
            .rects
            .iter()
            .find(|r| r.color.a > 0 && r.w > 150.0)
            .expect("scaled background rect");
        // Border box was ~100x40 → scaled to ~200x80.
        assert!((bg.w - 200.0).abs() < 2.0, "width doubled, got {}", bg.w);
        assert!((bg.h - 80.0).abs() < 2.0, "height doubled, got {}", bg.h);
    }

    #[test]
    fn transform_translate_shifts_subtree_without_affecting_flow() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // The first block is translated by (40, 30); its text moves but the second
        // block (in normal flow) is unaffected — it sits where it would without the
        // transform.
        let html = "<div style=\"transform: translate(40px, 30px)\">A</div><div>B</div>";
        let doc = parse(html);
        let lay = layout(&doc, &font, 400.0, &ImageSizes::new());
        let a = lay.runs.iter().find(|r| r.text == "A").unwrap();
        let b = lay.runs.iter().find(|r| r.text == "B").unwrap();
        // A shifted right ~40px from the page margin (~8 → ~48).
        assert!(a.x > 40.0, "A translated right, got {}", a.x);
        // B sits near the left and just below A's original line (transform didn't
        // push it down by 30px).
        assert!(b.x < 20.0, "B unaffected horizontally, got {}", b.x);
        assert!(b.baseline < a.baseline + 5.0, "B not pushed down by A's transform");
    }

    #[test]
    fn absolute_positioning_removes_from_flow_and_offsets() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // The absolute box is out of flow (so "B" starts near the top, not below it)
        // and shifted down by top:100.
        let html = "<div style=\"position:absolute; top:100px\">A</div><div>B</div>";
        let doc = parse(html);
        let lay = layout(&doc, &font, 400.0, &ImageSizes::new());
        let a = lay.runs.iter().find(|r| r.text == "A").unwrap();
        let b = lay.runs.iter().find(|r| r.text == "B").unwrap();
        // B is not pushed down by A (A is out of flow): B sits near the top.
        assert!(b.baseline < 40.0, "B should ignore the absolute box, got {}", b.baseline);
        // A is shifted down by ~100px from its static position.
        assert!(
            a.baseline > b.baseline + 90.0,
            "A should be offset ~100px below B, got A={} B={}",
            a.baseline,
            b.baseline
        );
    }

    #[test]
    fn aspect_ratio_derives_height_from_width() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // A 200px-wide block with aspect-ratio 2/1 is ~100px tall, so the next
        // block starts around y=100.
        let html = "<div style=\"width:200px; aspect-ratio:2/1\"></div><div>b</div>";
        let doc = parse(html);
        let lay = layout(&doc, &font, 400.0, &ImageSizes::new());
        let b = lay.runs.iter().find(|r| r.text == "b").unwrap();
        // The 100px aspect height (200/2) pushes b down well past where it would sit
        // with a zero-height first block (~20: body margin + text ascent).
        assert!(
            b.baseline > 100.0 && b.baseline < 140.0,
            "second block should start ~100px down (200/2 + offsets), got {}",
            b.baseline
        );
    }

    #[test]
    fn min_height_extends_a_short_block() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // A one-line block with min-height:200px occupies at least 200px of height,
        // so a following block starts below it.
        let html = "<div style=\"min-height:200px\">a</div><div id=\"next\">b</div>";
        let doc = parse(html);
        let lay = layout(&doc, &font, 400.0, &ImageSizes::new());
        let b = lay.runs.iter().find(|r| r.text == "b").unwrap();
        assert!(
            b.baseline > 200.0,
            "second block should start below the 200px min-height, got {}",
            b.baseline
        );
    }

    #[test]
    fn max_height_caps_an_explicit_height() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // height:300px capped by max-height:120px → the box is 120px tall, so the
        // following block starts around 120px (not 300px).
        let html = "<div style=\"height:300px; max-height:120px\">a</div><div id=\"next\">b</div>";
        let doc = parse(html);
        let lay = layout(&doc, &font, 400.0, &ImageSizes::new());
        let b = lay.runs.iter().find(|r| r.text == "b").unwrap();
        assert!(
            b.baseline > 120.0 && b.baseline < 180.0,
            "max-height should cap the 300px height to ~120px, got {}",
            b.baseline
        );
    }

    #[test]
    fn margin_auto_centers_a_fixed_width_block() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // A 100px-wide block with `margin: 0 auto` centers in a 400px viewport:
        // its left edge ≈ (400 - 100) / 2 = 150.
        let html = "<div style=\"width:100px; margin:0 auto\">hi</div>";
        let doc = parse(html);
        let lay = layout(&doc, &font, 400.0, &ImageSizes::new());
        let hi = lay.runs.iter().find(|r| r.text == "hi").unwrap();
        assert!(
            (hi.x - 150.0).abs() < 2.0,
            "centered text should start near x=150, got {}",
            hi.x
        );
        // Without auto margins, the same block sits at the left (just the UA body
        // margin, ~8px) — clearly not centered.
        let doc2 = parse("<div style=\"width:100px\">hi</div>");
        let lay2 = layout(&doc2, &font, 400.0, &ImageSizes::new());
        let hi2 = lay2.runs.iter().find(|r| r.text == "hi").unwrap();
        assert!(hi2.x < 20.0, "left-aligned block near the left, got {}", hi2.x);
    }

    #[test]
    fn flex_column_stacks_items_vertically() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let html = "<div style=\"display:flex; flex-direction:column\">\
                    <div>one</div><div>two</div></div>";
        let doc = parse(html);
        let layout = layout(&doc, &font, 400.0, &ImageSizes::new());
        let one = layout.runs.iter().find(|r| r.text == "one").unwrap();
        let two = layout.runs.iter().find(|r| r.text == "two").unwrap();
        // Column: the second item is stacked below the first, in the same column.
        assert!(
            two.baseline > one.baseline + 10.0,
            "second item should be on a lower line (got {} vs {})",
            two.baseline,
            one.baseline
        );
        assert!(
            (one.x - two.x).abs() < 1.0,
            "items should share the same x in a column"
        );
    }

    #[test]
    fn grid_flows_items_row_major() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let html = "<div style=\"display:grid; grid-template-columns: repeat(2, 1fr)\">\
                    <div>a</div><div>b</div><div>c</div><div>d</div></div>";
        let doc = parse(html);
        let layout = layout(&doc, &font, 400.0, &ImageSizes::new());
        let at = |t: &str| {
            let r = layout.runs.iter().find(|r| r.text == t).unwrap();
            (r.x, r.baseline)
        };
        let (ax, ay) = at("a");
        let (bx, by) = at("b");
        let (cx, cy) = at("c");
        // a,b on row 1 in two columns; c starts row 2 in column 1 (under a).
        assert!(
            (ay - by).abs() < 1.0 && bx > ax + 100.0,
            "row 1 not two columns"
        );
        assert!(
            cy > ay + 10.0 && (cx - ax).abs() < 1.0,
            "c not under a on row 2"
        );
    }

    #[test]
    fn grid_fixed_and_fr_tracks_size_columns() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // `100px 1fr 2fr` in a 400px content box (no gap): col0=100, remaining 300
        // split 1:2 → col1=100, col2=200. Column left edges: 0, 100, 200 (+page
        // margin 8). Items "a","b","c" land in those columns.
        let html = "<div style=\"display:grid; width:400px; grid-template-columns: 100px 1fr 2fr\">\
                    <div>a</div><div>b</div><div>c</div></div>";
        let doc = parse(html);
        let l = layout(&doc, &font, 800.0, &ImageSizes::new());
        let x = |t: &str| l.runs.iter().find(|r| r.text == t).unwrap().x;
        let (ax, bx, cx) = (x("a"), x("b"), x("c"));
        // a at content origin ~8; b after the 100px fixed column ~108; c after
        // the 100px fr column ~208.
        assert!((ax - 8.0).abs() < 3.0, "col0 at origin, got {ax}");
        assert!((bx - 108.0).abs() < 4.0, "col1 after 100px fixed, got {bx}");
        assert!((cx - 208.0).abs() < 4.0, "col2 after 1fr(=100), got {cx}");
    }

    #[test]
    fn float_left_wraps_following_text_to_its_right() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // A 100px-wide left float; the paragraph text after it should start to the
        // right of the float (x ≥ float width), not at the page margin.
        let html = "<div style=\"width:400px\">\
                      <div style=\"float:left; width:100px; height:50px\">F</div>\
                      <span>wrapping text beside the float here</span>\
                    </div>";
        let doc = parse(html);
        let l = layout(&doc, &font, 600.0, &ImageSizes::new());
        let word = l.runs.iter().find(|r| r.text == "wrapping").unwrap();
        // Page margin ~8 + float width 100 → text starts near x≈108.
        assert!(word.x > 100.0, "text should flow right of the float, got {}", word.x);
    }

    #[test]
    fn two_left_floats_stack_side_by_side_then_below() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // Container content width = 300px. Two 120px left floats fit side by side
        // (0-120, 120-240); a third 120px float can't fit (240+120>300) and drops
        // below the first row of floats.
        let html = "<div style=\"width:300px\">\
                      <div style=\"float:left; width:120px; height:40px\">a</div>\
                      <div style=\"float:left; width:120px; height:40px\">b</div>\
                      <div style=\"float:left; width:120px; height:40px\">c</div>\
                    </div>";
        let doc = parse(html);
        let l = layout(&doc, &font, 600.0, &ImageSizes::new());
        let at = |t: &str| {
            let r = l.runs.iter().find(|r| r.text == t).unwrap();
            (r.x, r.baseline)
        };
        let (ax, ay) = at("a");
        let (bx, by) = at("b");
        let (cx, cy) = at("c");
        // a and b on the same float row, b to the right of a.
        assert!((ay - by).abs() < 1.0, "a and b on the same row");
        assert!(bx > ax + 100.0, "b sits right of a, got a={ax} b={bx}");
        // c drops below (its baseline is lower) and returns to the left edge.
        assert!(cy > ay + 30.0, "c dropped below the float row, got {cy} vs {ay}");
        assert!((cx - ax).abs() < 2.0, "c back at the left edge, got {cx}");
    }

    #[test]
    fn float_right_keeps_text_to_its_left() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // A right float: text stays on the left (small x), not pushed right.
        let html = "<div style=\"width:400px\">\
                      <div style=\"float:right; width:100px; height:50px\">F</div>\
                      <span>left side text</span>\
                    </div>";
        let doc = parse(html);
        let l = layout(&doc, &font, 600.0, &ImageSizes::new());
        let word = l.runs.iter().find(|r| r.text == "left").unwrap();
        assert!(word.x < 30.0, "text should stay left of the right float, got {}", word.x);
    }

    #[test]
    fn clear_drops_below_the_float() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // A `clear:left` block must sit below a tall left float, not beside it.
        let html = "<div style=\"width:400px\">\
                      <div style=\"float:left; width:100px; height:80px\">F</div>\
                      <div style=\"clear:left\">below</div>\
                    </div>";
        let doc = parse(html);
        let l = layout(&doc, &font, 600.0, &ImageSizes::new());
        let word = l.runs.iter().find(|r| r.text == "below").unwrap();
        // The float starts at ~8 and is 80px tall → cleared block baseline > 80.
        assert!(word.baseline > 85.0, "cleared block should sit below float, got {}", word.baseline);
        // And it returns to the left edge (not indented by the float).
        assert!(word.x < 30.0, "cleared block back at left edge, got {}", word.x);
    }

    #[test]
    fn separate_row_and_column_gaps() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // `gap: 80px 4px` → 80px between rows, 4px between columns. Two columns, four
        // items → 2 rows. Row-2 item sits ~80px+ below row 1; columns are only 4px
        // apart so column 2's x is close behind column 1 + its content.
        let html = "<div style=\"display:grid; width:200px; grid-template-columns: repeat(2,1fr); gap: 80px 4px\">\
                      <div>a</div><div>b</div><div>c</div><div>d</div>\
                    </div>";
        let doc = parse(html);
        let l = layout(&doc, &font, 400.0, &ImageSizes::new());
        let at = |t: &str| {
            let r = l.runs.iter().find(|r| r.text == t).unwrap();
            (r.x, r.baseline)
        };
        let (_, ay) = at("a");
        let (_, cy) = at("c");
        // c is the start of row 2: at least the 80px row gap below row 1.
        assert!(cy > ay + 80.0, "row gap should push row 2 down ~80px, got {}", cy - ay);
    }

    #[test]
    fn grid_column_span_occupies_multiple_columns() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // Three equal columns. The first item spans 2, so it occupies columns 0-1;
        // the next item ("b") lands in column 2 of the same row, and "c" wraps to
        // row 2 column 0 (under "a").
        let html = "<div style=\"display:grid; width:300px; grid-template-columns: repeat(3, 1fr)\">\
                      <div style=\"grid-column: span 2\">a</div>\
                      <div>b</div>\
                      <div>c</div>\
                    </div>";
        let doc = parse(html);
        let l = layout(&doc, &font, 600.0, &ImageSizes::new());
        let at = |t: &str| {
            let r = l.runs.iter().find(|r| r.text == t).unwrap();
            (r.x, r.baseline)
        };
        let (ax, ay) = at("a");
        let (bx, by) = at("b");
        let (cx, cy) = at("c");
        // "b" is in column 2 (≈ origin + 2×100 = 208) on the same row as "a".
        assert!((ay - by).abs() < 1.0, "a and b share a row");
        assert!((bx - 208.0).abs() < 6.0, "b in the third column, got {bx}");
        // "c" wraps to the next row, back under "a".
        assert!(cy > ay + 10.0 && (cx - ax).abs() < 2.0, "c under a on row 2");
    }

    #[test]
    fn grid_row_span_reserves_cells_below() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // Two columns. The first item spans 2 rows in column 0; the next items flow
        // into column 1 (row 0), then column 1 (row 1) — column 0 stays reserved by
        // the spanning item. So: a@(r0,c0 spanning), b@(r0,c1), c@(r1,c1).
        let html = "<div style=\"display:grid; width:200px; grid-template-columns: repeat(2,1fr)\">\
                      <div style=\"grid-row: span 2; height:80px\">a</div>\
                      <div style=\"height:30px\">b</div>\
                      <div style=\"height:30px\">c</div>\
                    </div>";
        let doc = parse(html);
        let l = layout(&doc, &font, 400.0, &ImageSizes::new());
        let at = |t: &str| {
            let r = l.runs.iter().find(|r| r.text == t).unwrap();
            (r.x, r.baseline)
        };
        let (ax, ay) = at("a");
        let (bx, by) = at("b");
        let (cx, cy) = at("c");
        // a in column 0; b and c in column 1 (to the right of a).
        assert!(bx > ax + 50.0 && cx > ax + 50.0, "b,c in second column");
        // a and b share the top row; c is on the row below (reserved cell under b).
        assert!((ay - by).abs() < 2.0, "a and b on the first row");
        assert!(cy > by + 10.0, "c on the second row below b, got b={by} c={cy}");
        // c stays under b in column 1 (the spanning a keeps column 0 occupied).
        assert!((cx - bx).abs() < 2.0, "c aligns under b in column 1");
    }

    #[test]
    fn grid_fr_columns_widen_with_container() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        // Two `1fr` columns: the second column's left edge tracks half the content
        // width, so a wider container pushes "b" further right.
        let second_x = |w: u32| -> f32 {
            let html = format!(
                "<div style=\"display:grid; width:{w}px; grid-template-columns: 1fr 1fr\">\
                 <div>a</div><div>b</div></div>"
            );
            let doc = parse(&html);
            let l = layout(&doc, &font, 1000.0, &ImageSizes::new());
            l.runs.iter().find(|r| r.text == "b").unwrap().x
        };
        let narrow = second_x(200);
        let wide = second_x(600);
        assert!(wide > narrow + 150.0, "fr columns widen with container: {narrow} -> {wide}");
    }
}
