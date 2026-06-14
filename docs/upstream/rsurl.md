# Upstream Requirements: rsurl

**Consumer:** the **net service** behind [`argus-net`](../subsystems/networking.md).
**Critical path:** basic loads gate **Phase 1**; browser-grade control gates **Phase 3**.

## Baseline (what exists today)

From the `0.0.6` docs, rsurl already provides most of the transfer layer Argus needs:
HTTP/1.1·2·3 (ALPN, HPACK/QPACK, multiplexing via `send_multiplexed`), TLS (pluggable,
purecrypto), `CookieJar`, `ProxyConfig`, `Url`, `Timing`, `WebSocket`, redirects with
the jar threaded through, and a broad protocol set (ftp/file/data/etc.). It appears
**synchronous**, which is fine: rsurl runs on the net service's thread pool and Argus
presents async to the engine ([ARCHITECTURE §4](../ARCHITECTURE.md)).

The asks below are about giving the **browser** the *control* points it needs — the
things that turn "fetch bytes" into "fetch bytes under browser policy." Many may
already exist; verify and stabilize.

> Legend: **[add]** / **[expose/stabilize]** / **[confirm]** as in [kataan.md](kataan.md).

---

## Tier 1 — Phase 1 (first loads)

### 1. Incremental streaming bodies — **[expose/stabilize]**
Deliver response body chunks **as they arrive**, with backpressure, so the HTML parser
can stream-parse and the net service can pump chunks into shared memory. Not just
"return full body bytes."

### 2. Cancel / abort in flight — **[add/confirm]**
Cancel an in-progress transfer from another thread (navigation cancel, `fetch`
`AbortController`, `stop` button). Must promptly release the connection.

### 3. Streaming response head before body — **[confirm]**
Surface status + headers as soon as available, separately from the body stream (Argus
needs the head to apply policy and pick a parser before bytes flow).

### 4. Full header control + no hidden injection — **[expose/stabilize]**
Argus sets the exact request headers (UA, Accept, Referer per Referrer Policy,
range, …) and must be able to **prevent rsurl from auto-injecting** headers the browser
didn't sanction. Forbidden-header handling is Argus's job.

---

## Tier 2 — Phase 3 (browser policy)

### 5. Manual redirect control (per-hop) — **[add]**
Browsers must inspect **each** redirect hop (CORS, mixed content, Referrer Policy,
cookie handling, HSTS upgrade, scheme changes) rather than let rsurl auto-follow.
Need a "do not follow" mode that returns each 3xx to Argus, which then issues the next
request itself. (rsurl's jar-through-redirects convenience must be *optional*.)

### 6. TLS validation + handshake introspection — **[add]**
A **certificate-validation hook** so [`argus-security`](../subsystems/security.md) owns
trust decisions (custom/system roots, error→override UX). Expose the negotiated
protocol, cipher, peer **cert chain**, and ALPN result for the security indicator and
for `SecurityDetails` in headless CDP.

### 7. Don't enforce HSTS/upgrades internally — **[confirm]**
HSTS, HTTPS-upgrade, and mixed-content decisions live in `argus-net`/`argus-security`.
rsurl should transfer what it's told and **not** silently upgrade/redirect schemes;
make any such behavior opt-in.

### 8. CookieJar policy surface — **[expose/stabilize]**
`CookieJar` exists; confirm per-cookie attributes are accessible/settable —
`SameSite`, `Secure`, `HttpOnly`, domain/path, expiry, and **partition key** (CHIPS).
Argus owns the policy and the persistence (via the storage service); rsurl provides the
jar mechanics.

### 9. Rich timing — **[confirm]**
Confirm `Timing` granularity covers DNS, connect, TLS, request-sent, TTFB, and
download, for Navigation/Resource Timing APIs.

### 10. Streaming request bodies (upload) — **[add/confirm]**
Stream large/`POST` bodies and `fetch` `ReadableStream` uploads incrementally with
progress, not just an in-memory buffer.

---

## Tier 3 — Phase 5+ (breadth & tuning)

### 11. Request priorities + connection control — **[add]**
Priority hints per request and, on HTTP/2·3, stream priority; preconnect / keep-alive
/ pool-sizing controls; the ability to coalesce or isolate connections per partition.

### 12. Per-request proxy + PAC — **[expose/stabilize]**
`ProxyConfig` exists; confirm **per-request** proxy selection (not just global) and a
path toward PAC/system-proxy resolution. (Docs note "only `http://` proxies in this
milestone" — HTTPS/SOCKS proxies later.)

### 13. WebSocket API shape — **[confirm]**
Confirm the `websocket` module exposes message send/receive, close codes/reasons,
ping/pong, and per-message streaming suitable for the JS `WebSocket` API; subprotocol
negotiation and extension (permessage-deflate) hooks.

### 14. Pluggable DNS resolver — **[add]**
A resolver hook (async, cancellable) so Argus can add caching, split-horizon, and
DNS-over-HTTPS later, and so DNS is cancellable with the request (#2).

### 15. Connection partitioning — **[add]**
Allow the net service to key the connection pool / TLS session cache by top-level site
to match the cache/cookie partitioning privacy model.

---

## Boundary reminder

rsurl is the **transfer engine** (bytes on the wire: HTTP versions, TLS, proxies,
multiplexing). Argus owns **browser semantics** (cache, CORS, cookie *policy*, mixed
content, HSTS, Referrer Policy). The asks above are mostly about exposing control so
that division is clean — not about moving browser logic into rsurl.

## Verification checklist (before Phase 1 / Phase 3)

- [ ] Phase 1: confirm streaming bodies (#1), head-before-body (#3), abort (#2),
      header control (#4).
- [ ] Phase 3: agree manual redirect mode (#5) and the TLS validation hook (#6) — the
      two biggest "browser vs library" control points.
- [ ] Confirm `CookieJar` exposes SameSite + partition keys (#8) before wiring policy.
