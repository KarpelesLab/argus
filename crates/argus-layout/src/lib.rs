//! Layout engine (Phase 1 minimal slice).
//!
//! Block-and-inline layout producing positioned text runs. Block boxes stack
//! vertically with their UA margins; inline content (text, inline elements) within
//! a block is greedily broken into lines that fit the content width, measured with
//! the real font (`oxideav-scribe` via `argus-gfx`). The output is a flat list of
//! [`TextRun`]s ready for `argus-gfx::render_runs`.
//!
//! This is a deliberately small subset of `docs/subsystems/layout.md`: no flex/grid,
//! no floats/positioning, no margin collapsing, no inline-level boxes with their own
//! geometry, and inline runs adopt their block's font size. It is enough to render a
//! readable document, and grows into the real box/fragment tree later.

use argus_dom::{Document, NodeData, NodeId};
use argus_gfx::{Font, TextRun};
use argus_style::{computed_style, ComputedStyle, Display};

/// Vertical line spacing as a multiple of font size.
const LINE_HEIGHT: f32 = 1.2;
/// Default page margin (UA `body { margin: 8px }`), in CSS pixels.
const PAGE_MARGIN: f32 = 8.0;

/// The result of laying a document out at a given viewport width.
pub struct Layout {
    /// Positioned text runs, top-to-bottom.
    pub runs: Vec<TextRun>,
    /// Total content height in pixels (useful for scrolling later).
    pub height: f32,
}

/// Lay `doc` out into text runs for a viewport `viewport_width` pixels wide.
pub fn layout(doc: &Document, font: &Font, viewport_width: f32) -> Layout {
    let content_x = PAGE_MARGIN;
    let content_width = (viewport_width - 2.0 * PAGE_MARGIN).max(0.0);

    let mut ctx = Ctx {
        doc,
        font,
        runs: Vec::new(),
        cursor_y: PAGE_MARGIN,
    };

    let root_style = ComputedStyle::initial();
    let start = body_or_root(doc);
    let start_style = match &doc.node(start).data {
        NodeData::Element(e) => computed_style(e, &root_style),
        _ => root_style,
    };
    ctx.layout_block(start, start_style, content_x, content_width);

    Layout {
        runs: ctx.runs,
        height: ctx.cursor_y + PAGE_MARGIN,
    }
}

/// The `<body>` element if present, else the document root.
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
    runs: Vec<TextRun>,
    cursor_y: f32,
}

impl Ctx<'_> {
    /// Lay out the children of block `id` (whose computed style is `style`) within
    /// the column `[x, x + width)`.
    fn layout_block(&mut self, id: NodeId, style: ComputedStyle, x: f32, width: f32) {
        let mut inline = String::new();

        for child in self.doc.children(id) {
            match &self.doc.node(child).data {
                NodeData::Text(t) => {
                    inline.push_str(t);
                    inline.push(' ');
                }
                NodeData::Element(e) => {
                    let cstyle = computed_style(e, &style);
                    match cstyle.display {
                        Display::None => {}
                        Display::Inline => {
                            self.gather_inline_text(child, &mut inline);
                            inline.push(' ');
                        }
                        Display::Block => {
                            self.flush_inline(&mut inline, style.font_size, x, width);
                            self.cursor_y += cstyle.margin_top;
                            self.layout_block(child, cstyle, x, width);
                            self.cursor_y += cstyle.margin_bottom;
                        }
                    }
                }
                _ => {}
            }
        }
        self.flush_inline(&mut inline, style.font_size, x, width);
    }

    /// Concatenate all descendant text of an inline subtree.
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

    /// Break accumulated inline text into lines and emit them, then clear `inline`.
    fn flush_inline(&mut self, inline: &mut String, font_size: f32, x: f32, width: f32) {
        // Collapse runs of whitespace (normal `white-space`).
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
            if line.is_empty() || self.font.measure(&candidate, font_size) <= width {
                line = candidate;
            } else {
                self.emit_line(&line, font_size, x);
                line = word.to_string();
            }
        }
        if !line.is_empty() {
            self.emit_line(&line, font_size, x);
        }
    }

    fn emit_line(&mut self, line: &str, font_size: f32, x: f32) {
        let baseline = self.cursor_y + self.font.ascent_px(font_size);
        self.runs.push(TextRun {
            x,
            baseline,
            text: line.to_string(),
            size_px: font_size,
        });
        self.cursor_y += font_size * LINE_HEIGHT;
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
    fn lays_out_headings_and_wrapped_paragraph() {
        let Some(font) = system_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let html = "<h1>Title</h1><p>one two three four five six seven eight nine ten \
                    eleven twelve thirteen fourteen fifteen sixteen seventeen</p>";
        let doc = parse(html);
        let layout = layout(&doc, &font, 200.0);

        // The heading is one run at 32px; the long paragraph wraps to >1 line at 16px.
        let h1_runs: Vec<_> = layout.runs.iter().filter(|r| r.size_px == 32.0).collect();
        let p_runs: Vec<_> = layout.runs.iter().filter(|r| r.size_px == 16.0).collect();
        assert_eq!(h1_runs.len(), 1, "expected one heading run");
        assert!(
            p_runs.len() >= 2,
            "paragraph should wrap, got {}",
            p_runs.len()
        );

        // Runs are ordered top-to-bottom by baseline.
        let baselines: Vec<f32> = layout.runs.iter().map(|r| r.baseline).collect();
        assert!(
            baselines.windows(2).all(|w| w[0] <= w[1]),
            "runs must be ordered"
        );
        assert!(layout.height > 0.0);
    }
}
