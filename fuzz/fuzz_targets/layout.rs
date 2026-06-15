//! Fuzz target: the full parse → cascade → layout pipeline must never panic and
//! must produce finite geometry on arbitrary input.
#![no_main]

use std::sync::OnceLock;

use argus_gfx::Font;
use argus_layout::{layout, ImageSizes};
use libfuzzer_sys::fuzz_target;

fn font() -> Option<&'static Font> {
    static FONT: OnceLock<Option<Font>> = OnceLock::new();
    FONT.get_or_init(|| {
        for path in [
            "/System/Library/Fonts/Geneva.ttf",
            "/System/Library/Fonts/Monaco.ttf",
            "/System/Library/Fonts/SFNS.ttf",
            "/System/Library/Fonts/Supplemental/Arial.ttf",
        ] {
            if let Ok(bytes) = std::fs::read(path) {
                if let Ok(f) = Font::from_bytes(bytes) {
                    return Some(f);
                }
            }
        }
        None
    })
    .as_ref()
}

fuzz_target!(|data: &[u8]| {
    let Some(font) = font() else { return };
    let html = String::from_utf8_lossy(data);
    let doc = argus_html::parse(&html);
    let l = layout(&doc, font, 400.0, &ImageSizes::new());
    // No emitted geometry may be NaN/inf — guards the fr/flex-grow/shrink division
    // and float-band math against degenerate (e.g. zero-track, huge-span) inputs.
    assert!(l.height.is_finite());
    for r in &l.rects {
        assert!(r.x.is_finite() && r.y.is_finite() && r.w.is_finite() && r.h.is_finite());
    }
    for run in &l.runs {
        assert!(run.x.is_finite() && run.baseline.is_finite());
    }
    for im in &l.images {
        assert!(im.x.is_finite() && im.y.is_finite() && im.w.is_finite() && im.h.is_finite());
    }
});
