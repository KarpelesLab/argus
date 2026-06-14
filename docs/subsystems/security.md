# Subsystem: Security Model

**Crate:** `argus-security` (policy), enforced across browser/content/services
**Layer:** 3 (integration), but policy is consulted everywhere
**Depends on:** `argus-util`, `argus-geometry` (none web-heavy), origin/URL types
**Consumed by:** `argus-net`, `argus-script`, `argus-engine`, `argus-storage`, browser process

## Purpose

Define and enforce the web security model: origins and the same-origin policy,
isolation, CSP, mixed content, permissions, and TLS/connection trust. The
*process* boundary that backs this is in [`../PROCESS_MODEL.md`](../PROCESS_MODEL.md);
this subsystem is the **policy brain** those processes consult.

## Threat model (summary)

Argus assumes web content is hostile. The adversaries it defends against:

1. **Malicious page** trying to read another origin's data, escape to the OS, or
   spoof the UI. → same-origin policy, sandbox, no ambient OS authority,
   trustworthy chrome.
2. **Compromised renderer** (a content process whose memory safety is breached). →
   site isolation: it only ever held one site's data and has no OS capability, so
   the loss is bounded; services re-validate every request against the stamped
   origin.
3. **Network attacker** (MITM, downgrade). → TLS via purecrypto, HSTS, mixed-content
   blocking, secure-context gating.
4. **Cross-site tracker.** → partitioned cache/cookies/storage, referrer policy.

## Responsibilities

- **Origins & SOP** — origin computation, same-origin / same-site checks, opaque
  origins (sandboxed iframes, `data:`), the agent-cluster/site-instance keying that
  drives process allocation.
- **Isolation policy** — COOP/COEP/CORP, cross-origin isolation (gating
  `SharedArrayBuffer`/high-res timers), frame ancestry (`X-Frame-Options`,
  `frame-ancestors`), and **which content process** a document belongs in (feeds
  the process manager).
- **CSP** — parse and enforce Content-Security-Policy (script/style/img/connect/
  frame-src, nonces/hashes, `'strict-dynamic'`), report generation.
- **Mixed content** — block/upgrade insecure subresources on secure pages.
- **CORS decisioning** — the policy half of CORS (what `argus-net` then enforces on
  the wire): request mode, credentials, response tainting.
- **Permissions** — the Permissions model and prompts (geolocation, notifications,
  camera/mic, clipboard, storage) and secure-context requirements.
- **TLS/connection trust** — certificate validation policy, the trust store
  (`cacrt`/system roots), error handling and user overrides, connection-security
  state for the UI. Handshake crypto is purecrypto; *policy* is here.
- **Sandbox attributes** — `iframe sandbox` flags, and the OS-sandbox policy
  descriptors handed to `argus-platform` at process launch.

## Key data structures

- **`Origin`** — tuple/opaque origin with same-origin/same-site predicates.
- **`SiteInstance` key** — the grouping that decides content-process placement.
- **`Csp`** — parsed policy with a `check(directive, url, context)` decision API.
- **`PermissionState`** per (origin, feature), persisted via storage.
- **`TrustDecision`** — cert validation outcome + UI signal.

## Design decisions

1. **Policy is centralized, enforcement is distributed.** `argus-security` decides;
   the net service, content process, and storage service enforce. One source of
   truth, many checkpoints.
2. **Origin is assigned by the trusted side.** Content cannot declare its own
   origin; the browser process stamps it on every brokered request so a compromised
   renderer can't impersonate another site.
3. **Secure by default.** HTTPS-first, partitioned storage, mixed content blocked,
   powerful features gated on secure contexts and explicit permission.
4. **Site isolation is the backstop.** Even if all the above leaks within a process,
   the process held only one site's secrets and has no OS reach.

## Boundaries

- Does not perform crypto (purecrypto/broker) or open connections (`argus-net`); it
  *decides* and the capability owners *act*.
- Does not own the sandbox syscalls (`argus-platform`); it supplies the policy.

## Spec references

HTML origin/agent clusters, Fetch (CORS), CSP L3, Mixed Content, Secure Contexts,
Permissions, COOP/COEP, Referrer Policy, RFC 5280 (cert validation), HSTS.

## Open questions

- Trust-store source on macOS/Linux/Windows (system roots vs. bundled `cacrt`).
- Exact COOP/COEP → process-allocation mapping with the process manager.
- Permission-prompt UX surface in the shell vs. headless auto-grant policy.

## Roadmap mapping

Phase 0/3 (origins, site-instance keying, sandbox policy descriptors), Phase 3
(SOP, CORS, mixed content, TLS trust UX, basic CSP), Phase 5+ (full CSP,
COOP/COEP, permissions, isolation features).
