//! Character-reference decoding.
//!
//! Phase 1 handles numeric references (`&#123;` / `&#x1F;`), the common
//! punctuation/symbol names, the **full Latin-1 supplement** named set
//! (`&eacute;`, `&uuml;`, `&ntilde;`, `&szlig;`, …), and the **Greek letters**
//! plus common **arrow/math** symbols (`&alpha;`, `&Omega;`, `&rarr;`, `&sum;`,
//! `&radic;`, …). The remainder of the ~2200-entry table and the
//! without-semicolon legacy matching rules are deferred — noted in
//! `docs/subsystems/dom.md` — but the entry point is shaped to grow into them.

/// Consume a character reference beginning at `input[*pos] == '&'`, advancing
/// `*pos` past it and returning the decoded text. If the `&` does not begin a
/// valid reference, returns a literal `"&"` and advances past just the `&`.
pub(crate) fn consume_char_ref(input: &[char], pos: &mut usize) -> String {
    debug_assert_eq!(input.get(*pos), Some(&'&'));
    let amp = *pos;
    *pos += 1; // consume '&'

    match input.get(*pos) {
        Some('#') => {
            *pos += 1;
            let hex = matches!(input.get(*pos), Some('x') | Some('X'));
            if hex {
                *pos += 1;
            }
            let mut code: u32 = 0;
            let mut any = false;
            while let Some(&c) = input.get(*pos) {
                let digit = if hex { c.to_digit(16) } else { c.to_digit(10) };
                match digit {
                    Some(v) => {
                        code = code
                            .saturating_mul(if hex { 16 } else { 10 })
                            .saturating_add(v);
                        any = true;
                        *pos += 1;
                    }
                    None => break,
                }
            }
            if !any {
                *pos = amp + 1;
                return "&".to_string();
            }
            if input.get(*pos) == Some(&';') {
                *pos += 1;
            }
            map_codepoint(code).to_string()
        }
        Some(&c) if c.is_ascii_alphanumeric() => {
            let name_start = *pos;
            let mut end = *pos;
            while matches!(input.get(end), Some(c) if c.is_ascii_alphanumeric()) {
                end += 1;
            }
            if input.get(end) == Some(&';') {
                let name: String = input[name_start..end].iter().collect();
                if let Some(s) = named(&name) {
                    *pos = end + 1;
                    return s.to_string();
                }
            }
            *pos = amp + 1;
            "&".to_string()
        }
        _ => {
            *pos = amp + 1;
            "&".to_string()
        }
    }
}

/// Map a numeric code point to a char, applying the null/out-of-range and
/// surrogate fixups (everything invalid becomes U+FFFD) plus the HTML-mandated
/// Windows-1252 override for the C1 range (`0x80`–`0x9F`).
fn map_codepoint(code: u32) -> char {
    if code == 0 || code > 0x10_FFFF || (0xD800..=0xDFFF).contains(&code) {
        return '\u{FFFD}';
    }
    // HTML's numeric-reference table remaps the C1 controls to their Windows-1252
    // characters (e.g. `&#128;` → `€`). Codes with no mapping pass through unchanged.
    if (0x80..=0x9F).contains(&code) {
        if let Some(repl) = win1252_c1(code as u8) {
            return repl;
        }
    }
    char::from_u32(code).unwrap_or('\u{FFFD}')
}

