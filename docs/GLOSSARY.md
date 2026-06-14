# Glossary

Shared vocabulary for Argus docs. Web-platform terms follow WHATWG/W3C usage;
Argus-specific terms are marked **(Argus)**.

- **Browser process** **(Argus)** — the trusted process owning the window, UI,
  process management, and navigation. Brokers all privileged operations.
- **Content process** **(Argus)** — a sandboxed, untrusted process running one site
  instance's DOM/CSS/layout/script/paint. Holds no OS capability.
- **Service process** **(Argus)** — a trusted process performing one brokered
  capability: net (rsurl), storage (disk), crypto (purecrypto keys), media (oxideav).
- **Site instance** — a `scheme://eTLD+1` grouping (refined by COOP/COEP) that
  decides which content process a document belongs to. Backs site isolation.
- **Site isolation** — the guarantee that a content-process compromise exposes at
  most one site's data, because that process only ever held one site's data.
- **Sandbox** — the OS-enforced restriction (Seatbelt/seccomp/AppContainer) that
  strips a content/media process of network, filesystem, and spawn capabilities.
- **Brokered capability** **(Argus)** — an operation content can't do directly
  (network, disk, key crypto) and instead requests over IPC from a trusted service.
- **DOM tree** — the live node tree built by the HTML parser; the engine's spine.
- **Flat tree** — the DOM with shadow trees and slots resolved; what style/layout read.
- **Styled tree** **(Argus)** — the DOM annotated with `ComputedStyle` per element.
- **Computed style** — the fully resolved style for an element after cascade,
  inheritance, and value computation; immutable and shared across equal elements.
- **Box tree** — the intermediate structure mapping styled elements to layout boxes.
- **Fragment tree** **(Argus)** — layout's output: absolute geometry of every box
  and text run, ready to paint. The hard boundary between layout and paint.
- **Display list** **(Argus)** — a flat, serializable sequence of drawing commands
  produced by paint; the only artifact that crosses to the compositor.
- **Layer / layerization** — fragments promoted to independent compositor layers
  (scrollers, animated transforms/opacity, video/canvas) so interaction recomposites
  without repainting.
- **Compositor** — the trusted component assembling layer bitmaps into the final
  surface, applying transforms and scroll offsets. CPU now, GPU later.
- **Rasterizer** **(Argus)** — `argus-gfx`'s pure-Rust CPU path/glyph/image renderer
  turning a display list into bitmaps. The deterministic reference output.
- **Realm** — a kataan JS execution context (global object + heap context); one per
  `Window`/worker.
- **Host bindings** **(Argus)** — the DOM/Web-API objects Argus exposes into a kataan
  realm. Argus supplies these instead of kataan's default `std` host runtime.
- **Event loop** — the WHATWG task/microtask/rendering scheduler driving script,
  animations, and "update the rendering."
- **Resource loader** **(Argus)** — `argus-net`'s client side that turns engine/script
  requests into brokered loads via the net service.
- **Embedding** **(Argus)** — a client of the `argus-browser` API; the GUI shell and
  the headless runner are the two first-party embeddings.
- **Headless** — running the full engine off-screen, driven by an automation API
  (CDP-like), rendering to a `FrameBuffer` instead of a window.
- **CDP** — Chrome DevTools Protocol; the de-facto automation wire protocol Argus's
  headless surface emulates.

### First-party crates (the stack)

- **kataan** — pure-Rust JS/WASM engine (the language runtime).
- **rsurl** — pure-Rust curl equivalent (the transfer layer).
- **purecrypto** — pure-Rust crypto toolkit (TLS, X.509, AEAD, PQ, WebCrypto prims).
- **oxideav** — media demux/decode stack (audio/video).
