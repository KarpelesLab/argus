//! Style engine: the cascade (Phase 1).
//!
//! Computes element styles by cascading three origins — a built-in user-agent
//! stylesheet, author stylesheets (the page's `<style>` elements), and inline
//! `style` attributes — sorted by origin, `!important`, specificity, and source
//! order, on top of inherited values. Selector matching and value parsing come
//! from `argus-css`. A full property model is future work; we interpret the subset
//! Phase 1 layout/paint use. See `docs/subsystems/style.md`.

use argus_css::{matches, parse_color, parse_declaration_block, parse_length, parse_stylesheet};
use argus_css::{Specificity, Stylesheet};

pub use argus_css::Stylesheet as AuthorStylesheet;
use argus_dom::{Document, NodeData, NodeId};
use argus_geometry::Color;
use std::collections::HashMap;
use std::sync::OnceLock;

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
    pub margin_top: f32,
    pub margin_bottom: f32,
    pub bold: bool,
    pub color: Color,
    pub background_color: Color,
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
            color: Color::BLACK,
            background_color: Color::TRANSPARENT,
        }
    }
}

/// The built-in user-agent stylesheet (expressed as real CSS).
const UA_CSS: &str = "\
html, body, div, p, h1, h2, h3, h4, h5, h6, section, article, header, footer, nav, \
main, aside, figure, blockquote, ul, ol, li, dl, dt, dd, pre, table, form, hr, address \
{ display: block }
head, title, style, script, meta, link, base, noscript { display: none }
h1 { font-size: 2em; font-weight: bold; margin: 0.67em 0 }
h2 { font-size: 1.5em; font-weight: bold; margin: 0.83em 0 }
h3 { font-size: 1.17em; font-weight: bold; margin: 1em 0 }
h4 { font-weight: bold; margin: 1.33em 0 }
h5 { font-size: 0.83em; font-weight: bold; margin: 1.67em 0 }
h6 { font-size: 0.67em; font-weight: bold; margin: 2.33em 0 }
p { margin: 1em 0 }
b, strong { font-weight: bold }
ul, ol, blockquote, figure, pre { margin: 1em 0 }
";

fn ua_stylesheet() -> &'static Stylesheet {
    static UA: OnceLock<Stylesheet> = OnceLock::new();
    UA.get_or_init(|| parse_stylesheet(UA_CSS))
}

/// Parse the document's author stylesheet by concatenating every `<style>`
/// element's text content.
pub fn author_stylesheet(doc: &Document) -> Stylesheet {
    let mut css = String::new();
    collect_style_text(doc, doc.root(), &mut css);
    parse_stylesheet(&css)
}

