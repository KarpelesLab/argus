//! Style engine: the cascade (Phase 1).
//!
//! Computes element styles by cascading three origins — a built-in user-agent
//! stylesheet, author stylesheets (the page's `<style>` elements), and inline
//! `style` attributes — sorted by origin, `!important`, specificity, and source
//! order, on top of inherited values. Selector matching and value parsing come
//! from `argus-css`. We interpret the subset Phase 1 layout/paint use (display,
//! font, color/background, the box model, text-align). See
//! `docs/subsystems/style.md`.

use argus_css::{matches, parse_declaration_block, parse_length, parse_stylesheet};
use argus_css::{Specificity, Stylesheet};
use argus_dom::{Document, NodeData, NodeId};
use argus_geometry::Color;
use std::collections::HashMap;
use std::sync::OnceLock;

pub use argus_css::parse_color;
pub use argus_css::Length;
pub use argus_css::PseudoElement;
pub use argus_css::Stylesheet as AuthorStylesheet;

/// The `display` value, reduced to what layout understands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Display {
    Block,
    Inline,
    /// `display: inline-block` — an atomic box placed in the inline flow.
    InlineBlock,
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
    /// `decimal-leading-zero` — `01`, `02`, … `09`, `10`.
    DecimalLeadingZero,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
    /// `lower-greek` — α, β, γ, … ω (skipping final sigma).
    LowerGreek,
    None,
}

/// `object-fit` — how a replaced element's image fills its content box.
/// `Fill` (the default) stretches to the box; `Contain` fits inside preserving
/// aspect (letterboxed); `Cover` fills and crops the overflow.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ObjectFit {
    #[default]
    Fill,
    Contain,
    Cover,
}

/// `text-decoration-style` — how underline/line-through/overline lines are drawn.
/// `Wavy` has no curve primitive available, so it renders like `Solid`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DecorationStyle {
    #[default]
    Solid,
    Double,
    Dotted,
    Dashed,
    Wavy,
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
    Top,
    Middle,
    Bottom,
}

/// `position` (the subset layout honors: static flow, a relative offset, or
/// out-of-flow absolute/fixed positioning).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
}

/// `flex-direction` for a flex container (the subset layout honors: main axis
/// horizontal vs. vertical).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlexDirection {
    Row,
    Column,
}

/// Direction of a (axis-aligned) linear gradient.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GradientDir {
    ToRight,
    ToLeft,
    ToBottom,
    ToTop,
}

/// The most color stops we track per gradient (keeps [`Gradient`] `Copy`).
pub const GRAD_MAX_STOPS: usize = 8;

/// A gradient background — axis-aligned `linear-gradient` (using `dir`), or a
/// `radial-gradient` (`radial = true`; `from` is the center, `to` the edge).
///
/// `stops[..n_stops]` holds `(color, position)` pairs with positions in `0.0..=1.0`
/// along the gradient axis; `from`/`to` mirror the first/last stop for the radial
/// path and simple two-stop consumers.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Gradient {
    pub dir: GradientDir,
    pub from: Color,
    pub to: Color,
    pub radial: bool,
    pub stops: [(Color, f32); GRAD_MAX_STOPS],
    pub n_stops: u8,
}

impl Gradient {
    /// Color at fraction `t` (`0.0..=1.0`) along the axis, interpolating between the
    /// two bracketing stops. Falls back to `from`→`to` when fewer than two stops.
    pub fn color_at(&self, t: f32) -> Color {
        let n = self.n_stops as usize;
        if n < 2 {
            return lerp_color(self.from, self.to, t.clamp(0.0, 1.0));
        }
        let stops = &self.stops[..n];
        if t <= stops[0].1 {
            return stops[0].0;
        }
        if t >= stops[n - 1].1 {
            return stops[n - 1].0;
        }
        for w in stops.windows(2) {
            let (c0, p0) = w[0];
            let (c1, p1) = w[1];
            if t <= p1 {
                let span = (p1 - p0).max(f32::EPSILON);
                return lerp_color(c0, c1, (t - p0) / span);
            }
        }
        stops[n - 1].0
    }
}

/// Linear interpolation between two colors at fraction `t` (`0.0..=1.0`).
pub fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color {
        r: l(a.r, b.r),
        g: l(a.g, b.g),
        b: l(a.b, b.b),
        a: l(a.a, b.a),
    }
}

/// `float` — take a box out of flow to the left/right, with content flowing past.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Float {
    None,
    Left,
    Right,
}

/// `clear` — push a block below preceding floats on the given side(s).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Clear {
    None,
    Left,
    Right,
    Both,
}

/// The most grid columns we track per container (keeps [`ComputedStyle`] `Copy`).
pub const GRID_MAX_TRACKS: usize = 16;

/// A single `grid-template-columns` track size.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum GridTrack {
    /// A flexible `<n>fr` track that shares leftover space by its factor.
    Fr(f32),
    /// A fixed length (`px`, `em`, `%`, …), resolved against the container width.
    Len(Length),
    /// `auto` / content-sized — treated as `1fr` by the layout subset.
    Auto,
}

/// `justify-content` — main-axis distribution of free space in a flex container.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JustifyContent {
    FlexStart,
    FlexEnd,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

