# Argus Architecture

This document describes the overall shape of Argus: the process topology, the
workspace crate layout, the document-rendering pipeline, the concurrency model,
and the public embedding API. Subsystem-level detail lives in
[`subsystems/`](subsystems/README.md); the security boundary has its own document
in [`PROCESS_MODEL.md`](PROCESS_MODEL.md).

## 1. The ten-thousand-foot view

Argus is a **multi-process** browser engine. Untrusted web content is parsed,
styled, laid out, scripted, and painted entirely inside sandboxed **content
processes** that hold no OS capabilities. A trusted **browser process** owns the
window, the UI, and the lifecycle of everything else, and brokers all privileged
operations (network, disk, crypto keys) through dedicated **service processes**.

```
                    ┌─────────────────────────────────────────────┐
                    │              Browser process                 │
                    │  (trusted — owns window, UI, process mgmt)   │
                    │                                              │
                    │  chrome · tabs · omnibox · navigation ctrl   │
                    │  process manager · IPC router · compositor   │
                    └───────┬───────────────┬─────────────┬────────┘
                            │ IPC           │ IPC         │ IPC
            ┌───────────────▼──┐   ┌─────────▼────────┐  ┌▼────────────────┐
            │ Content process  │   │ Content process  │  │ Service processes│
            │  site instance A │   │  site instance B │  │                  │
            │ ───────────────  │   │ ───────────────  │  │  net service     │
            │ HTML/DOM         │   │  (sandboxed)     │  │  storage service │
            │ CSS/style        │   │                  │  │  crypto broker   │
            │ layout           │   │                  │  │  media service   │
            │ script (kataan)  │   │                  │  └──────────────────┘
            │ paint→displaylist│   │                  │     │ rsurl  │ purecrypto
            │ (sandboxed)      │   │                  │     │ disk   │ oxideav
            └──────────────────┘   └──────────────────┘
```

Key invariant: **a content process never calls rsurl, never touches the
filesystem, and never holds a private key.** It emits resource *requests* and
display *lists*; the trusted side does the privileged work and ships back bytes
and pixels. This is why Argus does not use kataan's default `std` host runtime
(whose `fetch`/fs/crypto reach the OS directly) — see
[`subsystems/scripting.md`](subsystems/scripting.md).

## 2. Workspace layout

Argus is a single Cargo workspace of many small internal crates (not published).
Fine-grained crates exist for three reasons: parallel compilation, enforced layer
boundaries (a crate can only reach what it depends on), and clean reuse across the
GUI and headless embeddings.

Crates are grouped into layers. **A crate may only depend on crates in the same or
lower layers.** This is the load-bearing architectural constraint.

### Layer 0 — Foundation (no web concepts)

| Crate | Responsibility |
|-------|----------------|
| `argus-util` | IDs, arenas, small-vec/interning helpers, logging, error scaffolding |
| `argus-geometry` | points, rects, sizes, transforms, edges, CSS units, color spaces |
| `argus-ipc` | message framing, typed channels, shared-memory regions, handle passing |
| `argus-platform` | OS-thin: window/surface creation, input events, font enumeration, process spawn, sandbox syscalls |

### Layer 1 — Graphics & text (no DOM)

| Crate | Responsibility |
|-------|----------------|
| `argus-text` | OpenType/TrueType parsing, shaping, bidi, line breaking, glyph outlines |
| `argus-gfx` | paths, fills/strokes, the CPU rasterizer, image buffers, blending |
| `argus-image` | image format decoders (PNG/JPEG/GIF/WebP/AVIF via the media stack) |

### Layer 2 — Engine core (the web platform)

| Crate | Responsibility |
|-------|----------------|
| `argus-dom` | DOM tree, node types, mutation, Shadow DOM, event targets |
| `argus-html` | HTML tokenizer + tree builder (spec parser) → `argus-dom` |
| `argus-css` | CSS tokenizer, parser, selector matching, cascade, computed values |
| `argus-style` | style engine: style sharing, invalidation, the styled tree |
| `argus-layout` | box generation, block/inline/flex/grid, fragment tree, text run layout |
| `argus-paint` | fragment tree → display list, layerization, hit-test tree |
| `argus-events` | DOM event dispatch, hit testing, input → event routing |

