//! Value parsing: colors and lengths.

use argus_geometry::Color;

/// A CSS length, kept in its specified unit until resolved against a font size.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Length {
    Px(f32),
    Em(f32),
    Percent(f32),
    Zero,
    /// A simplified `calc()`: the sum of a pixel, em, and percent term (each may be
    /// zero or negative). Covers the common `calc(100% - 80px)` / `calc(50% + 2em)`
    /// shapes; multiplication/division and nesting fall back to no-value.
    Calc { px: f32, em: f32, pct: f32 },
    /// `min()`/`max()`/`clamp()` over `calc`-style terms (each `[px, em, pct]`),
    /// resolved at use time. For `min`/`max` only `a`/`b` are used; for `clamp` the
    /// terms are `(min, value, max)`.
    Math { op: MathOp, a: [f32; 3], b: [f32; 3], c: [f32; 3] },
}

/// Which comparison [`Length::Math`] applies.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MathOp {
    Min,
    Max,
    Clamp,
}

impl Length {
    /// Resolve to pixels given the relevant `font_size` (for `em`) and a
    /// `percent_base` (for `%`).
    pub fn to_px(self, font_size: f32, percent_base: f32) -> f32 {
        let resolve3 = |t: [f32; 3]| t[0] + t[1] * font_size + t[2] / 100.0 * percent_base;
        match self {
            Length::Px(v) => v,
            Length::Em(v) => v * font_size,
            Length::Percent(v) => v / 100.0 * percent_base,
            Length::Zero => 0.0,
            Length::Calc { px, em, pct } => px + em * font_size + pct / 100.0 * percent_base,
            Length::Math { op, a, b, c } => {
                let (ra, rb, rc) = (resolve3(a), resolve3(b), resolve3(c));
                match op {
                    MathOp::Min => ra.min(rb),
                    MathOp::Max => ra.max(rb),
                    // clamp(min, value, max) — and never let min exceed max.
                    MathOp::Clamp => rb.clamp(ra.min(rc), rc.max(ra)),
                }
            }
        }
    }
}

/// Parse a length from a trimmed value string (e.g. `12px`, `1.5em`, `50%`, `0`,
/// `10pt`, `2cm`). Absolute units resolve to px via the CSS reference pixel
/// (`96px = 1in`); `em`/`rem` stay relative until resolved against a font size.
pub fn parse_length(s: &str) -> Option<Length> {
    let s = s.trim();
    if s == "0" {
        return Some(Length::Zero);
    }
    // `calc(...)` (case-insensitive keyword) → a summed px/em/% term. Compare the
    // ASCII prefix on bytes so arbitrary UTF-8 input can't split a char boundary.
    let b = s.as_bytes();
    if b.len() > 5 && b[..5].eq_ignore_ascii_case(b"calc(") && s.ends_with(')') {
        return parse_calc(s[5..s.len() - 1].trim());
    }
    // `min()`/`max()`/`clamp()` over comma-separated terms (each a length or calc).
    for (kw, op) in [
        ("min(", MathOp::Min),
        ("max(", MathOp::Max),
        ("clamp(", MathOp::Clamp),
    ] {
        let n = kw.len();
        if b.len() > n && b[..n].eq_ignore_ascii_case(kw.as_bytes()) && s.ends_with(')') {
            return parse_math(s[n..s.len() - 1].trim(), op);
        }
    }
    if let Some(num) = s.strip_suffix('%') {
        return num.trim().parse().ok().map(Length::Percent);
    }
    // Font-relative units. `rem` must be checked before `em` (else `em` matches the
    // `…rem` suffix and fails to parse the leftover `r`).
    if let Some(num) = s.strip_suffix("rem") {
        return num.trim().parse().ok().map(Length::Em); // rem≈em until root size lands
    }
    if let Some(num) = s.strip_suffix("em") {
        return num.trim().parse().ok().map(Length::Em);
    }
    // Absolute units → px.
    const ABS: &[(&str, f32)] = &[
        ("px", 1.0),
        ("pt", 96.0 / 72.0),
        ("pc", 16.0),
        ("in", 96.0),
        ("cm", 96.0 / 2.54),
        ("mm", 96.0 / 25.4),
        ("q", 96.0 / 101.6),
    ];
    for (suffix, mult) in ABS {
        if let Some(num) = s.strip_suffix(suffix) {
            return num.trim().parse::<f32>().ok().map(|v| Length::Px(v * mult));
        }
    }
    // Bare number → treat as px (lenient).
    s.parse().ok().map(Length::Px)
}

