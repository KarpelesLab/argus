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

| Phase | State |
|-------|-------|
| 0 — Foundations / multi-process | ✅ complete (sandbox, IPC, shared-mem framebuffer, AppKit window, CI) |
| 1 — Static document to pixels | ✅ essentially complete (HTML→DOM, CSS cascade + box model, block/inline layout, lists/hr, text shaping + raster, networking over rsurl, images) |
| 2 — Scripting & dynamic DOM | 🟡 page `<script>` runs in kataan; **synchronous DOM bindings now work** via a JS-side shim + reconciliation — no kataan host-callback API needed (ES6 Proxy/`Object.defineProperty`/`JSON` suffice). Supports `document.getElementById`/`querySelector` (full selector engine)/`createElement`/`body`/`write`, and element `textContent`/`innerHTML`/`className`/`classList`/`setAttribute`/`style.*`/scoped `querySelector`/`appendChild`/`remove`; the shared `argus-domscript` crate makes `--dump-dom`/`--dump-text`/`--dump-a11y` reflect the post-script DOM too. **Interactive click events also work** — `addEventListener('click')`/`onclick` handlers fire and accumulate JS + DOM state via deterministic event replay (the windowed browser hit-tests id'd elements and re-runs the history). **`setTimeout`/`setInterval`** callbacks run too (drained synchronously, delay-ordered, no wall clock). Real-time timers, promises/microtasks, continuous/keyboard events, and reading back computed layout still want a real embedding API ([upstream/kataan.md](upstream/kataan.md)) |
| 3 — Chrome, navigation & services | 🟡 links → fetch → re-render, URL + subresource resolution, **scroll-wheel**, **persistent cookie jar**. Tabs/history/back-forward, CSP, HTTP cache remain |
| 4 — Layout & CSS breadth | 🟡 box model, **box-sizing**, **min/max-width**, **line-height**, text-align (incl. **justify**), **text-transform**, **white-space: pre/nowrap**, **visibility**, **`<br>`**, **vertical-align (sub/sup)**, **position: relative**, **`@media` queries** (min/max-width), **custom properties (`var()`)**, attribute + `:first/last-child` + **`:nth-child`** + **`:not()`** selectors (with descendant/child combinators), lists + **list-style-type**, `<hr>`, **tables**, **flexbox**, **grid** + **gap**, underline + **line-through**, **border-radius**, **opacity**. Floats, absolute/fixed positioning, `flex-grow`/`justify`/`align`, grid spans/`fr`, web fonts, complex text remain |
| 5 — Web platform & headless | 🟡 headless surfaces: `--dump-page`, `--dump-dom`, `--dump-a11y`, **`--dump-text`**, `--eval` (JS). Web API breadth (needs JS bindings), full CDP, storage remain |
| 6 — Media & richer rendering | 🟡 PNG + GIF (oxideav) + **uncompressed BMP** image decode. JPEG/WebP/AVIF (blocked on oxideav codecs shipping), `<video>`/`<audio>`, animations, GPU compositor remain |
| 7 — Hardening / perf / conformance | 🟡 started — parser + **full layout-pipeline** robustness tests (random inputs) + cargo-fuzz harness (html/css/**layout**), accessibility tree. WPT, perf, sandbox hardening remain |

Honest scope note: **`document`/`window` bindings now work** without any kataan
changes — kataan supports enough JS (ES6 `Proxy` traps, `Object.defineProperty`,
`JSON`, closures) to model the DOM in JS-space and reconcile mutations back into the
real tree (`crates/argus-domscript`). **Discrete event handling works too**:
`addEventListener('click')` handlers fire and state accumulates via *deterministic
event replay* (re-run the script + full interaction history each event). What still
needs a real **embedding API** ([upstream/kataan.md](upstream/kataan.md)) is the
genuinely *asynchronous* surface — timers (`setTimeout`), microtasks/promises,
continuous input events, and reading back computed geometry — plus performance (replay
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
- [ ] HTML parser passes the bulk of html5lib tokenizer + tree-construction tests.
- [ ] CSS parser + selector matching + cascade for the box-model/text/colors property subset; `getComputedStyle` for that subset.
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
- [ ] WHATWG event loop: tasks, microtask checkpoint (Promises), `setTimeout`/`setInterval`, `requestAnimationFrame`, and the "update the rendering" step ordering.
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
- [ ] Images decode (`argus-image`) and render at correct intrinsic/object-fit sizing; gradients, multiple backgrounds, border-radius, box-shadow, basic filters.
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
