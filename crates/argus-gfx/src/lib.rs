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
use oxideav_core::{Group, Node, Paint, Path, PathNode, Point, Rgba, Transform2D, VectorFrame};
use oxideav_raster::Renderer;
use oxideav_scribe::{Face, FaceChain, Shaper};

fn rgba_of(c: Color) -> Rgba {
    Rgba::new(c.r, c.g, c.b, c.a)
}

/// Recursively set the fill paint of every `Path` node in a glyph subtree, so a
/// shaped run paints in `color` instead of the face's default black.
fn recolor(node: &mut Node, paint: &Paint) {
    match node {
        Node::Path(p) => p.fill = Some(paint.clone()),
        Node::Group(g) => {
            for child in &mut g.children {
                recolor(child, paint);
            }
        }
        _ => {}
    }
}

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

    /// Return this font with an added fallback face (from TTF/OTF bytes), consulted
    /// for glyphs the primary face lacks (emoji, CJK, symbols). On a parse failure
    /// the font is returned unchanged.
    pub fn with_fallback(self, bytes: Vec<u8>) -> Font {
        match Face::from_ttf_bytes(bytes.clone()).or_else(|_| Face::from_otf_bytes(bytes)) {
            Ok(face) => Font {
                chain: self.chain.push_fallback(face),
            },
            Err(_) => self,
        }
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

/// A shaped-and-positioned text run: its left edge at `x` and baseline at
/// `baseline`, in canvas pixels, painted in `color`.
#[derive(Clone, Debug)]
pub struct TextRun {
    pub x: f32,
    pub baseline: f32,
    pub text: String,
    pub size_px: f32,
    pub color: Color,
    /// Bold text is faux-bolded by overprinting the glyphs at a small x-offset.
    pub bold: bool,
    /// Italic text is faux-slanted by an x-shear of the glyph run.
    pub italic: bool,
    /// `text-shadow` as `(offset-x, offset-y, color)`, painted behind the glyphs.
    pub shadow: Option<(f32, f32, Color)>,
    /// `letter-spacing`: extra pixels inserted after each glyph (0 = none).
    pub letter_spacing: f32,
}

/// A filled rectangle in canvas pixels (e.g. an element background), optionally
/// with rounded corners (`radius`).
#[derive(Clone, Copy, Debug, Default)]
pub struct RectFill {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: Color,
    pub radius: f32,
}

/// A flat list of paint commands. Rectangles paint first (backgrounds), then text.
#[derive(Clone, Debug, Default)]
pub struct DisplayList {
    pub rects: Vec<RectFill>,
    pub runs: Vec<TextRun>,
}

/// Rasterize a [`DisplayList`] onto a transparent [`Canvas`] in one pass: filled
/// rects behind colored text runs.
pub fn render_display_list(list: &DisplayList, font: &Font, width: u32, height: u32) -> Canvas {
    let mut children: Vec<Node> = Vec::with_capacity(list.rects.len() + list.runs.len());
    for r in &list.rects {
        children.push(rect_node(r));
    }
    for run in &list.runs {
        push_run_nodes(font, run, &mut children);
    }
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

fn rect_node(r: &RectFill) -> Node {
    let mut path = Path::new();
    let rad = r.radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
    if rad <= 0.5 {
        path.move_to(Point::new(r.x, r.y));
        path.line_to(Point::new(r.x + r.w, r.y));
        path.line_to(Point::new(r.x + r.w, r.y + r.h));
        path.line_to(Point::new(r.x, r.y + r.h));
        path.close();
    } else {
        // Rounded rectangle: straight edges joined by quadratic corner arcs.
        let (x, y, w, h) = (r.x, r.y, r.w, r.h);
        path.move_to(Point::new(x + rad, y));
        path.line_to(Point::new(x + w - rad, y));
        path.quad_to(Point::new(x + w, y), Point::new(x + w, y + rad));
        path.line_to(Point::new(x + w, y + h - rad));
        path.quad_to(Point::new(x + w, y + h), Point::new(x + w - rad, y + h));
        path.line_to(Point::new(x + rad, y + h));
        path.quad_to(Point::new(x, y + h), Point::new(x, y + h - rad));
        path.line_to(Point::new(x, y + rad));
        path.quad_to(Point::new(x, y), Point::new(x + rad, y));
        path.close();
    }
    Node::Path(PathNode::new(path).with_fill(Paint::Solid(rgba_of(r.color))))
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
    let root = build_run(font, text, size_px, origin_x, baseline_y, 0.0);
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

/// Blit a source RGBA image into `dst` (a `dst_w`×`dst_h` RGBA buffer) at the
/// destination rect, nearest-neighbor scaled, source-over. Pixels outside `dst`
/// are clipped.
#[allow(clippy::too_many_arguments)]
pub fn blit_rgba(
    dst: &mut [u8],
    dst_w: u32,
    dst_h: u32,
    dest_x: i32,
    dest_y: i32,
    dest_w: u32,
    dest_h: u32,
    src: &[u8],
    src_w: u32,
    src_h: u32,
) {
    if dest_w == 0 || dest_h == 0 || src_w == 0 || src_h == 0 {
        return;
    }
    for dy in 0..dest_h as i32 {
        let py = dest_y + dy;
        if py < 0 || py >= dst_h as i32 {
            continue;
        }
        let sy = (dy as u32 * src_h / dest_h).min(src_h - 1);
        for dx in 0..dest_w as i32 {
            let px = dest_x + dx;
            if px < 0 || px >= dst_w as i32 {
                continue;
            }
            let sx = (dx as u32 * src_w / dest_w).min(src_w - 1);
            let s = (sy * src_w + sx) as usize * 4;
            let d = (py as u32 * dst_w + px as u32) as usize * 4;
            let a = src[s + 3] as u32;
            for c in 0..3 {
                dst[d + c] = ((src[s + c] as u32 * a + dst[d + c] as u32 * (255 - a)) / 255) as u8;
            }
            dst[d + 3] = 255;
        }
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
    let mut children: Vec<Node> = Vec::with_capacity(runs.len());
    for run in runs {
        push_run_nodes(font, run, &mut children);
    }
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

/// Append the colored glyph node(s) for a [`TextRun`] to `out`. Bold runs are
/// faux-bolded by overprinting a second copy offset ~0.6px on the x-axis, which
/// thickens the strokes without a dedicated bold face.
fn push_run_nodes(font: &Font, run: &TextRun, out: &mut Vec<Node>) {
    // Paint the text-shadow copy first (behind the glyphs).
    if let Some((dx, dy, scolor)) = run.shadow {
        let spaint = Paint::Solid(rgba_of(scolor));
        let mut sgroup = build_run(
            font,
            &run.text,
            run.size_px,
            run.x + dx,
            run.baseline + dy,
            run.letter_spacing,
        );
        if run.italic {
            sgroup.transform = sgroup.transform.compose(&Transform2D::skew_x(-0.21));
        }
        for child in &mut sgroup.children {
            recolor(child, &spaint);
        }
        out.push(Node::Group(sgroup));
    }
    let paint = Paint::Solid(rgba_of(run.color));
    let offsets: &[f32] = if run.bold { &[0.0, 0.6] } else { &[0.0] };
    for &dx in offsets {
        let mut group = build_run(
            font,
            &run.text,
            run.size_px,
            run.x + dx,
            run.baseline,
            run.letter_spacing,
        );
        // Faux-italic: shear the run's baseline-local space so glyph tops lean right.
        if run.italic {
            group.transform = group.transform.compose(&Transform2D::skew_x(-0.21));
        }
        for child in &mut group.children {
            recolor(child, &paint);
        }
        out.push(Node::Group(group));
    }
}

/// Build the placed-glyph run group for `text` (baseline at `origin_x`,
/// `baseline_y`). `letter_spacing` shifts each glyph right by its index times the
/// spacing. Shared by [`render_text`] and diagnostics.
fn build_run(
    font: &Font,
    text: &str,
    size_px: f32,
    origin_x: f32,
    baseline_y: f32,
    letter_spacing: f32,
) -> Group {
    let placed = Shaper::shape_to_paths(&font.chain, text, size_px);
    let children: Vec<Node> = placed
        .into_iter()
        .enumerate()
        .map(|(i, (_face_idx, node, tf))| {
            // Move glyph `i` right by `i * letter_spacing` in the run-local space.
            let tf = if letter_spacing != 0.0 {
                Transform2D::translate(i as f32 * letter_spacing, 0.0).compose(&tf)
            } else {
                tf
            };
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
    fn letter_spacing_offsets_each_glyph() {
        let Some(font) = system_font() else {
            eprintln!("no system font found; skipping");
            return;
        };
        // Each glyph i is shifted right by i*spacing relative to no spacing.
        let xs = |ls: f32| -> Vec<f32> {
            let g = build_run(&font, "abc", 16.0, 0.0, 0.0, ls);
            g.children
                .iter()
                .map(|c| match c {
                    Node::Group(grp) => grp.transform.e,
                    _ => 0.0,
                })
                .collect()
        };
        let base = xs(0.0);
        let spaced = xs(5.0);
        assert_eq!(base.len(), 3);
        assert_eq!(spaced.len(), 3);
        for i in 0..3 {
            let expected = base[i] + i as f32 * 5.0;
            assert!((spaced[i] - expected).abs() < 0.01, "glyph {i}: {} vs {expected}", spaced[i]);
        }
    }

    #[test]
    fn with_fallback_keeps_font_usable() {
        let Some(font) = system_font() else {
            eprintln!("no system font found; skipping");
            return;
        };
        let before = font.measure("hello", 16.0);
        // Pushing the same bytes as a fallback must keep the primary working; garbage
        // bytes are ignored (font returned unchanged).
        let font = font.with_fallback(system_font_bytes().unwrap());
        let font = font.with_fallback(vec![0, 1, 2, 3]);
        let after = font.measure("hello", 16.0);
        assert!((before - after).abs() < 0.01, "primary face still drives measurement");
    }

    fn system_font_bytes() -> Option<Vec<u8>> {
        for path in [
            "/System/Library/Fonts/Geneva.ttf",
            "/System/Library/Fonts/Monaco.ttf",
            "/System/Library/Fonts/SFNS.ttf",
            "/System/Library/Fonts/Supplemental/Arial.ttf",
        ] {
            if let Ok(b) = std::fs::read(path) {
                return Some(b);
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
