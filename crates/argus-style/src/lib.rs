//! Style engine (Phase 1 minimal slice).
//!
//! A full CSS parser + selector engine + cascade is `argus-css` (deferred). For
//! the first "document to pixels" slice this crate provides a built-in user-agent
//! stylesheet keyed by tag name and `font-size` inheritance — enough to give
//! headings, paragraphs, and inline text sensible defaults. See
//! `docs/subsystems/style.md`.

use argus_dom::ElementData;

/// The `display` value, reduced to what Phase 1 layout understands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Display {
    Block,
    Inline,
    None,
}

/// A computed style for one element. Lengths are in CSS pixels.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ComputedStyle {
    pub display: Display,
    pub font_size: f32,
    /// Vertical margins (block-axis). Horizontal margins are ignored for now.
    pub margin_top: f32,
    pub margin_bottom: f32,
    /// Whether this element's text is bold (headings/strong) — advisory for now.
    pub bold: bool,
}

impl ComputedStyle {
    /// The initial style for the root's containing block.
    pub fn initial() -> ComputedStyle {
        ComputedStyle {
            display: Display::Block,
            font_size: 16.0,
            margin_top: 0.0,
            margin_bottom: 0.0,
            bold: false,
        }
    }
}

/// Compute the style of an element from the built-in UA stylesheet, inheriting
/// `font_size` from `parent`.
pub fn computed_style(element: &ElementData, parent: &ComputedStyle) -> ComputedStyle {
    let name = &*element.name.local;
    let base = parent.font_size;

    let (display, em) = match name {
        // Not rendered.
        "head" | "title" | "style" | "script" | "meta" | "link" | "base" | "noscript" => {
            (Display::None, 1.0)
        }
        // Headings: size + bold + block margins (em-based, approximating UA css).
        "h1" => (Display::Block, 2.00),
        "h2" => (Display::Block, 1.50),
        "h3" => (Display::Block, 1.17),
        "h4" => (Display::Block, 1.00),
        "h5" => (Display::Block, 0.83),
        "h6" => (Display::Block, 0.67),
        // Common block containers.
        "html" | "body" | "div" | "p" | "section" | "article" | "header" | "footer" | "nav"
        | "main" | "aside" | "figure" | "blockquote" | "ul" | "ol" | "li" | "dl" | "dt" | "dd"
        | "pre" | "table" | "form" | "hr" | "address" => (Display::Block, 1.0),
        // Inline elements (and unknown elements default to inline, like browsers).
        _ => (Display::Inline, 1.0),
    };

    let font_size = if matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
        16.0 * em // headings size from the base medium, not the inherited size
    } else {
        base * em
    };

    let bold = parent.bold
        || matches!(
            name,
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "b" | "strong"
        );

    // Block margins: headings and paragraphs get vertical spacing.
    let (margin_top, margin_bottom) = match name {
        "p" => (font_size, font_size),
        "h1" => (0.67 * font_size, 0.67 * font_size),
        "h2" => (0.83 * font_size, 0.83 * font_size),
        "h3" => (1.0 * font_size, 1.0 * font_size),
        "h4" => (1.33 * font_size, 1.33 * font_size),
        "h5" => (1.67 * font_size, 1.67 * font_size),
        "h6" => (2.33 * font_size, 2.33 * font_size),
        "ul" | "ol" | "blockquote" | "figure" | "pre" => (font_size, font_size),
        _ => (0.0, 0.0),
    };

    ComputedStyle {
        display,
        font_size,
        margin_top,
        margin_bottom,
        bold,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argus_dom::QualName;

    fn el(name: &str) -> ElementData {
        ElementData {
            name: QualName::html(name),
            attrs: Vec::new(),
        }
    }

    #[test]
    fn headings_are_block_bold_and_larger() {
        let root = ComputedStyle::initial();
        let h1 = computed_style(&el("h1"), &root);
        assert_eq!(h1.display, Display::Block);
        assert!(h1.bold);
        assert_eq!(h1.font_size, 32.0);
        assert!(h1.margin_top > 0.0);
    }

    #[test]
    fn paragraphs_block_spans_inline_unknown_inline() {
        let root = ComputedStyle::initial();
        assert_eq!(computed_style(&el("p"), &root).display, Display::Block);
        assert_eq!(computed_style(&el("span"), &root).display, Display::Inline);
        assert_eq!(
            computed_style(&el("whatsit"), &root).display,
            Display::Inline
        );
        assert_eq!(computed_style(&el("head"), &root).display, Display::None);
    }

    #[test]
    fn font_size_inherits() {
        let mut big = ComputedStyle::initial();
        big.font_size = 20.0;
        // A span under a 20px context inherits 20px.
        assert_eq!(computed_style(&el("span"), &big).font_size, 20.0);
    }
}
