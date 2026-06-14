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
    ctx.layout_block(start, start_style, content_x, content_width);

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
    fn layout_block(&mut self, id: NodeId, style: ComputedStyle, x: f32, avail: f32) {
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

        // Children.
        let mut inline = String::new();
        for child in self.doc.children(id) {
            match &self.doc.node(child).data {
                NodeData::Text(t) => {
                    inline.push_str(t);
                    inline.push(' ');
                }
                NodeData::Element(e) if e.name.is_html("img") => {
                    self.flush_inline(&mut inline, &style, content_left, content_w);
                    self.place_image(e, content_left, content_w);
                }
                NodeData::Element(_) => {
                    let cstyle = computed_style(self.doc, child, &style, self.author);
                    match cstyle.display {
                        Display::None => {}
                        Display::Inline => {
                            self.gather_inline_text(child, &mut inline);
                            inline.push(' ');
                        }
                        Display::Block => {
                            self.flush_inline(&mut inline, &style, content_left, content_w);
                            self.cursor_y += cstyle.margin.top;
                            self.layout_block(child, cstyle, content_left, content_w);
                            self.cursor_y += cstyle.margin.bottom;
                        }
                    }
                }
                _ => {}
            }
        }
        self.flush_inline(&mut inline, &style, content_left, content_w);

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

    fn gather_inline_text(&self, id: NodeId, out: &mut String) {
        match &self.doc.node(id).data {
            NodeData::Text(t) => out.push_str(t),
            NodeData::Element(_) => {
                for child in self.doc.children(id) {
                    self.gather_inline_text(child, out);
                }
            }
            _ => {}
        }
    }

    fn flush_inline(&mut self, inline: &mut String, style: &ComputedStyle, x: f32, width: f32) {
        let text: String = inline.split_whitespace().collect::<Vec<_>>().join(" ");
        inline.clear();
        if text.is_empty() {
            return;
        }

        let mut line = String::new();
        for word in text.split(' ') {
            let candidate = if line.is_empty() {
                word.to_string()
            } else {
                format!("{line} {word}")
            };
            if line.is_empty() || self.font.measure(&candidate, style.font_size) <= width {
                line = candidate;
            } else {
                self.emit_line(&line, style, x, width);
                line = word.to_string();
            }
        }
        if !line.is_empty() {
            self.emit_line(&line, style, x, width);
        }
    }

    fn emit_line(&mut self, line: &str, style: &ComputedStyle, x: f32, width: f32) {
        let line_w = self.font.measure(line, style.font_size);
        let offset = match style.text_align {
            TextAlign::Left => 0.0,
            TextAlign::Center => ((width - line_w) / 2.0).max(0.0),
            TextAlign::Right => (width - line_w).max(0.0),
        };
        let baseline = self.cursor_y + self.font.ascent_px(style.font_size);
        self.runs.push(TextRun {
            x: x + offset,
            baseline,
            text: line.to_string(),
            size_px: style.font_size,
            color: style.color,
        });
        self.cursor_y += style.font_size * LINE_HEIGHT;
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