/// Parse a `calc()` body as a sum of space-separated `± <length>` terms (the CSS
/// grammar requires whitespace around binary `+`/`-`). Multiplication/division,
/// nested `calc()`/`var()`, and malformed input return `None` (no value), so the
/// property falls back to its initial value rather than mis-resolving.
/// A `[px, em, pct]` term from a single length/calc token (for `min`/`max`/`clamp`
/// arguments). Returns `None` for nested math or unparseable input.
fn term_triple(s: &str) -> Option<[f32; 3]> {
    match parse_length(s.trim())? {
        Length::Px(v) => Some([v, 0.0, 0.0]),
        Length::Em(v) => Some([0.0, v, 0.0]),
        Length::Percent(v) => Some([0.0, 0.0, v]),
        Length::Zero => Some([0.0, 0.0, 0.0]),
        Length::Calc { px, em, pct } => Some([px, em, pct]),
        Length::Math { .. } => None,
    }
}

/// Split a function body on top-level commas (ignoring commas inside nested parens).
fn split_top_commas(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    for (i, c) in body.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(body[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(body[start..].trim());
    out
}

/// Parse a `min()`/`max()`/`clamp()` body into a [`Length::Math`]. `min`/`max` take
/// two arguments, `clamp` three; anything else is unsupported (no value).
fn parse_math(body: &str, op: MathOp) -> Option<Length> {
    let args = split_top_commas(body);
    let z = [0.0; 3];
    match (op, args.len()) {
        (MathOp::Clamp, 3) => Some(Length::Math {
            op,
            a: term_triple(args[0])?,
            b: term_triple(args[1])?,
            c: term_triple(args[2])?,
        }),
        (MathOp::Min | MathOp::Max, 2) => Some(Length::Math {
            op,
            a: term_triple(args[0])?,
            b: term_triple(args[1])?,
            c: z,
        }),
        _ => None,
    }
}

fn parse_calc(body: &str) -> Option<Length> {
    if body.contains("calc(") || body.contains("var(") {
        return None;
    }
    let (mut px, mut em, mut pct) = (0.0f32, 0.0f32, 0.0f32);
    let mut sign = 1.0f32;
    let mut expect_term = true;
    for tok in body.split_whitespace() {
        if expect_term {
            // A term must be a plain length — reject `*`/`/` (unsupported here).
            if tok.contains(['*', '/']) {
                return None;
            }
            match parse_length(tok)? {
                Length::Px(v) => px += sign * v,
                Length::Em(v) => em += sign * v,
                Length::Percent(v) => pct += sign * v,
                Length::Zero => {}
                Length::Calc { .. } | Length::Math { .. } => return None,
            }
        } else {
            sign = match tok {
                "+" => 1.0,
                "-" => -1.0,
                _ => return None,
            };
        }
        expect_term = !expect_term;
    }
    // `expect_term` is true again only after an operator (or empty input) — i.e. a
    // dangling `+`/`-` or nothing at all, both invalid.
    if expect_term {
        return None;
    }
    Some(Length::Calc { px, em, pct })
}

/// Parse a color: `#rgb`, `#rrggbb`, `#rgba`, `#rrggbbaa`, `rgb()`/`rgba()`,
/// `hsl()`/`hsla()`, or a named color.
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex(hex);
    }
    if let Some(inner) = s.strip_prefix("rgb(").and_then(|s| s.strip_suffix(')')) {
        return parse_rgb(inner, false);
    }
    if let Some(inner) = s.strip_prefix("rgba(").and_then(|s| s.strip_suffix(')')) {
        return parse_rgb(inner, true);
    }
    if let Some(inner) = s.strip_prefix("hsl(").and_then(|s| s.strip_suffix(')')) {
        return parse_hsl(inner);
    }
    if let Some(inner) = s.strip_prefix("hsla(").and_then(|s| s.strip_suffix(')')) {
        return parse_hsl(inner);
    }
    let sb = s.as_bytes();
    if sb.len() > 10 && sb[..10].eq_ignore_ascii_case(b"color-mix(") && s.ends_with(')') {
        return parse_color_mix(&s[10..s.len() - 1]);
    }
    named_color(&s.to_ascii_lowercase())
}

