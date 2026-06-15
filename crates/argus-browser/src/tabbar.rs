//! Visual tab-bar geometry and hit-testing.
//!
//! The browser reserves a strip [`TAB_BAR_H`] pixels tall at the top of the
//! window for tabs: one rectangle per open tab (the active one highlighted) plus
//! a square "new tab" button on the right. This module is the pure geometry —
//! where each tab sits and what a click in the strip means — so it is unit-tested
//! independently of the windowed drawing/compositing.

/// Height of the tab strip in pixels.
pub(crate) const TAB_BAR_H: u32 = 28;
/// Width of the square "new tab" (+) button at the right of the strip.
const NEW_BTN_W: u32 = 28;
/// Width of the close ("×") hit zone at the right edge of each tab.
const CLOSE_ZONE_W: u32 = 18;
/// A tab never grows wider than this (extra space is left blank).
const MAX_TAB_W: u32 = 220;

/// What a click in the tab strip resolves to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TabHit {
    /// Activate the tab at this index.
    Switch(usize),
    /// Close the tab at this index (its "×" zone was clicked).
    Close(usize),
    /// Open a new tab (the "+" button).
    New,
}

/// The pixel x-range `[x0, x1)` of tab `i` given `count` tabs in a `width`-px strip.
pub(crate) fn tab_rect(i: usize, count: usize, width: u32) -> (u32, u32) {
    if count == 0 {
        return (0, 0);
    }
    let avail = width.saturating_sub(NEW_BTN_W);
    let tw = (avail / count as u32).clamp(1, MAX_TAB_W);
    let x0 = (i as u32) * tw;
    (x0, x0 + tw)
}

/// The pixel x-range `[x0, x1)` of the "new tab" (+) button.
pub(crate) fn new_button_rect(width: u32) -> (u32, u32) {
    (width.saturating_sub(NEW_BTN_W), width)
}

/// Resolve a click at `(x, y)` against a tab strip of `count` tabs in a `width`-px
/// window. Returns `None` when the click is below the strip (the caller routes it
/// to page content) or in dead space.
pub(crate) fn hit_test(x: u32, y: u32, count: usize, width: u32) -> Option<TabHit> {
    if y >= TAB_BAR_H || count == 0 {
        return None;
    }
    let (nb0, nb1) = new_button_rect(width);
    if x >= nb0 && x < nb1 {
        return Some(TabHit::New);
    }
    for i in 0..count {
        let (x0, x1) = tab_rect(i, count, width);
        if x >= x0 && x < x1 {
            // The right CLOSE_ZONE_W px (when the tab is wide enough) closes it;
            // never offer close when only one tab is open.
            if count > 1 && x1 - x0 > CLOSE_ZONE_W * 2 && x >= x1 - CLOSE_ZONE_W {
                return Some(TabHit::Close(i));
            }
            return Some(TabHit::Switch(i));
        }
    }
    None
}

/// Fill `[x0,x1) × [y0,y1)` of a `width`-px RGBA buffer with `(r,g,b)` (opaque).
fn fill_rect(buf: &mut [u8], width: u32, x0: u32, y0: u32, x1: u32, y1: u32, rgb: (u8, u8, u8)) {
    let h = buf.len() as u32 / 4 / width.max(1);
    for y in y0..y1.min(h) {
        for x in x0..x1.min(width) {
            let i = ((y * width + x) * 4) as usize;
            buf[i] = rgb.0;
            buf[i + 1] = rgb.1;
            buf[i + 2] = rgb.2;
            buf[i + 3] = 255;
        }
    }
}

/// Paint the tab strip into the top [`TAB_BAR_H`] rows of a `width`-px RGBA `buf`:
/// a dark background, one rounded-ish rectangle per tab (the active one lighter),
/// and a "+" new-tab button on the right.
pub(crate) fn draw(buf: &mut [u8], width: u32, count: usize, active: usize) {
    const BG: (u8, u8, u8) = (0x1b, 0x1f, 0x27);
    const TAB: (u8, u8, u8) = (0x2a, 0x2f, 0x39);
    const ACTIVE: (u8, u8, u8) = (0x3d, 0x6f, 0xb5);
    const INK: (u8, u8, u8) = (0xe6, 0xe9, 0xef);
    fill_rect(buf, width, 0, 0, width, TAB_BAR_H, BG);
    for i in 0..count {
        let (x0, x1) = tab_rect(i, count, width);
        let color = if i == active { ACTIVE } else { TAB };
        // Inset by 1px all round so a thin background gap separates tabs.
        fill_rect(buf, width, x0 + 1, 2, x1.saturating_sub(1), TAB_BAR_H, color);
    }
    // "+" button: a plus glyph drawn as two bars centered in the button square.
    let (nx0, nx1) = new_button_rect(width);
    let cx = (nx0 + nx1) / 2;
    let cy = TAB_BAR_H / 2;
    fill_rect(buf, width, cx - 5, cy - 1, cx + 5, cy + 1, INK); // horizontal
    fill_rect(buf, width, cx - 1, cy - 5, cx + 1, cy + 5, INK); // vertical
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draw_marks_the_active_tab_distinctly() {
        let (w, count, active) = (800u32, 3usize, 1usize);
        let mut buf = vec![0u8; (w * TAB_BAR_H * 4) as usize];
        draw(&mut buf, w, count, active);
        // Sample the center of tab 0 (inactive) vs tab 1 (active): different colors.
        let center = |i: usize| {
            let (x0, x1) = tab_rect(i, count, w);
            let (x, y) = ((x0 + x1) / 2, TAB_BAR_H / 2);
            let p = ((y * w + x) * 4) as usize;
            (buf[p], buf[p + 1], buf[p + 2])
        };
        assert_ne!(center(0), center(1), "active tab is a different color");
        assert_eq!(center(1), (0x3d, 0x6f, 0xb5), "active tab uses the accent");
    }


    #[test]
    fn click_below_strip_is_not_a_tab_hit() {
        assert_eq!(hit_test(100, TAB_BAR_H, 3, 800), None);
        assert_eq!(hit_test(100, TAB_BAR_H + 50, 3, 800), None);
    }

    #[test]
    fn new_button_is_at_the_right_edge() {
        let (x0, x1) = new_button_rect(800);
        assert_eq!(x1, 800);
        assert_eq!(hit_test(x0 + 1, 5, 3, 800), Some(TabHit::New));
    }

    #[test]
    fn clicks_resolve_to_the_right_tab() {
        // 3 tabs in 800px: avail = 772, tab width 257.
        assert_eq!(hit_test(10, 10, 3, 800), Some(TabHit::Switch(0)));
        assert_eq!(hit_test(300, 10, 3, 800), Some(TabHit::Switch(1)));
        assert_eq!(hit_test(600, 10, 3, 800), Some(TabHit::Switch(2)));
    }

    #[test]
    fn right_edge_of_a_wide_tab_closes_it() {
        // Tab 0 spans [0, 257); its close zone is the last 18px.
        let (_, x1) = tab_rect(0, 3, 800);
        assert_eq!(hit_test(x1 - 5, 10, 3, 800), Some(TabHit::Close(0)));
        assert_eq!(hit_test(x1 - 30, 10, 3, 800), Some(TabHit::Switch(0)));
    }

    #[test]
    fn single_tab_has_no_close_zone() {
        // With one tab, the whole tab switches (and can't be closed by click).
        let (_, x1) = tab_rect(0, 1, 800);
        assert_eq!(hit_test(x1 - 5, 10, 1, 800), Some(TabHit::Switch(0)));
    }
}
