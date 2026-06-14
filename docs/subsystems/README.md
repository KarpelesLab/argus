# Argus Subsystems

One design doc per core subsystem. Read [`../ARCHITECTURE.md`](../ARCHITECTURE.md)
first for the workspace crate map and the pipeline these slot into, and
[`../PROCESS_MODEL.md`](../PROCESS_MODEL.md) for the trust boundary they live across.

| Doc | Subsystem | Primary crates | Trust side |
|-----|-----------|----------------|------------|
| [dom.md](dom.md) | HTML parser, DOM tree, events | `argus-html`, `argus-dom`, `argus-events` | content (sandboxed) |
| [style.md](style.md) | CSS parser & style engine | `argus-css`, `argus-style` | content (sandboxed) |
| [layout.md](layout.md) | Layout engine, text & fonts | `argus-layout`, `argus-text` | content (sandboxed) |
| [rendering.md](rendering.md) | Paint, raster, compositing | `argus-paint`, `argus-gfx`, `argus-compositor` | content paints, browser composites |
| [scripting.md](scripting.md) | kataan + DOM bindings + event loop | `argus-script`, `argus-webapi` | content (sandboxed) |
| [networking.md](networking.md) | rsurl integration, cache, cookies, HSTS | `argus-net` + net service | trusted service |
| [security.md](security.md) | Origins, SOP, CSP, TLS policy, permissions | `argus-security` | trusted (policy) |
| [storage.md](storage.md) | Web Storage, IndexedDB, profiles | `argus-storage` + storage service | trusted service |
| [media.md](media.md) | oxideav A/V, image decode, canvas | `argus-media`, `argus-image` + media service | trusted service |
| [navigation.md](navigation.md) | Navigation, session history, lifecycle | `argus-browser` (nav controller) | trusted (browser) |
| [embedding.md](embedding.md) | GUI shell, headless API, input | `argus-shell`, `argus-headless`, `argus-events` | trusted (browser) |

## Dependency direction (engine core)

```
                         argus-dom ──────────────┐
                            ▲                     │
   argus-html ──────────────┘                     ▼
                         argus-css ─► argus-style ─► argus-layout ─► argus-paint
                                                        │  (argus-text)   │
                                                        ▼                 ▼
                                                  argus-events       argus-gfx
                                                                          │
   argus-script (kataan) ──reads/mutates──► DOM/CSSOM            argus-compositor
        │
        └─► argus-webapi ─► argus-net / argus-storage / argus-security / argus-media
                                  │            │           │            │
                              net service   storage svc  (policy)   media service
                              (rsurl)       (disk)                  (oxideav)
```

Lower layers never depend on higher ones (see the layering rule in
[`../ARCHITECTURE.md`](../ARCHITECTURE.md) §2). The trusted/untrusted split cuts
horizontally: everything a sandboxed content process needs that touches the OS is
reached only through the brokered services on the right.

## Doc template

Each subsystem doc follows the same shape so they stay comparable: **Purpose →
Responsibilities → Key data structures → Design decisions → Boundaries → Spec
references → Open questions → Roadmap mapping.**
