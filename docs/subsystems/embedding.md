# Subsystem: Embedding — GUI Shell, Headless API & Input

**Crates:** `argus-shell` (GUI), `argus-headless` (automation), `argus` (embedder lib),
`argus-events`/`argus-platform` (input)
**Layer:** 5 (embeddings), with input spanning 0/2
**Depends on:** `argus-browser`, `argus-platform`, `argus-compositor`, `argus-events`
**Consumed by:** end users (shell) and automation clients (headless)

## Purpose

Present the engine to the world in two forms from **one** `argus-browser` core: a
windowed desktop browser and a headless automation surface. Also defines how
real input becomes DOM events. "One engine, two faces" (README principle #4).

## The shared embedder API

Both embeddings are thin clients of the `argus-browser` API sketched in
[`../ARCHITECTURE.md`](../ARCHITECTURE.md) §5 (new browser, tabs, navigate,
resize, dispatch input, evaluate script, capture surface, dump DOM/layout, plus an
event stream). Nothing in the engine knows which embedding is driving it — that is
what keeps headless and GUI from diverging.

## GUI shell — `argus-shell`

- **Chrome** — window, tab strip, omnibox (URL + search + suggestions), nav buttons
  (back/forward/reload/stop), progress/security indicator, context menus, find-in-page,
  downloads, settings, dialogs (alert/confirm/prompt/file picker/permission prompts).
- **The chrome is itself rendered by Argus.** Rather than a separate UI toolkit, the
  browser chrome is drawn with `argus-gfx` (and may even be described in HTML/CSS and
  laid out by the engine — a privileged internal document). This dogfoods the engine
  and keeps the pure-Rust ethos; alternative is a minimal in-house widget layer.
- **Window & surface** — via `argus-platform`; the composited page surface from
  `argus-compositor` is presented in the content area, chrome composited around it.
- **Input plumbing** — OS input events → `argus-platform` → routed to the focused
  tab's content process as `DispatchInput`, or consumed by chrome.

## Headless — `argus-headless`

- **Same engine, off-screen.** Renders to a `FrameBuffer` instead of a window;
  everything else (multi-process, sandbox, services) is identical.
- **Automation protocol** — a CDP-like (Chrome DevTools Protocol-style) wire API so
  existing tooling can drive Argus: `Page.navigate`, `Runtime.evaluate`,
  `Page.captureScreenshot`, `DOM.getDocument`, `Network` interception, `Input`
  synthesis, `Emulation` (viewport, device-pixel-ratio, user-agent). Maps directly
  onto the shared embedder API.
- **Use cases** — testing (drives WPT runs), scraping/rendering, PDF/screenshot
  generation, CI for the engine itself.

## Input / event system — `argus-events` + `argus-platform`

- **Capture** — `argus-platform` normalizes OS input (mouse, keyboard with IME,
  wheel/trackpad, touch, pen) into platform-neutral input events.
- **Routing** — the browser process routes input to the focused tab; within the
  content process, `argus-events` hit-tests against the paint hit-test tree to find
  the target node, then dispatches DOM events (capture/target/bubble) with correct
  coordinates, modifiers, and default actions (focus, scroll, activation, form
  interaction).
- **Synthetic input** — headless `Input.*` and automation reuse the exact same
  dispatch path, so automated and human input are indistinguishable to the page.
- **Focus, selection, scrolling, drag-and-drop, clipboard** — coordinated here
  (clipboard access gated by `argus-security`).

## Design decisions

1. **One core, two embeddings, zero divergence.** Headless is the engine rendering
   off-screen, not a reduced build — prevents "works in headless, breaks in GUI."
2. **Chrome dogfoods the engine.** Drawing chrome with Argus's own gfx/engine keeps
   the stack pure and continuously exercises the renderer.
3. **Input is one path.** Real and synthetic input converge before DOM dispatch, so
   automation fidelity is automatic.
4. **Automation speaks a known protocol.** CDP-compatibility means the existing
   ecosystem of drivers works without bespoke clients.

## Boundaries

- Embeddings hold no web-content state; they drive `argus-browser` and present its
  output. All policy/security lives below them.
- Platform specifics (window, audio, fonts, input) are confined to `argus-platform`.

## Spec references

UI Events, Pointer Events, Input Events, Clipboard, Fullscreen, Drag-and-Drop;
Chrome DevTools Protocol (for the automation surface, de-facto standard).

## Open questions

- Chrome rendering approach: engine-rendered HTML/CSS chrome vs. a minimal in-house
  widget set (decide early in Phase 3).
- Which CDP domains/methods to implement first for headless (Page/Runtime/DOM/Input).
- IME and accessibility (a11y tree) timing — a11y is important and large; sequence it.

## Roadmap mapping

Phase 0 (blank window via platform), Phase 1 (present a rendered page), Phase 2
(input → DOM events), Phase 3 (full chrome, tabs, omnibox), Phase 5 (headless CDP
API), a11y/IME hardening later.