/// The Windows-1252 character for a C1 byte (`0x80`–`0x9F`), per HTML's numeric
/// character-reference override. `None` for the five unmapped slots.
fn win1252_c1(b: u8) -> Option<char> {
    Some(match b {
        0x80 => '\u{20AC}', // €
        0x82 => '\u{201A}', // ‚
        0x83 => '\u{0192}', // ƒ
        0x84 => '\u{201E}', // „
        0x85 => '\u{2026}', // …
        0x86 => '\u{2020}', // †
        0x87 => '\u{2021}', // ‡
        0x88 => '\u{02C6}', // ˆ
        0x89 => '\u{2030}', // ‰
        0x8A => '\u{0160}', // Š
        0x8B => '\u{2039}', // ‹
        0x8C => '\u{0152}', // Œ
        0x8E => '\u{017D}', // Ž
        0x91 => '\u{2018}', // ‘
        0x92 => '\u{2019}', // ’
        0x93 => '\u{201C}', // “
        0x94 => '\u{201D}', // ”
        0x95 => '\u{2022}', // •
        0x96 => '\u{2013}', // –
        0x97 => '\u{2014}', // —
        0x98 => '\u{02DC}', // ˜
        0x99 => '\u{2122}', // ™
        0x9A => '\u{0161}', // š
        0x9B => '\u{203A}', // ›
        0x9C => '\u{0153}', // œ
        0x9E => '\u{017E}', // ž
        0x9F => '\u{0178}', // Ÿ
        _ => return None,   // 0x81, 0x8D, 0x8F, 0x90, 0x9D: no mapping
    })
}

