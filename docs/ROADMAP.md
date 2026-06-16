# Argus Roadmap

A phased plan from empty repository to a usable browser. Each phase has a **theme**,
a **demo milestone** (the one-sentence "it can now do X" proof), concrete **exit
criteria**, and the **crates/subsystems** it builds. Phases are sequential in their
load-bearing parts but overlap at the edges.

The ordering reflects the four locked decisions: **multi-process from the start**,
**CPU raster first**, **in-house / OS-thin**, **GUI first** (headless follows as a
co-equal embedding, not an afterthought).

> Dates are deliberately omitted — this is a dependency-ordered plan, not a
> schedule. Effort markers: 🟢 small · 🟡 medium · 🔴 large · 🔴🔴 very large.

## Current status (snapshot)

Per-phase detail follows. Each phase lists its state and the concrete capabilities
shipped so far; the trailing "remaining" bullet in each phase names what is still
open. (This used to be a single mega-table with one thousand-word cell per phase —
it is now broken into sections and lists for readability.)

### Phase 0 — Foundations / multi-process — ✅ complete

- Sandbox, IPC, shared-memory framebuffer, AppKit window, CI.

### Phase 1 — Static document to pixels — ✅ essentially complete

- **HTML → DOM** with an **html5lib-format tree-construction conformance harness** at
  100% over a curated core set: implicit `tbody`/`tr`, `p`/`li`/`dt`/`dd`/`option`/
  `button` auto-closing, `pre`/`textarea` leading-newline strip, table text
  **foster-parenting**, RCDATA/RAWTEXT.
- **CSS cascade + box model**, block/inline layout, lists/`<hr>`.
- **Text shaping + raster**, with **glyph-fallback fonts** — emoji/CJK/symbol system
  faces pushed onto the `FaceChain` for code points the primary font lacks.
- **Networking** over rsurl.
- **Images** PNG/GIF/JPEG/WebP/QOI/ICO/BMP: CSS `width`/`height` size images (over the
  legacy attrs), oversized images shrink to the content box keeping aspect,
  **`max-width`** caps images (the `max-width:100%` responsive idiom); sized images
  **flow inline** with text as atomic boxes honoring `vertical-align`; responsive
  images resolve consistently (**`srcset`** picks the best `w`/`x` candidate,
  **`<picture>`** selects the first `<source>` whose `type` we decode and whose
  `media` matches before falling back to the `<img>`); broken images fall back to
  block alt text.

### Phase 2 — Scripting & dynamic DOM — 🟡 in progress

Page `<script>` runs in kataan; **synchronous DOM bindings work** via a JS-side shim
+ reconciliation — no kataan host-callback API needed (ES6 Proxy / `Object.define-
Property` / `JSON` suffice).

- **DOM query/build**: `document.getElementById` / `querySelector` (full selector
  engine) / `querySelectorAll` / `getElementsByTagName` / `getElementsByClassName`
  (JS-side collections over a seeded element tree, scoped too) / `createElement` /
  `body` / `write`.
- **Element ops**: `textContent` / `innerHTML` / `className` / `classList` /
  `setAttribute` / `style.*` / scoped `querySelector`; **tree traversal**
  (`parentElement`/`children`/`nextElementSibling`/…, `nodeType`/`nodeName`/`tagName`/
  `hasChildNodes`, `document.documentElement`/`head`/`body`); `matches`/`closest`/
  `contains`; `appendChild`/`prepend`/`before`/`after`/`replaceWith`/`replaceChildren`/
  `insertAdjacentElement`/`remove`. The shared `argus-domscript` crate makes
  `--dump-dom`/`--dump-text`/`--dump-a11y` reflect the post-script DOM.
- **Interactive click events**: `addEventListener('click')`/`onclick` fire and
  accumulate JS + DOM state via deterministic event replay (the windowed browser
  hit-tests id'd elements and re-runs the history).
- **Timers**: `setTimeout`/`setInterval`/`requestAnimationFrame` callbacks run
  (drained synchronously, delay-ordered, no wall clock; rAF gets a synthetic
  timestamp).
- **Storage + location**: `localStorage` (persisted across navigations in-session) /
  `sessionStorage`; `window.location` (read-only view seeded from the page URL:
  `href`/`protocol`/`hostname`/`pathname`/`search`/`hash`/`origin`).
- **Keyboard text input**: click a text field to focus and type (backspace deletes;
  **`maxlength` caps the length**); typed values survive event replay.
- **Focus ring + Tab navigation**: the focused field is marked with a `__argus_focus`
  sentinel attribute each render so the UA sheet draws a blue focus outline, and
  **Tab** advances focus to the next editable field (wrapping, skipping non-editable
  controls).
- **Promises/microtasks + `async`/`await`**: DOM writes inside `Promise.then` /
  awaited continuations are reconciled — scripts run through
  `argus_script::run_with_followup`, which drains kataan's event loop before the
  recorded ops are read back.
- **Geometry read-back**: `getBoundingClientRect`/`getClientRects`/`offset*`/`client*`
  return the real box of an `id`'d element from the most recent layout (one frame
  behind, fed back deterministically; no-geometry reads zero; `scrollTop/Left` stay
  0); `window.innerWidth/innerHeight/scrollX/scrollY/pageXOffset/pageYOffset` reflect
  the real viewport + scroll; `window.getComputedStyle(el)` returns a curated property
  set (color/background-color/display/font-size/-weight/-style/text-align/opacity/
  visibility) computed by the cascade and fed back like geometry.
