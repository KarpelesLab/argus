//! Style engine: the cascade (Phase 1).
//!
//! Computes element styles by cascading three origins — a built-in user-agent
//! stylesheet, author stylesheets (the page's `<style>` elements), and inline
//! `style` attributes — sorted by origin, `!important`, specificity, and source
//! order, on top of inherited values. Selector matching and value parsing come
//! from `argus-css`. We interpret the subset Phase 1 layout/paint use (display,
//! font, color/background, the box model, text-align). See
//! `docs/subsystems/style.md`.

use argus_css::{matches, parse_color, parse_declaration_block, parse_length, parse_stylesheet};
use argus_css::{Length, Specificity, Stylesheet};
use argus_dom::{Document, NodeData, NodeId};
use argus_geometry::Color;
use std::collections::HashMap;
use std::sync::OnceLock;

pub use argus_css::Stylesheet as AuthorStylesheet;

/// The `display` value, reduced to what layout understands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Display {
    Block,
    Inline,
    /// A flex container (display: flex); children lay out in a row.
    Flex,
    /// A grid container (display: grid); children flow into equal columns.
    Grid,
    None,
}

/// Inline-axis text alignment.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

/// `list-style-type` — the marker drawn beside a list item.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ListStyle {
    Disc,
    Circle,
    Square,
    Decimal,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    None,
}

/// `text-transform` — case mapping applied to rendered text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextTransform {
    None,
    Uppercase,
    Lowercase,
    Capitalize,
}

/// `box-sizing` — whether `width` measures the content box or the border box.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BoxSizing {
    ContentBox,
    BorderBox,
}

/// Four edge values (top/right/bottom/left) in CSS pixels.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Edges {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Edges {
    pub fn uniform(v: f32) -> Edges {
        Edges {
            top: v,
            right: v,
            bottom: v,
            left: v,
        }
    }
}

/// A computed style for one element. Lengths are in CSS pixels (except `width`,
/// resolved against the containing block during layout).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ComputedStyle {
    pub display: Display,
    pub font_size: f32,
    pub bold: bool,
    pub color: Color,
    pub background_color: Color,
    pub margin: Edges,
    pub padding: Edges,
    pub border: Edges,
    pub border_color: Color,
    /// Specified width, resolved during layout (`None` = auto).
    pub width: Option<Length>,
    pub text_align: TextAlign,
    /// `text-decoration: underline`.
    pub underline: bool,
    /// `text-decoration: line-through`.
    pub strike: bool,
    /// Column count for a grid container (from `grid-template-columns`).
    pub grid_columns: u32,
    /// Uniform `border-radius` in pixels.
    pub border_radius: f32,
    /// Element `opacity` in `0.0..=1.0`.
    pub opacity: f32,
    /// `white-space: pre*` — preserve whitespace and honor newlines (inherited).
    pub white_space_pre: bool,
    /// `list-style-type` for list items (inherited).
    pub list_style: ListStyle,
    /// `text-transform` case mapping (inherited).
    pub text_transform: TextTransform,
    /// `box-sizing` — how `width` maps to the box model (not inherited).
    pub box_sizing: BoxSizing,
}

impl ComputedStyle {
    /// The initial style for the root's containing block.
    pub fn initial() -> ComputedStyle {
        ComputedStyle {
            display: Display::Block,
            font_size: 16.0,
            bold: false,
            color: Color::BLACK,
            background_color: Color::TRANSPARENT,
            margin: Edges::default(),
            padding: Edges::default(),
            border: Edges::default(),
            border_color: Color::BLACK,
            width: None,
            text_align: TextAlign::Left,
            underline: false,
            strike: false,
            grid_columns: 1,
            border_radius: 0.0,
            opacity: 1.0,
            white_space_pre: false,
            list_style: ListStyle::Disc,
            text_transform: TextTransform::None,
            box_sizing: BoxSizing::ContentBox,
        }
    }

