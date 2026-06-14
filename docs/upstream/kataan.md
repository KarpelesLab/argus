# Upstream Requirements: kataan

**Consumer:** [`argus-script`](../subsystems/scripting.md) (+ `argus-webapi`)

> **Update (revised after testing kataan 0.0.3):** the original premise below — that
> Argus's DOM bindings are *blocked* until kataan ships a Rust host-function/embedder
> API — turned out to be **false for the synchronous case**. kataan's published JS
> already supports ES6 `Proxy` (get/set traps), `Object.defineProperty` accessors,
> `JSON.stringify`/`parse`, closures, and `this` — enough to model `document`/`window`
> **entirely in JS**. Argus now ships working synchronous DOM bindings via a JS-side
> shim + post-execution reconciliation (`crates/argus-content/src/dom_script.rs`): the
> page's scripts mutate proxy objects that record ops, which Rust replays into the real
> DOM before layout. The Tier-1 embedder API below is therefore **no longer
> Phase-2-blocking**; it remains the *better* long-term path and is **required for the
> interactive surface** (event loop, `setTimeout`, event listeners, live reflow, reading
> back computed geometry), which the JS-shim approach cannot provide.

**Critical path:** the embedding API + GC remain wanted for an **event-driven** DOM and
performance, but no longer gate basic Phase 2 scripting.

## Baseline (what exists today)

From the published `0.0.1` docs, kataan's **language core** is real and reusable:
lexer→parser→AST→bytecode→VM (`nbeval`/`nbexec`/`nbvm`), `shape`/`ic` (hidden classes
+ inline caches), `nanbox` values, `atom` interning, `rope` strings, `bignum`
(BigInt), `json`, `regex`, `env`, `intl` (dep), and a WASM stack (`wasm`, `wasm_rt`,
`wasm_spec`). Bytecode caching exists via `flatbc` (zero-copy reload) and `snapshot`.

What is **not yet exposed** (or is explicitly groundwork) is the **embedding/host
surface** a browser binds against: `realm`/`object`/`heap` show only primitives, the
GC is described as "groundwork … tracing/compaction layers on later," and there are no
public Promise/microtask/module/ArrayBuffer/host-object APIs. Today's host runtime
(`std` feature) bundles event loop + timers + fs + network + crypto together — which
Argus's sandbox cannot use as-is (see [Tier 1 #9](#9-split-the-std-host-runtime)).

> Legend: each item is **[add]** (likely new), **[expose/stabilize]** (may exist
> internally; needs a public, stable API), or **[confirm]** (probably present —
> verify and document).

---

## Tier 1 — blocking for Phase 2 (the binding bridge cannot exist without these)

### 1. Embedder façade — **[expose/stabilize]**
A documented "embed kataan" API distinct from the internal VM modules: create a
context, evaluate source, call functions, define globals, convert values,
throw/catch at the Rust↔JS boundary.

### 2. Native (Rust-backed) functions — **[add]**
Define JS-callable functions whose body is a Rust closure: receives JS args + a
context/`this` handle, returns a JS value or throws. With configurable `name`/`length`.
*Every DOM method depends on this.*

### 3. Rust-backed accessor properties — **[expose/stabilize]**
Native getter/setter properties (not JS closures). `object` already mentions "an
optional side list of accessor (getter/setter) properties"; these must accept native
callbacks. *Every reflected attribute (`element.id`, `.className`, …).*

### 4. Exotic objects (Proxy-like traps) — **[add]**
Overridable `[[Get]]`/`[[Set]]`/`[[HasProperty]]`/`[[OwnPropertyKeys]]`/
`[[DefineOwnProperty]]`. *Indexed access (`nodeList[0]`), named access
(`form.fieldName`), `CSSStyleDeclaration`, `localStorage`/`Storage`, `NamedNodeMap`.*
Non-trivial; likely absent.

### 5. Internal slot / native data pointer on objects — **[add]**
A reserved field associating a JS wrapper with Argus's `NodeId` (or a typed handle).
Without it there is no path from a JS `this` back to the Rust DOM node. *This is the
spine of the node↔wrapper identity map.*

### 6. Constructors + prototype-chain wiring — **[add]**
Define constructors with a `.prototype`, wire prototype chains, make `instanceof`
work, support subclassing. *The whole WebIDL hierarchy:
`EventTarget ← Node ← Element ← HTMLElement ← HTMLDivElement`.*

### 7. GC embedding API — **[add] (the big one)**
The heap is "groundwork." Argus needs:
- **Persistent/rooted handles** held from Rust (the `Document`, the global).
- **Weak handles + finalizer callbacks** — drop the wrapper-map entry when a wrapper
  is collected.
- **Custom trace callbacks** — a host object tells the GC which GC values it
  references (an `Element` wrapper keeps its listeners alive, a listener keeps its
  callback alive). Without this, the DOM↔JS boundary leaks or use-after-frees.
- *Nice-to-have:* help for DOM↔JS reference **cycles** (the classic cross-heap cycle
  problem). The primitives above let Argus implement collection; native cycle
  collection across the boundary would be a bonus.

### 8. Promise + host microtask control — **[add]**
- Create a Promise and obtain resolve/reject capabilities callable from Rust
  (`fetch()` resolves when the IPC load completes).
