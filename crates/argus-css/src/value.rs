//! Value parsing: colors and lengths.

use argus_geometry::Color;

/// A CSS length, kept in its specified unit until resolved against a font size.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Length {
    Px(f32),
    Em(f32),
    Percent(f32),
    Zero,
}

impl Length {
    /// Resolve to pixels given the relevant `font_size` (for `em`) and a
    /// `percent_base` (for `%`).
    pub fn to_px(self, font_size: f32, percent_base: f32) -> f32 {
        match self {
            Length::Px(v) => v,
            Length::Em(v) => v * font_size,
            Length::Percent(v) => v / 100.0 * percent_base,
            Length::Zero => 0.0,
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
    named_color(&s.to_ascii_lowercase())
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
    // Hue in degrees (wrapped to [0,360)); a bare number or a `deg` suffix.
    let h_raw: f32 = parts[0].trim().trim_end_matches("deg").trim().parse().ok()?;
    let h = h_raw.rem_euclid(360.0);
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
    Some(match name {
        "transparent" => Color::TRANSPARENT,
        "black" => Color::rgb(0, 0, 0),
        "white" => Color::rgb(255, 255, 255),
        "red" => Color::rgb(255, 0, 0),
        "green" => Color::rgb(0, 128, 0),
        "lime" => Color::rgb(0, 255, 0),
        "blue" => Color::rgb(0, 0, 255),
        "yellow" => Color::rgb(255, 255, 0),
        "cyan" | "aqua" => Color::rgb(0, 255, 255),
        "magenta" | "fuchsia" => Color::rgb(255, 0, 255),
        "gray" | "grey" => Color::rgb(128, 128, 128),
        "silver" => Color::rgb(192, 192, 192),
        "maroon" => Color::rgb(128, 0, 0),
        "olive" => Color::rgb(128, 128, 0),
        "navy" => Color::rgb(0, 0, 128),
        "teal" => Color::rgb(0, 128, 128),
        "purple" => Color::rgb(128, 0, 128),
        "orange" => Color::rgb(255, 165, 0),
        "rebeccapurple" => Color::rgb(102, 51, 153),
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
}