/// Parse `color-mix(in <space>, <c1> [<p1>%], <c2> [<p2>%])` — a weighted blend of
/// two colors (mixed component-wise in sRGB; the color space is accepted but
/// always treated as sRGB). Omitted percentages default so the two sum to 100%.
fn parse_color_mix(body: &str) -> Option<Color> {
    let args = split_top_commas(body);
    if args.len() != 3 || !args[0].trim().to_ascii_lowercase().starts_with("in ") {
        return None;
    }
    // Each color arg is `<color> [<pct>%]` (percentage optional).
    let parse_arg = |a: &str| -> Option<(Color, Option<f32>)> {
        let a = a.trim();
        if let Some(idx) = a.rfind('%') {
            // The token before `%` is the percentage; the rest is the color.
            let (head, _) = a.split_at(idx);
            if let Some(sp) = head.rfind(char::is_whitespace) {
                let pct: f32 = head[sp..].trim().parse().ok()?;
                let col = parse_color(head[..sp].trim())?;
                return Some((col, Some(pct)));
            }
        }
        Some((parse_color(a)?, None))
    };
    let (c1, p1) = parse_arg(args[1])?;
    let (c2, p2) = parse_arg(args[2])?;
    // Resolve weights: if one is missing it's `100 - other`; if both missing, 50/50.
    let (w1, w2) = match (p1, p2) {
        (Some(a), Some(b)) => (a, b),
        (Some(a), None) => (a, 100.0 - a),
        (None, Some(b)) => (100.0 - b, b),
        (None, None) => (50.0, 50.0),
    };
    let total = w1 + w2;
    if total <= 0.0 {
        return None;
    }
    let (f1, f2) = (w1 / total, w2 / total);
    let mix = |a: u8, b: u8| (a as f32 * f1 + b as f32 * f2).round().clamp(0.0, 255.0) as u8;
    Some(Color::rgba(
        mix(c1.r, c2.r),
        mix(c1.g, c2.g),
        mix(c1.b, c2.b),
        mix(c1.a, c2.a),
    ))
}

/// Parse `hsl()`/`hsla()` body — `H[deg], S%, L%[, A]` (comma- or space/slash-
/// separated) — converting to RGBA.
fn parse_hsl(inner: &str) -> Option<Color> {
    let parts: Vec<&str> = inner
        .split([',', '/', ' '])
        .filter(|p| !p.trim().is_empty())
        .collect();
    if parts.len() < 3 {
        return None;
    }
    // Hue as an angle, wrapped to [0,360). Accepts a bare number (degrees) or a
    // `deg`/`grad`/`rad`/`turn` unit.
    let h = parse_hue(parts[0].trim())?.rem_euclid(360.0);
    let pct = |s: &str| -> Option<f32> {
        Some(s.trim().trim_end_matches('%').trim().parse::<f32>().ok()? / 100.0)
    };
    let s = pct(parts[1])?.clamp(0.0, 1.0);
    let l = pct(parts[2])?.clamp(0.0, 1.0);
    let a = if parts.len() >= 4 {
        parse_alpha(parts[3])?
    } else {
        255
    };
    let (r, g, b) = hsl_to_rgb(h, s, l);
    Some(Color::rgba(r, g, b, a))
}