- A **host job queue Argus drives**: kataan calls a host hook to *enqueue* Promise
  jobs rather than running its own loop; Argus runs the microtask checkpoint at the
  spec-mandated points in its event loop.
- An **unhandled-rejection** tracking hook.

### 9. Split the `std` host runtime — **[add] (feature reorg)**
Today `std` = event loop + timers + fs + network(rsurl) + crypto(purecrypto). Argus's
sandboxed content process wants the **scheduler/promise/microtask primitives and
module-loader hooks** but **not** the OS-reaching fs/network/crypto bindings. Proposed
split:
- `core` (`no_std`/`alloc`): VM + GC + objects + promise/microtask primitives +
  module-loader *hooks*.
- `host-std` (optional): the OS-reaching `fetch`/fs/crypto/timers Argus will **not**
  link in content.

This is what makes "Argus owns the host, kataan owns the language" actually buildable.

### 10. Exception interop — **[expose/stabilize]**
Catch a JS exception as a Rust `Result`; construct and throw native error objects with
correct prototypes (so Argus throws real `DOMException`/`TypeError`); capture stacks.
(`error` module exists; needs JS-value-level throw/catch at the embedding boundary.)

---

## Tier 2 — Phase 5 breadth

### 11. ArrayBuffer / TypedArray / DataView from Rust — **[add]**
Create/wrap these from Rust byte buffers, ideally over **external/shared memory**
(zero-copy across IPC shared-memory regions). *fetch bodies, canvas `ImageData`,
WebCrypto, `Blob`/`File`, WASM memory.* SharedArrayBuffer gated on cross-origin
isolation.

### 12. DOMString/USVString interop — **[expose/stabilize]**
JS strings are UTF-16/WTF-16 (DOMString allows lone surrogates). Create-from/extract-to
without forcing UTF-8 validation where the spec permits surrogates; integrate with
`rope`/`atom` for cheap interning of common DOM strings.

### 13. Module loader hooks — **[add]**
Host-driven resolve + fetch + instantiate; dynamic `import()` returning a promise;
`import.meta`; import maps. (Front-end parsing exists; the host linkage layer is the gap.)

### 14. Multiple realms + cross-realm — **[confirm]**
One `Realm` per Window/worker; passing an object from realm A to realm B (same-origin
iframes). Confirm N-realm support + cross-realm value identity; Argus builds the
membrane/wrapper rules on top.

### 15. Reentrancy (nested native↔JS) — **[confirm]**
Event dispatch calls JS listeners from Rust, which call DOM methods back into Rust,
recursively. Confirm the VM supports arbitrarily nested native↔JS call stacks.

### 16. Interrupt / watchdog / limits — **[add]**
Interrupt a running script (Stop button, runaway-script dialog, automation timeout);
**catchable** OOM and per-context memory/time budgets (content-process caps) instead
of process abort.

### 17. `WebAssembly.*` JS namespace — **[add]**
Expose `WebAssembly.compile`/`instantiate`/`Module`/`Instance`/`Memory`/`Table`/
`Global` over `wasm_rt`, with `Memory` sharing an ArrayBuffer (ties to #11). The
Rust-level runtime exists; the JS-exposed glue likely does not.

---

## Tier 3 — later (DevTools / workers)

### 18. Inspector / debugger hooks — **[add]**
For CDP `Runtime`/`Debugger` ([headless](../subsystems/embedding.md)): inspect
scopes/objects, breakpoints, pause/step, evaluate-on-call-frame, console object
formatting, sampling profiler, heap snapshots (`snapshot` may partly cover heap
profiling).

### 19. Atomics + worker threading — **[add]**
`SharedArrayBuffer`/`Atomics` and the agent model (workers as separate realms/threads).

### 20. Bytecode/env caching — **[confirm]**
Confirm `flatbc`/`snapshot` are public + stable so Argus can cache compiled script in
the HTTP cache and snapshot the binding environment for fast content-process startup.
(Leverage existing, not a change.)

---

## Already provided — leverage as-is

JSON, regex, BigInt (`bignum`), Intl (`intl`), `rope` strings, `atom` interning,
NaN-boxing, shapes + inline caches, `wasm_rt`, `flatbc`/`snapshot`, and ECMAScript
conformance (test262 is kataan's responsibility, not Argus's).

## Sequencing

kataan's **embedding API + GC landing (Tier 1, esp. #7)** is effectively a
prerequisite milestone for Argus **Phase 2**. Argus **Phases 0–1** need *zero* script,
so they proceed in parallel while kataan matures. Track "kataan embedding API v1" as
the dependency that opens Phase 2.

## Verification checklist (do before Phase 2 design freeze)

- [ ] Read the `Realm`/`Object` struct pages (not just module docs) to mark #1–#6
      as add vs expose.
- [ ] Confirm the GC's planned tracing design can accommodate host trace callbacks (#7).
- [ ] Agree the `std` feature split (#9) so a sandbox-safe build target exists.
- [ ] Prototype one full interface (`Node`) end-to-end against the proposed API to
      validate #2–#8 before scaling to the rest of WebIDL.