/// `align-items` — cross-axis placement of items within the flex line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AlignItems {
    Stretch,
    FlexStart,
    FlexEnd,
    Center,
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
    /// `font-style: italic`/`oblique` (faux-slanted at render time).
    pub italic: bool,
    pub color: Color,
    pub background_color: Color,
    pub margin: Edges,
    /// Both horizontal margins are `auto` — a block with a definite width centers
    /// itself in its containing block.
    pub margin_auto_lr: bool,
    pub padding: Edges,
    pub border: Edges,
    pub border_color: Color,
    /// Per-side border colors (each defaults to `border_color`).
    pub border_top_color: Color,
    pub border_right_color: Color,
    pub border_bottom_color: Color,
    pub border_left_color: Color,
    /// `border-style` — solid (default) / double / dotted / dashed. Non-solid
    /// borders are painted (uniformly) by the frame painter.
    pub border_style: DecorationStyle,
    /// Specified width, resolved during layout (`None` = auto).
    pub width: Option<Length>,
    /// `min-width` / `max-width`, resolved during layout (`None` = no constraint).
    pub min_width: Option<Length>,
    pub max_width: Option<Length>,
    /// Specified content `height` (`None` = auto, sized to content).
    pub height: Option<Length>,
    /// `min-height` — a block grows to at least this (resolved during layout).
    pub min_height: Option<Length>,
    /// `max-height` — caps an explicit/aspect height (never below actual content,
    /// since we don't clip overflow). Resolved during layout.
    pub max_height: Option<Length>,
    /// `aspect-ratio` as width÷height; a definite-width block derives its height
    /// from it when `height` is auto (resolved during layout).
    pub aspect_ratio: Option<f32>,
    pub text_align: TextAlign,
    /// `text-decoration: underline`.
    pub underline: bool,
    /// `text-decoration: line-through`.
    pub strike: bool,
    /// `text-decoration: overline`.
    pub overline: bool,
    /// `text-decoration-color` — color of the decoration lines (`None` = text color).
    pub decoration_color: Option<Color>,
    /// `text-decoration-style` — how the decoration lines are drawn.
    pub decoration_style: DecorationStyle,
    /// `accent-color` — tint for form controls (checkbox/radio/progress); inherited.
    pub accent_color: Option<Color>,
    /// `text-shadow` as `(offset-x, offset-y, color)` in px (blur ignored); inherited.
    pub text_shadow: Option<(f32, f32, Color)>,
    /// `box-shadow` as `(offset-x, offset-y, blur, spread, color)` in px (outer
    /// only; blur faux-rendered as fading layers); not inherited.
    pub box_shadow: Option<(f32, f32, f32, f32, Color)>,
    /// A two-stop `linear-gradient` background (painted as stepped strips).
    pub background_gradient: Option<Gradient>,
    /// Column count for a grid container (from `grid-template-columns`).
    pub grid_columns: u32,
    /// Per-column track sizes (parallel to `grid_columns`, capped at
    /// [`GRID_MAX_TRACKS`]). Unspecified tracks are [`GridTrack::Auto`].
    pub grid_tracks: [GridTrack; GRID_MAX_TRACKS],
    /// Explicit row count from `grid-template-rows` (0 = rows sized by content).
    pub grid_rows: u32,
    /// Per-row track sizes from `grid-template-rows` (parallel to `grid_rows`).
    pub grid_row_tracks: [GridTrack; GRID_MAX_TRACKS],
    /// Number of columns a grid *item* spans (`grid-column: span N`); 1 by default.
    pub grid_column_span: u32,
    /// Explicit 1-based starting column line for a grid item (`grid-column: 2 / 4`
    /// or `grid-column-start: 2`); `None` = auto-placed.
    pub grid_column_start: Option<u32>,
    /// Number of rows a grid *item* spans (`grid-row: span N`); 1 by default.
    pub grid_row_span: u32,
    /// Explicit 1-based starting row line for a grid item (`grid-row: 2 / 4` or
    /// `grid-row-start: 2`); `None` = auto-placed.
    pub grid_row_start: Option<u32>,
    /// `float` (not inherited) — out-of-flow left/right with content flowing past.
    pub float: Float,
    /// `clear` (not inherited) — push below preceding floats on the given side(s).
    pub clear: Clear,
    /// `flex-direction` for a `display: flex` container (not inherited).
    pub flex_direction: FlexDirection,
    /// `justify-content` — main-axis free-space distribution (flex container).
    pub justify_content: JustifyContent,
    /// `align-items` — cross-axis item placement (flex container).
    pub align_items: AlignItems,
    /// `align-self` — a flex item's own cross-axis alignment, overriding the
    /// container's `align-items` (`None` = `auto`, i.e. use the container's).
    pub align_self: Option<AlignItems>,
    /// `flex-grow` factor for a flex item (0 = does not grow).
    pub flex_grow: f32,
    /// `flex-shrink` factor for a flex item (default 1; 0 = does not shrink).
    pub flex_shrink: f32,
    /// `flex-basis` — an item's base main size before grow/shrink (`None` = auto,
    /// i.e. content/`width`).
    pub flex_basis: Option<Length>,
    /// `order` for a flex item — visual ordering; lower comes first (default 0).
    pub order: i32,
    /// `flex-wrap: wrap` — allow flex items to break onto multiple lines.
    pub flex_wrap: bool,
    /// Uniform `border-radius` in pixels.
    pub border_radius: f32,
    /// Element `opacity` in `0.0..=1.0`.
    pub opacity: f32,
    /// `white-space: pre*` — preserve whitespace and honor newlines (inherited).
    pub white_space_pre: bool,
    /// `white-space: nowrap`/`pre` — suppress automatic line wrapping (inherited).
    pub nowrap: bool,
    /// `white-space: pre-line` — collapse spaces but keep newlines, and wrap
    /// (inherited). Distinguishes pre-line from `pre`/`pre-wrap` in the pre path.
    pub pre_line: bool,
    /// `tab-size` — spaces a tab expands to in preformatted text (inherited).
    pub tab_size: u32,
    /// `white-space: pre-wrap` — preserve whitespace and newlines, but wrap long
    /// lines (inherited).
    pub pre_wrap: bool,
    /// `overflow-wrap`/`word-break: break-word` — split words too long to fit
    /// rather than letting them overflow the line (inherited).
    pub break_word: bool,
    /// `text-overflow: ellipsis` — truncate an overflowing single (`nowrap`) line
    /// with `…` (not inherited).
    pub ellipsis: bool,
    /// `transform: translate(x, y)` — paints the subtree shifted by `(x, y)` with no
    /// effect on layout. `%` resolves against the element's own box (not inherited).
    pub transform_translate: Option<(Length, Length)>,
    /// `transform: scale(x, y)` — paints the subtree scaled about its center, with
    /// no effect on layout (not inherited).
    pub transform_scale: Option<(f32, f32)>,
    /// `list-style-type` for list items (inherited).
    pub list_style: ListStyle,
    /// `list-style-position: inside` — the marker is inline content (inherited).
    pub list_style_inside: bool,
    /// `text-transform` case mapping (inherited).
    pub text_transform: TextTransform,
    /// `box-sizing` — how `width` maps to the box model (not inherited).
    pub box_sizing: BoxSizing,
    /// `caption-side: bottom` — render a table `<caption>` below the rows.
    pub caption_side_bottom: bool,
    /// `object-fit` — how a replaced element's image is fitted into its box.
    pub object_fit: ObjectFit,
    /// `line-height` as a multiple of `font-size` (inherited).
    pub line_height: f32,
    /// `text-indent` for the first line, in pixels (inherited).
    pub text_indent: f32,
    /// `word-spacing` extra pixels added between words (inherited).
    pub word_spacing: f32,
    /// `letter-spacing` extra pixels added after each character (inherited).
    pub letter_spacing: f32,
    /// `border-spacing` (or the `cellspacing` attr) between table cells, in pixels
    /// (inherited; the table layout reads it). Defaults to 0.
    pub border_spacing: f32,
    /// `border-collapse: collapse` — share adjacent table-cell borders into one
    /// (inherited; the table layout reads it). Defaults to separated borders.
    pub border_collapse: bool,
    /// `table-layout: fixed` — size columns from `<col>`/explicit widths (and
    /// equal shares), ignoring cell content (inherited; the table layout reads it).
    pub table_layout_fixed: bool,
    /// `vertical-align` for inline content (not inherited).
    pub vertical_align: VerticalAlign,
    /// Column `gap` between flex/grid items in pixels (not inherited).
    pub gap: f32,
    /// Row `gap` between flex/grid rows (and wrapped flex lines) in pixels.
    pub row_gap: f32,
    /// `visibility: hidden` — the box keeps its space but paints nothing
    /// (inherited; a descendant may set `visibility: visible` to reappear).
    pub hidden: bool,
    /// `outline` — drawn just outside the border box; does not affect layout.
    pub outline_width: f32,
    pub outline_color: Color,
    /// `outline-offset` — gap between the border box and the outline (px).
    pub outline_offset: f32,
    /// `outline-style` — solid/double/dotted/dashed (shares the decoration painter).
    pub outline_style: DecorationStyle,
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
            italic: false,
            color: Color::BLACK,
            background_color: Color::TRANSPARENT,
            margin: Edges::default(),
            margin_auto_lr: false,
            padding: Edges::default(),
            border: Edges::default(),
            border_color: Color::BLACK,
            border_top_color: Color::BLACK,
            border_right_color: Color::BLACK,
            border_bottom_color: Color::BLACK,
            border_left_color: Color::BLACK,
            border_style: DecorationStyle::Solid,
            width: None,
            min_width: None,
            max_width: None,
            height: None,
            min_height: None,
            max_height: None,
            aspect_ratio: None,
            text_align: TextAlign::Left,
            underline: false,
            strike: false,
            overline: false,
            decoration_color: None,
            decoration_style: DecorationStyle::Solid,
            accent_color: None,
            text_shadow: None,
            box_shadow: None,
            background_gradient: None,
            grid_columns: 1,
            grid_tracks: [GridTrack::Auto; GRID_MAX_TRACKS],
            grid_rows: 0,
            grid_row_tracks: [GridTrack::Auto; GRID_MAX_TRACKS],
            grid_column_span: 1,
            grid_column_start: None,
            grid_row_span: 1,
            grid_row_start: None,
            float: Float::None,
            clear: Clear::None,
            flex_direction: FlexDirection::Row,
            justify_content: JustifyContent::FlexStart,
            align_items: AlignItems::Stretch,
            align_self: None,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: None,
            order: 0,
            flex_wrap: false,
            border_radius: 0.0,
            opacity: 1.0,
            white_space_pre: false,
            nowrap: false,
            pre_line: false,
            tab_size: 8,
            pre_wrap: false,
            break_word: false,
            ellipsis: false,
            transform_translate: None,
            transform_scale: None,
            list_style: ListStyle::Disc,
            list_style_inside: false,
            text_transform: TextTransform::None,
            box_sizing: BoxSizing::ContentBox,
            caption_side_bottom: false,
            object_fit: ObjectFit::Fill,
            line_height: 1.2,
            text_indent: 0.0,
            word_spacing: 0.0,
            letter_spacing: 0.0,
            border_spacing: 0.0,
            border_collapse: false,
            table_layout_fixed: false,
            vertical_align: VerticalAlign::Baseline,
            gap: 0.0,
            row_gap: 0.0,
            hidden: false,
            outline_width: 0.0,
            outline_offset: 0.0,
            outline_color: Color::TRANSPARENT,
            outline_style: DecorationStyle::Solid,
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
center { display: block; text-align: center }
head, title, style, script, meta, link, base, noscript, template { display: none }
dialog:not([open]) { display: none }
datalist { display: none }
h1 { font-size: 2em; font-weight: bold; margin: 0.67em 0 }
h2 { font-size: 1.5em; font-weight: bold; margin: 0.83em 0 }
h3 { font-size: 1.17em; font-weight: bold; margin: 1em 0 }
h4 { font-weight: bold; margin: 1.33em 0 }
h5 { font-size: 0.83em; font-weight: bold; margin: 1.67em 0 }
h6 { font-size: 0.67em; font-weight: bold; margin: 2.33em 0 }
p { margin: 1em 0 }
b, strong { font-weight: bold }
i, em, cite, var, dfn, address { font-style: italic }
q::before { content: open-quote }
q::after { content: close-quote }
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
figure { margin: 1em 40px }
dd { margin-left: 40px }
fieldset { display: block; border: 1px solid #a0a0a0; padding: 8px 10px; margin: 0 2px }
legend { display: block; font-weight: bold; padding: 0 4px }
hr { margin: 8px 0; border-top: 1px solid #c0c0c0 }
td, th { padding: 4px }
th { font-weight: bold; text-align: center }
caption { display: block; text-align: center; margin: 4px 0 }
input, textarea, select { display: block; border: 1px solid #999; background: #fff; \
  padding: 4px 6px; width: 220px; margin: 4px 0; white-space: nowrap }
button { display: block; border: 1px solid #888; background: #e8e8e8; padding: 4px 12px; \
  width: 120px; text-align: center; margin: 4px 0; border-radius: 4px }
option { display: none }
input[type=hidden] { display: none }
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
        "decimal-leading-zero" => ListStyle::DecimalLeadingZero,
        "lower-greek" => ListStyle::LowerGreek,
        "lower-alpha" | "lower-latin" => ListStyle::LowerAlpha,
        "upper-alpha" | "upper-latin" => ListStyle::UpperAlpha,
        "lower-roman" => ListStyle::LowerRoman,
        "upper-roman" => ListStyle::UpperRoman,
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
/// Map an element's legacy presentational HTML attributes to equivalent CSS
/// declarations (HTML "presentational hints"). Covers the common ones:
/// `align` (→ `text-align`, or `float` on `<img>`), `bgcolor`/`<body text>`
/// (→ colors), and `width`/`height` on tables/cells/images.
fn presentational_hints(doc: &Document, node: NodeId) -> Vec<(String, String)> {
    let Some(e) = doc.node(node).as_element() else {
        return Vec::new();
    };
    let tag: &str = &e.name.local;
    let mut out: Vec<(String, String)> = Vec::new();

    // A legacy color value: accept CSS colors and bare 3/6-hex digits (`ff0000`).
    fn legacy_color(v: &str) -> Option<String> {
        let v = v.trim();
        if parse_color(v).is_some() {
            Some(v.to_string())
        } else if (v.len() == 3 || v.len() == 6) && v.bytes().all(|b| b.is_ascii_hexdigit()) {
            Some(format!("#{v}"))
        } else {
            None
        }
    }

    if let Some(a) = e.attr("align") {
        let a = a.trim().to_ascii_lowercase();
        if tag == "img" && matches!(a.as_str(), "left" | "right") {
            // `<img align=left|right>` floats the image.
            out.push(("float".into(), a));
        } else if matches!(a.as_str(), "left" | "right" | "center" | "justify") {
            out.push(("text-align".into(), a));
        }
    }
    if let Some(bg) = e.attr("bgcolor").and_then(legacy_color) {
        out.push(("background-color".into(), bg));
    }
    // Legacy `<ol type=1|A|a|I|i>` / `<ul type=disc|circle|square>` list markers.
    if matches!(tag, "ol" | "ul" | "li") {
        if let Some(t) = e.attr("type") {
            let lst = match t.trim() {
                "1" => Some("decimal"),
                "A" => Some("upper-alpha"),
                "a" => Some("lower-alpha"),
                "I" => Some("upper-roman"),
                "i" => Some("lower-roman"),
                "disc" => Some("disc"),
                "circle" => Some("circle"),
                "square" => Some("square"),
                _ => None,
            };
            if let Some(lst) = lst {
                out.push(("list-style-type".into(), lst.into()));
            }
        }
    }
    if tag == "body" {
        if let Some(c) = e.attr("text").and_then(legacy_color) {
            out.push(("color".into(), c));
        }
    }
    if tag == "font" {
        if let Some(c) = e.attr("color").and_then(legacy_color) {
            out.push(("color".into(), c));
        }
        // `<font size=1..7>` maps to the legacy absolute font-size scale.
        if let Some(px) = e.attr("size").and_then(|s| match s.trim() {
            "1" => Some("10px"),
            "2" => Some("13px"),
            "3" => Some("16px"),
            "4" => Some("18px"),
            "5" => Some("24px"),
            "6" => Some("32px"),
            "7" => Some("48px"),
            _ => None,
        }) {
            out.push(("font-size".into(), px.into()));
        }
    }
    // `<table border=N>` (N>0) draws an N-px table border and a 1px border on
    // every cell; `border=0` (and no attr) draws nothing.
    if tag == "table" {
        if let Some(n) = e.attr("border").and_then(|v| v.trim().parse::<f32>().ok()) {
            if n > 0.0 {
                out.push(("border".into(), format!("{n}px solid #808080")));
            }
        }
        if let Some(n) = e.attr("cellspacing").and_then(|v| v.trim().parse::<f32>().ok()) {
            out.push(("border-spacing".into(), format!("{n}px")));
        }
    }
    // The legacy `<td nowrap>` boolean attribute prevents the cell from wrapping.
    if matches!(tag, "td" | "th") && e.attr("nowrap").is_some() {
        out.push(("white-space".into(), "nowrap".into()));
    }
    // `<caption align=top|bottom>` maps to `caption-side`.
    if tag == "caption" {
        if let Some(a) = e.attr("align") {
            if matches!(a.trim().to_ascii_lowercase().as_str(), "top" | "bottom") {
                out.push(("caption-side".into(), a.trim().to_ascii_lowercase()));
            }
        }
    }
    // A cell's `valign` (or its row's, since vertical-align doesn't inherit) maps
    // to `vertical-align`; cells honor top/middle/bottom.
    if matches!(tag, "td" | "th") {
        let row_valign = doc
            .node(node)
            .parent()
            .and_then(|p| doc.node(p).as_element())
            .filter(|pe| pe.name.is_html("tr"))
            .and_then(|pe| pe.attr("valign"));
        if let Some(v) = e.attr("valign").or(row_valign) {
            let v = v.trim().to_ascii_lowercase();
            if matches!(v.as_str(), "top" | "middle" | "bottom") {
                out.push(("vertical-align".into(), v));
            }
        }
    }
    if matches!(tag, "td" | "th") {
        let mut p = doc.node(node).parent();
        while let Some(pid) = p {
            if let Some(pe) = doc.node(pid).as_element() {
                if pe.name.is_html("table") {
                    if pe
                        .attr("border")
                        .and_then(|v| v.trim().parse::<f32>().ok())
                        .is_some_and(|n| n > 0.0)
                    {
                        out.push(("border".into(), "1px solid #b0b0b0".into()));
                    }
                    // `<table cellpadding=N>` sets every cell's padding.
                    if let Some(n) = pe.attr("cellpadding").and_then(|v| v.trim().parse::<f32>().ok())
                    {
                        out.push(("padding".into(), format!("{n}px")));
                    }
                    break;
                }
            }
            p = doc.node(pid).parent();
        }
    }
    // `width`/`height` attributes on tables and cells map to CSS lengths (a bare
    // number is px; a trailing `%` a percentage). Images are sized in layout.
    if matches!(tag, "table" | "td" | "th" | "col" | "colgroup") {
        for prop in ["width", "height"] {
            if let Some(v) = e.attr(prop) {
                let v = v.trim();
                if v.ends_with('%') && v[..v.len() - 1].parse::<f32>().is_ok() {
                    out.push((prop.into(), v.to_string()));
                } else if v.parse::<f32>().is_ok() {
                    out.push((prop.into(), format!("{v}px")));
                }
            }
        }
    }
    // `<hr>` legacy attributes: `size` (rule thickness), `width` (px or %), and
    // `color` (rule color → the top border that draws the line).
    if tag == "hr" {
        if let Some(s) = e.attr("size").and_then(|v| v.trim().parse::<f32>().ok()) {
            if s > 0.0 {
                out.push(("border-top-width".into(), format!("{s}px")));
            }
        }
        if let Some(v) = e.attr("width") {
            let v = v.trim();
            if v.ends_with('%') && v[..v.len() - 1].parse::<f32>().is_ok() {
                out.push(("width".into(), v.to_string()));
            } else if v.parse::<f32>().is_ok() {
                out.push(("width".into(), format!("{v}px")));
            }
        }
        if let Some(c) = e.attr("color").and_then(legacy_color) {
            out.push(("border-color".into(), c));
        }
    }
    out
}

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
    // Legacy presentational attributes (`align`, `bgcolor`, …) map to CSS at the
    // very bottom of the *author* origin: they beat UA rules (e.g. `td { padding }`)
    // but, being specificity-0 and ordered before the real author rules, lose to
    // any author or inline declaration.
    for (name, value) in presentational_hints(doc, node) {
        cands.push(Cand {
            rank: rank(Origin::Author, false),
            spec: Specificity::default(),
            order,
            name,
            value,
        });
        order += 1;
    }

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
        italic: parent.italic,
        color: parent.color,
        text_align: parent.text_align,           // text-align inherits
        white_space_pre: parent.white_space_pre, // white-space inherits
        nowrap: parent.nowrap,                   // white-space inherits
        pre_line: parent.pre_line,               // white-space inherits
        tab_size: parent.tab_size,               // tab-size inherits
        pre_wrap: parent.pre_wrap,               // white-space inherits
        break_word: parent.break_word,           // overflow-wrap inherits
        accent_color: parent.accent_color,       // accent-color inherits
        caption_side_bottom: parent.caption_side_bottom, // caption-side inherits
        text_shadow: parent.text_shadow,         // text-shadow inherits
        list_style: parent.list_style,           // list-style-type inherits
        list_style_inside: parent.list_style_inside, // list-style-position inherits
        // text-decoration isn't an inherited property, but it *propagates* to
        // descendant inline boxes; modeling it as inherited (with a child's own
        // `text-decoration` overriding below) gives the right visible result, e.g.
        // a link's nested `<span>` stays underlined.
        underline: parent.underline,
        strike: parent.strike,
        overline: parent.overline,
        decoration_color: parent.decoration_color,
        decoration_style: parent.decoration_style,
        text_transform: parent.text_transform,   // text-transform inherits
        line_height: parent.line_height,         // line-height inherits
        text_indent: parent.text_indent,         // text-indent inherits
        word_spacing: parent.word_spacing,       // word-spacing inherits
        letter_spacing: parent.letter_spacing,   // letter-spacing inherits
        border_spacing: parent.border_spacing,   // border-spacing inherits
        border_collapse: parent.border_collapse, // border-collapse inherits
        table_layout_fixed: parent.table_layout_fixed, // table-layout inherits
        hidden: parent.hidden,                   // visibility inherits
        ..ComputedStyle::initial()
    };
    apply(&mut cs, &map, parent);
    cs
}

