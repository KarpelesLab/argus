//! Layout engine (Phase 1 minimal slice).
//!
//! Block-and-inline layout producing a small display list: filled background rects
//! for block boxes plus positioned, colored text runs. Block boxes stack
//! vertically with their cascaded margins; inline content is greedily broken into
//! lines that fit the content width, measured with the real font. Styles come from
//! the cascade in `argus-style` (UA + author `<style>` + inline).
//!
//! Still a small subset of `docs/subsystems/layout.md`: no flex/grid, no
//! floats/positioning, no margin collapsing, no inline-level boxes with their own
//! geometry; inline runs adopt their block's font size and color.

use argus_dom::{Document, NodeData, NodeId};
use argus_gfx::{Font, RectFill, TextRun};
use argus_style::{author_stylesheet, computed_style, AuthorStylesheet, ComputedStyle, Display};

/// Vertical line spacing as a multiple of font size.
const LINE_HEIGHT: f32 = 1.2;
/// Default page margin (UA `body { margin: 8px }`), in CSS pixels.
const PAGE_MARGIN: f32 = 8.0;

/// The result of laying a document out at a given viewport width.
pub struct Layout {
    /// Block background rectangles, painted behind text (ancestors first).
    pub rects: Vec<RectFill>,
    /// Positioned, colored text runs, top-to-bottom.
    pub runs: Vec<TextRun>,
    /// Total content height in pixels.
    pub height: f32,
}

/// Lay `doc` out into a display list for a viewport `viewport_width` pixels wide.
pub fn layout(doc: &Document, font: &Font, viewport_width: f32) -> Layout {
    let content_x = PAGE_MARGIN;
    let content_width = (viewport_width - 2.0 * PAGE_MARGIN).max(0.0);
    let author = author_stylesheet(doc);

    let mut ctx = Ctx {
        doc,
        font,
        author: &author,
        rects: Vec::new(),
        runs: Vec::new(),
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
    rects: Vec<RectFill>,
    runs: Vec<TextRun>,
    cursor_y: f32,
}

impl Ctx<'_> {
    fn layout_block(&mut self, id: NodeId, style: ComputedStyle, x: f32, width: f32) {
        // Reserve a background rect (filled in once the block's height is known) so
        // ancestors paint behind descendants.
        let y_start = self.cursor_y;
        let rect_idx = if style.background_color.a > 0 {
            let idx = self.rects.len();
            self.rects.push(RectFill {
                x,
                y: y_start,
                w: width,
                h: 0.0,
                color: style.background_color,
            });
            Some(idx)
        } else {
            None
        };

        let mut inline = String::new();
        for child in self.doc.children(id) {
            match &self.doc.node(child).data {
                NodeData::Text(t) => {
                    inline.push_str(t);
                    inline.push(' ');
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
                            self.flush_inline(&mut inline, &style, x, width);
                            self.cursor_y += cstyle.margin_top;
                            self.layout_block(child, cstyle, x, width);
                            self.cursor_y += cstyle.margin_bottom;
                        }
                    }
                }
                _ => {}
            }
        }
        self.flush_inline(&mut inline, &style, x, width);

        if let Some(idx) = rect_idx {
            self.rects[idx].h = self.cursor_y - y_start;
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
                self.emit_line(&line, style, x);
                line = word.to_string();
            }
        }
        if !line.is_empty() {
            self.emit_line(&line, style, x);
        }
    }

    fn emit_line(&mut self, line: &str, style: &ComputedStyle, x: f32) {
        let baseline = self.cursor_y + self.font.ascent_px(style.font_size);
        self.runs.push(TextRun {
            x,
            baseline,
            text: line.to_string(),
            size_px: style.font_size,
            color: style.color,
        });
        self.cursor_y += style.font_size * LINE_HEIGHT;
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
    fn headings_wrap_color_and_background() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let html = "<style>p { color: #ff0000 } body { background-color: #eef }</style>\
                    <h1>Title</h1><p>one two three four five six seven eight nine ten \
                    eleven twelve thirteen fourteen fifteen sixteen seventeen</p>";
        let doc = parse(html);
        let layout = layout(&doc, &font, 200.0);

        let h1_runs: Vec<_> = layout.runs.iter().filter(|r| r.size_px == 32.0).collect();
        let p_runs: Vec<_> = layout.runs.iter().filter(|r| r.size_px == 16.0).collect();
        assert_eq!(h1_runs.len(), 1);
        assert!(p_runs.len() >= 2, "paragraph should wrap");
        // Author CSS colored the paragraph red and gave body a background.
        assert!(p_runs
            .iter()
            .all(|r| r.color == argus_geometry::Color::rgb(255, 0, 0)));
        assert!(
            !layout.rects.is_empty(),
            "body background should produce a rect"
        );

        let baselines: Vec<f32> = layout.runs.iter().map(|r| r.baseline).collect();
        assert!(baselines.windows(2).all(|w| w[0] <= w[1]));
    }
}
