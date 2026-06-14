//! Character-reference decoding.
//!
//! Phase 1 handles numeric references (`&#123;` / `&#x1F;`) and a common subset of
//! named references. The full named-entity table (~2200 entries) and the
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
/// surrogate fixups (everything invalid becomes U+FFFD).
fn map_codepoint(code: u32) -> char {
    if code == 0 || code > 0x10_FFFF || (0xD800..=0xDFFF).contains(&code) {
        return '\u{FFFD}';
    }
    char::from_u32(code).unwrap_or('\u{FFFD}')
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
}
