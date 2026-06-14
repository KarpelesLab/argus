# Argus

A web browser written in pure Rust.

Argus is the capstone of a self-contained, no-foreign-code Rust stack. Where most
browsers assemble dozens of C/C++ libraries, Argus is built on first-party crates
that already exist and are published to crates.io:

| Crate | Role | What it gives Argus |
|-------|------|---------------------|
| [`kataan`](https://docs.rs/kataan) | JS/WASM engine | ECMAScript + WebAssembly execution, GC heap, realms, event-loop substrate |
| [`rsurl`](https://docs.rs/rsurl) | curl-in-Rust | HTTP/1.1·2·3, WebSocket, cookies, proxy, FTP/etc. transfer layer |
| [`purecrypto`](https://docs.rs/purecrypto) | crypto toolkit | TLS 1.2/1.3, X.509, AEAD, RSA/EC, post-quantum, WebCrypto primitives |
| `oxideav` | media stack | audio/video demux + decode for `<video>`/`<audio>`/`<canvas>` |

Argus ties these together and adds everything a browser needs on top: an HTML
parser and DOM, a CSS parser and style engine, a layout engine, a CPU rasterizer
and compositor, a multi-process security architecture, a JS↔DOM binding bridge,
storage, navigation/session management, and both a windowed GUI shell **and** a
headless automation surface.

> **Status:** **Argus loads, renders, scripts, and navigates real web pages.**
> Phases 0–1 are essentially complete and Phases 2–3 have begun:
>
> - **Networking** — fetches `http(s)` over **rsurl** (TLS via purecrypto) in a
>   trusted net service; the sandboxed content process never touches a socket.
> - **HTML → DOM** — spec-subset tokenizer + tree builder.
> - **CSS** — a real cascade (UA + author `<style>` + inline) with selectors,
>   specificity, the **box model** (margins/borders/padding/width, `box-sizing`,
>   `min/max-width`), `text-align`, `line-height`, `text-transform`, `white-space:
>   pre`, `list-style-type`, `vertical-align` (sub/sup), colors/backgrounds,
>   `border-radius`, `opacity`, and per-run inline styling.
> - **Layout & paint** — block/inline formatting, **flexbox**, **grid** (with
>   `gap`), **tables**, lists (`ul`/`ol` markers), `<br>`, `<hr>`, underline/
>   line-through, line-breaking with real font metrics; shaped anti-aliased glyphs
>   + colored rects via the first-party **oxideav** stack (`oxideav-scribe`/`-raster`).
> - **Images** — `<img>` decoded (PNG via oxideav) with subresource loading.
> - **JavaScript** — page `<script>`s run in **kataan**, with **working
>   synchronous DOM bindings**: `document.getElementById(id)` →
>   `textContent`/`innerHTML`/`className`/`setAttribute`/`style.*` actually mutate
>   the rendered page (a JS-side `document` shim + reconciliation — no kataan
>   host-callback API needed). An event loop/timers/events still want a real
>   embedding API; see [`docs/upstream/kataan.md`](docs/upstream/kataan.md).
> - **Navigation** — clickable links resolve + fetch + re-render.
>
> Try it: `cargo run` (windowed; click links to navigate) ·
> `cargo run -- --url=https://example.com` (load a site) ·
> `cargo run -- --dump-page=/tmp/page.png` (render to an image) ·
> `cargo run -- --dump-text` / `--dump-dom` / `--dump-a11y` (headless text/DOM/a11y) ·
> `cargo run -- --headless` (multi-process verifier). 19 crates, green on macOS CI.
> See [`docs/ROADMAP.md`](docs/ROADMAP.md) for what remains (full JS/DOM,
> floats/positioning, more conformance, media, GPU compositing).

---

## Why "Argus"

Argus Panoptes, the all-seeing giant of myth, had a hundred eyes — a fitting name
for an engine whose whole job is to *see* the web: parse it, lay it out, paint it,
and watch it change.

## Design principles

1. **Pure Rust, OS-thin.** The engine — parsing, style, layout, text shaping,
   rasterization — is written in-house. We take dependencies only at the OS
   boundary that cannot reasonably be reimplemented (window creation, GPU driver
   access, font file enumeration, process/sandbox syscalls). No C/C++ web stack.
2. **Reuse our own stack.** kataan, rsurl, and purecrypto are first-party. Argus
   extends and integrates them rather than reinventing JS, HTTP, or crypto.
3. **Secure by construction.** Multi-process from day one. Content runs in
   sandboxed processes with **no** direct OS access; network, storage, and crypto
   are reached only through brokered, trusted service processes over IPC.
   See [`docs/PROCESS_MODEL.md`](docs/PROCESS_MODEL.md).
4. **One engine, two faces.** A single engine core powers both the GUI browser
   and the headless/automation embedding. Headless is not a stripped build — it
   is the same pipeline rendering to an off-screen surface.
5. **Deterministic where it counts.** The v1 renderer is a pure-Rust CPU
   rasterizer: identical pixels on every platform, ideal for screenshot diffing
   and Web Platform Tests. A GPU compositor lands later behind the same paint API.
6. **Spec-driven.** Subsystems track WHATWG/W3C specs and are validated against
   the Web Platform Tests suite as they mature.

## Build targets

Argus produces two primary embeddings from one engine:

- **`argus-shell`** — the desktop browser: chrome, tabs, address bar, navigation,
  one sandboxed content process per site instance.
- **`argus-headless`** — a windowless runner exposing a CDP-like automation API
  (navigate, evaluate script, screenshot, intercept network, dump DOM/layout).

Both link the same `argus-engine` core and the same service processes.

## Repository map

```
argus/
├── README.md                 ← you are here
├── docs/
│   ├── ARCHITECTURE.md        High-level architecture + workspace crate map
│   ├── PROCESS_MODEL.md       Multi-process, IPC, and sandbox design
│   ├── ROADMAP.md             Phased plan with exit criteria
│   ├── GLOSSARY.md            Shared vocabulary
│   ├── upstream/             What Argus needs from kataan/rsurl/oxideav/purecrypto
│   └── subsystems/            One design doc per core subsystem
│       ├── README.md          Subsystem index + dependency graph
│       ├── dom.md             HTML parser + DOM tree + events
│       ├── style.md           CSS parser + cascade + style engine
│       ├── layout.md          Box/fragment tree + text + fonts
│       ├── rendering.md        Paint, display lists, CPU raster, compositing
│       ├── scripting.md        kataan integration + DOM bindings + event loop
│       ├── networking.md       rsurl integration, cache, cookies, HSTS
│       ├── security.md         Origins, sandbox, CSP, TLS policy, permissions
│       ├── storage.md          Web Storage, IndexedDB, cache storage, profiles
│       ├── media.md            oxideav media pipeline, image decode, canvas
│       ├── navigation.md       Session history, navigation controller
│       └── embedding.md        GUI shell, headless API, input/event system
└── crates/                    (created in Phase 0 — see ROADMAP)
```

## Where to start reading

- New to the project → [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
- Curious about the security boundary → [`docs/PROCESS_MODEL.md`](docs/PROCESS_MODEL.md)
- Want to know what gets built when → [`docs/ROADMAP.md`](docs/ROADMAP.md)

## License

MIT, consistent with the rest of the stack (kataan, rsurl, purecrypto).