### Layer 3 — Integration (capabilities & scripting)

| Crate | Responsibility |
|-------|----------------|
| `argus-script` | kataan realm management + the DOM/Web-API binding bridge |
| `argus-webapi` | implementations of Web APIs (fetch, URL, timers, console, Web Storage, WebCrypto, Canvas 2D, …) bound into `argus-script` |
| `argus-net` | resource loader, HTTP cache, cookie policy, HSTS, content decoding (client of the net service) |
| `argus-security` | origins, CSP, mixed-content, sandbox flags, permission state, TLS policy |
| `argus-storage` | Web Storage, IndexedDB, cache storage, profile/disk layout (client of the storage service) |
| `argus-media` | oxideav integration: media element pipelines, decode orchestration |

### Layer 4 — Engine facade & processes

| Crate | Responsibility |
|-------|----------------|
| `argus-engine` | the document/pipeline orchestrator: owns a `Document`, drives parse→style→layout→paint, hosts script, exposes the embedder-facing engine API |
| `argus-content` | the content-process binary: hosts `argus-engine` per site instance behind the sandbox, speaks IPC to the browser process |
| `argus-compositor` | composites per-content display lists / layers into window surfaces (CPU now, GPU later) |
| `argus-services` | the service-process binaries: net (over `rsurl`), storage (disk), crypto broker (over `purecrypto`), media (over `oxideav`) |
| `argus-browser` | the browser-process core: process manager, IPC router, navigation controller, session/profile state, UI-agnostic |

### Layer 5 — Embeddings (binaries)

| Crate | Responsibility |
|-------|----------------|
| `argus-shell` | desktop GUI: chrome widgets, tab strip, omnibox, wired to `argus-browser` |
| `argus-headless` | headless runner + CDP-like automation API over `argus-browser` |
| `argus` | the public embedder library facade (for third parties embedding the engine) |

> The crate list will evolve; some Layer-2 crates may merge or split during
> implementation. The **layering rule** and the **trusted/untrusted split** are
> the parts that must not bend.

## 3. The rendering pipeline

Inside a content process, a document moves through a classic pipeline. Each stage
has a dedicated subsystem doc.

```
 bytes ──► HTML tokenizer ──► DOM tree ──────────────┐
 (net)     (argus-html)       (argus-dom)            │
                                                     ▼
 CSS ────► CSS parser ──► stylesheets ──► STYLE ──► styled tree
 (net)     (argus-css)                   (argus-style)
                                                     │
                                                     ▼
                              LAYOUT  ──►  box tree ──► fragment tree
                              (argus-layout)            (geometry + text runs)
                                                     │
                                                     ▼
                              PAINT   ──►  display list  +  hit-test tree
                              (argus-paint)
                                                     │  IPC (shared memory)
                                                     ▼
                              COMPOSITE ──► window surface / off-screen buffer
                              (argus-compositor, browser process)
```

Script (kataan, via `argus-script`) sits beside this pipeline, not inside it: it
mutates the DOM and stylesheets, and those mutations dirty the relevant stages.
The engine recomputes only what changed (style invalidation → relayout of dirty
subtrees → repaint of dirty regions). The event loop that orders script tasks,
microtasks, and rendering ("update the rendering" steps) is described in
[`subsystems/scripting.md`](subsystems/scripting.md).

### Stage boundaries are explicit data structures

The pipeline is a sequence of immutable-ish artifacts (DOM → styled tree →
fragment tree → display list), not a tangle of cross-calls. This is what makes
incremental updates, headless determinism, and the eventual GPU compositor
swap-in tractable: only the *display list* crosses the process boundary, so the
trusted compositor never needs to understand DOM or CSS.

## 4. Concurrency model

Two layers of concurrency:

