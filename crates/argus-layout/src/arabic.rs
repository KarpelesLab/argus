//! Arabic contextual shaping by Presentation-Forms substitution.
//!
//! The shaper (oxideav-scribe) renders the isolated cmap glyph for each Arabic
//! code point — it does not apply Arabic joining. But the Arabic
//! Presentation-Forms-B block (U+FE70..U+FEFF, plus a couple in FB50..FBFF)
//! encodes the *joined* shapes — isolated / initial / medial / final — as their
//! own code points, which Arabic fonts map in cmap. So we shape at the text level:
//! replace each Arabic letter with the presentation-form code point for its
//! contextual position, including the LAM-ALEF ligatures. The result stays in
//! logical order; [`crate::bidi`] then reorders it to visual order.
//!
//! Scope: the Arabic block U+0621..U+064A (+ TATWEEL) and LAM-ALEF. Combining
//! marks (harakat) are treated as transparent — they don't affect a letter's
//! joining or its neighbours. Other scripts pass through unchanged.

/// Presentation forms `[isolated, initial, medial, final]` for a base Arabic
/// letter (`0` = no such form). Returns `None` for non-Arabic-letter code points.
fn forms(c: char) -> Option<[u32; 4]> {
    let f = match c as u32 {
        0x0621 => [0xFE80, 0, 0, 0],
        0x0622 => [0xFE81, 0, 0, 0xFE82],
        0x0623 => [0xFE83, 0, 0, 0xFE84],
        0x0624 => [0xFE85, 0, 0, 0xFE86],
        0x0625 => [0xFE87, 0, 0, 0xFE88],
        0x0626 => [0xFE89, 0xFE8B, 0xFE8C, 0xFE8A],
        0x0627 => [0xFE8D, 0, 0, 0xFE8E],
        0x0628 => [0xFE8F, 0xFE91, 0xFE92, 0xFE90],
        0x0629 => [0xFE93, 0, 0, 0xFE94],
        0x062A => [0xFE95, 0xFE97, 0xFE98, 0xFE96],
        0x062B => [0xFE99, 0xFE9B, 0xFE9C, 0xFE9A],
        0x062C => [0xFE9D, 0xFE9F, 0xFEA0, 0xFE9E],
        0x062D => [0xFEA1, 0xFEA3, 0xFEA4, 0xFEA2],
        0x062E => [0xFEA5, 0xFEA7, 0xFEA8, 0xFEA6],
        0x062F => [0xFEA9, 0, 0, 0xFEAA],
        0x0630 => [0xFEAB, 0, 0, 0xFEAC],
        0x0631 => [0xFEAD, 0, 0, 0xFEAE],
        0x0632 => [0xFEAF, 0, 0, 0xFEB0],
        0x0633 => [0xFEB1, 0xFEB3, 0xFEB4, 0xFEB2],
        0x0634 => [0xFEB5, 0xFEB7, 0xFEB8, 0xFEB6],
        0x0635 => [0xFEB9, 0xFEBB, 0xFEBC, 0xFEBA],
        0x0636 => [0xFEBD, 0xFEBF, 0xFEC0, 0xFEBE],
        0x0637 => [0xFEC1, 0xFEC3, 0xFEC4, 0xFEC2],
        0x0638 => [0xFEC5, 0xFEC7, 0xFEC8, 0xFEC6],
        0x0639 => [0xFEC9, 0xFECB, 0xFECC, 0xFECA],
        0x063A => [0xFECD, 0xFECF, 0xFED0, 0xFECE],
        0x0640 => [0x0640, 0x0640, 0x0640, 0x0640], // TATWEEL
        0x0641 => [0xFED1, 0xFED3, 0xFED4, 0xFED2],
        0x0642 => [0xFED5, 0xFED7, 0xFED8, 0xFED6],
        0x0643 => [0xFED9, 0xFEDB, 0xFEDC, 0xFEDA],
        0x0644 => [0xFEDD, 0xFEDF, 0xFEE0, 0xFEDE],
        0x0645 => [0xFEE1, 0xFEE3, 0xFEE4, 0xFEE2],
        0x0646 => [0xFEE5, 0xFEE7, 0xFEE8, 0xFEE6],
        0x0647 => [0xFEE9, 0xFEEB, 0xFEEC, 0xFEEA],
        0x0648 => [0xFEED, 0, 0, 0xFEEE],
        0x0649 => [0xFEEF, 0xFBE8, 0xFBE9, 0xFEF0],
        0x064A => [0xFEF1, 0xFEF3, 0xFEF4, 0xFEF2],
        _ => return None,
    };
    Some(f)
}

/// Whether `c` is a transparent Arabic mark (harakat / combining) that does not
/// participate in joining and is emitted verbatim between its neighbours.
fn is_transparent(c: char) -> bool {
    matches!(c as u32, 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06ED)
}

