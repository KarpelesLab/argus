//! Layout engine (Phase 1 slice).
//!
//! Block-and-inline layout producing a display list: filled background and border
//! rects for block boxes plus positioned, colored, aligned text runs. Block boxes
//! stack vertically with their cascaded margins; each box honors width, padding,
//! and borders (the standard content/padding/border box geometry). Inline content
//! is greedily broken into lines that fit the content width, measured with the real
//! font, and aligned per `text-align`. Styles come from the `argus-style` cascade.
//!
//! Still a subset of `docs/subsystems/layout.md`: no flex/grid, no floats/
//! positioning, no margin collapsing, no inline-level boxes with their own geometry
//! (inline runs adopt their block's font size/color).

use argus_dom::{Document, ElementData, NodeData, NodeId};
use argus_gfx::{Font, RectFill, TextRun};
use argus_style::{
    author_stylesheet, computed_style, AuthorStylesheet, ComputedStyle, Display, TextAlign,
};
use std::collections::HashMap;

const LINE_HEIGHT: f32 = 1.2;
const PAGE_MARGIN: f32 = 8.0;

/// A list container kind, for `<li>` marker generation.
#[derive(Clone, Copy)]
enum ListKind {
    Unordered,
    Ordered,
}

impl ListKind {
    fn marker(self, index: u32) -> String {
        match self {
            ListKind::Unordered => "\u{2022}".to_string(), // •
            ListKind::Ordered => format!("{index}."),
        }
    }
}

