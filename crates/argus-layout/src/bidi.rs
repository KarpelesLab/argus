//! Unicode bidirectional reordering (UAX #9) for a single line of text.
//!
//! Argus's shaper places glyphs left-to-right with no native RTL support, so to
//! render right-to-left text we reorder the *characters* into visual order here
//! and then shape that visual string LTR. `oxideav_scribe::bidi` supplies the
//! normative class table plus the weak (W) and neutral (N) resolution passes; on
//! top of those this module computes the implicit embedding levels (I1/I2),
//! applies the L1 trailing-whitespace reset, and the L2 run reversal.
//!
//! Scope and limitations:
//! - Explicit embedding/override/isolate controls (LRE…PDI) are not honored —
//!   plain mixed L/R/AL/EN/AN text (the common case) is handled.
//! - Arabic (`AL`) is reordered as RTL but **not contextually joined** (the
//!   shaper has no Arabic shaping), so Arabic shows isolated forms in correct
//!   right-to-left order. Hebrew and other non-joining RTL scripts are correct.
//! - Reordering is per call (one line); callers apply it to text that is not
//!   wrapped across multiple visual lines.

use oxideav_scribe::bidi::{bidi_class, paragraph_level, resolve_neutral_types, resolve_weak_types, BidiClass};

/// Reorder `text` from logical into visual order for left-to-right painting,
/// returning `None` when the text contains no right-to-left character (so the
/// caller keeps the original string and pays nothing for pure-LTR content).
pub(crate) fn reorder_visual(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return None;
    }
    // Fast path: nothing right-to-left → no reordering needed.
    let has_rtl = chars
        .iter()
        .any(|&c| matches!(bidi_class(c), BidiClass::R | BidiClass::AL | BidiClass::AN));
    if !has_rtl {
        return None;
    }

    let base = paragraph_level(text); // 0 = LTR, 1 = RTL
    let mut classes: Vec<BidiClass> = chars.iter().map(|&c| bidi_class(c)).collect();
    let sos = if base % 2 == 0 { BidiClass::L } else { BidiClass::R };
    // No explicit embeddings, so the whole text is one level run: sos == eos.
    resolve_weak_types(&mut classes, sos, sos);
    resolve_neutral_types(&mut classes, base, sos, sos);

    // Implicit levels (UAX #9 §3.3.6, I1/I2) from the base embedding level.
    let mut levels: Vec<u8> = classes
        .iter()
        .map(|&c| implicit_level(base, c))
        .collect();

    // L1: trailing whitespace (and segment separators) on the line reset to the
    // base level so it sits on the base-direction side.
    for i in (0..chars.len()).rev() {
        match bidi_class(chars[i]) {
            BidiClass::WS | BidiClass::S | BidiClass::B | BidiClass::BN => levels[i] = base,
            _ => break,
        }
    }

    // L2: from the highest level down to the lowest odd level, reverse every
    // contiguous run of characters whose level is >= that level.
    let order = l2_reorder(&levels);
    let visual: String = order.iter().map(|&i| chars[i]).collect();
    Some(visual)
}

/// The implicit embedding level for a resolved class at base level `base`.
fn implicit_level(base: u8, c: BidiClass) -> u8 {
    if base % 2 == 0 {
        // Even (LTR) level: R → +1, AN/EN → +2.
        match c {
            BidiClass::R => base + 1,
            BidiClass::AN | BidiClass::EN => base + 2,
            _ => base,
        }
    } else {
        // Odd (RTL) level: L/EN/AN → +1.
        match c {
            BidiClass::L | BidiClass::EN | BidiClass::AN => base + 1,
            _ => base,
        }
    }
}

/// UAX #9 rule L2: produce the visual left-to-right index order by reversing
/// contiguous runs at each level from the highest down to the lowest odd level.
fn l2_reorder(levels: &[u8]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..levels.len()).collect();
    let Some(&max) = levels.iter().max() else {
        return order;
    };
    let min_odd = levels
        .iter()
        .copied()
        .filter(|l| l % 2 == 1)
        .min()
        .unwrap_or(max + 1);
    let mut level = max;
    while level >= min_odd {
        let mut i = 0;
        while i < levels.len() {
            if levels[i] >= level {
                let start = i;
                while i < levels.len() && levels[i] >= level {
                    i += 1;
                }
                order[start..i].reverse();
            } else {
                i += 1;
            }
        }
        if level == 0 {
            break;
        }
        level -= 1;
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    // Hebrew letters (class R): א U+05D0, ב U+05D1, ג U+05D2.
    const ALEF: char = '\u{05D0}';
    const BET: char = '\u{05D1}';
    const GIMEL: char = '\u{05D2}';

    #[test]
    fn pure_ltr_is_unchanged() {
        assert_eq!(reorder_visual("hello world"), None);
        assert_eq!(reorder_visual("abc 123"), None);
    }

    #[test]
    fn pure_hebrew_reverses_to_visual_order() {
        // Logical א ב ג → visual ג ב א (rightmost logical char paints rightmost).
        let s: String = [ALEF, BET, GIMEL].iter().collect();
        let v = reorder_visual(&s).expect("rtl reordered");
        assert_eq!(v.chars().collect::<Vec<_>>(), vec![GIMEL, BET, ALEF]);
    }

    #[test]
    fn latin_inside_hebrew_keeps_its_order() {
        // Logical: א ב "ab" ג  (base RTL). The Latin run "ab" stays L-to-R while
        // the Hebrew letters reverse around it.
        let mut s = String::new();
        s.push(ALEF);
        s.push(BET);
        s.push('a');
        s.push('b');
        s.push(GIMEL);
        let v = reorder_visual(&s).expect("mixed reordered");
        let got: Vec<char> = v.chars().collect();
        // Visual L-to-R: ג, then "ab" (still in order), then ב, א.
        assert_eq!(got, vec![GIMEL, 'a', 'b', BET, ALEF]);
    }

    #[test]
    fn numbers_in_hebrew_stay_left_to_right() {
        // Logical: א "12" ב (base RTL). Digits render left-to-right within the run.
        let mut s = String::new();
        s.push(ALEF);
        s.push('1');
        s.push('2');
        s.push(BET);
        let v = reorder_visual(&s).expect("reordered");
        let got: Vec<char> = v.chars().collect();
        // Visual: ב, then 12 (ascending), then א.
        assert_eq!(got, vec![BET, '1', '2', ALEF]);
    }
}