/// Can `c` connect to the *following* letter (has an initial or medial form)?
fn connects_after(c: char) -> bool {
    forms(c).map(|f| f[1] != 0 || f[2] != 0).unwrap_or(false)
}
/// Can `c` connect to the *preceding* letter (has a final or medial form)?
fn connects_before(c: char) -> bool {
    forms(c).map(|f| f[3] != 0 || f[2] != 0).unwrap_or(false)
}

/// The LAM-ALEF ligature `(isolated, final)` for a following ALEF variant.
fn lam_alef(alef: char) -> Option<(char, char)> {
    let pair = match alef as u32 {
        0x0622 => (0xFEF5, 0xFEF6),
        0x0623 => (0xFEF7, 0xFEF8),
        0x0625 => (0xFEF9, 0xFEFA),
        0x0627 => (0xFEFB, 0xFEFC),
        _ => return None,
    };
    Some((char::from_u32(pair.0)?, char::from_u32(pair.1)?))
}

/// Reshape Arabic text from logical base letters into logical-order presentation
/// forms (joining + LAM-ALEF). Returns `None` when the text has no Arabic letter
/// (the caller keeps the original string and pays nothing).
pub(crate) fn reshape(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    if !chars.iter().any(|&c| forms(c).is_some_and(|_| !is_transparent(c))) {
        return None;
    }
    // Index of the previous / next non-transparent letter, for join context.
    let prev_letter = |i: usize| {
        chars[..i]
            .iter()
            .rev()
            .find(|&&c| !is_transparent(c))
            .copied()
    };
    let next_letter = |i: usize| chars[i + 1..].iter().find(|&&c| !is_transparent(c)).copied();

    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // LAM directly followed by an ALEF variant → a single ligature glyph.
        if c as u32 == 0x0644 {
            if let Some(next) = next_letter(i) {
                if let Some((iso, fin)) = lam_alef(next) {
                    let joins_prev = prev_letter(i).is_some_and(connects_after);
                    out.push(if joins_prev { fin } else { iso });
                    // Skip up to and including the ALEF (and any transparent marks
                    // between the LAM and the ALEF, which are dropped — rare).
                    let mut j = i + 1;
                    while j < chars.len() && chars[j] != next {
                        j += 1;
                    }
                    i = j + 1;
                    continue;
                }
            }
        }
        match forms(c) {
            Some(f) => {
                let joins_prev = prev_letter(i).is_some_and(connects_after) && connects_before(c);
                let joins_next = next_letter(i).is_some_and(connects_before) && connects_after(c);
                let idx = match (joins_prev, joins_next) {
                    (true, true) => 2,   // medial
                    (true, false) => 3,  // final
                    (false, true) => 1,  // initial
                    (false, false) => 0, // isolated
                };
                let cp = if f[idx] != 0 { f[idx] } else { f[0] };
                out.push(char::from_u32(cp).unwrap_or(c));
            }
            None => out.push(c),
        }
        i += 1;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEH: char = '\u{0628}';
    const LAM: char = '\u{0644}';
    const ALEF: char = '\u{0627}';

    fn s(cps: &[u32]) -> String {
        cps.iter().map(|&c| char::from_u32(c).unwrap()).collect()
    }

    #[test]
    fn no_arabic_is_unchanged() {
        assert_eq!(reshape("hello"), None);
        assert_eq!(reshape("123 abc"), None);
    }

    #[test]
    fn isolated_letter_uses_isolated_form() {
        // A lone BEH → its isolated presentation form FE8F.
        assert_eq!(reshape(&BEH.to_string()), Some(s(&[0xFE8F])));
    }

    #[test]
    fn two_letters_join_initial_then_final() {
        // BEH BEH → initial FE91, final FE90.
        let input: String = [BEH, BEH].iter().collect();
        assert_eq!(reshape(&input), Some(s(&[0xFE91, 0xFE90])));
    }

    #[test]
    fn three_letters_use_medial_in_the_middle() {
        // BEH BEH BEH → initial, medial, final.
        let input: String = [BEH, BEH, BEH].iter().collect();
        assert_eq!(reshape(&input), Some(s(&[0xFE91, 0xFE92, 0xFE90])));
    }

    #[test]
    fn lam_alef_forms_a_ligature() {
        // LAM ALEF (alone) → isolated ligature FEFB.
        let input: String = [LAM, ALEF].iter().collect();
        assert_eq!(reshape(&input), Some(s(&[0xFEFB])));
        // BEH LAM ALEF → BEH initial, then the final LAM-ALEF ligature FEFC.
        let input2: String = [BEH, LAM, ALEF].iter().collect();
        assert_eq!(reshape(&input2), Some(s(&[0xFE91, 0xFEFC])));
    }
}