fn collect_style_text(doc: &Document, id: NodeId, out: &mut String) {
    if let NodeData::Element(e) = &doc.node(id).data {
        if e.name.is_html("style") {
            for child in doc.children(id) {
                if let NodeData::Text(t) = &doc.node(child).data {
                    out.push_str(t);
                    out.push('\n');
                }
            }
            return;
        }
    }
    for child in doc.children(id) {
        collect_style_text(doc, child, out);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    Ua,
    Author,
    Inline,
}

/// Cascade priority rank: higher wins. Mirrors the CSS cascade order.
fn rank(origin: Origin, important: bool) -> u8 {
    match (origin, important) {
        (Origin::Ua, false) => 0,
        (Origin::Author, false) => 1,
        (Origin::Inline, false) => 2,
        (Origin::Author, true) => 3,
        (Origin::Inline, true) => 4,
        (Origin::Ua, true) => 5,
    }
}

struct Cand {
    rank: u8,
    spec: Specificity,
    order: usize,
    name: String,
    value: String,
}

/// Compute the cascaded style of element `node`, inheriting from `parent`, given
/// the page's `author` stylesheet.
pub fn computed_style(
    doc: &Document,
    node: NodeId,
    parent: &ComputedStyle,
    author: &Stylesheet,
) -> ComputedStyle {
    let mut cands = Vec::new();
    let mut order = 0usize;
    collect(
        ua_stylesheet(),
        Origin::Ua,
        doc,
        node,
        &mut cands,
        &mut order,
    );
    collect(author, Origin::Author, doc, node, &mut cands, &mut order);

    if let NodeData::Element(e) = &doc.node(node).data {
        if let Some(style) = e.attr("style") {
            for d in parse_declaration_block(style) {
                cands.push(Cand {
                    rank: rank(Origin::Inline, d.important),
                    spec: Specificity::default(),
                    order,
                    name: d.name,
                    value: d.value,
                });
                order += 1;
            }
        }
    }

    // Ascending so later (higher priority) declarations overwrite earlier ones.
    cands.sort_by_key(|c| (c.rank, c.spec, c.order));
    let mut map: HashMap<String, String> = HashMap::new();
    for c in cands {
        map.insert(c.name, c.value);
    }

    let mut cs = ComputedStyle {
        display: Display::Inline,
        font_size: parent.font_size,
        margin_top: 0.0,
        margin_bottom: 0.0,
        bold: parent.bold,
        color: parent.color,
        background_color: Color::TRANSPARENT,
    };
    apply(&mut cs, &map, parent);
    cs
}

fn collect(
    sheet: &Stylesheet,
    origin: Origin,
    doc: &Document,
    node: NodeId,
    cands: &mut Vec<Cand>,
    order: &mut usize,
) {
    for rule in &sheet.rules {
        let best = rule
            .selectors
            .iter()
            .filter(|s| matches(doc, node, s))
            .map(|s| s.specificity())
            .max();
        if let Some(spec) = best {
            for d in &rule.declarations {
                cands.push(Cand {
                    rank: rank(origin, d.important),
                    spec,
                    order: *order,
                    name: d.name.clone(),
                    value: d.value.clone(),
                });
                *order += 1;
            }
        }
    }
}

fn apply(cs: &mut ComputedStyle, map: &HashMap<String, String>, parent: &ComputedStyle) {
    if let Some(v) = map.get("display") {
        cs.display = match v.as_str() {
            "block" => Display::Block,
            "none" => Display::None,
            _ => Display::Inline,
        };
    }
    if let Some(v) = map.get("font-size") {
        if let Some(px) = resolve_font_size(v, parent.font_size) {
            cs.font_size = px;
        }
    }
    if let Some(v) = map.get("font-weight") {
        cs.bold = is_bold(v);
    }
    if let Some(v) = map.get("color") {
        if let Some(c) = parse_color(v) {
            cs.color = c;
        }
    }
    if let Some(v) = map
        .get("background-color")
        .or_else(|| map.get("background"))
    {
        if let Some(c) = color_in(v) {
            cs.background_color = c;
        }
    }

    let fs = cs.font_size;
    if let Some(v) = map.get("margin") {
        let (t, b) = margin_shorthand(v, fs);
        cs.margin_top = t;
        cs.margin_bottom = b;
    }
    if let Some(v) = map.get("margin-top").and_then(|v| parse_length(v)) {
        cs.margin_top = v.to_px(fs, 0.0);
    }
    if let Some(v) = map.get("margin-bottom").and_then(|v| parse_length(v)) {
        cs.margin_bottom = v.to_px(fs, 0.0);
    }
}

fn resolve_font_size(v: &str, parent_fs: f32) -> Option<f32> {
    if let Some(len) = parse_length(v) {
        return Some(len.to_px(parent_fs, parent_fs));
    }
    Some(match v {
        "xx-small" => 9.0,
        "x-small" => 10.0,
        "small" => 13.0,
        "medium" => 16.0,
        "large" => 18.0,
        "x-large" => 24.0,
        "xx-large" => 32.0,
        "smaller" => parent_fs * 0.83,
        "larger" => parent_fs * 1.2,
        _ => return None,
    })
}

fn is_bold(v: &str) -> bool {
    match v {
        "bold" | "bolder" => true,
        "normal" | "lighter" => false,
        n => n.parse::<u32>().map(|w| w >= 600).unwrap_or(false),
    }
}

/// First token of `v` that parses as a color (handles the `background` shorthand).
fn color_in(v: &str) -> Option<Color> {
    parse_color(v).or_else(|| v.split_whitespace().find_map(parse_color))
}

fn margin_shorthand(v: &str, fs: f32) -> (f32, f32) {
    let vals: Vec<f32> = v
        .split_whitespace()
        .map(|tok| parse_length(tok).map(|l| l.to_px(fs, 0.0)).unwrap_or(0.0))
        .collect();
    let top = vals.first().copied().unwrap_or(0.0);
    let bottom = vals.get(2).or_else(|| vals.first()).copied().unwrap_or(0.0);
    (top, bottom)
}

#[cfg(test)]
mod tests {
    use super::*;
    use argus_dom::{Attribute, QualName};

    fn doc_with(html_like: impl FnOnce(&mut Document) -> NodeId) -> (Document, NodeId) {
        let mut doc = Document::new();
        let node = html_like(&mut doc);
        (doc, node)
    }

    #[test]
    fn ua_headings() {
        let (doc, h1) = doc_with(|doc| {
            let root = doc.root();
            let el = doc.create_element(QualName::html("h1"), vec![]);
            doc.append(root, el);
            el
        });
        let author = Stylesheet::default();
        let cs = computed_style(&doc, h1, &ComputedStyle::initial(), &author);
        assert_eq!(cs.display, Display::Block);
        assert!(cs.bold);
        assert_eq!(cs.font_size, 32.0);
        assert!(cs.margin_top > 0.0);
    }

    #[test]
    fn author_and_inline_cascade() {
        // <p class="lead" style="color: red">  with author `p { color: blue }`
        let (doc, p) = doc_with(|doc| {
            let root = doc.root();
            let el = doc.create_element(
                QualName::html("p"),
                vec![
                    Attribute::new("class", "lead"),
                    Attribute::new("style", "color: red"),
                ],
            );
            doc.append(root, el);
            el
        });
        let author =
            parse_stylesheet("p { color: blue; background-color: #eee } .lead { color: green }");
        let cs = computed_style(&doc, p, &ComputedStyle::initial(), &author);
        // Inline `color: red` beats author rules; background comes from author.
        assert_eq!(cs.color, Color::rgb(255, 0, 0));
        assert_eq!(cs.background_color, Color::rgb(0xee, 0xee, 0xee));
    }

    #[test]
    fn specificity_decides_within_author() {
        let (doc, p) = doc_with(|doc| {
            let root = doc.root();
            let el = doc.create_element(QualName::html("p"), vec![Attribute::new("class", "lead")]);
            doc.append(root, el);
            el
        });
        // `.lead` (0,1,0) beats `p` (0,0,1) regardless of source order.
        let author = parse_stylesheet(".lead { color: green } p { color: blue }");
        let cs = computed_style(&doc, p, &ComputedStyle::initial(), &author);
        assert_eq!(cs.color, Color::rgb(0, 128, 0));
    }
}