/// Parse a CSS alpha component to 0–255: either a `0..=1` number (`0.5`) or a
/// percentage (`50%`), clamped to range.
fn parse_alpha(s: &str) -> Option<u8> {
    let s = s.trim();
    let frac = if let Some(p) = s.strip_suffix('%') {
        p.trim().parse::<f32>().ok()? / 100.0
    } else {
        s.parse::<f32>().ok()?
    };
    Some((frac.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// Parse a CSS `<angle>` to degrees: a bare number, or a `deg`/`grad`/`rad`/`turn`
/// unit (`1turn = 360deg`, `400grad = 360deg`, `2π rad = 360deg`).
fn parse_hue(s: &str) -> Option<f32> {
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

/// Convert HSL (`h` in degrees, `s`/`l` in `0..=1`) to 8-bit RGB.
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to8 = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to8(r1), to8(g1), to8(b1))
}

fn parse_hex(hex: &str) -> Option<Color> {
    let bytes = hex.as_bytes();
    let h = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    match bytes.len() {
        3 => {
            let r = h(bytes[0])?;
            let g = h(bytes[1])?;
            let b = h(bytes[2])?;
            Some(Color::rgb(r * 17, g * 17, b * 17))
        }
        4 => {
            let r = h(bytes[0])?;
            let g = h(bytes[1])?;
            let b = h(bytes[2])?;
            let a = h(bytes[3])?;
            Some(Color::rgba(r * 17, g * 17, b * 17, a * 17))
        }
        6 => Some(Color::rgb(
            hex_byte(&hex[0..2])?,
            hex_byte(&hex[2..4])?,
            hex_byte(&hex[4..6])?,
        )),
        8 => Some(Color::rgba(
            hex_byte(&hex[0..2])?,
            hex_byte(&hex[2..4])?,
            hex_byte(&hex[4..6])?,
            hex_byte(&hex[6..8])?,
        )),
        _ => None,
    }
}

fn hex_byte(s: &str) -> Option<u8> {
    u8::from_str_radix(s, 16).ok()
}

fn parse_rgb(inner: &str, _alpha: bool) -> Option<Color> {
    let parts: Vec<&str> = inner
        .split([',', '/', ' '])
        .filter(|p| !p.trim().is_empty())
        .collect();
    if parts.len() < 3 {
        return None;
    }
    let chan = |s: &str| -> Option<u8> {
        let s = s.trim();
        if let Some(p) = s.strip_suffix('%') {
            p.trim()
                .parse::<f32>()
                .ok()
                .map(|v| (v / 100.0 * 255.0).round() as u8)
        } else {
            s.parse::<f32>()
                .ok()
                .map(|v| v.round().clamp(0.0, 255.0) as u8)
        }
    };
    let r = chan(parts[0])?;
    let g = chan(parts[1])?;
    let b = chan(parts[2])?;
    let a = if parts.len() >= 4 {
        parse_alpha(parts[3])?
    } else {
        255
    };
    Some(Color::rgba(r, g, b, a))
}

/// A small subset of CSS named colors.
fn named_color(name: &str) -> Option<Color> {
    let rgb = |r, g, b| Color::rgb(r, g, b);
    Some(match name {
        "transparent" => Color::TRANSPARENT,
        // The full CSS named-color set (CSS Color 4 <named-color>), plus the
        // `grey` spelling aliases. Values are the canonical sRGB triples.
        "aliceblue" => rgb(240, 248, 255),
        "antiquewhite" => rgb(250, 235, 215),
        "aqua" | "cyan" => rgb(0, 255, 255),
        "aquamarine" => rgb(127, 255, 212),
        "azure" => rgb(240, 255, 255),
        "beige" => rgb(245, 245, 220),
        "bisque" => rgb(255, 228, 196),
        "black" => rgb(0, 0, 0),
        "blanchedalmond" => rgb(255, 235, 205),
        "blue" => rgb(0, 0, 255),
        "blueviolet" => rgb(138, 43, 226),
        "brown" => rgb(165, 42, 42),
        "burlywood" => rgb(222, 184, 135),
        "cadetblue" => rgb(95, 158, 160),
        "chartreuse" => rgb(127, 255, 0),
        "chocolate" => rgb(210, 105, 30),
        "coral" => rgb(255, 127, 80),
        "cornflowerblue" => rgb(100, 149, 237),
        "cornsilk" => rgb(255, 248, 220),
        "crimson" => rgb(220, 20, 60),
        "darkblue" => rgb(0, 0, 139),
        "darkcyan" => rgb(0, 139, 139),
        "darkgoldenrod" => rgb(184, 134, 11),
        "darkgray" | "darkgrey" => rgb(169, 169, 169),
        "darkgreen" => rgb(0, 100, 0),
        "darkkhaki" => rgb(189, 183, 107),
        "darkmagenta" => rgb(139, 0, 139),
        "darkolivegreen" => rgb(85, 107, 47),
        "darkorange" => rgb(255, 140, 0),
        "darkorchid" => rgb(153, 50, 204),
        "darkred" => rgb(139, 0, 0),
        "darksalmon" => rgb(233, 150, 122),
        "darkseagreen" => rgb(143, 188, 143),
        "darkslateblue" => rgb(72, 61, 139),
        "darkslategray" | "darkslategrey" => rgb(47, 79, 79),
        "darkturquoise" => rgb(0, 206, 209),
        "darkviolet" => rgb(148, 0, 211),
        "deeppink" => rgb(255, 20, 147),
        "deepskyblue" => rgb(0, 191, 255),
        "dimgray" | "dimgrey" => rgb(105, 105, 105),
        "dodgerblue" => rgb(30, 144, 255),
        "firebrick" => rgb(178, 34, 34),
        "floralwhite" => rgb(255, 250, 240),
        "forestgreen" => rgb(34, 139, 34),
        "fuchsia" | "magenta" => rgb(255, 0, 255),
        "gainsboro" => rgb(220, 220, 220),
        "ghostwhite" => rgb(248, 248, 255),
        "gold" => rgb(255, 215, 0),
        "goldenrod" => rgb(218, 165, 32),
        "gray" | "grey" => rgb(128, 128, 128),
        "green" => rgb(0, 128, 0),
        "greenyellow" => rgb(173, 255, 47),
        "honeydew" => rgb(240, 255, 240),
        "hotpink" => rgb(255, 105, 180),
        "indianred" => rgb(205, 92, 92),
        "indigo" => rgb(75, 0, 130),
        "ivory" => rgb(255, 255, 240),
        "khaki" => rgb(240, 230, 140),
        "lavender" => rgb(230, 230, 250),
        "lavenderblush" => rgb(255, 240, 245),
        "lawngreen" => rgb(124, 252, 0),
        "lemonchiffon" => rgb(255, 250, 205),
        "lightblue" => rgb(173, 216, 230),
        "lightcoral" => rgb(240, 128, 128),
        "lightcyan" => rgb(224, 255, 255),
        "lightgoldenrodyellow" => rgb(250, 250, 210),
        "lightgray" | "lightgrey" => rgb(211, 211, 211),
        "lightgreen" => rgb(144, 238, 144),
        "lightpink" => rgb(255, 182, 193),
        "lightsalmon" => rgb(255, 160, 122),
        "lightseagreen" => rgb(32, 178, 170),
        "lightskyblue" => rgb(135, 206, 250),
        "lightslategray" | "lightslategrey" => rgb(119, 136, 153),
        "lightsteelblue" => rgb(176, 196, 222),
        "lightyellow" => rgb(255, 255, 224),
        "lime" => rgb(0, 255, 0),
        "limegreen" => rgb(50, 205, 50),
        "linen" => rgb(250, 240, 230),
        "maroon" => rgb(128, 0, 0),
        "mediumaquamarine" => rgb(102, 205, 170),
        "mediumblue" => rgb(0, 0, 205),
        "mediumorchid" => rgb(186, 85, 211),
        "mediumpurple" => rgb(147, 112, 219),
        "mediumseagreen" => rgb(60, 179, 113),
        "mediumslateblue" => rgb(123, 104, 238),
        "mediumspringgreen" => rgb(0, 250, 154),
        "mediumturquoise" => rgb(72, 209, 204),
        "mediumvioletred" => rgb(199, 21, 133),
        "midnightblue" => rgb(25, 25, 112),
        "mintcream" => rgb(245, 255, 250),
        "mistyrose" => rgb(255, 228, 225),
        "moccasin" => rgb(255, 228, 181),
        "navajowhite" => rgb(255, 222, 173),
        "navy" => rgb(0, 0, 128),
        "oldlace" => rgb(253, 245, 230),
        "olive" => rgb(128, 128, 0),
        "olivedrab" => rgb(107, 142, 35),
        "orange" => rgb(255, 165, 0),
        "orangered" => rgb(255, 69, 0),
        "orchid" => rgb(218, 112, 214),
        "palegoldenrod" => rgb(238, 232, 170),
        "palegreen" => rgb(152, 251, 152),
        "paleturquoise" => rgb(175, 238, 238),
        "palevioletred" => rgb(219, 112, 147),
        "papayawhip" => rgb(255, 239, 213),
        "peachpuff" => rgb(255, 218, 185),
        "peru" => rgb(205, 133, 63),
        "pink" => rgb(255, 192, 203),
        "plum" => rgb(221, 160, 221),
        "powderblue" => rgb(176, 224, 230),
        "purple" => rgb(128, 0, 128),
        "rebeccapurple" => rgb(102, 51, 153),
        "red" => rgb(255, 0, 0),
        "rosybrown" => rgb(188, 143, 143),
        "royalblue" => rgb(65, 105, 225),
        "saddlebrown" => rgb(139, 69, 19),
        "salmon" => rgb(250, 128, 114),
        "sandybrown" => rgb(244, 164, 96),
        "seagreen" => rgb(46, 139, 87),
        "seashell" => rgb(255, 245, 238),
        "sienna" => rgb(160, 82, 45),
        "silver" => rgb(192, 192, 192),
        "skyblue" => rgb(135, 206, 235),
        "slateblue" => rgb(106, 90, 205),
        "slategray" | "slategrey" => rgb(112, 128, 144),
        "snow" => rgb(255, 250, 250),
        "springgreen" => rgb(0, 255, 127),
        "steelblue" => rgb(70, 130, 180),
        "tan" => rgb(210, 180, 140),
        "teal" => rgb(0, 128, 128),
        "thistle" => rgb(216, 191, 216),
        "tomato" => rgb(255, 99, 71),
        "turquoise" => rgb(64, 224, 208),
        "violet" => rgb(238, 130, 238),
        "wheat" => rgb(245, 222, 179),
        "white" => rgb(255, 255, 255),
        "whitesmoke" => rgb(245, 245, 245),
        "yellow" => rgb(255, 255, 0),
        "yellowgreen" => rgb(154, 205, 50),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(parse_color("#fff"), Some(Color::rgb(255, 255, 255)));
        assert_eq!(parse_color("#ff8800"), Some(Color::rgb(255, 136, 0)));
        assert_eq!(parse_color("rgb(255, 0, 0)"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(
            parse_color("rgba(0,0,0,0.5)"),
            Some(Color::rgba(0, 0, 0, 128))
        );
        assert_eq!(parse_color("teal"), Some(Color::rgb(0, 128, 128)));
        // Extended named colors + grey-spelling aliases (case-insensitive).
        assert_eq!(parse_color("rebeccapurple"), Some(Color::rgb(102, 51, 153)));
        assert_eq!(parse_color("CornflowerBlue"), Some(Color::rgb(100, 149, 237)));
        assert_eq!(parse_color("lightgray"), parse_color("lightgrey"));
        assert_eq!(parse_color("gold"), Some(Color::rgb(255, 215, 0)));
        assert_eq!(parse_color("bogus"), None);
        // CSS Color 4: space-separated channels with a percentage alpha.
        assert_eq!(
            parse_color("rgb(255 0 0 / 50%)"),
            Some(Color::rgba(255, 0, 0, 128))
        );
        assert_eq!(
            parse_color("rgba(0, 0, 0, 25%)"),
            Some(Color::rgba(0, 0, 0, 64))
        );
    }

    #[test]
    fn hsl_colors() {
        // Primary/secondary hues at full saturation, 50% lightness.
        assert_eq!(parse_color("hsl(0, 100%, 50%)"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(parse_color("hsl(120, 100%, 50%)"), Some(Color::rgb(0, 255, 0)));
        assert_eq!(parse_color("hsl(240, 100%, 50%)"), Some(Color::rgb(0, 0, 255)));
        // Grayscale: 0 saturation → r=g=b regardless of hue.
        assert_eq!(parse_color("hsl(0, 0%, 50%)"), Some(Color::rgb(128, 128, 128)));
        assert_eq!(parse_color("hsl(0,0%,100%)"), Some(Color::rgb(255, 255, 255)));
        // `deg` suffix, hue wrap-around, and hsla alpha.
        assert_eq!(parse_color("hsl(360deg, 100%, 50%)"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(
            parse_color("hsla(0, 100%, 50%, 0.5)"),
            Some(Color::rgba(255, 0, 0, 128))
        );
        // Space/slash modern syntax.
        assert_eq!(
            parse_color("hsl(120 100% 50% / 1)"),
            Some(Color::rgba(0, 255, 0, 255))
        );
        // Percentage alpha in the slash position.
        assert_eq!(
            parse_color("hsl(240 100% 50% / 50%)"),
            Some(Color::rgba(0, 0, 255, 128))
        );
        // Angle units for hue: 0.5turn and 200grad are both 180° (cyan).
        assert_eq!(parse_color("hsl(0.5turn, 100%, 50%)"), Some(Color::rgb(0, 255, 255)));
        assert_eq!(parse_color("hsl(200grad, 100%, 50%)"), Some(Color::rgb(0, 255, 255)));
    }

    #[test]
    fn color_mix() {
        // 50/50 red+blue → purple (128, 0, 128).
        assert_eq!(parse_color("color-mix(in srgb, red, blue)"), Some(Color::rgb(128, 0, 128)));
        // Weighted: 25% red, 75% blue.
        assert_eq!(
            parse_color("color-mix(in srgb, red 25%, blue 75%)"),
            Some(Color::rgb(64, 0, 191))
        );
        // One percentage given → the other is 100 - it.
        assert_eq!(
            parse_color("color-mix(in srgb, white 100%, black)"),
            Some(Color::rgb(255, 255, 255))
        );
        // Mixing two named/hex colors.
        assert_eq!(
            parse_color("color-mix(in srgb, #000000, #ffffff)"),
            Some(Color::rgb(128, 128, 128))
        );
        assert_eq!(parse_color("color-mix(in srgb, red)"), None); // needs two colors
    }

    #[test]
    fn min_max_clamp() {
        let px = |l: Option<Length>, base: f32| l.unwrap().to_px(16.0, base);
        // min(100%, 500px) against a 800px base → 500; against 300px → 300.
        assert_eq!(px(parse_length("min(100%, 500px)"), 800.0), 500.0);
        assert_eq!(px(parse_length("min(100%, 500px)"), 300.0), 300.0);
        // max(50%, 200px) against 800px → 400; against 300px → 200.
        assert_eq!(px(parse_length("max(50%, 200px)"), 800.0), 400.0);
        assert_eq!(px(parse_length("max(50%, 200px)"), 300.0), 200.0);
        // clamp(200px, 50%, 600px): value 50% of base, clamped to [200,600].
        assert_eq!(px(parse_length("clamp(200px, 50%, 600px)"), 2000.0), 600.0); // 1000→600
        assert_eq!(px(parse_length("clamp(200px, 50%, 600px)"), 800.0), 400.0); // 400 in range
        assert_eq!(px(parse_length("clamp(200px, 50%, 600px)"), 100.0), 200.0); // 50→200
        // An em term resolves against font-size; a calc arg works too.
        assert_eq!(parse_length("min(2em, 40px)").unwrap().to_px(16.0, 0.0), 32.0);
        assert_eq!(parse_length("max(calc(10px + 1em), 50px)").unwrap().to_px(16.0, 0.0), 50.0);
        // Wrong arity → no value.
        assert_eq!(parse_length("clamp(1px, 2px)"), None);
    }

    #[test]
    fn lengths() {
        assert_eq!(parse_length("12px"), Some(Length::Px(12.0)));
        assert_eq!(parse_length("1.5em"), Some(Length::Em(1.5)));
        assert_eq!(parse_length("50%"), Some(Length::Percent(50.0)));
        assert_eq!(parse_length("0"), Some(Length::Zero));
        assert_eq!(Length::Em(2.0).to_px(16.0, 0.0), 32.0);
    }

    #[test]
    fn rem_and_absolute_units() {
        // Regression: `rem` must parse (previously the `em` branch swallowed it).
        assert_eq!(parse_length("2rem"), Some(Length::Em(2.0)));
        // Absolute units resolve to px (96px = 1in).
        assert_eq!(parse_length("1in"), Some(Length::Px(96.0)));
        assert_eq!(parse_length("72pt"), Some(Length::Px(96.0)));
        assert_eq!(parse_length("1pc"), Some(Length::Px(16.0)));
        let cm = parse_length("2.54cm").unwrap().to_px(16.0, 0.0);
        assert!((cm - 96.0).abs() < 0.01, "2.54cm ≈ 96px, got {cm}");
        let mm = parse_length("25.4mm").unwrap().to_px(16.0, 0.0);
        assert!((mm - 96.0).abs() < 0.01, "25.4mm ≈ 96px, got {mm}");
        let q = parse_length("40q").unwrap().to_px(16.0, 0.0);
        assert!((q - 37.795).abs() < 0.01, "40q = 1cm ≈ 37.8px, got {q}");
    }

    #[test]
    fn calc_sums_terms() {
        // `calc(100% - 80px)` against a 500px base → 420px.
        let l = parse_length("calc(100% - 80px)").unwrap();
        assert!((l.to_px(16.0, 500.0) - 420.0).abs() < 0.01, "got {}", l.to_px(16.0, 500.0));
        // Mixed em + percent + px, case-insensitive keyword.
        let l2 = parse_length("CALC(2em + 50% + 10px)").unwrap();
        // 2*16 + 0.5*200 + 10 = 32 + 100 + 10 = 142.
        assert!((l2.to_px(16.0, 200.0) - 142.0).abs() < 0.01, "got {}", l2.to_px(16.0, 200.0));
        // A leading-percent-minus-em form can go negative.
        let l3 = parse_length("calc(10px - 1em)").unwrap();
        assert!((l3.to_px(16.0, 0.0) - (-6.0)).abs() < 0.01);
        // Unsupported shapes (mul/div, nesting, dangling op) yield no value.
        assert_eq!(parse_length("calc(100% / 3)"), None);
        assert_eq!(parse_length("calc(100% -)"), None);
        assert_eq!(parse_length("calc(calc(1px + 2px))"), None);
    }
}
