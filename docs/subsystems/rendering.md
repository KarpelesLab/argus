# Subsystem: Rendering — Paint, Raster & Compositing

**Crates:** `argus-paint` (display lists), `argus-gfx` (rasterizer), `argus-image`
(decoders), `argus-compositor` (composite)
**Layer:** 1 (`argus-gfx`/`argus-image`), 2 (`argus-paint`), 4 (`argus-compositor`)
**Depends on:** `argus-layout`, `argus-geometry`, `argus-text`
**Consumed by:** `argus-engine`, the browser process, both embeddings

## Purpose

Turn the fragment tree into pixels. v1 is a **pure-Rust CPU rasterizer** that
produces identical output on every platform — ideal for headless screenshots and
reftests — with a GPU compositor planned later behind the same paint API.

## The three stages

```
fragment tree ──► PAINT ──► display list ──► RASTER ──► layer bitmaps ──► COMPOSITE ──► surface
 (argus-layout)  (argus-paint)             (argus-gfx)                 (argus-compositor)
```

### 1. Paint — `argus-paint`
Walks the fragment tree in CSS paint order (stacking contexts, z-index, the
backgrounds→borders→content→outline ordering) and emits a **display list**: a flat,
serializable sequence of high-level paint commands (fill rect, fill/stroke path,
draw glyph run, draw image, push/pop clip, push/pop transform, push layer with
opacity/blend/filter). It also builds the **hit-test tree** used by
`argus-events`. The display list is the *only* artifact that crosses the
process boundary to the compositor — it carries no DOM or CSS, just drawing.

### 2. Raster — `argus-gfx`
An in-house 2D rasterizer: anti-aliased path filling (scanline/coverage with
nonzero & even-odd), stroking, gradients, image sampling with filtering, clipping,
group opacity/blend modes, and glyph rendering (rasterizing outlines from
`argus-text`, with a glyph atlas cache). Renders a display list (or a layer's
slice of it) into an RGBA bitmap. Tiling allows multi-threaded raster.

### 3. Composite — `argus-compositor`
Assembles layer bitmaps into the final window surface (or off-screen buffer for
headless), applying layer transforms, **scroll offsets**, and opacity. Because
scroll and many transforms/opacity animations are handled here, common
interactions repaint nothing — they recomposite existing layers. Output goes to an
`argus-platform` window surface (GUI) or a `FrameBuffer` (headless capture).

## Key data structures

- **Display list** — flat command buffer, serializable, deterministic; versioned
  so the compositor can be a different process/version.
- **Layer tree** — the subset of the fragment/stacking structure promoted to
  independent compositor layers (scrollers, transforms, opacity animations, `<video>`,
  `<canvas>`). Keeps animation cheap.
- **Glyph atlas / image cache** — rasterized glyphs and decoded images cached across
  frames.
- **Damage / dirty rects** — only changed regions re-raster and re-composite.

## Design decisions

1. **CPU first, deterministic.** No GPU dependency in v1; the rasterizer is the
   reference implementation and the source of truth for tests. Determinism (same
   pixels everywhere) is a feature, not a temporary limitation.
2. **Paint API stable across backends.** The display-list/layer interface is
   designed so a future `argus-gpu` compositor (wgpu or an in-house Vulkan/Metal
   abstraction) drops in behind it without touching paint or layout.
3. **Compositor is trusted and dumb.** It understands drawing and layers, nothing
   web. Untrusted content hands it a validated display list; it cannot be steered
   into DOM/script-level mischief.
4. **Layerize for interaction, not everything.** Over-layering wastes memory;
   promote only what benefits (scroll, animated transform/opacity, video/canvas).

## Boundaries

- Knows nothing about CSS cascade or DOM; consumes fragments and a display list.
- Image *decoding* is `argus-image`/the media service; raster only samples decoded
  bitmaps.
- Window/surface creation is `argus-platform`; the compositor presents into it.

## Spec references

CSS paint order & stacking (CSS 2.1 Appendix E, CSS Positioned Layout), CSS
Backgrounds & Borders, Images, Filter Effects, Compositing & Blending, Transforms.
Validated by reftests/screenshot diffs.

## Open questions

- Compositor process placement (browser process for v1; own process with GPU —
  decide before Phase 6, see [`../ARCHITECTURE.md`](../ARCHITECTURE.md) §7).
- Display-list representation: enum command stream vs. typed builder; eviction of
  glyph/image caches.

## Roadmap mapping

Phase 1 (paint → CPU raster → composite to window for static pages), Phase 2
(damage-based repaint on DOM change), Phase 6 (compositor layers, GPU backend).