    /// Apply this element's opacity to `color`'s alpha channel.
    pub fn fade(&self, color: Color) -> Color {
        if self.opacity >= 1.0 {
            return color;
        }
        Color::rgba(
            color.r,
            color.g,
            color.b,
            (color.a as f32 * self.opacity.clamp(0.0, 1.0)) as u8,
        )
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
a { color: #0645ad; text-decoration: underline }
u, ins { text-decoration: underline }
s, del, strike { text-decoration: line-through }
ul, ol, blockquote, figure, pre { margin: 1em 0 }
pre { white-space: pre }
ul { list-style-type: disc }
ol { list-style-type: decimal }
ul, ol { padding-left: 40px }
blockquote { margin: 1em 40px }
hr { margin: 8px 0; border-top: 1px solid #c0c0c0 }
td, th { padding: 4px }
th { font-weight: bold; text-align: center }
";

fn ua_stylesheet() -> &'static Stylesheet {
    static UA: OnceLock<Stylesheet> = OnceLock::new();
    UA.get_or_init(|| parse_stylesheet(UA_CSS))
}

/// Map a `list-style-type` keyword to a [`ListStyle`] (ignoring unknown tokens).
fn parse_list_style(token: &str) -> Option<ListStyle> {
    Some(match token {
        "disc" => ListStyle::Disc,
        "circle" => ListStyle::Circle,
        "square" => ListStyle::Square,
        "decimal" => ListStyle::Decimal,
        "lower-alpha" | "lower-latin" => ListStyle::LowerAlpha,
        "upper-alpha" | "upper-latin" => ListStyle::UpperAlpha,
        "lower-roman" => ListStyle::LowerRoman,
        "none" => ListStyle::None,
        _ => return None,
    })
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

    cands.sort_by_key(|c| (c.rank, c.spec, c.order));
    let mut map: HashMap<String, String> = HashMap::new();
    for c in cands {
        map.insert(c.name, c.value);
    }

    let mut cs = ComputedStyle {
        display: Display::Inline,
        font_size: parent.font_size,
        bold: parent.bold,
        color: parent.color,
        text_align: parent.text_align,           // text-align inherits
        white_space_pre: parent.white_space_pre, // white-space inherits
        list_style: parent.list_style,           // list-style-type inherits
        text_transform: parent.text_transform,   // text-transform inherits
        ..ComputedStyle::initial()
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
            "flex" | "inline-flex" => Display::Flex,
            "grid" | "inline-grid" => Display::Grid,
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
    if let Some(v) = map.get("text-align") {
        cs.text_align = match v.as_str() {
            "center" => TextAlign::Center,
            "right" | "end" => TextAlign::Right,
            _ => TextAlign::Left,
        };
    }
    if let Some(v) = map
        .get("text-decoration")
        .or_else(|| map.get("text-decoration-line"))
    {
        cs.underline = v.split_whitespace().any(|t| t == "underline");
        cs.strike = v.split_whitespace().any(|t| t == "line-through");
    }
    if let Some(v) = map
        .get("list-style-type")
        .or_else(|| map.get("list-style"))
        .and_then(|v| v.split_whitespace().find_map(parse_list_style))
    {
        cs.list_style = v;
    }
    if let Some(v) = map.get("text-transform") {
        cs.text_transform = match v.as_str() {
            "uppercase" => TextTransform::Uppercase,
            "lowercase" => TextTransform::Lowercase,
            "capitalize" => TextTransform::Capitalize,
            _ => TextTransform::None,
        };
    }
    if let Some(v) = map.get("box-sizing") {
        cs.box_sizing = match v.as_str() {
            "border-box" => BoxSizing::BorderBox,
            _ => BoxSizing::ContentBox,
        };
    }

    let fs = cs.font_size;
    // Margins.
    if let Some(v) = map.get("margin") {
        cs.margin = edges_shorthand(v, fs);
    }
    side_edge(map, "margin", fs, &mut cs.margin);
    // Padding.
    if let Some(v) = map.get("padding") {
        cs.padding = edges_shorthand(v, fs);
    }
    side_edge(map, "padding", fs, &mut cs.padding);
    // Borders.
    if let Some(v) = map.get("border") {
        let (w, c) = parse_border(v, fs);
        cs.border = Edges::uniform(w);
        if let Some(c) = c {
            cs.border_color = c;
        }
    }
    if let Some(v) = map.get("border-width").and_then(|v| len_px(v, fs)) {
        cs.border = Edges::uniform(v);
    }
    if let Some(v) = map.get("border-color").and_then(|v| parse_color(v)) {
        cs.border_color = v;
    }
    if let Some(px) = map.get("border-top-width").and_then(|v| len_px(v, fs)) {
        cs.border.top = px;
    }
    if let Some(px) = map.get("border-right-width").and_then(|v| len_px(v, fs)) {
        cs.border.right = px;
    }
    if let Some(px) = map.get("border-bottom-width").and_then(|v| len_px(v, fs)) {
        cs.border.bottom = px;
    }
    if let Some(px) = map.get("border-left-width").and_then(|v| len_px(v, fs)) {
        cs.border.left = px;
    }
    // Width.
    if let Some(v) = map.get("width") {
        cs.width = if v == "auto" { None } else { parse_length(v) };
    }
    if let Some(v) = map.get("grid-template-columns") {
        cs.grid_columns = grid_track_count(v);
    }
    if let Some(px) = map
        .get("border-radius")
        .and_then(|v| len_px(v.split_whitespace().next().unwrap_or(v), fs))
    {
        cs.border_radius = px;
    }
    if let Some(o) = map
        .get("opacity")
        .and_then(|v| v.trim().parse::<f32>().ok())
    {
        cs.opacity = o.clamp(0.0, 1.0);
    }
    if let Some(ws) = map.get("white-space") {
        cs.white_space_pre = matches!(
            ws.as_str(),
            "pre" | "pre-wrap" | "pre-line" | "break-spaces"
        );
    }
}

/// Count the columns named by a `grid-template-columns` value (a simplified read
/// of `repeat(n, …)` and whitespace-separated track lists).
fn grid_track_count(v: &str) -> u32 {
    let v = v.trim();
    if let Some(rest) = v.strip_prefix("repeat(") {
        if let Some(n) = rest
            .split(',')
            .next()
            .and_then(|s| s.trim().parse::<u32>().ok())
        {
            return n.max(1);
        }
    }
    (v.split_whitespace().count() as u32).max(1)
}

/// Resolve per-side overrides like `margin-top`, `padding-left`.
fn side_edge(map: &HashMap<String, String>, prop: &str, fs: f32, edges: &mut Edges) {
    if let Some(px) = map.get(&format!("{prop}-top")).and_then(|v| len_px(v, fs)) {
        edges.top = px;
    }
    if let Some(px) = map
        .get(&format!("{prop}-right"))
        .and_then(|v| len_px(v, fs))
    {
        edges.right = px;
    }
    if let Some(px) = map
        .get(&format!("{prop}-bottom"))
        .and_then(|v| len_px(v, fs))
    {
        edges.bottom = px;
    }
    if let Some(px) = map.get(&format!("{prop}-left")).and_then(|v| len_px(v, fs)) {
        edges.left = px;
    }
}

fn len_px(v: &str, fs: f32) -> Option<f32> {
    parse_length(v).map(|l| l.to_px(fs, 0.0))
}

/// `top right bottom left` shorthand with 1–4 values.
fn edges_shorthand(v: &str, fs: f32) -> Edges {
    let vals: Vec<f32> = v
        .split_whitespace()
        .map(|t| len_px(t, fs).unwrap_or(0.0))
        .collect();
    match vals.len() {
        0 => Edges::default(),
        1 => Edges::uniform(vals[0]),
        2 => Edges {
            top: vals[0],
            bottom: vals[0],
            right: vals[1],
            left: vals[1],
        },
        3 => Edges {
            top: vals[0],
            right: vals[1],
            left: vals[1],
            bottom: vals[2],
        },
        _ => Edges {
            top: vals[0],
            right: vals[1],
            bottom: vals[2],
            left: vals[3],
        },
    }
}

/// Parse `border: <width> <style> <color>` (any order), returning width + color.
fn parse_border(v: &str, fs: f32) -> (f32, Option<Color>) {
    let mut width = 0.0;
    let mut color = None;
    for tok in v.split_whitespace() {
        if let Some(px) = len_px(tok, fs) {
            width = px;
        } else if let Some(c) = parse_color(tok) {
            color = Some(c);
        } else if tok == "none" || tok == "hidden" {
            width = 0.0;
        }
        // border-style keywords (solid/dashed/…) are ignored for now.
    }
    (width, color)
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

fn color_in(v: &str) -> Option<Color> {
    parse_color(v).or_else(|| v.split_whitespace().find_map(parse_color))
}

#[cfg(test)]
mod tests {
    use super::*;
    use argus_dom::{Attribute, QualName};

    fn one(doc: &mut Document, tag: &str, attrs: Vec<Attribute>) -> NodeId {
        let root = doc.root();
        let el = doc.create_element(QualName::html(tag), attrs);
        doc.append(root, el);
        el
    }

    #[test]
    fn ua_headings() {
        let mut doc = Document::new();
        let h1 = one(&mut doc, "h1", vec![]);
        let cs = computed_style(&doc, h1, &ComputedStyle::initial(), &Stylesheet::default());
        assert_eq!(cs.display, Display::Block);
        assert!(cs.bold);
        assert_eq!(cs.font_size, 32.0);
        assert!(cs.margin.top > 0.0);
    }

    #[test]
    fn cascade_color_background_inline() {
        let mut doc = Document::new();
        let p = one(
            &mut doc,
            "p",
            vec![
                Attribute::new("class", "lead"),
                Attribute::new("style", "color: red"),
            ],
        );
        let author =
            parse_stylesheet("p { color: blue; background-color: #eee } .lead { color: green }");
        let cs = computed_style(&doc, p, &ComputedStyle::initial(), &author);
        assert_eq!(cs.color, Color::rgb(255, 0, 0));
        assert_eq!(cs.background_color, Color::rgb(0xee, 0xee, 0xee));
    }

    #[test]
    fn box_model_properties() {
        let mut doc = Document::new();
        let d = one(&mut doc, "div", vec![]);
        let author = parse_stylesheet(
            "div { padding: 10px 20px; border: 2px solid #000; width: 50%; text-align: center }",
        );
        let cs = computed_style(&doc, d, &ComputedStyle::initial(), &author);
        assert_eq!(cs.padding.top, 10.0);
        assert_eq!(cs.padding.left, 20.0);
        assert_eq!(cs.border, Edges::uniform(2.0));
        assert_eq!(cs.text_align, TextAlign::Center);
        assert_eq!(cs.width, Some(Length::Percent(50.0)));
    }
}
