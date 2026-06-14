# Subsystem: HTML Parser & DOM

**Crates:** `argus-html` (parser), `argus-dom` (tree), `argus-events` (dispatch)
**Layer:** 2 (engine core)
**Depends on:** `argus-util`, `argus-geometry`
**Consumed by:** `argus-style`, `argus-script`, `argus-layout`, `argus-engine`

## Purpose

Turn a byte stream into a live, mutable DOM tree that faithfully implements the
WHATWG HTML and DOM standards, and dispatch events over it. This is the spine of
the engine: every other subsystem reads or mutates the DOM.

## Responsibilities

- **Tokenization** — the HTML tokenizer state machine (WHATWG §13.2.5).
- **Tree construction** — the insertion-mode state machine, including the messy
  real-world bits: implied tags, foster parenting, the active formatting elements
  list, `<template>`, foreign content (SVG/MathML namespaces), fragment parsing
  (`innerHTML`).
- **Encoding** — sniffing + decoding to a Unicode scalar stream before tokenizing.
- **DOM data model** — `Node`/`Element`/`Text`/`Comment`/`Document`/`DocumentType`/
  `DocumentFragment`, attributes, namespaces, the node tree with parent/child/sibling
  links and fast ordering queries.
- **Mutation** — insert/remove/move with correct mutation semantics, observed by
  style/layout invalidation and by `MutationObserver`.
- **Shadow DOM** — shadow roots, slotting, the flat tree used downstream by style
  and layout.
- **Ranges & traversal** — `Range`, `NodeIterator`/`TreeWalker`, selection.
- **Events** — capture/target/bubble dispatch, `Event`/`CustomEvent`, passive
  listeners, default actions (in `argus-events`).

## Key data structures

- **Arena-allocated tree.** Nodes live in a per-document arena keyed by a `NodeId`
  (index handle), not `Rc<RefCell<…>>`. This gives cache-friendly traversal, cheap
  parent/sibling links, O(1) "is A before B" via maintained document order, and a
  clean ownership story for the single-writer document thread.
- **Interned names.** Tag/attribute names and namespaces are interned (`argus-util`)
  for O(1) comparison during parsing, selector matching, and bindings.
- **Flat tree view.** A computed view that resolves shadow trees + slots, consumed
  by style and layout so they never special-case shadow boundaries.
- **The script bridge.** Each `Node` can be associated with a kataan object handle
  (its JS wrapper), created lazily on first script access. See
  [`scripting.md`](scripting.md) — the DOM does not depend on kataan; the binding
  layer reaches *into* the DOM.

## Design decisions

1. **Spec state machines, generated where possible.** The tokenizer and tree
   builder are large but mechanical; we transcribe the spec faithfully and back it
   with the html5lib test suite from day one rather than approximating.
2. **Single-writer per document.** DOM mutation happens on the document's own
   thread. Parallel style/layout read snapshots or run after mutation settles —
   matching platform semantics and avoiding locks on the hot tree.
3. **Streaming parse.** The tokenizer consumes network chunks incrementally so
   parsing overlaps with download; the parser yields to let `<script>` run and to
   service the event loop.
4. **Invalidation at the source.** Mutations emit typed invalidation hints
   (subtree styled-dirty, sibling-relationship changed, attribute affecting
   selectors) consumed by `argus-style`/`argus-layout`, instead of blunt
   "restyle everything."

## Boundaries

- Knows nothing about CSS, layout geometry, or painting.
- Does not call the network; the parser is *fed* bytes by `argus-engine`'s loader.
- `<script>` execution is delegated to `argus-script` via a callback the engine
  installs; the parser blocks/unblocks per the spec's script handling.

## Spec references

WHATWG HTML (parsing §13), WHATWG DOM, DOM Parsing & Serialization, Selection API,
UI Events. Conformance via html5lib-tests and the DOM portions of WPT.

## Open questions

- Exact arena/handle API shared with `argus-style`'s styled tree (avoid duplicate
  node maps).
- `MutationObserver` microtask integration ordering with the event loop.

## Roadmap mapping

Phase 1 (static parse → DOM), Phase 2 (mutation, events, Shadow DOM basics),
ongoing conformance thereafter.