/// A word in an inline formatting context, carrying its own style so spans, links,
/// and emphasis keep their color/size within a paragraph.
struct InlineWord {
    text: String,
    font_size: f32,
    color: argus_geometry::Color,
    /// Whether whitespace precedes this word (a break opportunity + a space glyph).
    space_before: bool,
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

/// The result of laying a document out at a given viewport width.
pub struct Layout {
    /// Background + border rectangles, painted behind text (ancestors first).
    pub rects: Vec<RectFill>,
    /// Positioned, colored text runs, top-to-bottom.
    pub runs: Vec<TextRun>,
    /// Placed images (blitted by the content process from decoded bytes).
    pub images: Vec<ImageBox>,
    /// Total content height in pixels.
    pub height: f32,
}

/// Intrinsic `(width, height)` of each image by source URL, for sizing boxes.
pub type ImageSizes = HashMap<String, (u32, u32)>;

/// Lay `doc` out into a display list for a viewport `viewport_width` pixels wide,
/// given the intrinsic sizes of any images.
pub fn layout(doc: &Document, font: &Font, viewport_width: f32, images: &ImageSizes) -> Layout {
    let content_x = PAGE_MARGIN;
    let content_width = (viewport_width - 2.0 * PAGE_MARGIN).max(0.0);
    let author = author_stylesheet(doc);

    let mut ctx = Ctx {
        doc,
        font,
        author: &author,
        image_sizes: images,
        rects: Vec::new(),
        runs: Vec::new(),
        images: Vec::new(),
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
        marker: Option<String>,
    ) {
        let border_box_top = self.cursor_y;
        let border_box_left = x + style.margin.left;

        let h_extra = style.margin.left
            + style.margin.right
            + style.border.left
            + style.border.right
            + style.padding.left
            + style.padding.right;
        let content_w = match style.width {
            Some(len) => len.to_px(style.font_size, avail),
            None => (avail - h_extra).max(0.0),
        };
        let content_left = border_box_left + style.border.left + style.padding.left;
        let border_box_w = content_w
            + style.padding.left
            + style.padding.right
            + style.border.left
            + style.border.right;

        // Reserve background + border rect slots up front so ancestors paint first.
        let bg_idx = (style.background_color.a > 0).then(|| {
            self.rects.push(RectFill {
                x: border_box_left,
                y: border_box_top,
                w: border_box_w,
                h: 0.0,
                color: style.background_color,
            });
            self.rects.len() - 1
        });
        let has_border = style.border_color.a > 0
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
                });
            }
            i
        });

        self.cursor_y += style.border.top + style.padding.top;

        // A list-item marker sits on the first line, just left of the content.
        if let Some(marker) = &marker {
            let baseline = self.cursor_y + self.font.ascent_px(style.font_size);
            let mw = self.font.measure(marker, style.font_size);
            self.runs.push(TextRun {
                x: content_left - mw - 8.0,
                baseline,
                text: marker.clone(),
                size_px: style.font_size,
                color: style.color,
            });
        }

        // Is this a list container? Its <li> children get markers.
        let list_kind = self.doc.node(id).as_element().and_then(|e| {
            if e.name.is_html("ul") {
                Some(ListKind::Unordered)
            } else if e.name.is_html("ol") {
                Some(ListKind::Ordered)
            } else {
                None
            }
        });
        let mut item_index = 0u32;

        // Children. Inline-level content accumulates into `words` (each with its own
        // style); block-level children flush the line box and lay out separately.
        let mut words: Vec<InlineWord> = Vec::new();
        let mut pending_space = false;
        for child in self.doc.children(id) {
            match &self.doc.node(child).data {
                NodeData::Text(_) => {
                    self.gather_inline(child, &style, &mut words, &mut pending_space);
                }
                NodeData::Element(e) if e.name.is_html("img") => {
                    self.flush_words(&mut words, &style, content_left, content_w);
                    pending_space = false;
                    self.place_image(e, content_left, content_w);
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
                NodeData::Element(_) => {
                    let cstyle = computed_style(self.doc, child, &style, self.author);
                    match cstyle.display {
                        Display::None => {}
                        Display::Inline => {
                            self.gather_inline(child, &cstyle, &mut words, &mut pending_space);
                        }
                        Display::Block => {
                            self.flush_words(&mut words, &style, content_left, content_w);
                            pending_space = false;
                            let child_marker = match list_kind {
                                Some(kind) if self.is_li(child) => {
                                    item_index += 1;
                                    Some(kind.marker(item_index))
                                }
                                _ => None,
                            };
                            self.cursor_y += cstyle.margin.top;
                            self.layout_block(child, cstyle, content_left, content_w, child_marker);
                            self.cursor_y += cstyle.margin.bottom;
                        }
                    }
                }
                _ => {}
            }
        }
        self.flush_words(&mut words, &style, content_left, content_w);

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
    }

    /// Place an `<img>` as a block-level replaced box on its own line.
    fn place_image(&mut self, e: &ElementData, x: f32, avail: f32) {
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
            self.images.push(ImageBox {
                x,
                y: self.cursor_y,
                w,
                h,
                src: src.to_string(),
            });
            self.cursor_y += h;
        }
    }

    fn is_li(&self, id: NodeId) -> bool {
        matches!(&self.doc.node(id).data, NodeData::Element(e) if e.name.is_html("li"))
    }

    /// Flatten an inline subtree into styled words, collapsing whitespace and
    /// tracking break opportunities via `space_before`.
    fn gather_inline(
        &self,
        id: NodeId,
        style: &ComputedStyle,
        words: &mut Vec<InlineWord>,
        pending_space: &mut bool,
    ) {
        match &self.doc.node(id).data {
            NodeData::Text(t) => {
                if t.starts_with(char::is_whitespace) {
                    *pending_space = true;
                }
                let mut first = true;
                for word in t.split_whitespace() {
                    words.push(InlineWord {
                        text: word.to_string(),
                        font_size: style.font_size,
                        color: style.color,
                        // Words within a text node are separated by whitespace.
                        space_before: *pending_space || !first,
                    });
                    *pending_space = false;
                    first = false;
                }
                if t.ends_with(char::is_whitespace) {
                    *pending_space = true;
                }
            }
            NodeData::Element(_) => {
                let cstyle = computed_style(self.doc, id, style, self.author);
                if cstyle.display == Display::None {
                    return;
                }
                for child in self.doc.children(id) {
                    self.gather_inline(child, &cstyle, words, pending_space);
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
            let space = if i > line_start && w.space_before {
                self.font.measure(" ", w.font_size)
            } else {
                0.0
            };
            let ww = self.font.measure(&w.text, w.font_size);
            if i > line_start && pen + space + ww > width {
                lines.push(line_start..i);
                line_start = i;
                pen = ww;
            } else {
                pen += space + ww;
            }
        }
        lines.push(line_start..taken.len());

        for range in lines {
            let line = &taken[range.clone()];
            // Line width and tallest font for baseline/height.
            let mut line_w = 0.0f32;
            let mut max_size = 0.0f32;
            for (j, w) in line.iter().enumerate() {
                let space = if j > 0 && w.space_before {
                    self.font.measure(" ", w.font_size)
                } else {
                    0.0
                };
                line_w += space + self.font.measure(&w.text, w.font_size);
                max_size = max_size.max(w.font_size);
            }
            let offset = match block.text_align {
                TextAlign::Left => 0.0,
                TextAlign::Center => ((width - line_w) / 2.0).max(0.0),
                TextAlign::Right => (width - line_w).max(0.0),
            };
            let baseline = self.cursor_y + self.font.ascent_px(max_size);

            let mut pen_x = x + offset;
            for (j, w) in line.iter().enumerate() {
                if j > 0 && w.space_before {
                    pen_x += self.font.measure(" ", w.font_size);
                }
                self.runs.push(TextRun {
                    x: pen_x,
                    baseline,
                    text: w.text.clone(),
                    size_px: w.font_size,
                    color: w.color,
                });
                pen_x += self.font.measure(&w.text, w.font_size);
            }
            self.cursor_y += max_size * LINE_HEIGHT;
        }
    }
}

fn rect(x: f32, y: f32, w: f32, h: f32, color: argus_geometry::Color) -> RectFill {
    RectFill { x, y, w, h, color }
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
}
