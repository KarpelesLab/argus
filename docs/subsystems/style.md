# Subsystem: CSS Parser & Style Engine

**Crates:** `argus-css` (parse + selectors + cascade), `argus-style` (engine)
**Layer:** 2 (engine core)
**Depends on:** `argus-dom`, `argus-geometry`, `argus-util`
**Consumed by:** `argus-layout`, `argus-paint`, `argus-script` (CSSOM)

## Purpose

Parse CSS, match selectors against the DOM, run the cascade, and produce
**computed styles** for every element — then keep those styles correct under DOM
and stylesheet mutation without recomputing the world.

## Responsibilities

- **Tokenizing & parsing** — CSS Syntax Level 3: stylesheets, rules, declarations,
  `@media`/`@supports`/`@font-face`/`@keyframes`/`@layer`/`@import`, the value
  grammar for the properties we support.
- **Selector engine** — parse and match complex selectors, combinators,
  pseudo-classes/elements, `:has()`, namespaces; fast rightmost-match with
  ancestor filters (Bloom filter) for performance.
- **The cascade** — origin/importance, specificity, cascade layers, `@scope`,
  inline styles, `!important`, custom properties (variables) with substitution.
- **Computed values** — inheritance, initial values, unit resolution, `calc()`,
  `var()` resolution, relative→absolute lengths, color resolution.
- **CSSOM** — `CSSStyleSheet`/`CSSRule`/`CSSStyleDeclaration`/`getComputedStyle`,
  surfaced to script by `argus-script`.
- **Invalidation** — given a DOM mutation or stylesheet edit, compute the minimal
  set of elements whose computed style may have changed.

## Key data structures

- **Styled tree.** A parallel tree (or a column on the DOM arena) holding each
  element's `ComputedStyle`. `ComputedStyle` is a sharable, reference-counted,
  interned bundle: identical computed styles (very common across siblings/list
  items) share one allocation ("style sharing").
- **Rule database.** Stylesheets compiled into hash-bucketed rule maps keyed by id,
  class, tag, and attribute for fast candidate lookup; media/support conditions
  pre-evaluated per environment.
- **Invalidation sets.** Selectors are indexed so that "class `x` was added to
  element E" maps to the rules and descendants that could be affected, rather than
  restyling E's whole subtree.

## Design decisions

1. **Computed styles are immutable and shared.** Recompute produces a new
   `ComputedStyle`; equality lets siblings share. This shrinks memory and makes
   "did style actually change?" a pointer comparison feeding layout invalidation.
2. **Two-phase styling.** (a) selector matching collects declarations per element;
   (b) the cascade + computed-value resolution produces the final style. Phase (a)
   is the parallelizable, expensive part.
3. **Parallel-ready, sequential-first.** The styled tree is built to allow
   subtree-parallel restyle (independent subtrees on a worker pool), but v1 runs
   sequentially until correct (parallelize in Phase 4).
4. **Custom properties are first-class.** Variables, `@property` registrations, and
   substitution are handled in the value pipeline, not bolted on — they interact
   with inheritance and invalidation in ways that must be designed in.

## Boundaries

- Produces styles; performs **no** layout or geometry beyond resolving lengths to
  absolute units. Box generation belongs to `argus-layout`.
- Reads the flat tree from `argus-dom`; does not mutate the DOM.
- Animations/transitions: the style engine computes keyframe-interpolated values
  per frame, but the timeline/event-loop driving is shared with `argus-script`'s
  "update the rendering" step.

## Spec references

CSS Syntax L3, Selectors L4, CSS Cascade L5 (+ Layers, Scope), CSSOM, CSS Values &
Units, Custom Properties. Conformance via the CSS portions of WPT.

## Open questions

- Storage of `ComputedStyle`: side-table on the DOM arena vs. separate styled tree
  (coordinate with [`dom.md`](dom.md)).
- Which property set ships in v1 (block/inline/text/box-model first; fl/grid Phase 4).

## Roadmap mapping

Phase 1 (parse + selectors + cascade for the box-model/text subset), Phase 4 (full
selector set, layers, custom properties, animations), ongoing conformance.