/// The generated `content` string for an element's `::before`/`::after`
/// pseudo-element, if any author rule sets one (the highest-specificity winner).
/// Returns `None` for `content: none`/`normal` or when there's no content.
/// The cascade-winning raw value of a single property `prop` for `node` (UA +
/// presentational hints + author + inline), or `None` if unset. Used for
/// properties not stored in the (Copy) `ComputedStyle`, e.g. `counter-reset`.
pub fn cascaded_value(doc: &Document, node: NodeId, author: &Stylesheet, prop: &str) -> Option<String> {
    let mut cands = Vec::new();
    let mut order = 0usize;
    collect(ua_stylesheet(), Origin::Ua, doc, node, &mut cands, &mut order);
    for (name, value) in presentational_hints(doc, node) {
        cands.push(Cand {
            rank: rank(Origin::Author, false),
            spec: Specificity::default(),
            order,
            name,
            value,
        });
        order += 1;
    }
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
    cands.into_iter().rfind(|c| c.name == prop).map(|c| c.value)
}

/// Whether any UA or author rule uses CSS counters (`counter-reset`/`-increment`
/// or a `counter(`/`counters(` in a `content` value) — lets the layout skip the
/// counter machinery entirely on the common no-counter page.
pub fn uses_counters(author: &Stylesheet) -> bool {
    ua_stylesheet().rules.iter().chain(author.rules.iter()).any(|r| {
        r.declarations.iter().any(|d| {
            matches!(d.name.as_str(), "counter-reset" | "counter-increment")
                || (d.name == "content"
                    && (d.value.contains("counter(") || d.value.contains("counters(")))
        })
    })
}

pub fn pseudo_content(
    doc: &Document,
    node: NodeId,
    author: &Stylesheet,
    which: argus_css::PseudoElement,
    counters: &HashMap<String, i32>,
) -> Option<String> {
    let mut best: Option<(Specificity, usize, String)> = None;
    let mut order = 0usize;
    // Scan the UA stylesheet first (lowest priority), then author rules — later
    // rules (higher `order`) win ties, so author content overrides UA defaults.
    for rule in ua_stylesheet().rules.iter().chain(author.rules.iter()) {
        let spec = rule
            .selectors
            .iter()
            .filter(|s| s.pseudo_element() == Some(which) && matches(doc, node, s))
            .map(|s| s.specificity())
            .max();
        if let Some(spec) = spec {
            for d in &rule.declarations {
                if d.name == "content" {
                    let key = (spec, order);
                    if best.as_ref().is_none_or(|(s, o, _)| (key.0, key.1) >= (*s, *o)) {
                        best = Some((spec, order, d.value.clone()));
                    }
                }
                order += 1;
            }
        }
    }
    let raw = best?.2;
    let v = raw.trim();
    if v.eq_ignore_ascii_case("none") || v.eq_ignore_ascii_case("normal") {
        return None;
    }
    Some(resolve_content(doc, node, v, counters))
}

/// Resolve a `content` value into its rendered string: a sequence of quoted
/// strings and `attr(<name>)` references, concatenated (other functions →
/// empty). `attr()` reads the element's attribute (empty if absent, per spec).
fn resolve_content(
    doc: &Document,
    node: NodeId,
    v: &str,
    counters: &HashMap<String, i32>,
) -> String {
    let mut out = String::new();
    let mut rest = v.trim();
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("attr(") {
            if let Some(end) = after.find(')') {
                let name = after[..end].trim().trim_matches(['"', '\'']);
                if let Some(val) = doc.node(node).as_element().and_then(|e| e.attr(name)) {
                    out.push_str(val);
                }
                rest = after[end + 1..].trim_start();
                continue;
            }
        }
        // `counter(<name>[, <style>])` / `counters(<name>, <sep>[, <style>])` output
        // the named counter's current value (list-style ignored; default 0). With
        // the flat counter model there's a single value, so the separator is unused.
        let mut consumed_counter = false;
        for prefix in ["counter(", "counters("] {
            if let Some(after) = rest.strip_prefix(prefix) {
                if let Some(end) = after.find(')') {
                    let name = after[..end].split(',').next().unwrap_or("").trim();
                    out.push_str(&counters.get(name).copied().unwrap_or(0).to_string());
                    rest = after[end + 1..].trim_start();
                    consumed_counter = true;
                    break;
                }
            }
        }
        if consumed_counter {
            continue;
        }
        // Otherwise the leading token is a (possibly quoted) string or a quote
        // keyword (`open-quote`/`close-quote` render curly quotes).
        let (head, tail) = split_content_token(rest);
        match head {
            "open-quote" => out.push('\u{201C}'),
            "close-quote" => out.push('\u{201D}'),
            "no-open-quote" | "no-close-quote" => {}
            _ => out.push_str(&unquote_content(head)),
        }
        rest = tail.trim_start();
        if head.is_empty() {
            break;
        }
    }
    out
}

/// Split off the first whitespace-delimited token, but keep a quoted string (which
/// may contain spaces) whole. Returns `(token, remainder)`.
fn split_content_token(s: &str) -> (&str, &str) {
    let bytes = s.as_bytes();
    if bytes[0] == b'"' || bytes[0] == b'\'' {
        let q = bytes[0];
        if let Some(i) = s[1..].find(q as char) {
            let end = 1 + i + 1;
            return (&s[..end], &s[end..]);
        }
    }
    match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    }
}

/// Normalize a `content` string value. The CSS value parser already strips quotes,
/// so the common case is a bare string; we strip any residual surrounding quotes
/// and decode `\A`. `content()` functions (`attr()`, `counter()`, …) we don't
/// evaluate yield an empty string.
fn unquote_content(v: &str) -> String {
    let v = v.trim();
    let bytes = v.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[bytes.len() - 1] == bytes[0]
    {
        decode_css_escapes(&v[1..v.len() - 1])
    } else if v.contains('(') {
        String::new() // an unsupported content function
    } else {
        decode_css_escapes(v)
    }
}

