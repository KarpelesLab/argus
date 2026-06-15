# Subsystem: Scripting — kataan Integration, DOM Bindings & the Event Loop

**Crates:** `argus-script` (kataan host + binding bridge), `argus-webapi` (Web API impls)
**Layer:** 3 (integration)
**Depends on:** `kataan`, `argus-dom`, `argus-css` (CSSOM), `argus-events`, `argus-net`, `argus-security`
**Consumed by:** `argus-engine`
**Upstream asks:** [`../upstream/kataan.md`](../upstream/kataan.md) (gates Phase 2)

## Purpose

Run page JavaScript/WebAssembly in kataan, expose the DOM and Web platform APIs to
it, and drive the HTML event loop that orders tasks, microtasks, and rendering.
This is the bridge that makes pages *dynamic*.

## The kataan relationship — and a critical decision

kataan provides the language: lexer→parser→bytecode→VM, GC heap, **realms**, atoms,
shapes/inline-caches, and a `wasm_rt`. Its optional `std` feature also ships a host
runtime with an event loop, timers, fs, `fetch` (over rsurl) and `crypto` (over
purecrypto).

**Argus does not use kataan's `std` host runtime in the content process.** That
runtime reaches the OS directly, which is exactly what the sandbox forbids
([`../PROCESS_MODEL.md`](../PROCESS_MODEL.md)). Instead Argus drives kataan's core
(realm + VM + GC) and supplies **its own host bindings**:

- `fetch`/network → IPC to the net service via `argus-net` (not kataan's rsurl call).
- `crypto`/WebCrypto → in-process for pure-data ops, IPC to the crypto broker for key ops.
- timers / microtask queue → Argus's event loop (below), so timing integrates with
  rendering and navigation.
- storage, console, etc. → Argus implementations in `argus-webapi`.

So kataan is consumed as an **engine**, with Argus owning the host environment. (We
may still use kataan's `wasm_rt` to back `WebAssembly.*` — open question in
[`../ARCHITECTURE.md`](../ARCHITECTURE.md) §7.)

## Current implementation (what ships today)

The native-binding design below is the **target**; it assumes kataan's embedder API
(native functions / accessor properties). That API isn't published yet — but testing
kataan 0.0.3 showed its JS is rich enough (ES6 `Proxy`, `Object.defineProperty`,
`JSON`, closures, `this`) to bind the DOM **without** it. So today, scripting runs
through a **JS-side `document`/`window` shim + post-execution reconciliation** in
[`argus-domscript`](../../crates/argus-domscript/src/lib.rs):

1. The real DOM's id'd elements are serialized into a seed object.
2. A JS prelude defines `document`/`window` and proxy element handles whose get/set
   traps record mutation ops; it's run with the page scripts in one kataan execution.
3. The page scripts run via `argus_script::run_with_followup`, which drains kataan's
   event loop (promise microtasks + `async`/`await` + native `setTimeout` callbacks),
   then reads the recorded ops array back as a followup expression.
4. The recorded ops (JSON) are parsed in Rust and replayed into the real `Document`
   before layout.

What works today: `getElementById`/`querySelector`/`createElement`/`body`/`write`,
element `textContent`/`innerHTML`/`className`/`setAttribute`/`style.*`/`classList`/
`appendChild`/`insertBefore`/`remove`; **discrete events** (`addEventListener('click')`/
`onclick` via deterministic replay); **timers** (`setTimeout`/`setInterval`, shim-queued
and delay-ordered) and **promises/microtasks/`async`-`await`** (drained by kataan's event
loop before reconciliation); `localStorage`/`sessionStorage`; keyboard text input.
What still wants the embedder API is the **wall-clock** surface — real-time timer
scheduling, continuous input events, live reflow, and geometry read-back. When the
native embedder API lands, this shim is replaced by the direct bindings described below.

## Responsibilities

- **Realm/global setup** — create a kataan realm per `Window`/worker, install the
  global object (`window`, `self`, `globalThis`), wire `Document`, and manage realm
  lifecycle across navigation (and `document.open`).
- **The binding bridge** — expose DOM/CSSOM/Web API objects as kataan host objects:
  property getters/setters, methods, constructors, prototype chains, `instanceof`,
  live collections (`HTMLCollection`/`NodeList`), and reflected attributes. Manage
  the DOM-node ↔ JS-wrapper mapping and its GC interaction (wrappers keep nodes
  alive and vice-versa without leaks).
- **The event loop** — implement WHATWG's event loop: task queues, the microtask
  checkpoint (Promises, `MutationObserver`, `queueMicrotask`), `setTimeout`/
  `setInterval`, `requestAnimationFrame`, the "update the rendering" step (run rAF
  callbacks → run animations → style → layout → paint), and `requestIdleCallback`.
- **Script execution integration** — classic and module scripts, `<script>`
  parser blocking/`async`/`defer`, dynamic `import()`, import maps.
- **Workers** — dedicated/shared/service workers as additional realms (later);
  `postMessage` and structured clone.
- **Error handling** — `onerror`, unhandled rejection, console error surfacing.

## Web API surface (`argus-webapi`)

Implemented incrementally, each bound through `argus-script`:
`console`, timers, `URL`/`URLSearchParams`, `fetch`/`Request`/`Response`/`Headers`,
`XMLHttpRequest`, `FormData`, DOM events, `CustomEvent`, `localStorage`/
`sessionStorage`, `IndexedDB`, `SubtleCrypto` (WebCrypto), `TextEncoder`/`Decoder`,
`Blob`/`File`, `Canvas` 2D context, `History`/`Location`, `Navigator`,
`structuredClone`, `Intl` (kataan ships an `intl` dep). Each maps to the owning
subsystem (storage→`argus-storage`, fetch→`argus-net`, crypto→broker, canvas→`argus-gfx`).

## Design decisions

1. **Argus owns the host; kataan owns the language.** Clean split that preserves
   the sandbox and lets each evolve independently.
2. **Wrappers are lazy and cached.** A JS wrapper is created on first access and
   reused; the node↔wrapper map is integrated with kataan's GC so neither side
   leaks or dangles.
3. **One event loop to rule timing.** Script, animations, rendering, and resource
   completion all sequence through the single event loop, so spec-mandated ordering
   ("update the rendering" after microtasks, rAF before paint) is exact.
4. **Single-threaded per document.** Matches the platform: one realm, one DOM
   writer per document; workers are separate realms communicating by messages.

## Boundaries

- Does not itself perform network/disk/key crypto — it issues requests to the
  brokered services (sandbox invariant).
- Does not lay out or paint — it dirties the DOM/style and the event loop's
  "update the rendering" step invokes layout/paint.

## Spec references

WHATWG HTML (scripting, the event loop, workers), WebIDL (binding semantics),
DOM, Fetch, WebCrypto, the individual API specs. ECMAScript conformance is kataan's
domain (test262); Argus tracks the binding/Web-API portions of WPT.

## Open questions

- WebIDL→binding generation: hand-written vs. a small codegen from IDL.
- Whether `WebAssembly.*` wraps kataan's `wasm_rt` or a dedicated path.
- Worker process/thread placement under the multi-process model.

## Roadmap mapping

Phase 2 (realm, DOM bindings, event loop, core Web APIs, events), Phase 5 (fetch/
XHR/storage/crypto/canvas breadth, modules, headless `evaluate`), workers later.
