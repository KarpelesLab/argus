//! 2D graphics for Argus: text shaping + rasterization (Layer 1).
//!
//! Rather than hand-write a shaper and rasterizer, `argus-gfx` wraps the
//! first-party **oxideav** graphics stack (all pure Rust):
//!
//! * [`oxideav_scribe`] — loads TTF/OTF faces and shapes a string into positioned
//!   glyph outlines (`Shaper::shape_to_paths` → scene `Node`s + transforms).
//! * [`oxideav_raster`] — rasterizes the resulting vector scene into a packed
//!   RGBA buffer with anti-aliasing.
//!
//! This is the rasterization half of `docs/subsystems/rendering.md`; the paint
//! layer (`argus-paint`) will build the scene from a fragment tree. For now the
//! crate exposes just enough to draw a shaped text run onto an RGBA canvas, which
//! the content process composites into its framebuffer.

use argus_geometry::Color;
use oxideav_core::{Group, Node, Transform2D, VectorFrame};
use oxideav_raster::Renderer;
use oxideav_scribe::{Face, FaceChain, Shaper};

/// A loaded font face ready to shape and render text.
pub struct Font {
    chain: FaceChain,
}

impl Font {
    /// Load a font from TTF/OTF bytes.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Font, String> {
        let face = Face::from_ttf_bytes(bytes.clone())
            .or_else(|_| Face::from_otf_bytes(bytes))
            .map_err(|e| format!("failed to parse font: {e:?}"))?;
        Ok(Font {
            chain: FaceChain::new(face),
        })
    }

    /// Distance from the top of a line to the baseline, in pixels, at `size_px`.
    pub fn ascent_px(&self, size_px: f32) -> f32 {
        self.chain.face(0).ascent_px(size_px)
    }

    /// Distance from the baseline to the bottom of the line, in pixels.
    pub fn descent_px(&self, size_px: f32) -> f32 {
        self.chain.face(0).descent_px(size_px)
    }

    /// Advance width of `text` at `size_px`, in pixels (sum of glyph advances).
    pub fn measure(&self, text: &str, size_px: f32) -> f32 {
        match self.chain.shape(text, size_px) {
            Ok(glyphs) => glyphs.iter().map(|g| g.x_advance).sum(),
            Err(_) => 0.0,
        }
    }
}

/// A shaped-and-positioned text run for [`render_runs`]: its left edge at `x` and
/// baseline at `baseline`, in canvas pixels.
#[derive(Clone, Debug)]
pub struct TextRun {
    pub x: f32,
    pub baseline: f32,
    pub text: String,
    pub size_px: f32,
}

/// An RGBA8 canvas (row-major, tightly packed, straight alpha).
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Canvas {
    /// A transparent canvas of the given size.
    pub fn new(width: u32, height: u32) -> Canvas {
        Canvas {
            width,
            height,
            pixels: vec![0; width as usize * height as usize * 4],
        }
    }

    /// Number of pixels with non-zero alpha — handy for tests/diagnostics.
    pub fn covered_pixels(&self) -> usize {
        self.pixels.chunks_exact(4).filter(|p| p[3] != 0).count()
    }
}

/// Render `text` in `font` at `size_px`, with its left edge at `origin_x` and its
/// baseline at `baseline_y`, onto a fresh transparent [`Canvas`] of `width` ×
/// `height`. Color handling (currently the face's default fill) is refined as the
/// paint layer lands.
pub fn render_text(
    font: &Font,
    text: &str,
    size_px: f32,
    origin_x: f32,
    baseline_y: f32,
    width: u32,
    height: u32,
    _color: Color,
) -> Canvas {
    let root = build_run(font, text, size_px, origin_x, baseline_y);
    let video = render_run(root, width, height);
    let pixels = video
        .planes
        .into_iter()
        .next()
        .map(|p| p.data)
        .unwrap_or_else(|| vec![0; width as usize * height as usize * 4]);

    Canvas {
        width,
        height,
        pixels,
    }
}

/// Source-over composite straight-alpha RGBA `src` onto opaque RGBA `dst`
/// (both tightly packed, same length). `dst` stays opaque.
pub fn composite_over(dst: &mut [u8], src: &[u8]) {
    for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        let a = s[3] as u32;
        if a == 0 {
            continue;
        }
        for c in 0..3 {
            d[c] = ((s[c] as u32 * a + d[c] as u32 * (255 - a)) / 255) as u8;
        }
        d[3] = 255;
    }
}