/// Decode CSS string escapes: `\<1-6 hex>` (with an optional trailing space) is a
/// Unicode code point; any other `\<char>` is that literal char.
fn decode_css_escapes(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some(h) if h.is_ascii_hexdigit() => {
                let mut hex = String::new();
                while hex.len() < 6 && chars.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                    hex.push(chars.next().unwrap());
                }
                if chars.peek() == Some(&' ') {
                    chars.next(); // a single whitespace terminator is consumed
                }
                if let Some(cp) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(cp);
                }
            }
            Some(_) => out.push(chars.next().unwrap()),
            None => {}
        }
    }
    out
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
            // A `::before`/`::after` rule styles a generated box, not the element.
            .filter(|s| s.pseudo_element().is_none() && matches(doc, node, s))
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
            "inline-block" => Display::InlineBlock,
            "flex" | "inline-flex" => Display::Flex,
            "grid" | "inline-grid" => Display::Grid,
            "none" => Display::None,
            _ => Display::Inline,
        };
    }
    // `font` shorthand: `[style] [variant] [weight] [stretch] size[/line-height]
    // family`. We extract the font-size (the first length-ish token), an optional
    // `/line-height`, and a `bold`/700+ weight before it. The longhands below
    // override, so an explicit `font-size`/`font-weight` still wins.
    if let Some(v) = map.get("font").filter(|v| !is_system_font(v)) {
        let toks: Vec<&str> = v.split_whitespace().collect();
        if let Some(idx) = toks
            .iter()
            .position(|t| resolve_font_size(t.split('/').next().unwrap_or(t), parent.font_size).is_some())
        {
            let (size_part, lh_part) = match toks[idx].split_once('/') {
                Some((s, l)) => (s, Some(l)),
                None => (toks[idx], None),
            };
            if let Some(px) = resolve_font_size(size_part, parent.font_size) {
                cs.font_size = px;
            }
            if let Some(lh) = lh_part {
                if let Ok(n) = lh.parse::<f32>() {
                    cs.line_height = n;
                } else if let Some(l) = parse_length(lh) {
                    let px = l.to_px(cs.font_size, 0.0);
                    if cs.font_size > 0.0 {
                        cs.line_height = px / cs.font_size;
                    }
                }
            }
            cs.bold = toks[..idx].iter().any(|t| is_bold(t));
            cs.italic = toks[..idx].iter().any(|t| *t == "italic" || *t == "oblique");
        }
    }
    if let Some(v) = map.get("font-style") {
        cs.italic = matches!(v.trim(), "italic" | "oblique");
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
    if let Some(v) = map
        .get("background-image")
        .or_else(|| map.get("background"))
    {
        if v.contains("linear-gradient(") {
            cs.background_gradient = parse_linear_gradient(v, cs.color);
        } else if v.contains("radial-gradient(") {
            cs.background_gradient = parse_radial_gradient(v, cs.color);
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
        cs.overline = v.split_whitespace().any(|t| t == "overline");
        // A style keyword in the shorthand sets how the lines are drawn.
        for tok in v.split_whitespace() {
            if let Some(s) = decoration_style_of(tok) {
                cs.decoration_style = s;
            }
        }
        // A color token in the `text-decoration` shorthand also sets the line color.
        for tok in v.split_whitespace() {
            if let Some(c) = resolve_color(tok, cs.color, parent.color) {
                cs.decoration_color = Some(c);
            }
        }
    }
    if let Some(s) = map.get("text-decoration-style").and_then(|v| decoration_style_of(v.trim())) {
        cs.decoration_style = s;
    }
    if let Some(c) = map
        .get("text-decoration-color")
        .and_then(|v| resolve_color(v, cs.color, parent.color))
    {
        cs.decoration_color = Some(c);
    }
    if let Some(v) = map.get("accent-color") {
        cs.accent_color = if v.trim() == "auto" {
            None
        } else {
            resolve_color(v, cs.color, parent.accent_color.unwrap_or(cs.color))
        };
    }
    if let Some(v) = map.get("text-shadow") {
        cs.text_shadow = if v.trim() == "none" {
            None
        } else {
            parse_text_shadow(v, cs.color, cs.font_size)
        };
    }
    if let Some(v) = map.get("box-shadow") {
        cs.box_shadow = parse_box_shadow(v, cs.font_size, cs.color);
    }
    if let Some(v) = map
        .get("list-style-type")
        .or_else(|| map.get("list-style"))
        .and_then(|v| v.split_whitespace().find_map(parse_list_style))
    {
        cs.list_style = v;
    }
    if let Some(v) = map
        .get("list-style-position")
        .or_else(|| map.get("list-style"))
    {
        if v.split_whitespace().any(|t| t == "inside") {
            cs.list_style_inside = true;
        } else if v.split_whitespace().any(|t| t == "outside") {
            cs.list_style_inside = false;
        }
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
    if let Some(v) = map.get("caption-side") {
        cs.caption_side_bottom = v.trim() == "bottom";
    }
    if let Some(v) = map.get("object-fit") {
        cs.object_fit = match v.trim() {
            "contain" | "scale-down" => ObjectFit::Contain,
            "cover" => ObjectFit::Cover,
            _ => ObjectFit::Fill,
        };
    }
    // `gap` shorthand: `<row-gap> [<column-gap>]` (column defaults to row).
    if let Some(v) = map.get("gap").or_else(|| map.get("grid-gap")) {
        let mut toks = v.split_whitespace();
        if let Some(row) = toks.next().and_then(parse_length) {
            let r = row.to_px(cs.font_size, 0.0).max(0.0);
            cs.row_gap = r;
            cs.gap = toks
                .next()
                .and_then(parse_length)
                .map(|l| l.to_px(cs.font_size, 0.0).max(0.0))
                .unwrap_or(r);
        }
    }
    // Longhands override the shorthand.
    if let Some(px) = map
        .get("column-gap")
        .or_else(|| map.get("grid-column-gap"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(parse_length)
        .map(|l| l.to_px(cs.font_size, 0.0))
    {
        cs.gap = px.max(0.0);
    }
    if let Some(px) = map
        .get("row-gap")
        .or_else(|| map.get("grid-row-gap"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(parse_length)
        .map(|l| l.to_px(cs.font_size, 0.0))
    {
        cs.row_gap = px.max(0.0);
    }
    if let Some(v) = map.get("vertical-align") {
        cs.vertical_align = match v.as_str() {
            "sub" => VerticalAlign::Sub,
            "super" => VerticalAlign::Super,
            "top" | "text-top" => VerticalAlign::Top,
            "middle" => VerticalAlign::Middle,
            "bottom" | "text-bottom" => VerticalAlign::Bottom,
            _ => VerticalAlign::Baseline,
        };
    }
    if let Some(v) = map.get("visibility") {
        cs.hidden = matches!(v.as_str(), "hidden" | "collapse");
    }
    if let Some(v) = map.get("position") {
        // `sticky` falls back to static (it needs scroll tracking).
        cs.position = match v.as_str() {
            "relative" => Position::Relative,
            "absolute" => Position::Absolute,
            "fixed" => Position::Fixed,
            _ => Position::Static,
        };
    }
    // `inset` shorthand: 1–4 values → top/right/bottom/left (CSS edge order); each
    // `auto` token clears that inset.
    if let Some(v) = map.get("inset") {
        let t: Vec<&str> = v.split_whitespace().collect();
        if !t.is_empty() {
            let set = |tok: &str| -> Option<Length> {
                (!tok.eq_ignore_ascii_case("auto"))
                    .then(|| parse_length(tok))
                    .flatten()
            };
            cs.inset_top = set(t[0]);
            cs.inset_right = set(t.get(1).copied().unwrap_or(t[0]));
            cs.inset_bottom = set(t.get(2).copied().unwrap_or(t[0]));
            cs.inset_left = set(t.get(3).or(t.get(1)).copied().unwrap_or(t[0]));
        }
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
    // Logical inset (horizontal-tb): inline-start/end → left/right, block → top/bottom.
    // The `-inline`/`-block` shorthands take 1–2 values (start, end).
    let logical_inset = |v: &str| -> Option<Length> {
        v.split_whitespace()
            .next()
            .filter(|t| !t.eq_ignore_ascii_case("auto"))
            .and_then(parse_length)
    };
    let logical_inset_pair = |v: &str, second: bool| -> Option<Length> {
        let t: Vec<&str> = v.split_whitespace().collect();
        let tok = if second { t.get(1).or(t.first()) } else { t.first() }?;
        (!tok.eq_ignore_ascii_case("auto"))
            .then(|| parse_length(tok))
            .flatten()
    };
    if let Some(v) = map.get("inset-inline") {
        cs.inset_left = logical_inset_pair(v, false);
        cs.inset_right = logical_inset_pair(v, true);
    }
    if let Some(v) = map.get("inset-block") {
        cs.inset_top = logical_inset_pair(v, false);
        cs.inset_bottom = logical_inset_pair(v, true);
    }
    if let Some(v) = map.get("inset-inline-start") {
        cs.inset_left = logical_inset(v);
    }
    if let Some(v) = map.get("inset-inline-end") {
        cs.inset_right = logical_inset(v);
    }
    if let Some(v) = map.get("inset-block-start") {
        cs.inset_top = logical_inset(v);
    }
    if let Some(v) = map.get("inset-block-end") {
        cs.inset_bottom = logical_inset(v);
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
    if let Some(v) = map.get("letter-spacing") {
        cs.letter_spacing = if v.trim() == "normal" {
            0.0
        } else {
            parse_length(v).map_or(cs.letter_spacing, |l| l.to_px(cs.font_size, 0.0))
        };
    }
    if let Some(v) = map.get("border-collapse") {
        cs.border_collapse = v.trim() == "collapse";
    }
    if let Some(v) = map.get("table-layout") {
        cs.table_layout_fixed = v.trim() == "fixed";
    }
    // `border-spacing` (the first/horizontal value if two are given).
    if let Some(v) = map.get("border-spacing") {
        if let Some(px) = v
            .split_whitespace()
            .next()
            .and_then(parse_length)
            .map(|l| l.to_px(cs.font_size, 0.0))
        {
            cs.border_spacing = px.max(0.0);
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
    logical_edges(map, "margin", fs, &mut cs.margin);
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
    logical_edges(map, "padding", fs, &mut cs.padding);
    // Borders.
    if let Some(v) = map.get("border") {
        let (w, c) = parse_border(v, fs);
        cs.border = Edges::uniform(w);
        if let Some(c) = c {
            cs.border_color = c;
        } else if mentions_current_color(v) {
            cs.border_color = cs.color;
        }
        for tok in v.split_whitespace() {
            if let Some(s) = decoration_style_of(tok) {
                cs.border_style = s;
            }
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
    // `border-style: none|hidden` suppresses the border even when a width/color was
    // set; per-side `border-<side>-style` zeroes just that edge. The visible style
    // keywords (solid/double/dotted/dashed) pick how the (uniform) frame is drawn.
    let is_none_style = |v: &str| matches!(v.trim(), "none" | "hidden");
    if let Some(s) = map.get("border-style").and_then(|v| decoration_style_of(v.trim())) {
        cs.border_style = s;
    }
    if map.get("border-style").is_some_and(|v| is_none_style(v)) {
        cs.border = Edges::uniform(0.0);
    }
    if map.get("border-top-style").is_some_and(|v| is_none_style(v)) {
        cs.border.top = 0.0;
    }
    if map.get("border-right-style").is_some_and(|v| is_none_style(v)) {
        cs.border.right = 0.0;
    }
    if map.get("border-bottom-style").is_some_and(|v| is_none_style(v)) {
        cs.border.bottom = 0.0;
    }
    if map.get("border-left-style").is_some_and(|v| is_none_style(v)) {
        cs.border.left = 0.0;
    }
    // Per-side border colors default to the shorthand color, then per-side longhands
    // override them.
    cs.border_top_color = cs.border_color;
    cs.border_right_color = cs.border_color;
    cs.border_bottom_color = cs.border_color;
    cs.border_left_color = cs.border_color;
    if let Some(c) = map.get("border-top-color").and_then(|v| resolve_color(v, cs.color, parent.border_color)) {
        cs.border_top_color = c;
    }
    if let Some(c) = map.get("border-right-color").and_then(|v| resolve_color(v, cs.color, parent.border_color)) {
        cs.border_right_color = c;
    }
    if let Some(c) = map.get("border-bottom-color").and_then(|v| resolve_color(v, cs.color, parent.border_color)) {
        cs.border_bottom_color = c;
    }
    if let Some(c) = map.get("border-left-color").and_then(|v| resolve_color(v, cs.color, parent.border_color)) {
        cs.border_left_color = c;
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
        for tok in v.split_whitespace() {
            if let Some(s) = decoration_style_of(tok) {
                cs.outline_style = s;
            }
        }
    }
    if let Some(s) = map.get("outline-style").and_then(|v| decoration_style_of(v.trim())) {
        cs.outline_style = s;
    }
    if let Some(v) = map.get("outline-width").and_then(|v| len_px(v, fs)) {
        cs.outline_width = v;
    }
    if let Some(v) = map.get("outline-offset").and_then(|v| len_px(v, fs)) {
        cs.outline_offset = v.max(0.0);
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
    if let Some(v) = map.get("max-height") {
        cs.max_height = if v == "none" { None } else { parse_length(v) };
    }
    // CSS logical sizing (horizontal-tb): inline = width, block = height.
    if let Some(v) = map.get("inline-size") {
        cs.width = if v == "auto" { None } else { parse_length(v) };
    }
    if let Some(v) = map.get("min-inline-size") {
        cs.min_width = if v == "auto" || v == "0" { None } else { parse_length(v) };
    }
    if let Some(v) = map.get("max-inline-size") {
        cs.max_width = if v == "none" { None } else { parse_length(v) };
    }
    if let Some(v) = map.get("block-size") {
        cs.height = if v == "auto" { None } else { parse_length(v) };
    }
    if let Some(v) = map.get("min-block-size") {
        cs.min_height = if v == "auto" || v == "0" { None } else { parse_length(v) };
    }
    if let Some(v) = map.get("max-block-size") {
        cs.max_height = if v == "none" { None } else { parse_length(v) };
    }
    if let Some(v) = map.get("aspect-ratio") {
        cs.aspect_ratio = parse_aspect_ratio(v);
    }
    if let Some(v) = map.get("grid-template-columns") {
        let (cols, tracks) = parse_grid_tracks(v);
        cs.grid_columns = cols;
        cs.grid_tracks = tracks;
    }
    if let Some(v) = map.get("grid-template-rows") {
        let (rows, tracks) = parse_grid_tracks(v);
        cs.grid_rows = rows;
        cs.grid_row_tracks = tracks;
    }
    // `grid-column` / `grid-column-end` span for a grid item: `span N`, or an
    // `a / b` line range whose width is `b - a`. The explicit start line (when
    // present) drives line-based placement.
    if let Some(v) = map.get("grid-column").or_else(|| map.get("grid-column-end")) {
        cs.grid_column_span = parse_grid_span(v);
        cs.grid_column_start = parse_grid_line_start(v);
    }
    if let Some(v) = map.get("grid-column-start") {
        cs.grid_column_start = parse_grid_line_start(v);
    }
    if let Some(v) = map.get("grid-row").or_else(|| map.get("grid-row-end")) {
        cs.grid_row_span = parse_grid_span(v);
        cs.grid_row_start = parse_grid_line_start(v);
    }
    if let Some(v) = map.get("grid-row-start") {
        cs.grid_row_start = parse_grid_line_start(v);
    }
    if let Some(v) = map.get("float") {
        cs.float = match v.trim() {
            "left" => Float::Left,
            "right" => Float::Right,
            _ => Float::None,
        };
    }
    if let Some(v) = map.get("clear") {
        cs.clear = match v.trim() {
            "left" => Clear::Left,
            "right" => Clear::Right,
            "both" => Clear::Both,
            _ => Clear::None,
        };
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
    if let Some(v) = map.get("justify-content") {
        cs.justify_content = match v.trim() {
            "flex-end" | "end" | "right" => JustifyContent::FlexEnd,
            "center" => JustifyContent::Center,
            "space-between" => JustifyContent::SpaceBetween,
            "space-around" => JustifyContent::SpaceAround,
            "space-evenly" => JustifyContent::SpaceEvenly,
            _ => JustifyContent::FlexStart,
        };
    }
    let parse_align = |v: &str| match v.trim() {
        "flex-start" | "start" => AlignItems::FlexStart,
        "flex-end" | "end" => AlignItems::FlexEnd,
        "center" => AlignItems::Center,
        _ => AlignItems::Stretch,
    };
    if let Some(v) = map.get("align-items") {
        cs.align_items = parse_align(v);
    }
    if let Some(v) = map.get("align-self") {
        cs.align_self = (v.trim() != "auto").then(|| parse_align(v));
    }
    // The `flex` shorthand first: it sets grow + shrink together. Keywords:
    // `none` → 0 0 auto, `auto` → 1 1 auto, `initial` → 0 1 auto. Numeric forms
    // take the first two numbers as grow then shrink (shrink defaults to 1).
    if let Some(v) = map.get("flex") {
        let t = v.trim();
        match t {
            "none" => {
                cs.flex_grow = 0.0;
                cs.flex_shrink = 0.0;
            }
            "auto" => {
                cs.flex_grow = 1.0;
                cs.flex_shrink = 1.0;
            }
            "initial" => {
                cs.flex_grow = 0.0;
                cs.flex_shrink = 1.0;
            }
            _ => {
                // `flex: <grow> <shrink>? <basis>?`. Bare numbers are grow then
                // shrink; an `auto` keyword or a length token is the basis; a third
                // bare number is the basis (in px). The shorthand resets an omitted
                // basis to 0 (so `flex: 1` means a 0 base size).
                let mut nums = Vec::new();
                let mut basis: Option<Option<Length>> = None; // outer Some = explicit
                for tok in t.split_whitespace() {
                    if tok == "auto" {
                        basis = Some(None);
                    } else if let Ok(n) = tok.parse::<f32>() {
                        nums.push(n);
                    } else if let Some(len) = parse_length(tok) {
                        basis = Some(Some(len));
                    }
                }
                if let Some(&g) = nums.first() {
                    cs.flex_grow = g.max(0.0);
                }
                cs.flex_shrink = nums.get(1).copied().unwrap_or(1.0).max(0.0);
                cs.flex_basis = match nums.get(2) {
                    Some(&b) => Some(Length::Px(b)),
                    None => basis.unwrap_or(Some(Length::Zero)),
                };
            }
        }
    }
    if let Some(v) = map.get("flex-basis") {
        cs.flex_basis = if v.trim() == "auto" {
            None
        } else {
            parse_length(v.trim())
        };
    }
    // Longhands override the shorthand when both are present.
    if let Some(v) = map.get("flex-grow") {
        if let Ok(g) = v.trim().parse::<f32>() {
            cs.flex_grow = g.max(0.0);
        }
    }
    if let Some(v) = map.get("flex-shrink") {
        if let Ok(s) = v.trim().parse::<f32>() {
            cs.flex_shrink = s.max(0.0);
        }
    }
    if let Some(v) = map.get("order") {
        if let Ok(o) = v.trim().parse::<i32>() {
            cs.order = o;
        }
    }
    // `flex-wrap` (also expressible as a token of the `flex-flow` shorthand).
    if let Some(v) = map.get("flex-wrap").or_else(|| map.get("flex-flow")) {
        cs.flex_wrap = v
            .split_whitespace()
            .any(|tok| tok == "wrap" || tok == "wrap-reverse");
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
        cs.pre_line = ws.as_str() == "pre-line";
        cs.pre_wrap = matches!(ws.as_str(), "pre-wrap" | "break-spaces");
    }
    if let Some(v) = map.get("tab-size").or_else(|| map.get("-moz-tab-size")) {
        // A bare number is a count of spaces; lengths aren't modeled here.
        if let Ok(n) = v.trim().parse::<u32>() {
            cs.tab_size = n.min(32);
        }
    }
    // `overflow-wrap`/`word-wrap: break-word` (or `word-break: break-all`) splits
    // over-long words to avoid overflow.
    if let Some(v) = map.get("overflow-wrap").or_else(|| map.get("word-wrap")) {
        cs.break_word = matches!(v.trim(), "break-word" | "anywhere");
    }
    if let Some(v) = map.get("word-break") {
        if matches!(v.trim(), "break-all" | "break-word") {
            cs.break_word = true;
        }
    }
    if let Some(v) = map.get("text-overflow") {
        cs.ellipsis = v.trim() == "ellipsis";
    }
    if let Some(v) = map.get("transform") {
        cs.transform_translate = parse_transform_translate(v);
        cs.transform_scale = parse_transform_scale(v);
    }
}

/// Count the columns named by a `grid-template-columns` value (a simplified read
/// of `repeat(n, …)` and whitespace-separated track lists).
/// Parse `grid-template-columns` into a track list (count + sizes). Supports a
/// flat list of `<n>fr` / lengths / `auto`, plus a single leading
/// `repeat(n, <track>...)`. The track list is capped at [`GRID_MAX_TRACKS`].
fn parse_grid_tracks(v: &str) -> (u32, [GridTrack; GRID_MAX_TRACKS]) {
    let mut tracks = [GridTrack::Auto; GRID_MAX_TRACKS];
    let mut count = 0usize;
    let mut push = |t: GridTrack| {
        if count < GRID_MAX_TRACKS {
            tracks[count] = t;
            count += 1;
        }
    };
    let parse_one = |tok: &str| -> GridTrack {
        let tok = tok.trim();
        if let Some(fr) = tok.strip_suffix("fr") {
            GridTrack::Fr(fr.trim().parse::<f32>().unwrap_or(1.0).max(0.0))
        } else if tok == "auto" || tok == "min-content" || tok == "max-content" {
            GridTrack::Auto
        } else if tok.starts_with("minmax(") {
            // Approximate `minmax(a, b)` by its max term.
            tok.trim_start_matches("minmax(")
                .trim_end_matches(')')
                .split(',')
                .nth(1)
                .map(parse_one_len)
                .unwrap_or(GridTrack::Auto)
        } else {
            parse_one_len(tok)
        }
    };
    let v = v.trim();
    // Expand a single leading `repeat(n, <tracks>)`.
    if let Some(rest) = v.strip_prefix("repeat(") {
        let inner = rest.strip_suffix(')').unwrap_or(rest);
        if let Some((n_str, tracks_str)) = inner.split_once(',') {
            if let Ok(n) = n_str.trim().parse::<u32>() {
                for _ in 0..n {
                    for tok in tracks_str.split_whitespace() {
                        push(parse_one(tok));
                    }
                }
                return (count.max(1) as u32, tracks);
            }
        }
    }
    for tok in v.split_whitespace() {
        push(parse_one(tok));
    }
    (count.max(1) as u32, tracks)
}

/// Parse the `translate`/`translateX`/`translateY` part of a `transform` value
/// into an `(x, y)` length pair. Other functions (scale/rotate/…) are ignored.
fn parse_transform_translate(v: &str) -> Option<(Length, Length)> {
    for seg in v.split(')') {
        let seg = seg.trim();
        if let Some(args) = seg.strip_prefix("translate(") {
            let mut it = args.split(',');
            let x = it.next().and_then(|s| parse_length(s.trim()))?;
            let y = it
                .next()
                .and_then(|s| parse_length(s.trim()))
                .unwrap_or(Length::Zero);
            return Some((x, y));
        }
        if let Some(args) = seg.strip_prefix("translateX(") {
            return parse_length(args.trim()).map(|x| (x, Length::Zero));
        }
        if let Some(args) = seg.strip_prefix("translateY(") {
            return parse_length(args.trim()).map(|y| (Length::Zero, y));
        }
    }
    None
}

/// Parse a (single) `text-shadow` into `(offset-x, offset-y, color)` in px. The
/// first two lengths are the offsets (a third, the blur radius, is ignored); a
/// color token anywhere sets the shadow color (default: the element's text color).
fn parse_text_shadow(v: &str, text_color: Color, fs: f32) -> Option<(f32, f32, Color)> {
    // Only the first comma-separated shadow layer is modeled.
    let layer = v.split(',').next()?.trim();
    let mut lengths: Vec<f32> = Vec::new();
    let mut color = text_color;
    for tok in layer.split_whitespace() {
        if let Some(l) = parse_length(tok) {
            lengths.push(l.to_px(fs, 0.0));
        } else if let Some(c) = parse_color(tok) {
            color = c;
        }
    }
    if lengths.len() >= 2 {
        Some((lengths[0], lengths[1], color))
    } else {
        None
    }
}

/// Parse a two-stop axis-aligned `linear-gradient(...)`. The direction is taken
/// from a `to <side>` keyword or an angle (snapped to the nearest axis); the first
/// and last color stops become the endpoints. Returns `None` if under two colors
/// parse.
fn parse_linear_gradient(v: &str, current: Color) -> Option<Gradient> {
    let start = v.find("linear-gradient(")? + "linear-gradient(".len();
    let rest = &v[start..];
    let inner = &rest[..rest.find(')').unwrap_or(rest.len())];
    let parts = split_top_level_commas(inner);
    if parts.is_empty() {
        return None;
    }
    // An optional leading direction (`to <side>` or `<angle>deg`).
    let mut dir = GradientDir::ToBottom;
    let mut color_parts = &parts[..];
    let first = parts[0].as_str();
    if first.starts_with("to ") {
        dir = match first {
            "to right" => GradientDir::ToRight,
            "to left" => GradientDir::ToLeft,
            "to top" => GradientDir::ToTop,
            _ => GradientDir::ToBottom,
        };
        color_parts = &parts[1..];
    } else if let Some(deg) = parse_angle_deg(first) {
        let a = ((deg % 360.0) + 360.0) % 360.0;
        dir = if !(45.0..315.0).contains(&a) {
            GradientDir::ToTop
        } else if a < 135.0 {
            GradientDir::ToRight
        } else if a < 225.0 {
            GradientDir::ToBottom
        } else {
            GradientDir::ToLeft
        };
        color_parts = &parts[1..];
    }
    let (stops, n_stops) = parse_gradient_stops(color_parts, current)?;
    Some(Gradient {
        dir,
        from: stops[0].0,
        to: stops[n_stops as usize - 1].0,
        radial: false,
        stops,
        n_stops,
    })
}

/// Resolve a gradient-stop color token, handling the `currentColor` keyword
/// (`current`) in addition to the normal color syntaxes.
fn stop_color(tok: &str, current: Color) -> Option<Color> {
    if tok.eq_ignore_ascii_case("currentcolor") {
        Some(current)
    } else {
        parse_color(tok)
    }
}

/// Map a `text-decoration-style` keyword to its enum, or `None` if not one.
fn decoration_style_of(tok: &str) -> Option<DecorationStyle> {
    match tok {
        "solid" => Some(DecorationStyle::Solid),
        "double" => Some(DecorationStyle::Double),
        "dotted" => Some(DecorationStyle::Dotted),
        "dashed" => Some(DecorationStyle::Dashed),
        "wavy" => Some(DecorationStyle::Wavy),
        _ => None,
    }
}

/// Split on commas that sit at paren depth zero, keeping color functions
/// (`rgb(…)`, `oklch(…)`) intact. Trims and drops empty segments.
fn split_top_level_commas(inner: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for ch in inner.chars() {
        match ch {
            '(' => {
                depth += 1;
                cur.push(ch);
            }
            ')' => {
                depth -= 1;
                cur.push(ch);
            }
            ',' if depth == 0 => {
                parts.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        parts.push(cur.trim().to_string());
    }
    parts.retain(|p| !p.is_empty());
    parts
}

/// Parse an ordered list of `<color> [<position%>]` stops into a fixed array with
/// monotonic positions in `0.0..=1.0`. Stops without an explicit position are
/// spaced evenly between their positioned neighbors (CSS gradient stop rules).
fn parse_gradient_stops(
    parts: &[String],
    current: Color,
) -> Option<([(Color, f32); GRAD_MAX_STOPS], u8)> {
    // Collect (color, explicit-position?) pairs, capped to the array size.
    let mut raw: Vec<(Color, Option<f32>)> = Vec::new();
    for p in parts {
        let color = p.split_whitespace().find_map(|t| stop_color(t, current))?;
        let pos = p
            .split_whitespace()
            .find_map(|t| t.strip_suffix('%'))
            .and_then(|n| n.trim().parse::<f32>().ok())
            .map(|pct| (pct / 100.0).clamp(0.0, 1.0));
        raw.push((color, pos));
        if raw.len() == GRAD_MAX_STOPS {
            break;
        }
    }
    if raw.len() < 2 {
        return None;
    }
    // Anchor the endpoints, then fill gaps by even interpolation between anchors.
    let n = raw.len();
    if raw[0].1.is_none() {
        raw[0].1 = Some(0.0);
    }
    if raw[n - 1].1.is_none() {
        raw[n - 1].1 = Some(1.0);
    }
    let mut i = 0;
    while i < n {
        if raw[i].1.is_some() {
            i += 1;
            continue;
        }
        // Find the next positioned stop and distribute evenly across the gap.
        let prev = raw[i - 1].1.unwrap();
        let mut j = i;
        while raw[j].1.is_none() {
            j += 1;
        }
        let next = raw[j].1.unwrap();
        let span = next - prev;
        let count = (j - i + 1) as f32;
        for (k, idx) in (i..j).enumerate() {
            raw[idx].1 = Some(prev + span * (k as f32 + 1.0) / count);
        }
        i = j;
    }
    // Enforce non-decreasing positions (later stops never precede earlier ones).
    let mut out = [(Color::TRANSPARENT, 0.0f32); GRAD_MAX_STOPS];
    let mut last = 0.0f32;
    for (idx, (c, p)) in raw.iter().enumerate() {
        last = p.unwrap().max(last);
        out[idx] = (*c, last);
    }
    Some((out, n as u8))
}

/// Parse a CSS `<angle>` to degrees: a bare number or `deg`, or a `grad`/`rad`/
/// `turn` unit (`1turn = 360deg`, `400grad = 360deg`, `2π rad = 360deg`).
fn parse_angle_deg(s: &str) -> Option<f32> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix("turn") {
        Some(n.trim().parse::<f32>().ok()? * 360.0)
    } else if let Some(n) = s.strip_suffix("grad") {
        Some(n.trim().parse::<f32>().ok()? * 0.9)
    } else if let Some(n) = s.strip_suffix("rad") {
        Some(n.trim().parse::<f32>().ok()?.to_degrees())
    } else {
        s.trim_end_matches("deg").trim().parse::<f32>().ok()
    }
}

/// Parse a `radial-gradient(...)` — the first color is the center, the last the
/// edge. A leading shape/position prefix (`circle`, `at center`, …) carries no
/// color and is dropped before reading the stops.
fn parse_radial_gradient(v: &str, current: Color) -> Option<Gradient> {
    let start = v.find("radial-gradient(")? + "radial-gradient(".len();
    let rest = &v[start..];
    let inner = &rest[..rest.find(')').unwrap_or(rest.len())];
    // Keep only segments that name a color (drops the optional shape prefix).
    let color_parts: Vec<String> = split_top_level_commas(inner)
        .into_iter()
        .filter(|p| p.split_whitespace().any(|t| stop_color(t, current).is_some()))
        .collect();
    let (stops, n_stops) = parse_gradient_stops(&color_parts, current)?;
    Some(Gradient {
        dir: GradientDir::ToBottom,
        from: stops[0].0,
        to: stops[n_stops as usize - 1].0,
        radial: true,
        stops,
        n_stops,
    })
}

/// Parse a (single, outer) `box-shadow` into `(offset-x, offset-y, blur, spread,
/// color)` in px. The lengths are offset-x, offset-y, blur, spread (in order); a
/// color token sets the color (default and `currentColor` → the element's text
/// color `current`, per spec). `inset` shadows are skipped.
fn parse_box_shadow(v: &str, fs: f32, current: Color) -> Option<(f32, f32, f32, f32, Color)> {
    let layer = v.split(',').next()?.trim();
    if layer == "none" || layer.split_whitespace().any(|t| t == "inset") {
        return None;
    }
    let mut lengths: Vec<f32> = Vec::new();
    let mut color = current;
    for tok in layer.split_whitespace() {
        if let Some(l) = parse_length(tok) {
            lengths.push(l.to_px(fs, 0.0));
        } else if let Some(c) = stop_color(tok, current) {
            color = c;
        }
    }
    if lengths.len() >= 2 {
        let blur = lengths.get(2).copied().unwrap_or(0.0).max(0.0);
        let spread = lengths.get(3).copied().unwrap_or(0.0);
        Some((lengths[0], lengths[1], blur, spread, color))
    } else {
        None
    }
}

/// Parse the `scale`/`scaleX`/`scaleY` part of a `transform` value into an
/// `(sx, sy)` factor pair. `scale(s)` is uniform; other functions are ignored.
fn parse_transform_scale(v: &str) -> Option<(f32, f32)> {
    for seg in v.split(')') {
        let seg = seg.trim();
        if let Some(args) = seg.strip_prefix("scale(") {
            let mut it = args.split(',');
            let x: f32 = it.next()?.trim().parse().ok()?;
            let y: f32 = it.next().and_then(|s| s.trim().parse().ok()).unwrap_or(x);
            return Some((x, y));
        }
        if let Some(args) = seg.strip_prefix("scaleX(") {
            return args.trim().parse().ok().map(|x| (x, 1.0));
        }
        if let Some(args) = seg.strip_prefix("scaleY(") {
            return args.trim().parse().ok().map(|y| (1.0, y));
        }
    }
    None
}

/// Parse a `grid-column` value into a span count (columns occupied). Handles
/// `span N`, an `a / b` line range (width `b - a`), and a bare `span`/number.
fn parse_grid_span(v: &str) -> u32 {
    let v = v.trim();
    if let Some((start, end)) = v.split_once('/') {
        let start = start.trim();
        let end = end.trim();
        if let Some(n) = end.strip_prefix("span") {
            return n.trim().parse::<u32>().unwrap_or(1).max(1);
        }
        if let (Ok(a), Ok(b)) = (start.parse::<i32>(), end.parse::<i32>()) {
            // A negative end line (e.g. `1 / -1`) means "to the last line"; signal
            // that with the 0 sentinel, resolved against the track count in layout.
            if b < 0 {
                return 0;
            }
            return (b - a).max(1) as u32;
        }
        return 1;
    }
    if let Some(n) = v.strip_prefix("span") {
        return n.trim().parse::<u32>().unwrap_or(1).max(1);
    }
    1
}

/// Parse the explicit starting column line from a `grid-column`/`-start` value:
/// the `a` in `a / b`, or a bare positive line number. Returns `None` for `span …`,
/// `auto`, or negative (end-relative) lines, which fall back to auto-placement.
fn parse_grid_line_start(v: &str) -> Option<u32> {
    let head = v.trim().split_once('/').map(|(a, _)| a).unwrap_or(v).trim();
    if head.is_empty() || head.starts_with("span") || head == "auto" {
        return None;
    }
    match head.parse::<i32>() {
        Ok(n) if n >= 1 => Some(n as u32),
        _ => None,
    }
}

/// Parse a single fixed-length grid track token (falling back to `Auto`).
fn parse_one_len(tok: &str) -> GridTrack {
    parse_length(tok.trim())
        .map(GridTrack::Len)
        .unwrap_or(GridTrack::Auto)
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

/// Apply CSS logical box properties for `prop` (`margin`/`padding`) onto `edges`,
/// assuming the default `horizontal-tb` writing mode: inline = left/right, block =
/// top/bottom. Handles the `-inline`/`-block` shorthands (1–2 values) and the
/// `-inline-start/-end`/`-block-start/-end` longhands. Applied after the physical
/// longhands so an explicit logical value wins (approximating later-in-cascade).
fn logical_edges(map: &HashMap<String, String>, prop: &str, fs: f32, edges: &mut Edges) {
    if let Some(v) = map.get(&format!("{prop}-inline")) {
        let t: Vec<&str> = v.split_whitespace().collect();
        if let Some(px) = t.first().and_then(|s| len_px(s, fs)) {
            edges.left = px;
            edges.right = px;
        }
        if let Some(px) = t.get(1).and_then(|s| len_px(s, fs)) {
            edges.right = px;
        }
    }
    if let Some(v) = map.get(&format!("{prop}-block")) {
        let t: Vec<&str> = v.split_whitespace().collect();
        if let Some(px) = t.first().and_then(|s| len_px(s, fs)) {
            edges.top = px;
            edges.bottom = px;
        }
        if let Some(px) = t.get(1).and_then(|s| len_px(s, fs)) {
            edges.bottom = px;
        }
    }
    for (suffix, side) in [
        ("inline-start", 0u8),
        ("inline-end", 1),
        ("block-start", 2),
        ("block-end", 3),
    ] {
        if let Some(px) = map.get(&format!("{prop}-{suffix}")).and_then(|v| len_px(v, fs)) {
            match side {
                0 => edges.left = px,
                1 => edges.right = px,
                2 => edges.top = px,
                _ => edges.bottom = px,
            }
        }
    }
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

/// Whether a `font` shorthand value is a system-font keyword (which has no explicit
/// size to extract).
fn is_system_font(v: &str) -> bool {
    matches!(
        v.trim(),
        "caption" | "icon" | "menu" | "message-box" | "small-caption" | "status-bar"
    )
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
    fn border_style_none_suppresses_border() {
        let mut doc = Document::new();
        let el = one(&mut doc, "div", vec![]);
        // A width is set but `border-style: none` zeroes the whole border...
        let author = parse_stylesheet("div { border-width: 4px; border-style: none }");
        let cs = computed_style(&doc, el, &ComputedStyle::initial(), &author);
        assert_eq!(cs.border, Edges::uniform(0.0), "border-style:none zeroes width");

        // ...while a per-side `none` only zeroes that edge.
        let el2 = one(&mut doc, "div", vec![]);
        let author2 = parse_stylesheet("div { border: 3px solid black; border-left-style: none }");
        let cs2 = computed_style(&doc, el2, &ComputedStyle::initial(), &author2);
        assert_eq!(cs2.border.left, 0.0, "left edge suppressed");
        assert_eq!(cs2.border.top, 3.0, "top edge kept");
        assert_eq!(cs2.border.right, 3.0, "right edge kept");
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
    fn ua_dd_is_indented() {
        let mut doc = Document::new();
        let dd = one(&mut doc, "dd", vec![]);
        let cs = computed_style(&doc, dd, &ComputedStyle::initial(), &Stylesheet::default());
        assert_eq!(cs.display, Display::Block);
        assert_eq!(cs.margin.left, 40.0, "dd is indented by the UA default");
    }

    #[test]
    fn flex_shorthand_basis() {
        let parse = |decl: &str| {
            let mut doc = Document::new();
            let el = one(&mut doc, "div", vec![]);
            let author = parse_stylesheet(&format!("div {{ {decl} }}"));
            computed_style(&doc, el, &ComputedStyle::initial(), &author)
        };
        // `flex: 1` → grow 1, shrink 1, basis 0.
        let a = parse("flex: 1");
        assert_eq!((a.flex_grow, a.flex_shrink, a.flex_basis), (1.0, 1.0, Some(Length::Zero)));
        // Explicit length basis.
        assert_eq!(parse("flex: 1 1 200px").flex_basis, Some(Length::Px(200.0)));
        // `auto` basis stays None (content-sized).
        assert_eq!(parse("flex: 0 0 auto").flex_basis, None);
        assert_eq!(parse("flex: 0 0 auto").flex_grow, 0.0);
        // Longhand.
        assert_eq!(parse("flex-basis: 50px").flex_basis, Some(Length::Px(50.0)));
    }

    #[test]
    fn border_collapse_parses_and_inherits() {
        let mut doc = Document::new();
        let table = one(&mut doc, "table", vec![]);
        let author = parse_stylesheet("table { border-collapse: collapse }");
        let cs = computed_style(&doc, table, &ComputedStyle::initial(), &author);
        assert!(cs.border_collapse);
        let td = one(&mut doc, "td", vec![]);
        assert!(
            computed_style(&doc, td, &cs, &Stylesheet::default()).border_collapse,
            "border-collapse inherits to cells"
        );
    }

    #[test]
    fn letter_spacing_parses_and_inherits() {
        let mut doc = Document::new();
        let p = one(&mut doc, "p", vec![]);
        let author = parse_stylesheet("p { letter-spacing: 3px }");
        let cs = computed_style(&doc, p, &ComputedStyle::initial(), &author);
        assert_eq!(cs.letter_spacing, 3.0);
        // Inherits to a child, and `normal` resets it to 0.
        let span = one(&mut doc, "span", vec![]);
        let child = computed_style(&doc, span, &cs, &Stylesheet::default());
        assert_eq!(child.letter_spacing, 3.0, "letter-spacing inherits");
        let reset = parse_stylesheet("span { letter-spacing: normal }");
        let child2 = computed_style(&doc, span, &cs, &reset);
        assert_eq!(child2.letter_spacing, 0.0, "normal resets it");
    }

    #[test]
    fn gradient_angle_units() {
        // 90deg / 0.25turn / 100grad all point to the right.
        for v in [
            "linear-gradient(90deg, red, blue)",
            "linear-gradient(0.25turn, red, blue)",
            "linear-gradient(100grad, red, blue)",
        ] {
            let g = parse_linear_gradient(v, Color::BLACK).expect(v);
            assert_eq!(g.dir, GradientDir::ToRight, "{v}");
        }
    }

    #[test]
    fn text_decoration_propagates_to_descendants() {
        // <a><span>…</span></a>: the link's underline propagates to the span,
        // but a child's explicit `text-decoration: none` removes it.
        let mut doc = Document::new();
        let a = one(&mut doc, "a", vec![Attribute::new("href", "/x")]);
        let span = doc.create_element(QualName::html("span"), vec![]);
        doc.append(a, span);
        let plain = doc.create_element(
            QualName::html("span"),
            vec![Attribute::new("style", "text-decoration: none")],
        );
        doc.append(a, plain);

        let author = Stylesheet::default();
        let a_cs = computed_style(&doc, a, &ComputedStyle::initial(), &author);
        assert!(a_cs.underline, "the link itself is underlined");
        let span_cs = computed_style(&doc, span, &a_cs, &author);
        assert!(span_cs.underline, "nested span inherits the underline");
        let plain_cs = computed_style(&doc, plain, &a_cs, &author);
        assert!(!plain_cs.underline, "text-decoration:none overrides the inherited one");
    }

    #[test]
    fn ua_figure_is_indented() {
        let mut doc = Document::new();
        let fig = one(&mut doc, "figure", vec![]);
        let cs = computed_style(&doc, fig, &ComputedStyle::initial(), &Stylesheet::default());
        assert_eq!(cs.display, Display::Block);
        assert_eq!(cs.margin.left, 40.0, "figure indents 40px like browsers");
        assert_eq!(cs.margin.right, 40.0);
    }

    #[test]
    fn ua_fieldset_has_border_and_block() {
        let mut doc = Document::new();
        let fs = one(&mut doc, "fieldset", vec![]);
        let cs = computed_style(&doc, fs, &ComputedStyle::initial(), &Stylesheet::default());
        assert_eq!(cs.display, Display::Block);
        assert!(cs.border.top > 0.0, "fieldset has a default border");
        let legend = one(&mut doc, "legend", vec![]);
        let cl = computed_style(&doc, legend, &ComputedStyle::initial(), &Stylesheet::default());
        assert_eq!(cl.display, Display::Block);
        assert!(cl.bold, "legend is bold by default");
    }

    #[test]
    fn presentational_attributes_map_to_css() {
        let mut doc = Document::new();
        // <td align=center bgcolor="ff0000"> → text-align + background-color.
        let td = one(
            &mut doc,
            "td",
            vec![
                Attribute::new("align", "center"),
                Attribute::new("bgcolor", "ff0000"),
            ],
        );
        let cs = computed_style(&doc, td, &ComputedStyle::initial(), &Stylesheet::default());
        assert_eq!(cs.text_align, TextAlign::Center);
        assert_eq!(cs.background_color, Color::rgb(255, 0, 0));

        // An author rule still overrides a presentational hint.
        let td2 = one(&mut doc, "td", vec![Attribute::new("align", "center")]);
        let author = parse_stylesheet("td { text-align: right }");
        let cs2 = computed_style(&doc, td2, &ComputedStyle::initial(), &author);
        assert_eq!(cs2.text_align, TextAlign::Right, "author rule beats the hint");

        // <img align=right> floats the image.
        let img = one(&mut doc, "img", vec![Attribute::new("align", "right")]);
        let cs3 = computed_style(&doc, img, &ComputedStyle::initial(), &Stylesheet::default());
        assert_eq!(cs3.float, Float::Right);

        // <ol type=A> selects the upper-alpha marker.
        let ol = one(&mut doc, "ol", vec![Attribute::new("type", "A")]);
        let cs4 = computed_style(&doc, ol, &ComputedStyle::initial(), &Stylesheet::default());
        assert_eq!(cs4.list_style, ListStyle::UpperAlpha);

        // <td nowrap> suppresses wrapping in the cell.
        let td = one(&mut doc, "td", vec![Attribute::new("nowrap", "")]);
        let cs5 = computed_style(&doc, td, &ComputedStyle::initial(), &Stylesheet::default());
        assert!(cs5.nowrap, "td nowrap → white-space: nowrap");

        // <font size=5 color=red> maps to font-size + color.
        let font = one(
            &mut doc,
            "font",
            vec![Attribute::new("size", "5"), Attribute::new("color", "red")],
        );
        let cs6 = computed_style(&doc, font, &ComputedStyle::initial(), &Stylesheet::default());
        assert_eq!(cs6.font_size, 24.0, "font size=5 → 24px");
        assert_eq!(cs6.color, Color::rgb(255, 0, 0));

        // <caption align=bottom> → caption-side: bottom.
        let cap = one(&mut doc, "caption", vec![Attribute::new("align", "bottom")]);
        let cs7 = computed_style(&doc, cap, &ComputedStyle::initial(), &Stylesheet::default());
        assert!(cs7.caption_side_bottom, "caption align=bottom → caption-side: bottom");
    }

    #[test]
    fn table_border_attr_borders_cells() {
        // <table border="1"><tr><td>…: the cell gets a 1px border; border="0" none.
        let mut doc = Document::new();
        let root = doc.root();
        let table = doc.create_element(QualName::html("table"), vec![Attribute::new("border", "1")]);
        doc.append(root, table);
        let tr = doc.create_element(QualName::html("tr"), vec![]);
        doc.append(table, tr);
        let td = doc.create_element(QualName::html("td"), vec![]);
        doc.append(tr, td);
        let cs = computed_style(&doc, td, &ComputedStyle::initial(), &Stylesheet::default());
        assert!(cs.border.top > 0.0, "cell inherits a border from table[border=1]");

        let table0 = doc.create_element(QualName::html("table"), vec![Attribute::new("border", "0")]);
        doc.append(root, table0);
        let tr0 = doc.create_element(QualName::html("tr"), vec![]);
        doc.append(table0, tr0);
        let td0 = doc.create_element(QualName::html("td"), vec![]);
        doc.append(tr0, td0);
        let cs0 = computed_style(&doc, td0, &ComputedStyle::initial(), &Stylesheet::default());
        assert_eq!(cs0.border.top, 0.0, "border=0 draws no cell border");

        // <table cellpadding="10"> sets every cell's padding (over the UA 4px).
        let tablep = doc.create_element(QualName::html("table"), vec![Attribute::new("cellpadding", "10")]);
        doc.append(root, tablep);
        let trp = doc.create_element(QualName::html("tr"), vec![]);
        doc.append(tablep, trp);
        let tdp = doc.create_element(QualName::html("td"), vec![]);
        doc.append(trp, tdp);
        let csp = computed_style(&doc, tdp, &ComputedStyle::initial(), &Stylesheet::default());
        assert_eq!(csp.padding.top, 10.0, "cellpadding sets cell padding");
    }

    #[test]
    fn hidden_input_is_display_none() {
        let mut doc = Document::new();
        let hidden = one(&mut doc, "input", vec![Attribute::new("type", "hidden")]);
        let text = one(&mut doc, "input", vec![Attribute::new("type", "text")]);
        let cs_hidden = computed_style(&doc, hidden, &ComputedStyle::initial(), &Stylesheet::default());
        let cs_text = computed_style(&doc, text, &ComputedStyle::initial(), &Stylesheet::default());
        assert_eq!(cs_hidden.display, Display::None, "type=hidden is not rendered");
        assert_eq!(cs_text.display, Display::Block, "type=text still renders");
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

    #[test]
    fn font_shorthand_extracts_size_weight_line_height() {
        let mut doc = Document::new();
        let d = one(&mut doc, "div", vec![]);
        let cs = computed_style(
            &doc,
            d,
            &ComputedStyle::initial(),
            &parse_stylesheet("div { font: italic bold 20px/1.5 Helvetica, sans-serif }"),
        );
        assert_eq!(cs.font_size, 20.0);
        assert!(cs.bold);
        assert_eq!(cs.line_height, 1.5);
        // An explicit font-size longhand overrides the shorthand.
        let cs2 = computed_style(
            &doc,
            d,
            &ComputedStyle::initial(),
            &parse_stylesheet("div { font: 20px serif; font-size: 30px }"),
        );
        assert_eq!(cs2.font_size, 30.0);
    }

    #[test]
    fn inset_shorthand_expands() {
        let mut doc = Document::new();
        let d = one(&mut doc, "div", vec![]);
        // 4 values → top/right/bottom/left; `auto` clears.
        let cs = computed_style(
            &doc,
            d,
            &ComputedStyle::initial(),
            &parse_stylesheet("div { inset: 1px 2px 3px auto }"),
        );
        assert_eq!(cs.inset_top, Some(Length::Px(1.0)));
        assert_eq!(cs.inset_right, Some(Length::Px(2.0)));
        assert_eq!(cs.inset_bottom, Some(Length::Px(3.0)));
        assert_eq!(cs.inset_left, None);
        // 1 value → all four.
        let cs2 = computed_style(
            &doc,
            d,
            &ComputedStyle::initial(),
            &parse_stylesheet("div { inset: 5px }"),
        );
        assert_eq!(cs2.inset_top, Some(Length::Px(5.0)));
        assert_eq!(cs2.inset_left, Some(Length::Px(5.0)));
    }

    #[test]
    fn linear_gradient_multi_stop_positions() {
        // Three stops: red 0%, white at midpoint (default 50%), blue 100%.
        let g = parse_linear_gradient("linear-gradient(to right, red, white, blue)", Color::BLACK).unwrap();
        assert_eq!(g.n_stops, 3);
        assert_eq!(g.stops[0].1, 0.0);
        assert!((g.stops[1].1 - 0.5).abs() < 1e-6);
        assert_eq!(g.stops[2].1, 1.0);
        // The midpoint color is the middle stop, not a red→blue blend.
        let mid = g.color_at(0.5);
        assert_eq!((mid.r, mid.g, mid.b), (255, 255, 255));
    }

    #[test]
    fn linear_gradient_explicit_percent_stops() {
        // A hard stop: red up to 30%, then blue from 30% on.
        let g = parse_linear_gradient("linear-gradient(red 30%, blue 30%)", Color::BLACK).unwrap();
        assert_eq!(g.n_stops, 2);
        assert!((g.stops[0].1 - 0.3).abs() < 1e-6);
        assert!((g.stops[1].1 - 0.3).abs() < 1e-6);
        // Just below the stop is red; at/after the stop is blue.
        assert_eq!(g.color_at(0.2).r, 255);
        assert_eq!(g.color_at(0.4).b, 255);
    }

    #[test]
    fn radial_gradient_drops_shape_prefix() {
        // The `circle` prefix carries no color and must not become a stop.
        let g = parse_radial_gradient("radial-gradient(circle, yellow, green)", Color::BLACK).unwrap();
        assert_eq!(g.n_stops, 2);
        assert_eq!(g.from, parse_color("yellow").unwrap());
        assert_eq!(g.to, parse_color("green").unwrap());
    }

    #[test]
    fn logical_properties_map_to_physical() {
        let mut doc = Document::new();
        let d = one(&mut doc, "div", vec![]);
        let cs = computed_style(
            &doc,
            d,
            &ComputedStyle::initial(),
            &parse_stylesheet(
                "div { inline-size: 200px; block-size: 80px; \
                 margin-inline: 10px 20px; padding-block-start: 5px; \
                 padding-inline-end: 7px }",
            ),
        );
        // inline/block sizing → width/height (horizontal-tb).
        assert_eq!(cs.width, Some(Length::Px(200.0)));
        assert_eq!(cs.height, Some(Length::Px(80.0)));
        // margin-inline: 10px 20px → left 10, right 20.
        assert_eq!(cs.margin.left, 10.0);
        assert_eq!(cs.margin.right, 20.0);
        // padding-block-start → top; padding-inline-end → right.
        assert_eq!(cs.padding.top, 5.0);
        assert_eq!(cs.padding.right, 7.0);
    }

    #[test]
    fn logical_inset_maps_to_physical() {
        let mut doc = Document::new();
        let d = one(&mut doc, "div", vec![]);
        let cs = computed_style(
            &doc,
            d,
            &ComputedStyle::initial(),
            &parse_stylesheet(
                "div { position: absolute; inset-inline: 4px 8px; inset-block-start: 3px }",
            ),
        );
        assert_eq!(cs.inset_left, Some(Length::Px(4.0)));
        assert_eq!(cs.inset_right, Some(Length::Px(8.0)));
        assert_eq!(cs.inset_top, Some(Length::Px(3.0)));
    }

    #[test]
    fn box_shadow_color_defaults_to_current_color() {
        let mut doc = Document::new();
        let d = one(&mut doc, "div", vec![]);
        // No color token → the shadow takes the element's `color` (spec default),
        // and an explicit `currentColor` resolves the same way.
        for decl in [
            "div { color: #ff0000; box-shadow: 0 2px 4px }",
            "div { color: #ff0000; box-shadow: 0 2px 4px currentColor }",
        ] {
            let cs = computed_style(
                &doc,
                d,
                &ComputedStyle::initial(),
                &parse_stylesheet(decl),
            );
            let (_, _, _, _, c) = cs.box_shadow.expect("shadow parsed");
            assert_eq!(c, Color { r: 255, g: 0, b: 0, a: 255 }, "{decl}");
        }
    }

    #[test]
    fn gradient_currentcolor_resolves_to_text_color() {
        let red = Color { r: 255, g: 0, b: 0, a: 255 };
        // `currentColor` in a stop takes the element's resolved text color.
        let g = parse_linear_gradient("linear-gradient(currentColor, transparent)", red).unwrap();
        assert_eq!(g.from, red);
        // And it resolves end-to-end through the cascade with `color` set.
        let mut doc = Document::new();
        let d = one(&mut doc, "div", vec![]);
        let cs = computed_style(
            &doc,
            d,
            &ComputedStyle::initial(),
            &parse_stylesheet(
                "div { color: #00ff00; background: linear-gradient(currentColor, #000) }",
            ),
        );
        let grad = cs.background_gradient.expect("gradient parsed");
        assert_eq!(grad.from, Color { r: 0, g: 255, b: 0, a: 255 });
    }
}
