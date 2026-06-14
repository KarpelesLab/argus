# Subsystem: Navigation, Session & History

**Crate:** part of `argus-browser` (navigation controller), with `argus-engine` hooks
**Layer:** 4 (browser process)
**Depends on:** `argus-security` (origins, site instances), `argus-net`, `argus-storage`
**Consumed by:** the embeddings (`argus-shell`, `argus-headless`), `argus-script` (History/Location)

## Purpose

Own *how a tab moves between documents*: the navigation algorithm, session history
(back/forward), the lifecycle of documents and their content processes, and the
session-restore / persistence model. This is the conductor sitting in the trusted
browser process.

## Responsibilities

- **Navigation algorithm** — turn a navigation request (typed URL, link click, form
  submit, `location` assignment, redirect, `history.pushState`) into: choose the
  target **site instance** → pick/spawn the **content process** → load the document
  → commit or cancel. Handles same-document vs. cross-document, replace vs. push,
  client vs. server redirects, and download vs. render decisions (Content-Disposition,
  unhandled MIME).
- **Session history** — the per-tab back/forward list, joint session history across
  frames, `history.length`/`go`/`back`/`forward`, `pushState`/`replaceState`,
  scroll restoration, and the `popstate`/`hashchange`/Navigation API events.
- **Browsing-context tree** — the tab's top-level browsing context and its nested
  iframes; `window.open`/named targets, `noopener`, and the opener relationship.
- **Document lifecycle** — `DOMContentLoaded`/`load`/`pagehide`/`unload`, the
  back/forward cache (bfcache) for instant back-forward, and **page lifecycle
  freeze/discard** states that let the process manager reclaim memory.
- **Process coordination** — proactive process swap on cross-site navigation,
  process reuse, spare-renderer warm-up; coordinated with
  [`../PROCESS_MODEL.md`](../PROCESS_MODEL.md).
- **Session persistence** — save/restore open tabs + their history on
  restart/crash, via the storage service.

## Key data structures

- **`Navigation`** — an in-flight navigation: request, target site instance, chosen
  process, state machine (pending → response → committing → committed / failed).
- **Session history list** — ordered history entries (url, state object, scroll,
  document identity) per browsing context, with the joint cross-frame structure.
- **Browsing-context tree** — the live frame hierarchy per tab.
- **bfcache** — frozen, restorable document snapshots with eligibility rules.

## Design decisions

1. **Navigation lives in the trusted process.** Untrusted content can *request* a
   navigation but cannot itself decide process placement or forge history — the
   browser process arbitrates, preserving site isolation.
2. **Site-instance-driven process choice.** The security subsystem's site-instance
   key decides whether a navigation stays in-process or swaps, unifying the
   security and performance stories.
3. **Lifecycle states enable reclamation.** Explicit freeze/discard/bfcache states
   give the process manager safe points to reclaim memory while keeping back/forward
   fast.
4. **History is authoritative server-side.** The trusted side holds the canonical
   session history; `History`/Navigation API in script is a constrained view.

## Boundaries

- Does not parse/render — it drives `argus-engine` instances and routes their
  load/commit events.
- Does not perform the transfer (that's `argus-net`) or persist bytes (that's
  storage); it sequences them.

## Spec references

WHATWG HTML navigation & session history, the Navigation API, Page Lifecycle, bfcache
behavior, browsing contexts, `window.open`/opener semantics.

## Open questions

- bfcache eligibility scope for v1 (start without bfcache; add once lifecycle is solid).
- Cross-process iframe (OOPIF) timing — large; likely staged after single-process-per-tab works.
- Session-restore format and crash-recovery granularity.

## Roadmap mapping

Phase 3 (navigation algorithm, back/forward, link/form nav, process swap, basic
session history), Phase 5+ (Navigation API, bfcache, lifecycle, OOPIF, session restore).