- **Across processes** — the security boundary (Section 1, and
  [`PROCESS_MODEL.md`](PROCESS_MODEL.md)). Each content process owns one site
  instance's documents.
- **Within a process** — an **actor / message-passing** core. Each major unit
  (a document, the resource loader, the layout worker pool) owns its state and
  communicates by messages. No shared-mutable global state across threads.

The pipeline parallelizes internally where it pays off: style resolution and
layout fan out across a worker pool over independent subtrees; rasterization tiles
across threads. These are data-parallel and bounded — they do not change the
single-writer ownership of the DOM (script and DOM mutation are single-threaded
per document, matching the platform's semantics).

### The synchronous-rsurl question

rsurl is synchronous. That is *fine* in Argus because networking lives in the
**net service process**, where blocking transfers run on a thread pool. Content
processes see networking as async: they send a `LoadResource` message and later
receive `ResponseHead` / `BodyChunk` / `LoadComplete` messages. kataan's event
loop is driven by Argus, so a `fetch()` from script becomes an IPC round-trip
that resolves a promise on a later microtask turn — never a blocking call inside
the sandbox. See [`subsystems/networking.md`](subsystems/networking.md).

## 5. The embedding API

Everything above the `argus-browser` core is an embedding. Both first-party
embeddings (`argus-shell`, `argus-headless`) are clients of the same API, which is
roughly:

```text
Engine / Browser
  ├─ new(profile, settings) -> Browser
  ├─ new_tab() -> TabId
  ├─ tab(id).navigate(url) / reload() / stop() / go_back() / go_forward()
  ├─ tab(id).resize(size) / set_device_pixel_ratio(f)
  ├─ tab(id).evaluate_script(src) -> Promise<Value>      (automation)
  ├─ tab(id).dispatch_input(event)                        (mouse/key/touch/scroll)
  ├─ tab(id).capture_surface() -> FrameBuffer             (headless screenshot)
  ├─ tab(id).dump_dom() / dump_layout()                   (automation/debug)
  └─ events: TitleChanged, LoadStateChanged, FaviconChanged,
             DialogRequested, NavigationRequested, ConsoleMessage, ...
```

- The **GUI shell** pumps OS input events into `dispatch_input`, and presents the
  composited surface in an OS window.
- The **headless runner** drives the same calls programmatically and reads back
  `capture_surface()` / `dump_*`, exposing them over a CDP-like wire protocol so
  existing automation clients can drive Argus.

Because both go through one API, headless is never a second-class or divergent
code path — a regression in either is a regression in the shared engine.

## 6. What lives where (quick reference)

| Concern | Trusted side | Sandboxed side |
|---------|--------------|----------------|
| HTML/CSS parsing, DOM, layout, paint | – | content process |
| JS/WASM execution (kataan) | – | content process |
| Window, input capture, compositing | browser process | – |
| Network transfers (rsurl) | net service | – |
| Disk / storage | storage service | – |
| Private keys, TLS handshake (purecrypto) | crypto broker / net service | – |
| Media demux/decode (oxideav) | media service | – |
| Navigation, session history, profiles | browser process | – |

## 7. Open architectural questions

Tracked here, resolved as phases land:

1. **Compositor location.** CPU compositing can live in the browser process for
   v1. A future GPU compositor may warrant its own process (à la Chromium's GPU
   process). Decide before Phase 6.
2. **Layout threading granularity.** Subtree-parallel vs. fully sequential for v1.
   Start sequential, parallelize once correct (Phase 4).
3. **WASM host.** kataan ships its own `wasm_rt`; confirm whether Argus exposes
   `WebAssembly.*` through kataan's runtime directly or wraps it.
4. **oxideav surface area.** Exact decode/demux API to be pinned down with the
   media subsystem (Phase 6) — may drive feature requests upstream.
5. **Per-crate `no_std` reach.** kataan/purecrypto have `no_std` cores; decide how
   much of Argus's Layer 0–1 stays `no_std`-clean for portability.
