# Subsystem: Networking

**Crates:** `argus-net` (loader/cache/policy client), service side over `rsurl`
**Layer:** 3 (integration); the net **service** is a process (Layer 4)
**Depends on:** `rsurl`, `purecrypto` (TLS, via rsurl), `argus-security`, `argus-storage`
**Consumed by:** `argus-engine` (resource loads), `argus-script` (fetch/XHR)
**Upstream asks:** [`../upstream/rsurl.md`](../upstream/rsurl.md)

## Purpose

Fetch every resource a page needs — documents, stylesheets, scripts, images,
fonts, media, `fetch()`/XHR — over rsurl, with the cache, cookies, HSTS, and
security policy a browser requires, all on the **trusted side** of the sandbox.

## Topology

Networking runs in the **net service process**. Content processes never open
sockets; they send `RequestResource` messages (origin stamped by the browser
process) and receive streamed responses through shared memory. `argus-net` is the
client-side library that models requests/responses and the cache/policy logic; the
service binary wires it to rsurl.

```
content ──RequestResource──► browser (policy check) ──► net service ──► rsurl ──► origin
        ◄──ResponseHead, BodyChunk*, LoadComplete── (shared memory) ──┘
```

## Responsibilities

- **Resource loading** — scheme dispatch (`http(s)`, `data:`, `blob:`, `file:`
  under policy, `about:`), request construction, redirect handling, streaming
  bodies, content decoding (gzip/deflate/br), MIME sniffing.
- **HTTP cache** — an HTTP-semantics disk+memory cache (freshness, validators,
  `Cache-Control`/`ETag`/`Vary`, revalidation), partitioned by top-level site to
  prevent cross-site cache leaks.
- **Cookies** — rsurl provides `CookieJar`; `argus-net` layers policy: `SameSite`,
  `Secure`/`HttpOnly`, domain/path matching, partitioned (CHIPS) cookies, and the
  jar's persistence via the storage service.
- **HSTS / HTTPS** — HSTS preload + dynamic pin store, upgrade `http`→`https`,
  and the connection-security signal surfaced to the UI.
- **TLS policy** — rsurl+purecrypto perform the handshake; `argus-net`/`argus-security`
  own cert validation policy, the trust store, error/override UX, and OCSP/CRL
  decisions.
- **CORS** — preflight, credentials mode, response tainting (opaque responses),
  enforced for `fetch`/XHR/subresources per `argus-security`.
- **Prioritization & connection reuse** — request priorities, HTTP/2·3
  multiplexing (rsurl's `send_multiplexed`), connection pooling, preconnect.
- **Protocol breadth** — WebSocket (rsurl), and `EventSource`/SSE on top of HTTP.

## Key data structures

- **Resource request/response** — Argus's loader model (carrying origin,
  destination, mode, credentials, priority), distinct from rsurl's `Request`/
  `Response` which it builds underneath.
- **Cache entries** — keyed by (partition, url, vary), with body in the storage
  service; metadata for revalidation.
- **Cookie jar** — rsurl `CookieJar` + Argus policy + storage persistence.
- **HSTS store** — preload table + dynamic entries (persisted).

## Design decisions

1. **All network is brokered.** The sandbox invariant: no content process touches
   a socket. This also centralizes cache/cookies/HSTS so policy can't be bypassed.
2. **Synchronous rsurl, async to the engine.** Blocking transfers run on the net
   service's thread pool; the engine and script see promises/callbacks resolved via
   the event loop. No blocking inside content. (See [`../ARCHITECTURE.md`](../ARCHITECTURE.md) §4.)
3. **Cache & cookies partitioned by site.** Privacy by default — no cross-site
   cache timing or cookie linkage.
4. **rsurl is the transfer engine, Argus owns browser semantics.** rsurl handles
   bytes-on-the-wire (HTTP versions, TLS, proxies); Argus handles cache, CORS,
   cookies policy, mixed content, HSTS — the things that make it a *browser*.

## Boundaries

- Does not parse HTML/CSS/JS; delivers bytes + metadata to `argus-engine`.
- Does not decide *whether* a load is allowed in isolation — it enforces decisions
  from `argus-security` (CSP, mixed content, CORS mode) against the stamped origin.
- Persistence (cache files, cookie DB) is delegated to the storage service.

## Spec references

Fetch standard, HTTP caching (RFC 9111), Cookies (RFC 6265bis), HSTS (RFC 6797),
CORS, Mixed Content, Referrer Policy, WebSocket (RFC 6455), URL standard.

## Open questions

- Cache backend shared with storage service vs. dedicated.
- Feature requests to rsurl (priority hints, fine-grained streaming/abort,
  per-request proxy) — gather during Phase 1/3.
- Network prediction (preconnect/prefetch) scope for v1.

## Roadmap mapping

Phase 1 (http(s) GET via net service → page bytes), Phase 3 (cache, cookies, HSTS,
redirects, CORS, mixed content), Phase 5 (fetch/XHR/WebSocket/SSE breadth).
