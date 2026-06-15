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
    author_stylesheet, computed_style, AuthorStylesheet, BoxSizing, ComputedStyle, Display,
    FlexDirection, Length, ListStyle, Position, TextAlign, TextTransform, VerticalAlign,
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
            for line in raw.trim_end_matches('\n').split('\n') {
                let baseline = self.cursor_y + self.font.ascent_px(style.font_size);
                self.runs.push(TextRun {
                    x: content_left,
                    baseline,
                    text: line.to_string(),
                    size_px: style.font_size,
                    color: if style.hidden {
                        argus_geometry::Color::TRANSPARENT
                    } else {
                        style.fade(style.color)
                    },
                });
                self.cursor_y += style.font_size * style.line_height;
            }
        } else {
            // Children. Inline-level content accumulates into `words` (each with its own
            // style); block-level children flush the line box and lay out separately.
            let mut words: Vec<InlineWord> = Vec::new();
            let mut pending_space = false;
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
                match &self.doc.node(child).data {
                    NodeData::Text(_) => {
                        self.gather_inline(child, &style, None, &mut words, &mut pending_space);
                    }
                    NodeData::Element(e) if e.name.is_html("img") => {
                        self.flush_words(&mut words, &style, content_left, content_w);
                        pending_space = false;
                        self.place_image(e, content_left, content_w, style.hidden);
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
            self.flush_words(&mut words, &style, content_left, content_w);
        } // end !white_space_pre

        // Honor a specified `height` / `min-height`: extend the content box down to
        // it (we don't clip overflow, so taller content still grows the box). Both
        // only extend, so the larger target wins.
        let content_top = border_box_top + style.border.top + style.padding.top;
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
                style.border_color,
            );
            self.rects[i + 1] = rect(
                border_box_left,
                border_box_top + border_box_h - b.bottom,
                border_box_w,
                b.bottom,
                style.border_color,
            );
            self.rects[i + 2] = rect(
                border_box_left,
                border_box_top,
                b.left,
                border_box_h,
                style.border_color,
            );
            self.rects[i + 3] = rect(
                border_box_left + border_box_w - b.right,
                border_box_top,
                b.right,
                border_box_h,
                style.border_color,
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

        // `position: relative` paints the box (and its subtree) shifted by its
        // inset, without affecting the normal flow of following siblings.
        if style.position == Position::Relative {
            let (dx, dy) = relative_offset(&style, avail);
            if dx != 0.0 || dy != 0.0 {
                self.shift_display_list(ds_start, dx, dy);
            }
        }
    }

    /// Shift every display-list item appended since `start` by `(dx, dy)`.
    fn shift_display_list(&mut self, start: (usize, usize, usize, usize, usize), dx: f32, dy: f32) {
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

    /// Place an `<img>` as a block-level replaced box on its own line.
    fn place_image(&mut self, e: &ElementData, x: f32, avail: f32, hidden: bool) {
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
        }
    }

    fn is_li(&self, id: NodeId) -> bool {
        matches!(&self.doc.node(id).data, NodeData::Element(e) if e.name.is_html("li"))
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
    /// apart. A basic subset — no wrapping, `flex-grow`, or `justify`/`align`.
    fn layout_flex(&mut self, id: NodeId, style: ComputedStyle, x: f32, avail: f32) {
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
            // between them. The container height is the sum of item heights + gaps.
            for (i, &item) in items.iter().enumerate() {
                if i > 0 {
                    self.cursor_y += style.gap;
                }
                let istyle = computed_style(self.doc, item, &style, self.author);
                self.layout_block(item, istyle, content_left, content_w, None);
            }
            self.cursor_y += style.padding.bottom + style.border.bottom;
        } else {
            // Row: items share the main axis, each an equal slice of the free width.
            let total_gap = style.gap * (n - 1.0);
            let item_w = ((content_w - total_gap) / n).max(0.0);
            let mut max_h = 0.0f32;
            for (i, &item) in items.iter().enumerate() {
                self.cursor_y = row_top;
                let istyle = computed_style(self.doc, item, &style, self.author);
                let item_x = content_left + i as f32 * (item_w + style.gap);
                self.layout_block(item, istyle, item_x, item_w, None);
                max_h = max_h.max(self.cursor_y - row_top);
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
        let col_w =
            ((content_w - style.gap * (cols.saturating_sub(1)) as f32) / cols as f32).max(0.0);
        let mut idx = 0;
        let mut first_row = true;
        while idx < items.len() {
            if !first_row {
                self.cursor_y += style.gap; // row gap
            }
            first_row = false;
            let row_top = self.cursor_y;
            let mut max_h = 0.0f32;
            for c in 0..cols {
                if idx >= items.len() {
                    break;
                }
                let item = items[idx];
                idx += 1;
                self.cursor_y = row_top;
                let istyle = computed_style(self.doc, item, &style, self.author);
                let item_x = content_left + c as f32 * (col_w + style.gap);
                self.layout_block(item, istyle, item_x, col_w, None);
                max_h = max_h.max(self.cursor_y - row_top);
            }
            self.cursor_y = row_top + max_h;
        }
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
        let taken = std::mem::take(words);

        // Greedily assign words to lines, recording each line's word range.
        let mut lines: Vec<std::ops::Range<usize>> = Vec::new();
        let mut line_start = 0usize;
        let mut pen = 0.0f32;
        for (i, w) in taken.iter().enumerate() {
            // A <br> forces a line break before it (without itself being placed).
            if w.hard_break && i > line_start {
                lines.push(line_start..i);
                line_start = i;
                pen = 0.0;
                continue;
            }
            let space = if i > line_start && w.space_before {
                self.font.measure(" ", w.font_size) + block.word_spacing
            } else {
                0.0
            };
            let ww = self.font.measure(&w.text, w.font_size);
            // The first line has less room when `text-indent` is set.
            let line_width = if line_start == 0 {
                (width - block.text_indent).max(0.0)
            } else {
                width
            };
            if !block.nowrap && i > line_start && pen + space + ww > line_width {
                lines.push(line_start..i);
                line_start = i;
                pen = ww;
            } else {
                pen += space + ww;
            }
        }
        lines.push(line_start..taken.len());

        let line_count = lines.len();
        for (line_idx, range) in lines.into_iter().enumerate() {
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
                ((width - line_w) / gaps as f32).max(0.0)
            } else {
                0.0
            };
            let offset = match block.text_align {
                TextAlign::Center => ((width - line_w) / 2.0).max(0.0),
                TextAlign::Right => (width - line_w).max(0.0),
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
            let mut pen_x = x + offset + indent;
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
                    self.rects.push(rect(pen_x, uy, word_w, uh, w.color));
                }
                if w.strike {
                    let sy = wb - self.font.ascent_px(w.font_size) * 0.32;
                    let sh = (w.font_size / 16.0).max(1.0);
                    self.rects.push(rect(pen_x, sy, word_w, sh, w.color));
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
        // (flex/grid/tables/lists) are exercised, not just text.
        const BIAS: &[u8] =
            b"<>/=\"' ;:{}().%#-\nstyledivpaulitbflexgridcolorwidthpaddingmarginbordedisplay0123";
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
            // must produce finite geometry.
            let l = layout(&doc, &font, 400.0, &ImageSizes::new());
            assert!(l.height.is_finite());
            for r in &l.rects {
                assert!(r.w.is_finite() && r.h.is_finite());
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
}
