# Subsystem: Layout Engine

**Crates:** `argus-layout` (layout), `argus-text` (shaping/fonts/line breaking)
**Layer:** 2 (engine core; `argus-text` is Layer 1)
**Depends on:** `argus-style`, `argus-text`, `argus-geometry`, `argus-dom`
**Consumed by:** `argus-paint`, `argus-events` (hit testing), `argus-script` (geometry APIs)

## Purpose

Consume the styled tree and produce a **fragment tree**: the absolute geometry of
every box and text run, ready to paint. This is the engine's most algorithmically
involved subsystem.

## Responsibilities

- **Box generation** — styled elements → boxes, honoring `display` (block, inline,
  inline-block, flex, grid, none, contents), anonymous box creation, run-in/marker
  boxes, pseudo-element boxes.
- **Formatting contexts** — block formatting (margins/collapse/floats/clearance),
  inline formatting (line boxes, baselines, vertical-align), flexbox, grid,
  table, absolute/fixed positioning, stacking.
- **Fragmentation** — line breaking, and later pagination/multicol/overflow
  fragments.
- **Text layout** (`argus-text`) — font matching/fallback, shaping (GSUB/GPOS),
  bidi reordering (UAX #9), grapheme/line-break opportunities (UAX #14/#29),
  measuring runs, mapping to glyph IDs + positions.
- **Intrinsic sizing** — min-content/max-content for the above contexts.
- **Output** — the fragment tree: positioned, sized fragments with the resolved
  text runs and the back-pointers to DOM/style needed for hit testing and paint.

## Key data structures

- **Box tree.** The intermediate structure between styled tree and fragments;
  encodes formatting-context membership and anonymous boxes.
- **Fragment tree.** Immutable output: each fragment has a rect (in its containing
  block's space), a transform, clipping, a paint order key, and the source node.
  Geometry is absolute by the time it leaves layout.
- **Text runs & glyph buffers.** Shaped runs (font, glyph ids, advances, offsets)
  produced by `argus-text`, cached per (text, style, font) so re-layout of
  unchanged text is free.
- **Font stack.** `argus-text` owns an in-house OpenType/TrueType parser (glyf/CFF
  outlines, cmap, GSUB/GPOS, kern), a system-font enumerator (via `argus-platform`),
  and `@font-face`/web-font loading hooks.

## Design decisions

1. **Text via the first-party oxideav stack.** `argus-text` wraps
   **`oxideav-scribe`** (pure-Rust TTF/OTF parsing, shaping, bidi, line wrapping,
   glyph outlines) rather than binding HarfBuzz/ICU/FreeType or hand-writing it.
   Still consistent with the pure-Rust ethos — scribe is first-party. Glyph outlines
   feed `argus-gfx`/`oxideav-raster` for rendering. Latin/LTR is solid today;
   complex-script and full bidi ride on scribe's coverage.
2. **Fragment tree as a hard boundary.** Layout's only output is fragments. Paint
   and hit testing read fragments and never re-derive geometry — this keeps paint
   simple and the eventual GPU compositor decoupled from CSS semantics.
3. **Incremental relayout.** Dirty bits from style/DOM invalidation mark subtrees
   for relayout; clean subtrees reuse cached fragments and shaped runs. Containing
   blocks bound the dirty region.
4. **Correct before parallel, correct before fast.** v1 prioritizes spec-correct
   block + inline layout. Flex/grid follow; subtree parallelism follows that.

## Boundaries

- No painting, no rasterization, no display lists (that is `argus-paint`).
- No DOM mutation; consumes styled/flat tree read-only.
- Scroll *offsets* are applied at composite time, not baked into fragments (so
  scrolling doesn't force relayout).

## Spec references

CSS 2.1 visual formatting, CSS Display L3, Box Model, Flexbox L1, Grid L1/L2,
Positioned Layout, Writing Modes, Inline L3; UAX #9 (bidi), #14 (line break),
#29 (segmentation); OpenType spec. Conformance via WPT + reftests.

## Open questions

- Box-tree vs. direct styled-tree→fragment generation for simple cases.
- Shaping cache key/eviction; web-font swap and reflow timing.
- How much of `argus-text` is needed at Phase 1 (probably: cmap + glyf + basic
  horizontal metrics + UAX#14 line breaking for Latin).

## Roadmap mapping

Phase 1 (block + inline + basic Latin text → fragments), Phase 4 (flex, grid,
positioning, floats, complex text/bidi, web fonts), ongoing conformance.
