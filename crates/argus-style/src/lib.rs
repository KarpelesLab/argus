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
use argus_css::{Specificity, Stylesheet};
use argus_dom::{Document, NodeData, NodeId};
use argus_geometry::Color;
use std::collections::HashMap;
use std::sync::OnceLock;

pub use argus_css::Length;
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
    Justify,
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

/// `vertical-align` for inline content (the subset layout honors).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VerticalAlign {
    Baseline,
    Sub,
    Super,
}

/// `position` (the subset layout honors: static flow vs. a relative offset).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Position {
    Static,
    Relative,
}

/// `flex-direction` for a flex container (the subset layout honors: main axis
/// horizontal vs. vertical).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlexDirection {
    Row,
    Column,
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
    /// Both horizontal margins are `auto` — a block with a definite width centers
    /// itself in its containing block.
    pub margin_auto_lr: bool,
    pub padding: Edges,
    pub border: Edges,
    pub border_color: Color,
    /// Specified width, resolved during layout (`None` = auto).
    pub width: Option<Length>,
    /// `min-width` / `max-width`, resolved during layout (`None` = no constraint).
    pub min_width: Option<Length>,
    pub max_width: Option<Length>,
    /// Specified content `height` (`None` = auto, sized to content).
    pub height: Option<Length>,
    /// `min-height` — a block grows to at least this (resolved during layout).
    pub min_height: Option<Length>,
    /// `aspect-ratio` as width÷height; a definite-width block derives its height
    /// from it when `height` is auto (resolved during layout).
    pub aspect_ratio: Option<f32>,
    pub text_align: TextAlign,
    /// `text-decoration: underline`.
    pub underline: bool,
    /// `text-decoration: line-through`.
    pub strike: bool,
    /// Column count for a grid container (from `grid-template-columns`).
    pub grid_columns: u32,
    /// `flex-direction` for a `display: flex` container (not inherited).
    pub flex_direction: FlexDirection,
    /// Uniform `border-radius` in pixels.
    pub border_radius: f32,
    /// Element `opacity` in `0.0..=1.0`.
    pub opacity: f32,
    /// `white-space: pre*` — preserve whitespace and honor newlines (inherited).
    pub white_space_pre: bool,
    /// `white-space: nowrap`/`pre` — suppress automatic line wrapping (inherited).
    pub nowrap: bool,
    /// `list-style-type` for list items (inherited).
    pub list_style: ListStyle,
    /// `text-transform` case mapping (inherited).
    pub text_transform: TextTransform,
    /// `box-sizing` — how `width` maps to the box model (not inherited).
    pub box_sizing: BoxSizing,
    /// `line-height` as a multiple of `font-size` (inherited).
    pub line_height: f32,
    /// `text-indent` for the first line, in pixels (inherited).
    pub text_indent: f32,
    /// `word-spacing` extra pixels added between words (inherited).
    pub word_spacing: f32,
    /// `vertical-align` for inline content (not inherited).
    pub vertical_align: VerticalAlign,
    /// `gap` between flex/grid items in pixels (not inherited).
    pub gap: f32,
    /// `visibility: hidden` — the box keeps its space but paints nothing
    /// (inherited; a descendant may set `visibility: visible` to reappear).
    pub hidden: bool,
    /// `outline` — drawn just outside the border box; does not affect layout.
    pub outline_width: f32,
    pub outline_color: Color,
    /// `position` and its inset offsets (resolved during layout; not inherited).
    pub position: Position,
    pub inset_top: Option<Length>,
    pub inset_right: Option<Length>,
    pub inset_bottom: Option<Length>,
    pub inset_left: Option<Length>,
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
            margin_auto_lr: false,
            padding: Edges::default(),
            border: Edges::default(),
            border_color: Color::BLACK,
            width: None,
            min_width: None,
            max_width: None,
            height: None,
            min_height: None,
            aspect_ratio: None,
            text_align: TextAlign::Left,
            underline: false,
            strike: false,
            grid_columns: 1,
            flex_direction: FlexDirection::Row,
            border_radius: 0.0,
            opacity: 1.0,
            white_space_pre: false,
            nowrap: false,
            list_style: ListStyle::Disc,
            text_transform: TextTransform::None,
            box_sizing: BoxSizing::ContentBox,
            line_height: 1.2,
            text_indent: 0.0,
            word_spacing: 0.0,
            vertical_align: VerticalAlign::Baseline,
            gap: 0.0,
            hidden: false,
            outline_width: 0.0,
            outline_color: Color::TRANSPARENT,
            position: Position::Static,
            inset_top: None,
            inset_right: None,
            inset_bottom: None,
            inset_left: None,
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
main, aside, figure, blockquote, ul, ol, li, dl, dt, dd, pre, table, form, hr, address, \
details, summary \
{ display: block }
summary { font-weight: bold }
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
sub { vertical-align: sub; font-size: 0.75em }
sup { vertical-align: super; font-size: 0.75em }
small { font-size: 0.83em }
mark { background-color: #fef08a }
code, kbd, samp { background-color: #eef0f2 }
ul, ol, blockquote, figure, pre { margin: 1em 0 }
pre { white-space: pre }
ul { list-style-type: disc }
ol { list-style-type: decimal }
ul, ol { padding-left: 40px }
blockquote { margin: 1em 40px }
hr { margin: 8px 0; border-top: 1px solid #c0c0c0 }
td, th { padding: 4px }
th { font-weight: bold; text-align: center }
caption { display: block; text-align: center; margin: 4px 0 }
input, textarea, select { display: block; border: 1px solid #999; background: #fff; \
  padding: 4px 6px; width: 220px; margin: 4px 0; white-space: nowrap }
button { display: block; border: 1px solid #888; background: #e8e8e8; padding: 4px 12px; \
  width: 120px; text-align: center; margin: 4px 0; border-radius: 4px }
option { display: none }
input[type=checkbox], input[type=radio] { width: 14px; height: 14px; padding: 0; margin: 4px 6px 4px 0 }
input[type=radio] { border-radius: 8px }
input:disabled, textarea:disabled, select:disabled, button:disabled \
  { background: #eeeeee; color: #999999; border-color: #cccccc }
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
        nowrap: parent.nowrap,                   // white-space inherits
        list_style: parent.list_style,           // list-style-type inherits
        text_transform: parent.text_transform,   // text-transform inherits
        line_height: parent.line_height,         // line-height inherits
        text_indent: parent.text_indent,         // text-indent inherits
        word_spacing: parent.word_spacing,       // word-spacing inherits
        hidden: parent.hidden,                   // visibility inherits
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
        if let Some(c) = resolve_color(v, cs.color, parent.background_color) {
            cs.background_color = c;
        }
    }
    if let Some(v) = map.get("text-align") {
        cs.text_align = match v.as_str() {
            "center" => TextAlign::Center,
            "right" | "end" => TextAlign::Right,
            "justify" => TextAlign::Justify,
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
    if let Some(px) = map
        .get("gap")
        .or_else(|| map.get("column-gap"))
        .or_else(|| map.get("grid-gap"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(parse_length)
        .map(|l| l.to_px(cs.font_size, 0.0))
    {
        cs.gap = px.max(0.0);
    }
    if let Some(v) = map.get("vertical-align") {
        cs.vertical_align = match v.as_str() {
            "sub" => VerticalAlign::Sub,
            "super" => VerticalAlign::Super,
            _ => VerticalAlign::Baseline,
        };
    }
    if let Some(v) = map.get("visibility") {
        cs.hidden = matches!(v.as_str(), "hidden" | "collapse");
    }
    if let Some(v) = map.get("position") {
        // Only `relative` is honored; absolute/fixed/sticky fall back to static.
        cs.position = if v == "relative" {
            Position::Relative
        } else {
            Position::Static
        };
    }
    if let Some(v) = map
        .get("top")
        .filter(|v| v.as_str() != "auto")
        .and_then(|v| parse_length(v))
    {
        cs.inset_top = Some(v);
    }
    if let Some(v) = map
        .get("right")
        .filter(|v| v.as_str() != "auto")
        .and_then(|v| parse_length(v))
    {
        cs.inset_right = Some(v);
    }
    if let Some(v) = map
        .get("bottom")
        .filter(|v| v.as_str() != "auto")
        .and_then(|v| parse_length(v))
    {
        cs.inset_bottom = Some(v);
    }
    if let Some(v) = map
        .get("left")
        .filter(|v| v.as_str() != "auto")
        .and_then(|v| parse_length(v))
    {
        cs.inset_left = Some(v);
    }
    if let Some(px) = map
        .get("text-indent")
        .and_then(|v| parse_length(v))
        .map(|l| l.to_px(cs.font_size, 0.0))
    {
        cs.text_indent = px;
    }
    if let Some(v) = map.get("word-spacing").filter(|v| v.as_str() != "normal") {
        if let Some(px) = parse_length(v).map(|l| l.to_px(cs.font_size, 0.0)) {
            cs.word_spacing = px;
        }
    }
    if let Some(v) = map.get("line-height") {
        let v = v.trim();
        if v == "normal" {
            cs.line_height = 1.2;
        } else if let Ok(n) = v.parse::<f32>() {
            // Unitless: a direct multiple of font-size.
            cs.line_height = n.max(0.0);
        } else if let Some(px) = parse_length(v).map(|l| l.to_px(cs.font_size, cs.font_size)) {
            // Length: store as a multiple of this element's font-size.
            if cs.font_size > 0.0 {
                cs.line_height = (px / cs.font_size).max(0.0);
            }
        }
    }

    let fs = cs.font_size;
    // Margins.
    if let Some(v) = map.get("margin") {
        cs.margin = edges_shorthand(v, fs);
    }
    side_edge(map, "margin", fs, &mut cs.margin);
    // Detect `auto` left+right margins (block centering). The horizontal component
    // of the `margin` shorthand is its 2nd token (1/2/3 values) or {2nd, 4th} for 4;
    // explicit `margin-left`/`margin-right` longhands override.
    {
        let (mut left_auto, mut right_auto) = (false, false);
        if let Some(v) = map.get("margin") {
            let t: Vec<&str> = v.split_whitespace().collect();
            let (l, r) = match t.len() {
                1 => (t[0], t[0]),
                2 | 3 => (t[1], t[1]),
                n if n >= 4 => (t[3], t[1]),
                _ => ("", ""),
            };
            left_auto = l.eq_ignore_ascii_case("auto");
            right_auto = r.eq_ignore_ascii_case("auto");
        }
        if let Some(v) = map.get("margin-left") {
            left_auto = v.trim().eq_ignore_ascii_case("auto");
        }
        if let Some(v) = map.get("margin-right") {
            right_auto = v.trim().eq_ignore_ascii_case("auto");
        }
        cs.margin_auto_lr = left_auto && right_auto;
    }
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
        } else if mentions_current_color(v) {
            cs.border_color = cs.color;
        }
    }
    if let Some(v) = map.get("border-width").and_then(|v| len_px(v, fs)) {
        cs.border = Edges::uniform(v);
    }
    if let Some(v) = map
        .get("border-color")
        .and_then(|v| resolve_color(v, cs.color, parent.border_color))
    {
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
    // Outline (drawn outside the border box; reuses the border shorthand parser).
    if let Some(v) = map.get("outline") {
        let (w, c) = parse_border(v, fs);
        cs.outline_width = w;
        if let Some(c) = c {
            cs.outline_color = c;
        } else if mentions_current_color(v) {
            cs.outline_color = cs.color;
        }
    }
    if let Some(v) = map.get("outline-width").and_then(|v| len_px(v, fs)) {
        cs.outline_width = v;
    }
    if let Some(v) = map
        .get("outline-color")
        .and_then(|v| resolve_color(v, cs.color, parent.outline_color))
    {
        cs.outline_color = v;
    }
    // Width.
    if let Some(v) = map.get("width") {
        cs.width = if v == "auto" { None } else { parse_length(v) };
    }
    if let Some(v) = map.get("min-width") {
        cs.min_width = if v == "auto" || v == "0" {
            None
        } else {
            parse_length(v)
        };
    }
    if let Some(v) = map.get("max-width") {
        cs.max_width = if v == "none" { None } else { parse_length(v) };
    }
    if let Some(v) = map.get("height") {
        cs.height = if v == "auto" { None } else { parse_length(v) };
    }
    if let Some(v) = map.get("min-height") {
        cs.min_height = if v == "auto" || v == "0" {
            None
        } else {
            parse_length(v)
        };
    }
    if let Some(v) = map.get("aspect-ratio") {
        cs.aspect_ratio = parse_aspect_ratio(v);
    }
    if let Some(v) = map.get("grid-template-columns") {
        cs.grid_columns = grid_track_count(v);
    }
    if let Some(v) = map
        .get("flex-direction")
        .or_else(|| map.get("flex-flow"))
    {
        // `column`/`column-reverse` → vertical main axis; otherwise row.
        let first = v.split_whitespace().next().unwrap_or("");
        cs.flex_direction = if first.starts_with("column") {
            FlexDirection::Column
        } else {
            FlexDirection::Row
        };
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
        // `nowrap` and `pre` both suppress automatic wrapping.
        cs.nowrap = matches!(ws.as_str(), "nowrap" | "pre");
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

/// Parse an `aspect-ratio` value to width÷height. Accepts `W / H`, `W/H`, or a bare
/// ratio `R`. `auto` (or a non-positive result) yields `None`.
fn parse_aspect_ratio(v: &str) -> Option<f32> {
    let v = v.trim();
    if v.eq_ignore_ascii_case("auto") {
        return None;
    }
    let (w, h) = match v.split_once('/') {
        Some((a, b)) => (a.trim().parse::<f32>().ok()?, b.trim().parse::<f32>().ok()?),
        None => (v.parse::<f32>().ok()?, 1.0),
    };
    (w > 0.0 && h > 0.0).then_some(w / h)
}

/// Whether a value contains the `currentColor` keyword (case-insensitive).
fn mentions_current_color(v: &str) -> bool {
    v.split_whitespace().any(|t| t.eq_ignore_ascii_case("currentcolor"))
}

/// Resolve a color value, mapping `currentColor` to `cur` (the element's computed
/// `color`) and the `inherit` keyword to `inherited` (the parent's value for this
/// property). Used for background/border/outline colors.
fn resolve_color(v: &str, cur: Color, inherited: Color) -> Option<Color> {
    if v.trim().eq_ignore_ascii_case("inherit") {
        return Some(inherited);
    }
    if mentions_current_color(v) {
        return Some(cur);
    }
    color_in(v)
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
    fn inherit_keyword_for_non_inherited_colors() {
        // background/border colors don't inherit by default; `inherit` opts in.
        let mut doc = Document::new();
        let el = one(&mut doc, "div", vec![]);
        let mut parent = ComputedStyle::initial();
        parent.background_color = Color::rgb(1, 2, 3);
        parent.border_color = Color::rgb(4, 5, 6);
        let author =
            parse_stylesheet("div { background-color: inherit; border-color: inherit }");
        let cs = computed_style(&doc, el, &parent, &author);
        assert_eq!(cs.background_color, Color::rgb(1, 2, 3));
        assert_eq!(cs.border_color, Color::rgb(4, 5, 6));
    }

    #[test]
    fn current_color_resolves_to_color() {
        // `currentColor` on border/background/outline resolves to the element's
        // computed `color`.
        let mut doc = Document::new();
        let el = one(&mut doc, "div", vec![]);
        let author = parse_stylesheet(
            "div { color: rgb(10, 20, 30); \
                   border: 2px solid currentColor; \
                   background-color: currentColor; \
                   outline-color: currentColor }",
        );
        let cs = computed_style(&doc, el, &ComputedStyle::initial(), &author);
        let c = Color::rgb(10, 20, 30);
        assert_eq!(cs.color, c);
        assert_eq!(cs.border_color, c, "border-color: currentColor");
        assert_eq!(cs.background_color, c, "background-color: currentColor");
        assert_eq!(cs.outline_color, c, "outline-color: currentColor");
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