/// A common subset of named references (semicolon-terminated).
fn named(name: &str) -> Option<&'static str> {
    Some(match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => "\u{00A0}",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        "hellip" => "…",
        "mdash" => "—",
        "ndash" => "–",
        "lsquo" => "‘",
        "rsquo" => "’",
        "ldquo" => "“",
        "rdquo" => "”",
        "laquo" => "«",
        "raquo" => "»",
        "deg" => "°",
        "plusmn" => "±",
        "times" => "×",
        "divide" => "÷",
        "frac12" => "½",
        "frac14" => "¼",
        "frac34" => "¾",
        "euro" => "€",
        "pound" => "£",
        "cent" => "¢",
        "yen" => "¥",
        "sect" => "§",
        "para" => "¶",
        "middot" => "·",
        "bull" => "•",
        "shy" => "\u{00AD}",   // soft hyphen (a break opportunity)
        "ensp" => "\u{2002}",
        "emsp" => "\u{2003}",
        "thinsp" => "\u{2009}",
        "zwnj" => "\u{200C}",
        "zwj" => "\u{200D}",
        "dagger" => "†",
        "Dagger" => "‡",
        "permil" => "‰",
        "prime" => "′",
        "Prime" => "″",
        "infin" => "∞",
        "ne" => "≠",
        "le" => "≤",
        "ge" => "≥",
        "micro" => "µ",
        // The Latin-1 supplement named entities (U+00A1–U+00FF) — the accented
        // letters and symbols common in French/German/Spanish/Portuguese text.
        "iexcl" => "\u{00A1}",
        "curren" => "\u{00A4}",
        "brvbar" => "\u{00A6}",
        "uml" => "\u{00A8}",
        "ordf" => "\u{00AA}",
        "not" => "\u{00AC}",
        "macr" => "\u{00AF}",
        "sup2" => "\u{00B2}",
        "sup3" => "\u{00B3}",
        "acute" => "\u{00B4}",
        "cedil" => "\u{00B8}",
        "sup1" => "\u{00B9}",
        "ordm" => "\u{00BA}",
        "iquest" => "\u{00BF}",
        "Agrave" => "\u{00C0}",
        "Aacute" => "\u{00C1}",
        "Acirc" => "\u{00C2}",
        "Atilde" => "\u{00C3}",
        "Auml" => "\u{00C4}",
        "Aring" => "\u{00C5}",
        "AElig" => "\u{00C6}",
        "Ccedil" => "\u{00C7}",
        "Egrave" => "\u{00C8}",
        "Eacute" => "\u{00C9}",
        "Ecirc" => "\u{00CA}",
        "Euml" => "\u{00CB}",
        "Igrave" => "\u{00CC}",
        "Iacute" => "\u{00CD}",
        "Icirc" => "\u{00CE}",
        "Iuml" => "\u{00CF}",
        "ETH" => "\u{00D0}",
        "Ntilde" => "\u{00D1}",
        "Ograve" => "\u{00D2}",
        "Oacute" => "\u{00D3}",
        "Ocirc" => "\u{00D4}",
        "Otilde" => "\u{00D5}",
        "Ouml" => "\u{00D6}",
        "Oslash" => "\u{00D8}",
        "Ugrave" => "\u{00D9}",
        "Uacute" => "\u{00DA}",
        "Ucirc" => "\u{00DB}",
        "Uuml" => "\u{00DC}",
        "Yacute" => "\u{00DD}",
        "THORN" => "\u{00DE}",
        "szlig" => "\u{00DF}",
        "agrave" => "\u{00E0}",
        "aacute" => "\u{00E1}",
        "acirc" => "\u{00E2}",
        "atilde" => "\u{00E3}",
        "auml" => "\u{00E4}",
        "aring" => "\u{00E5}",
        "aelig" => "\u{00E6}",
        "ccedil" => "\u{00E7}",
        "egrave" => "\u{00E8}",
        "eacute" => "\u{00E9}",
        "ecirc" => "\u{00EA}",
        "euml" => "\u{00EB}",
        "igrave" => "\u{00EC}",
        "iacute" => "\u{00ED}",
        "icirc" => "\u{00EE}",
        "iuml" => "\u{00EF}",
        "eth" => "\u{00F0}",
        "ntilde" => "\u{00F1}",
        "ograve" => "\u{00F2}",
        "oacute" => "\u{00F3}",
        "ocirc" => "\u{00F4}",
        "otilde" => "\u{00F5}",
        "ouml" => "\u{00F6}",
        "oslash" => "\u{00F8}",
        "ugrave" => "\u{00F9}",
        "uacute" => "\u{00FA}",
        "ucirc" => "\u{00FB}",
        "uuml" => "\u{00FC}",
        "yacute" => "\u{00FD}",
        "thorn" => "\u{00FE}",
        "yuml" => "\u{00FF}",
        // Greek letters (common on math/science/Wikipedia pages).
        "Alpha" => "\u{0391}",
        "Beta" => "\u{0392}",
        "Gamma" => "\u{0393}",
        "Delta" => "\u{0394}",
        "Epsilon" => "\u{0395}",
        "Zeta" => "\u{0396}",
        "Eta" => "\u{0397}",
        "Theta" => "\u{0398}",
        "Iota" => "\u{0399}",
        "Kappa" => "\u{039A}",
        "Lambda" => "\u{039B}",
        "Mu" => "\u{039C}",
        "Nu" => "\u{039D}",
        "Xi" => "\u{039E}",
        "Omicron" => "\u{039F}",
        "Pi" => "\u{03A0}",
        "Rho" => "\u{03A1}",
        "Sigma" => "\u{03A3}",
        "Tau" => "\u{03A4}",
        "Upsilon" => "\u{03A5}",
        "Phi" => "\u{03A6}",
        "Chi" => "\u{03A7}",
        "Psi" => "\u{03A8}",
        "Omega" => "\u{03A9}",
        "alpha" => "\u{03B1}",
        "beta" => "\u{03B2}",
        "gamma" => "\u{03B3}",
        "delta" => "\u{03B4}",
        "epsilon" => "\u{03B5}",
        "zeta" => "\u{03B6}",
        "eta" => "\u{03B7}",
        "theta" => "\u{03B8}",
        "iota" => "\u{03B9}",
        "kappa" => "\u{03BA}",
        "lambda" => "\u{03BB}",
        "mu" => "\u{03BC}",
        "nu" => "\u{03BD}",
        "xi" => "\u{03BE}",
        "omicron" => "\u{03BF}",
        "pi" => "\u{03C0}",
        "rho" => "\u{03C1}",
        "sigmaf" => "\u{03C2}",
        "sigma" => "\u{03C3}",
        "tau" => "\u{03C4}",
        "upsilon" => "\u{03C5}",
        "phi" => "\u{03C6}",
        "chi" => "\u{03C7}",
        "psi" => "\u{03C8}",
        "omega" => "\u{03C9}",
        // Arrows and common math operators.
        "larr" => "\u{2190}",
        "uarr" => "\u{2191}",
        "rarr" => "\u{2192}",
        "darr" => "\u{2193}",
        "harr" => "\u{2194}",
        "lArr" => "\u{21D0}",
        "rArr" => "\u{21D2}",
        "hArr" => "\u{21D4}",
        "forall" => "\u{2200}",
        "part" => "\u{2202}",
        "exist" => "\u{2203}",
        "empty" => "\u{2205}",
        "nabla" => "\u{2207}",
        "isin" => "\u{2208}",
        "notin" => "\u{2209}",
        "ni" => "\u{220B}",
        "prod" => "\u{220F}",
        "sum" => "\u{2211}",
        "minus" => "\u{2212}",
        "lowast" => "\u{2217}",
        "radic" => "\u{221A}",
        "prop" => "\u{221D}",
        "ang" => "\u{2220}",
        "and" => "\u{2227}",
        "or" => "\u{2228}",
        "cap" => "\u{2229}",
        "cup" => "\u{222A}",
        "int" => "\u{222B}",
        "there4" => "\u{2234}",
        "sim" => "\u{223C}",
        "cong" => "\u{2245}",
        "asymp" => "\u{2248}",
        "equiv" => "\u{2261}",
        "sub" => "\u{2282}",
        "sup" => "\u{2283}",
        "sube" => "\u{2286}",
        "supe" => "\u{2287}",
        "oplus" => "\u{2295}",
        "otimes" => "\u{2297}",
        "perp" => "\u{22A5}",
        "sdot" => "\u{22C5}",
        "loz" => "\u{25CA}",
        "spades" => "\u{2660}",
        "clubs" => "\u{2663}",
        "hearts" => "\u{2665}",
        "diams" => "\u{2666}",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(s: &str) -> String {
        let input: Vec<char> = s.chars().collect();
        let mut pos = 0;
        consume_char_ref(&input, &mut pos)
    }

    #[test]
    fn named_numeric_and_invalid() {
        assert_eq!(decode("&amp;"), "&");
        assert_eq!(decode("&nbsp;"), "\u{00A0}");
        assert_eq!(decode("&#65;"), "A");
        assert_eq!(decode("&#x1F600;"), "😀");
        assert_eq!(decode("&#0;"), "\u{FFFD}");
        assert_eq!(decode("&notareal;"), "&"); // unknown name → literal &
        assert_eq!(decode("&amp"), "&"); // no semicolon (subset requires it)
    }

    #[test]
    fn latin1_named_entities() {
        assert_eq!(decode("&eacute;"), "é");
        assert_eq!(decode("&Uuml;"), "Ü");
        assert_eq!(decode("&ntilde;"), "ñ");
        assert_eq!(decode("&szlig;"), "ß");
        assert_eq!(decode("&ccedil;"), "ç");
        assert_eq!(decode("&iquest;"), "¿");
        assert_eq!(decode("&AElig;"), "Æ");
        // Case-sensitive: the accented-letter names differ by case.
        assert_ne!(decode("&Eacute;"), decode("&eacute;"));
    }

    #[test]
    fn greek_arrow_and_math_entities() {
        assert_eq!(decode("&alpha;"), "α");
        assert_eq!(decode("&Omega;"), "Ω");
        assert_eq!(decode("&pi;"), "π");
        assert_eq!(decode("&rarr;"), "→");
        assert_eq!(decode("&hArr;"), "⇔");
        assert_eq!(decode("&sum;"), "∑");
        assert_eq!(decode("&radic;"), "√");
        assert_eq!(decode("&ne;"), "≠");
        // Case-sensitive Greek (Pi ≠ pi).
        assert_ne!(decode("&Pi;"), decode("&pi;"));
    }

    #[test]
    fn c1_windows_1252_override() {
        // The HTML numeric-reference C1 remap: these are NOT the raw C1 controls.
        assert_eq!(decode("&#128;"), "€");
        assert_eq!(decode("&#x80;"), "€");
        assert_eq!(decode("&#133;"), "…");
        assert_eq!(decode("&#145;"), "‘");
        assert_eq!(decode("&#146;"), "’");
        assert_eq!(decode("&#153;"), "™");
        // Unmapped C1 slots pass through as the raw code point.
        assert_eq!(decode("&#129;"), "\u{0081}");
        // Surrogates and out-of-range still become U+FFFD.
        assert_eq!(decode("&#xD800;"), "\u{FFFD}");
    }
}