- **Remaining**: real-time (wall-clock) timers, continuous events (mousemove/keydown),
  on-disk storage across browser restarts, and reading back computed layout still want
  a real embedding API ([upstream/kataan.md](upstream/kataan.md)).

### Phase 3 — Chrome, navigation & services — 🟡 in progress

- **Navigation**: links → fetch → re-render, with **charset-aware HTML decoding**
  (UTF-8 when valid/declared, else windows-1252/Latin-1 so legacy pages don't
  mojibake); URL + subresource resolution incl. **`<base href>`** overriding the
  relative base for the headless extractors.
- **Scrolling**: **scroll-wheel** + **keyboard scrolling** (Page Up/Down, Space, arrow
  up/down, Home/End scroll when no text field is focused — content computes the
  clamped target offset and reports it via the scroll sentinel, so Space still types
  into a focused field).
- **`#fragment` anchors**: clicking `<a href="#id">` scrolls to the target's document
  position instead of reloading (content reports the Y via a `\u{1}scroll:` sentinel
  `ClickResult`; the browser clamps + sets the tab's scroll and re-renders);
  **cross-page `page#id` deep links** scroll too (after the navigation renders, the
  browser sends `ScrollToFragment` and applies the reported Y).
- **Networking**: **persistent cookie jar**; **HTTP cache** (Cache-Control `max-age` +
  `Expires`/`Date` freshness; **conditional revalidation** — stale `ETag`/
  `Last-Modified` entries refetch with `If-None-Match`/`If-Modified-Since` and refresh
  in place on `304`).
- **CSP enforcement** (inline-script `script-src`/`default-src`) from `<meta>` and
  **response headers threaded across IPC** (net service extracts the
  `Content-Security-Policy` header — preserved through the HTTP cache — into
  `ResourceLoaded`; the browser carries it into `LoadDocument`; content enforces it on
  every script run via `apply_scripts_session_geom_csp`), **all policies enforced**
  (multiple metas + headers; strictest wins): `script-src-elem` CSP3 precedence;
  **`'nonce-…'` allow-listing** (case-sensitive; `'unsafe-inline'` ignored when a
  nonce source is present, per CSP3); **`'sha256/384/512-…'` hash-source allow-listing**
  (inline-script body digest via **purecrypto**, base64-compared; hashes disable
  `'unsafe-inline'` per CSP3). **`img-src` enforcement** (`<img>` + background-images):
  a blocked source is never fetched/decoded, so it doesn't render — source list models
  `'none'`/`*`/`'self'`/scheme-sources (`data:`/`https:`)/host-sources (with a `*.`
  subdomain wildcard), with `img-src` falling back to `default-src`; the page URL
  (threaded into `LoadDocument`) resolves `'self'`/relative sources.
- **History**: **reload** (Cmd+R, keeping scroll position); **back/forward** (Cmd+`[`/
  `]`) with **per-entry scroll restoration** (each entry remembers the offset it was
  left at; a new navigation starts at the top).
- **Multi-tab**: Cmd+T new / Cmd+W close / Cmd+Shift+`[`/`]` prev-next / Cmd+1…9 jump —
  each tab keeps its own history + scroll; a **visual tab-bar** (per-tab rectangles
  with the active one accented, a `+` button; clicks switch / close (right edge) /
  open; page clicks offset past the strip); **one isolated content process per tab**
  (own sandboxed process, DOM/JS/scroll preserved when inactive, instant switch-back;
  closing a tab shuts down + reaps it).
- **Remaining**: more CSP directives (`style-src`/`connect-src`; external `<script
  src>` isn't executed at all, so `script-src` for it is moot); `report-uri`/`report-to`.

### Phase 4 — Layout & CSS breadth — 🟡 in progress

**Box model & block flow**

- Box model, **`box-sizing`**, **`display: inline-block`** (atomic box laid at the
  origin then shifted into the inline line, sizing the line box to its height),
  **min/max-width**, **line-height**, **`margin: 0 auto`** block centering.
- **`float: left/right`** (out-of-flow at the content-box edge, inline text flows
  around it via per-line float bands; floats contained by their block; multiple stack
  then drop to the next band) + **`clear: left/right/both`**.
- **position: relative** + **absolute** (anchored to the nearest positioned ancestor's
  padding box via `top`/`left`/`right`/`bottom`) + **fixed** (anchored to the viewport
  and **scroll-stable** — stays put as the page scrolls) + **sticky** (flows normally,
  then sticks to its `top`/`bottom` inset once scrolled past it). `layout_scrolled`
  threads the scroll offset; fixed/sticky subtrees are hoisted into an **overlay pass**
  (background rects → background-images → text → `<img>`s painted as a unit above the
  base layer via the shared `paint_layer`) so they — and any image they contain —
  occlude the content scrolling under them, ordered by **`z-index`** then document order
  (nested boxes take the innermost level). `--dump-page --scroll=N` captures a scrolled
  frame.
- **CSS logical properties** (`inline-size`/`block-size` + min/max, `margin`/`padding`/
  `inset`-`inline`/`block`), **`min-height`**/**`max-height`**, **`aspect-ratio`**.
- **`overflow: hidden`/`clip`** (descendant paint confined to the border box; a
  definite `height` becomes a hard size; nested clips intersect and track
  `position: relative`) + **`clip-path: inset()`**.

**Inline & text wrapping**

- text-align (incl. **justify** + direction-relative **start/end**),
  **text-transform**, **`letter-spacing`**, **`word-spacing`**, **`text-indent`**.
- **white-space: pre/nowrap/pre-line/pre-wrap** (+ **`tab-size`**),
  **`overflow-wrap`/`word-break: break-word`**, **`<wbr>` + `&shy;` + ZWSP** breaks,
  **`&nbsp;` non-breaking**, **`text-overflow: ellipsis`**, **visibility**, **`<br>`**,
  **vertical-align** (sub/sup; top/middle/bottom for inline-block boxes).

**Positioning effects, transforms, outline**

- **`outline` + `outline-offset` + `outline-style`** (outside the border box, no layout
  effect; solid/double/dotted/dashed).
- **`transform: translate()`**, **`scale()`**, and **`rotate()`** (paint the subtree
  shifted/scaled/rotated about the border-box center; no layout effect). `rotate()`
  accepts `deg`/`rad`/`grad`/`turn` and rotates both boxes and text via a `Transform2D`
  applied in gfx (glyph orientation stays correct); hit regions use the rotated bbox.

**Generated content, at-rules, custom properties**

- **`::before`/`::after`** (string `content` + **`attr()`** + concatenation, inline &
  block; CSS `\<hex>` escapes; **`open-quote`/`close-quote`**, UA `<q>` quotes; **CSS
  counters**; **`content: url(...)` generated images** — rendered as an inline
  replaced box, intrinsic-sized; the URL is collected pre-layout from the cascade
  and fetched like an `<img>`).
- **`@media`** (min/max-width + prefers-color-scheme/hover/pointer/orientation),
  **`var()`** + **`calc()`/`min()`/`max()`/`clamp()`**, **`@supports`**.

**Selectors** (descendant/child/`+`/`~` combinators; correct specificity)

- attribute, `:first/last/only-child`, **`:nth-child`/`:nth-last-child`**,
  **`:first/last/only/nth/nth-last-of-type`**.
- **`:not()`** (CSS4 list form), **`:is()`/`:where()`**, **`:has()`** (relational —
  `div:has(img)`, `:has(> .x)` tolerated; contributes the argument's specificity),
  **`:root`/`:empty`/`:lang()`**.
- **form-state** (`:checked`/`:disabled`/`:enabled`/`:required`/`:read-only`/
  `:optional`/`:read-write`), **`:focus`/`:focus-visible`/`:focus-within`**
  (focused field marked `__argus_focus` before the cascade), **`:placeholder-shown`**,
  **`:target`** (URL-fragment element — CSS-only tabs/lightboxes work).

**Lists / tables / legacy attrs**

- **list-style-type** (disc/circle/square, decimal(-leading-zero), lower/upper-alpha,
  lower/upper-roman, lower-greek) + **`<ol start>`/`reversed`/`<li value>`** + **`type`
  attr** + **`list-style-position`**, `<hr>`.
- **tables**: content-based ("auto") column widths + **`<colgroup>`/`<col>`** pinning,
  **`colspan`/`rowspan`**, **`caption-side`**, **`<thead>`/`<tfoot>`** reordering,
  **`border-spacing`**, **`border-collapse: collapse`**, **cell `vertical-align`**.
- **legacy presentational attributes** (`align`, `bgcolor`/`<font>`, cell `width`/
  `height`, `<table border>`/`cellpadding`, `<td nowrap>`, `<hr>` attrs, `<center>`)
  mapped to CSS; UA defaults for `<fieldset>`/`<legend>`, `<dd>` indent, hidden
  `<template>`/`<datalist>`/closed `<dialog>`/`<input type=hidden>`.

**Form controls & interaction**

- input/textarea/button render with their value; submit/reset default labels;
  **editable `<textarea>`** (multi-line; Enter inserts a newline; edited `value`
  submits); **`<progress>`/`<meter>`** bars; **`<input type=password>`** masked (value
  still submits); **`<input type=color>`** swatch; **`<input type=range>`** track+thumb;
  **`accent-color`**.
- **checkbox/radio toggling + `<select>` cycling** (click an id'd control to
  flip/select/advance; persisted via `checked`/`selected` maps, fed to submission);
  **`<label>` activation**; **`<details>`/`<summary>` disclosure** (sentinel-href the
  content process intercepts).
- **GET form submission** (serialize the `method=get` form to `action?query`; **Enter**
  submits too); **`method=post` submission** (`SubmitRegion`/`post_body` → browser
  `PostUrl`s to the net service, rsurl POST through the cookie jar, never cached;
  action URL pushed to history, body not replayed).

**Images**

- **broken-image `alt` text**; **`object-fit`** (contain/cover/fill) +
  **`object-position`**; **`filter`** on images (`grayscale`/`sepia`/`invert`/`opacity`/
  `brightness`/`contrast`/`saturate` per-pixel; `blur` parsed not applied; non-image
  filters want offscreen compositing).

**Flexbox**

- row + **`flex-direction: column`**, **`justify-content`**, **`align-items`** +
  **`align-self`**; **`flex-basis`** (+ `flex` shorthand), **`flex-grow`**,
  **`flex-wrap`** (per-line align/justify), **`flex-shrink`** (respects `min-width`),
  **`order`**.

**Grid**

- row-major flow, **`grid-template-columns`/`-rows`** (fixed + `fr` + `auto` +
  `repeat()`/`minmax()`), **`span N`** across columns & rows (cell-occupancy
  auto-placement, measured heights), **line-based placement** (`2 / 4` / `-start`),
  **gap** (+ separate `row-gap`/`column-gap` + shorthand).

**Text styling & fonts**

- **bold** + **italic** (faux overprint / x-shear) + **per-run font selection**
  (`font-family` → face key) + **web fonts (`@font-face`)** (fetched + registered; `src`
  prefers raw sfnt > **WOFF** (zlib) > **WOFF2** (Brotli); UA monospace for
  `<code>`/`<pre>`/…; **weight/style matching** suppresses faux synthesis).
- **`text-shadow`** (blur ignored), **`box-shadow`** (blur as fading concentric layers;
  inset ignored), underline + **line-through** + **overline** (with
  **`text-decoration-color`/`-style`** solid/double/dotted/dashed; propagate to
  descendant inlines, child `none` overrides).

**Borders, backgrounds, color**

- **border-radius**, **per-side border colors** (incl. the 1–4-value `border-color`
  and `border-width` box shorthands, CSS edge order with replication),
  **`border-style`** (none/hidden,
  solid/double/dotted/dashed); **mitered border corners** — each solid border edge
  is a trapezoid from the outer to the inner (padding-box) corner, so adjacent
  differently-colored sides meet on the diagonal, and a `0×0` box with thick borders
  yields the **CSS-triangle technique** (tooltip arrows, dropdown carets, speech
  bubbles) via an optional polygon fill on `RectFill`.
- **`background-image: url()`** (URL resolved at layout via `cascaded_value`; two-pass
  render rects → backgrounds → text; **`background-repeat`** repeat/no-repeat/repeat-x/
  repeat-y, clipped; **`background-size: cover`/`contain`** + **`background-position`**
  — hero backgrounds work), **`background:
  linear-gradient`** (multi-stop, `to <side>`/`<angle>`) + **`radial-gradient`**.
- **opacity**, **color syntax** (`#rgb(a)`/`#rrggbb(aa)`, `rgb()`/`hsl()`/`hwb()`/
  `oklab()`/`oklch()`, full named-color set, **`color-mix()`**).

**Bidi & Arabic**

- **`direction: rtl`/`dir=rtl`** + **character-level bidi reordering** (UAX#9) +
  **Arabic contextual joining** (Presentation-Forms-B reshaping); per-line.

**Remaining**: AVIF (av1) + `<video>` first-frame are wired through the oxideav
demux/codec pipeline but graceful-`None` until the upstream pixel codecs land;
continuous video/audio playback, GPU compositor & Phase-2 wall-clock/continuous-event
JS remain.

### Phase 5 — Web platform & headless — 🟡 in progress

- **Headless surfaces**: `--dump-page` (off-screen render to PNG; **`--width`/
  `--height`** drive `@media`/layout), `--dump-dom`, `--dump-a11y`, **`--dump-text`**
  (skips hidden/`display:none`/`visibility:hidden`), **`--dump-links`** (`<a>`/`<area>`
  hrefs), **`--dump-headings`**, **`--dump-forms`** (controls with name/type/value),
  **`--dump-meta`** (title/lang/charset/description/canonical/favicon/hreflang/feed/
  refresh/`og:`+`twitter:`), **`--dump-json`** (`{title, headings, links}`),
  **`--dump-jsonld`** (every ld+json block), **`--dump-microdata`** (`itemscope`/
  `itemprop` → `{type, props}`), **`--dump-domtree`** (post-script DOM as nested JSON —
  a CDP-style snapshot), **`--dump-tables`** (TSV), **`--dump-images`** (src/alt/dims,
  srcset fallback), `--eval` (JS).
- **On-disk `localStorage` persistence** — the trusted browser owns a store file
  (`$ARGUS_STORAGE` or `~/.argus_localstorage`), seeds each content process at spawn
  via `ProvideStorage`, and rewrites the file on `StorageChanged`, so it survives
  restarts (escaped `key\tvalue` via `encode_storage`/`decode_storage`).
- **Remaining**: Web API breadth (needs JS bindings) and full CDP.

### Phase 6 — Media & richer rendering — 🟡 in progress

- **Image decode**: PNG + GIF + **JPEG** (oxideav-mjpeg, YUV→RGBA) + **WebP** (lossless
  + lossy VP8) + **QOI** + **ICO/CUR favicons** + **BMP** (1/4/8-bit palette incl.
  RLE4/RLE8, 24/32-bit) + **TGA** (true-color/grayscale/palette, RLE, fails closed) +
  **Netpbm** (PPM/PGM, ASCII + binary) + **PCX** (RLE) + **TIFF** (uncompressed/
  PackBits/LZW/Deflate, 8/16-bit, predictor, strips + tiles, both byte orders).
- **`<video>`/`<audio>` placeholders** (dark video box with a play square, or a
  `<video poster>` as its frame; a thin audio bar) plus **`<video>` first-frame wiring**
  — poster/src/`<source>` URLs decoded through the first-party demux pipeline
  (`decode_video_frame`: oxideav-mp4/mkv probe + demux → oxideav-h264/vp9/av1) and
  rendered as the frame still; graceful-`None` until the upstream codecs land.
- **Multi-frame decode core** — `decode_video_frames(bytes, max_frames)` demuxes +
  decodes up to N leading frames in presentation order (draining B-frame reorder), the
  decode half of playback/scrubbing; graceful-empty until the codecs land, fuzz-verified
  to fail closed.
- **Remaining**: continuous playback scheduling (A/V sync, seeking, audio out,
  wall-clock); animations; GPU compositor.

### Phase 7 — Hardening / perf / conformance — 🟡 started

- **Robustness tests**: parser + **full layout-pipeline** (random inputs, finite
  geometry across rects/runs/images) + **CSS robustness** (stylesheet + inline-decl
  parsers, selectors, value parsers — never panic).
- **cargo-fuzz harness** (html/css/**layout**/**image**; the image target asserts every
  decode fails closed *and* any decoded buffer is exactly `w*h*4`) + **image-decoder
  fuzz** (all formats fail closed on hostile bytes — incl. the AVIF + `<video>`
  first-frame demux/codec container paths) + **DOM-ops JSON fuzz**.
- **Accessibility tree**: implicit + explicit `role` (input-`type`-refined roles,
  listbox/option, separator/meter/spinbutton, rowheader/columnheader, region/article/
  …); `aria-label`/`aria-labelledby`; caption naming (`<figcaption>`/`<legend>`/
  `<caption>`); `<label>` association; `aria-hidden` pruning; `role=presentation`/`none`;
  state annotations (`[disabled]`/`[checked]`/`[required]`/`[pressed]`/`[expanded]`/
  `[current]`).
- **Remaining**: WPT, perf, sandbox hardening.


Honest scope note: **`document`/`window` bindings now work** without any kataan
changes — kataan supports enough JS (ES6 `Proxy` traps, `Object.defineProperty`,
`JSON`, closures) to model the DOM in JS-space and reconcile mutations back into the
real tree (`crates/argus-domscript`). **Discrete event handling works too**:
`addEventListener('click')` handlers fire and state accumulates via *deterministic
event replay* (re-run the script + full interaction history each event).
**Asynchronous JS now reconciles too**: `setTimeout` (shim-queued, delay-ordered) and
native promises/microtasks/`async`-`await` both run before the DOM ops are read back
(via `argus_script::run_with_followup`, which drains kataan's event loop). What still
needs a real **embedding API** ([upstream/kataan.md](upstream/kataan.md)) is the
*wall-clock* asynchronous surface — real-time timers, continuous input events
(mousemove/keydown), and reading back computed geometry — plus performance (replay
is O(history)). Phases 4–7 are a large, multi-cycle effort beyond the current
foundation.

---

## Phase 0 — Foundations & the multi-process skeleton 🔴

**Theme:** stand up the workspace and the process/sandbox/IPC plumbing *before* any
web concept, so nothing is ever retrofitted across the security boundary.

**Demo milestone:** the browser process spawns a sandboxed content process and a net
service; the content process fills a window with a solid color via a shared-memory
framebuffer; killing the content process shows a "crashed" surface and the browser
survives.

**Builds:** `argus-util`, `argus-geometry`, `argus-ipc`, `argus-platform`,
`argus-compositor` (trivial blit), `argus-browser` (process manager + IPC router),
`argus-content` (sandboxed shell), `argus-services` (skeleton), `argus-shell` (blank window).

**Status: substantially complete.** Built as a single `argus` binary that re-execs
itself per role (`--role=…`); the on-screen window uses AppKit directly via `objc2`.

**Exit criteria:**
- [x] Cargo workspace with the crate skeleton compiling; CI runs fmt + clippy (`-D warnings`) + tests on macOS (Linux later).
- [x] `argus-platform` opens a native window and presents a shared-memory RGBA buffer on macOS.
- [x] Browser process spawns a content process **inside the OS sandbox** (macOS Seatbelt): no network, no fs write, verified by a self-probe that fails closed.
- [x] `argus-ipc`: versioned, length-prefixed typed messages + shared-memory regions (SCM_RIGHTS fd passing); a content process holds only its one channel to the browser.
- [x] Content process renders a solid color into the window through the compositor; input is plumbed end-to-end (a click is forwarded over IPC and logged in the sandboxed content process).
- [x] Crash isolation: content-process kill is contained; the browser and net service survive (covered by the `phase0` end-to-end test).
- [~] The embedder API shape (`Browser`/`Tab`/events) is sketched — currently a direct `run`/`run_windowed`; the typed embedder API is fleshed out alongside Phase 1.
- [ ] Service auto-restart on crash (deferred; services currently exit-and-reap).

**De-risks:** the hardest, least web-like plumbing — sandbox, IPC, shared memory,
crash handling — up front, per the user's explicit choice.

---

## Phase 1 — Static document to pixels 🔴🔴

**Theme:** the first real web page. Parse → style → layout → paint → composite, for
static HTML/CSS, fetched over the network.

**Demo milestone:** `argus-shell` navigates to an `https://` URL and renders a real,
text-and-box static page (e.g. a simple article) correctly, with system fonts.

**Builds:** `argus-html`, `argus-dom`, `argus-css`, `argus-style` (box-model/text
subset), `argus-text` (cmap + glyf + Latin metrics + UAX#14 line breaking),
`argus-layout` (block + inline), `argus-paint`, `argus-gfx` (AA fills + glyph
raster), `argus-net` + net service (http(s) GET over rsurl), `argus-engine`.

**Exit criteria:**
- [~] HTML parser runs **html5lib-format conformance harnesses** for both halves: tree-construction (`crates/argus-html/tests/html5lib_tree_construction.rs`, 37 core cases — implicit html/head/body, attribute sorting, text/entity merging, comments/doctype, `p`/`li`/`dt`/`dd`/`option`/`button` auto-closing with nested-list shielding, implicit table `tbody`/`tr`, table-text **foster-parenting**, `pre`/`textarea` newline strip, `<image>`→`<img>`/`</br>`/empty-`</p>` quirks, basic **SVG/MathML foreign-content** namespacing, RCDATA/RAWTEXT) and tokenizer (`tests/tokenizer_conformance.rs`, 14 cases — named/numeric char refs, attribute quoting, comments, doctype, RAWTEXT/RCDATA). Both pass at 100% and are CI-gated. The **full adoption agency algorithm** now runs — active-formatting-element reconstruction plus reparenting of misnested formatting (`<b>1<p>2</b>3`, incl. multi-level `<a>1<div>2<div>3</a>4`). Full table insertion modes, foreign-content integration points, template contents, and the full upstream corpus remain.
- [~] CSS parser + selector matching + cascade for the box-model/text/colors property subset (`argus-css`/`argus-style`); a **cascade conformance harness** (`crates/argus-style/tests/cascade_conformance.rs`) gates specificity ordering (id > class > type > `*`), source-order tiebreaks, `!important`, inline-vs-author precedence, and inheritance. A standalone `getComputedStyle` JS binding (vs the internal `computed_style`) still wants the embedding API.
- [ ] Block + inline layout produces a correct fragment tree for paragraphs, headings, nested blocks, basic margins/padding/borders, and wrapped Latin text.
- [ ] `argus-text` loads a system font, shapes Latin LTR, and the rasterizer draws anti-aliased glyphs + filled rects/borders.
- [ ] Net service fetches documents and subresources (CSS) via rsurl with TLS (purecrypto); content process gets bytes only over IPC.
- [ ] A curated set of static pages renders pixel-correct against reference screenshots (deterministic CPU raster).

**De-risks:** the entire read-only pipeline and the in-house text/raster bet.

---

## Phase 2 — Scripting & a dynamic DOM 🔴🔴

**Theme:** make pages alive. Embed kataan with Argus's own host bindings, the DOM
binding bridge, the event loop, and input → DOM events.

**Demo milestone:** a page's JavaScript mutates the DOM in response to a click, and
the change re-styles, re-lays-out, and repaints — a working counter/todo page.

**Builds:** `argus-script` (kataan realm + bindings + event loop), `argus-webapi`
(console, timers, DOM/events, `requestAnimationFrame`, `URL`), `argus-events` (hit
testing + DOM event dispatch), incremental style/layout/paint invalidation.

**Exit criteria:**
- [ ] A kataan realm per document with Argus-supplied global + `document` (NOT kataan's `std` host runtime — sandbox-safe bindings only).
- [ ] DOM/CSSOM bindings: element/attribute/text APIs, live collections, node↔wrapper map integrated with kataan GC (no leaks/dangles under a stress test).
- [~] WHATWG event loop: microtask checkpoint (Promises) + `async`/`await` + `setTimeout`/`setInterval` drain before reconciliation (via kataan's event loop through `run_with_followup`); `requestAnimationFrame` and full wall-clock "update the rendering" step ordering still want the embedding API.
- [ ] Input events hit-test against the paint tree and dispatch capture/target/bubble DOM events with correct coordinates/modifiers/default actions.
- [ ] DOM mutation triggers minimal restyle → relayout of dirty subtrees → damage-based repaint (not full-page).
- [ ] `<script>` execution integrates with the parser (blocking/`async`/`defer`).
- [ ] Runs a set of small interactive pages correctly; kataan passes its own test262 (engine-side).

**De-risks:** the JS↔DOM bridge and event-loop timing — historically where engines
get subtly wrong.

---

## Phase 3 — Browser chrome, navigation & the trusted services 🔴🔴

**Theme:** become an actual browser — multiple tabs (= multiple content processes),
real chrome, navigation/history, and the network policy a browser needs.

**Demo milestone:** browse the real web across multiple tabs: type a URL, click
links, submit a form, go back/forward, with cookies and the HTTP cache working and a
correct security indicator.

**Builds:** full `argus-shell` chrome (tabs, omnibox, nav buttons, dialogs),
`argus-browser` navigation controller + session history, `argus-security` (origins,
site instances, SOP, mixed content, basic CSP, TLS trust UX), `argus-net` (cache,
cookies, HSTS, redirects, CORS), `argus-storage` (cookie/HSTS/profile persistence),
process-per-site-instance with cross-site process swap.

**Exit criteria:**
- [ ] Tab strip + omnibox (URL/search, suggestions) + back/forward/reload/stop, drawn by Argus; alert/confirm/prompt/file-picker dialogs.
- [ ] Navigation algorithm: link click, form GET/POST, `location` assignment, server/client redirects, download vs. render; per-tab session history with correct back/forward and scroll restoration.
- [ ] Cross-site navigation swaps to a different content process; same-site stays; origin is stamped by the browser process on every brokered request.
- [ ] HTTP cache (freshness/validators/Vary, site-partitioned), cookies (SameSite/Secure/HttpOnly, partitioned), HSTS preload+dynamic, redirect handling.
- [ ] Same-origin policy, mixed-content blocking, CORS for subresources/`fetch`, and a basic CSP enforced; TLS cert validation policy + error/override UX + security indicator.
- [ ] Profiles persist cookies/HSTS/settings via the storage service; sessions restore open tabs after restart.

**De-risks:** the trusted-side policy + multi-process navigation, the load-bearing
security work.

---

## Phase 4 — Layout & CSS breadth, images 🔴🔴

**Theme:** render the *modern* web — flexbox, grid, positioning, floats, real text,
images, gradients, scrolling.

**Demo milestone:** complex real-world sites (a docs site, a dashboard layout) render
correctly, including images and scrollable regions.

**Builds:** `argus-layout` (flex, grid, abs/fixed positioning, floats, stacking,
writing modes), `argus-css`/`argus-style` (full selector set incl. `:has()`, cascade
layers, custom properties, media queries, `@font-face`), `argus-text` (complex-script
shaping + full bidi, web-font loading), `argus-image` (PNG/JPEG/GIF/WebP/AVIF),
gradients/backgrounds/borders/filters in `argus-gfx`, scroll handling in the compositor.

**Exit criteria:**
- [ ] Flexbox and CSS Grid pass a strong majority of the relevant WPT.
- [ ] Absolute/fixed/sticky positioning, floats, stacking contexts, overflow/scroll containers correct.
- [ ] Custom properties, cascade layers, `@scope`, media/container queries, `:has()` and the full selector set.
- [ ] Web fonts (`@font-face`) load via the net service with correct reflow/swap; bidi + at least one complex script shape correctly.
- [~] Images decode (`argus-image`: PNG/GIF/JPEG/WebP/QOI/ICO/BMP/TGA/Netpbm/PCX/TIFF via oxideav; **AVIF wired** through oxideav-avif + oxideav-av1 — graceful-`None` until the upstream AV1 pixel decoder lands, then renders with no further changes) and render at correct intrinsic sizing; gradients, border-radius present. object-fit, multiple backgrounds, box-shadow, filters remain.
- [ ] Smooth scrolling recomposites without relayout/repaint of static content.

**De-risks:** the long tail of CSS correctness and the in-house complex-text effort.

---

## Phase 5 — Web platform breadth & first-class headless 🔴🔴

**Theme:** the API surface real apps need, and the headless/automation embedding as a
co-equal face of the engine. Start running WPT broadly.

**Demo milestone:** `argus-headless` drives a page via a CDP client — navigate,
evaluate script, intercept network, screenshot — and a JS-heavy SPA (fetch + history
+ storage + canvas) works in both shell and headless.

**Builds:** `argus-headless` + CDP-like protocol, `argus-webapi` breadth (`fetch`,
`XMLHttpRequest`, `Headers`/`Request`/`Response`, `FormData`, `Blob`/`File`,
`localStorage`/`sessionStorage`, `IndexedDB`, `SubtleCrypto` via the crypto broker,
`Canvas` 2D, `History`/Navigation API, `structuredClone`, `TextEncoder`/`Decoder`),
`argus-storage` (Web Storage + IndexedDB + Cache API + quota), JS modules + dynamic
`import()`, the WPT harness integration.

**Exit criteria:**
- [ ] `fetch`/XHR over the net service with CORS/credentials/streaming/abort; `WebSocket`/`EventSource`.
- [ ] `localStorage`/`sessionStorage`, IndexedDB transactions/cursors/indexes, Cache API, `navigator.storage` quota — all origin-partitioned via the storage service.
- [ ] WebCrypto `SubtleCrypto`: key ops brokered to the crypto broker (purecrypto), data ops in-process; passes WebCrypto WPT subset.
- [ ] Canvas 2D context (paths, text, images, compositing) rendered by `argus-gfx`.
- [ ] ES modules, dynamic `import()`, import maps.
- [ ] Headless runner exposes Page/Runtime/DOM/Input/Network/Emulation CDP domains; an off-the-shelf CDP client can drive it. Real + synthetic input share one dispatch path.
- [ ] Continuous WPT runs in CI with a tracked, rising pass rate.

**De-risks:** breadth + the headless protocol; locks in "one engine, two faces."

---

## Phase 6 — Media & richer rendering 🔴

**Theme:** audio/video via oxideav, animations/transitions, compositor layers, and the
optional GPU compositor behind the stable paint API.

**Demo milestone:** a page plays `<video>` with synced audio in an isolated media
process while CSS animations run smoothly composited.

**Builds:** `argus-media` + media service (oxideav demux/decode, A/V sync, audio out
via `argus-platform`), `<video>`/`<audio>` element pipelines, CSS transitions/
animations + Web Animations API, compositor layerization (video/canvas/animated
transforms), and a prototype `argus-gpu` compositor backend.

**Exit criteria:**
- [ ] `<video>`/`<audio>` play common formats (per oxideav support) decoded in the sandboxed media service; A/V sync, seeking, buffering, `timeupdate`.
- [ ] Video presented as a compositor layer (no page repaint during playback); audio routed to the platform sink.
- [ ] CSS transitions/animations + Web Animations API run on the compositor where possible (transform/opacity off the main thread).
- [ ] A GPU compositor backend renders a layer tree behind the unchanged paint/display-list API, selectable at runtime, with CPU as the deterministic fallback.

**De-risks:** the oxideav integration and the GPU swap-in the paint API was designed for.

---

## Phase 7 — Hardening, performance & conformance 🔴 (continuous)

**Theme:** make it fast, stable, and trustworthy. Largely overlaps earlier phases.

**Exit criteria (ongoing targets):**
- [ ] Sandbox hardening on all target OSes (macOS, Linux, then Windows); fuzzing of the parsers (HTML/CSS), the net/IPC message decoders, and image/media decoders.
- [ ] Performance: layout/raster parallelism turned on, memory budgets + process reaping, bfcache, startup/navigation latency targets.
- [ ] Accessibility tree + IME; printing/PDF export.
- [ ] WPT pass-rate targets per subsystem; reftest suite green; no known cross-site isolation escapes.
- [ ] Stability: crash-free sessions over long browsing; graceful service restarts.

---

## Cross-cutting tracks (run throughout)

- **Conformance** — html5lib (Phase 1+), test262 (kataan, Phase 2+), WPT (Phase 3+
  broadly), reftests/screenshot diffs (Phase 1+).
- **Security** — threat-model review at each phase boundary; the sandbox/IPC
  invariants are CI-enforced, not aspirational.
- **Upstream feedback** — feature requests to kataan (host/DOM hooks, WASM surface),
  rsurl (priority/streaming/abort), purecrypto (WebCrypto coverage), oxideav (codec/
  demux API) gathered as each phase consumes them. Tracked as living checklists in
  [`upstream/`](upstream/README.md) — note that kataan's embedding API + GC gate Phase 2.
- **Docs** — keep `subsystems/` and `ARCHITECTURE.md` in sync as designs solidify;
  promote "open questions" to decisions with a short ADR note when resolved.

## Phase → crate quick map

| Phase | New/major crates |
|-------|------------------|
| 0 | util, geometry, ipc, platform, browser, content, services (skel), compositor (blit), shell (blank) |
| 1 | html, dom, css, style, text, layout, paint, gfx, net, engine |
| 2 | script, webapi (core), events |
| 3 | security, storage, browser (nav/history), net (cache/cookies/HSTS), shell (chrome) |
| 4 | layout (flex/grid/pos), style (full), text (complex/bidi/webfont), image, gfx (effects) |
| 5 | headless, webapi (breadth), storage (idb/web-storage), security (csp/perm) |
| 6 | media, image (more), compositor (layers), gpu (proto) |
| 7 | (hardening across all) |