/// Render many text runs onto one transparent [`Canvas`] in a single rasterization
/// pass (glyphs are black; color support lands with the paint layer).
pub fn render_runs(runs: &[TextRun], font: &Font, width: u32, height: u32) -> Canvas {
    let children: Vec<Node> = runs
        .iter()
        .map(|run| Node::Group(build_run(font, &run.text, run.size_px, run.x, run.baseline)))
        .collect();
    let root = Group {
        children,
        ..Group::default()
    };
    let video = render_run(root, width, height);
    let pixels = video
        .planes
        .into_iter()
        .next()
        .map(|p| p.data)
        .unwrap_or_else(|| vec![0; width as usize * height as usize * 4]);
    Canvas {
        width,
        height,
        pixels,
    }
}

/// Rasterize a prepared run group onto a `width` × `height` canvas.
fn render_run(root: Group, width: u32, height: u32) -> oxideav_core::VideoFrame {
    let frame = VectorFrame::new(width as f32, height as f32).with_root(root);
    Renderer::new(width, height).render(&frame)
}

/// Build the placed-glyph run group for `text` (baseline at `origin_x`,
/// `baseline_y`). Shared by [`render_text`] and diagnostics.
fn build_run(font: &Font, text: &str, size_px: f32, origin_x: f32, baseline_y: f32) -> Group {
    let placed = Shaper::shape_to_paths(&font.chain, text, size_px);
    let children: Vec<Node> = placed
        .into_iter()
        .map(|(_face_idx, node, tf)| {
            Node::Group(Group {
                transform: tf,
                children: vec![node],
                ..Group::default()
            })
        })
        .collect();
    Group {
        transform: Transform2D::translate(origin_x, baseline_y),
        children,
        ..Group::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// First available plain-TTF system font (present on macOS dev + CI).
    fn system_font() -> Option<Font> {
        for path in [
            "/System/Library/Fonts/Geneva.ttf",
            "/System/Library/Fonts/Monaco.ttf",
            "/System/Library/Fonts/SFNS.ttf",
            "/System/Library/Fonts/Supplemental/Arial.ttf",
        ] {
            if let Ok(bytes) = std::fs::read(path) {
                if let Ok(font) = Font::from_bytes(bytes) {
                    return Some(font);
                }
            }
        }
        None
    }

    #[test]
    fn shapes_and_rasterizes_text() {
        let Some(font) = system_font() else {
            eprintln!("no system font found; skipping");
            return;
        };
        let size = 48.0;
        let canvas = render_text(
            &font,
            "Argus",
            size,
            4.0,
            font.ascent_px(size),
            400,
            64,
            Color::BLACK,
        );

        assert_eq!(canvas.pixels.len(), 400 * 64 * 4);
        // Real glyphs must produce a meaningful amount of coverage.
        let covered = canvas.covered_pixels();
        assert!(
            covered > 200,
            "expected substantial glyph coverage, got {covered} px"
        );
        // ...but not the whole canvas (it's text, not a fill).
        assert!(
            covered < 400 * 64 / 2,
            "unexpectedly dense coverage: {covered}"
        );
    }

    /// Diagnostic: render text on a white background and write a PNG to /tmp so it
    /// can be eyeballed (orientation, baseline, anti-aliasing). Run with
    /// `cargo test -p argus-gfx -- --ignored dump_text_png --nocapture`.
    #[test]
    #[ignore = "writes a PNG for manual inspection"]
    fn dump_text_png() {
        use oxideav_core::{PixelFormat, VideoFrame, VideoPlane};

        let font = system_font().expect("a system font");
        let (w, h) = (420u32, 80u32);
        let size = 56.0;
        let canvas = render_text(
            &font,
            "Argus",
            size,
            6.0,
            font.ascent_px(size),
            w,
            h,
            Color::BLACK,
        );

        // Composite over white so black glyphs are visible.
        let mut rgba = vec![255u8; (w * h * 4) as usize];
        for (dst, src) in rgba.chunks_exact_mut(4).zip(canvas.pixels.chunks_exact(4)) {
            let a = src[3] as u32;
            for c in 0..3 {
                dst[c] = ((src[c] as u32 * a + dst[c] as u32 * (255 - a)) / 255) as u8;
            }
        }

        let frame = VideoFrame {
            pts: None,
            planes: vec![VideoPlane {
                stride: (w * 4) as usize,
                data: rgba,
            }],
        };
        let png =
            oxideav_png::encode_single(&frame, w, h, PixelFormat::Rgba, &[]).expect("encode png");
        std::fs::write("/tmp/argus_text.png", png).expect("write png");
        eprintln!("wrote /tmp/argus_text.png");
    }
}
