# Subsystem: Storage

**Crate:** `argus-storage` (client), service side owns the disk
**Layer:** 3 (integration); the storage **service** is a process (Layer 4)
**Depends on:** `argus-security` (origin/quota policy), `purecrypto` (profile encryption), `argus-platform` (paths)
**Consumed by:** `argus-webapi` (storage APIs), `argus-net` (cache/cookie persistence)

## Purpose

Persist and serve per-origin web storage and browser profile data, on the
**trusted side** of the sandbox, partitioned and quota-limited. Content never
touches the disk; it asks the storage service.

## Responsibilities

- **Web Storage** — `localStorage`/`sessionStorage` (session in memory per
  top-level browsing context; local persisted), per-origin.
- **IndexedDB** — the structured object store: databases, object stores, indexes,
  transactions, cursors, key ranges, structured-clone values.
- **Cache Storage** — the Cache API backing (for fetch/service workers later).
- **Cookie & HSTS persistence** — durable backing for `argus-net`'s cookie jar and
  HSTS store.
- **HTTP cache backing** — disk bodies + metadata for the network cache (may share
  this service).
- **Profiles** — the on-disk user-data layout: multiple profiles, per-profile
  partition of all the above, plus settings/history/bookmarks (history detail in
  [`navigation.md`](navigation.md)).
- **Quota & eviction** — per-origin quotas, storage pressure eviction (LRU /
  importance), `navigator.storage` estimate/persist.

## Key data structures

- **Origin-partitioned key space** — every record is keyed by (profile, partition,
  origin, store). Partitioning by top-level site prevents cross-site storage
  linkage (privacy).
- **Storage backends** — a simple embedded key-value/record store (in-house, pure
  Rust) under the service; IndexedDB and Web Storage are layered on it.
- **Transaction model** — IndexedDB's transactional semantics mapped onto the
  backend with the required durability/ordering guarantees.

## Design decisions

1. **Brokered, like network.** All persistence is in the storage service; the
   sandbox keeps content off the filesystem. Requests carry the browser-stamped
   origin so partitioning can't be spoofed.
2. **Partitioned by default.** Storage is keyed by top-level site to match the
   cache/cookie partitioning in [`networking.md`](networking.md) — one coherent
   privacy story.
3. **Optional at-rest encryption.** Profile data can be encrypted with purecrypto
   (keys held by the crypto broker), for sensitive profiles / managed deployments.
4. **In-house backend.** Consistent with the pure-Rust ethos; a compact embedded
   record store rather than binding SQLite. Durability via write-ahead logging.

## Boundaries

- Does not implement the JS API shapes (those are in `argus-webapi`); it provides
  the storage operations they call over IPC.
- Does not decide *policy* (quota limits, partition keys come from `argus-security`).

## Spec references

Web Storage, IndexedDB 3.0, Storage (quota/StorageManager), Cache API, structured
clone (HTML).

## Open questions

- Single storage service vs. per-profile; sharing with the HTTP cache backend.
- Embedded store design (durability vs. simplicity) and its WAL format.
- sessionStorage lifetime under process-per-site-instance navigation.

## Roadmap mapping

Phase 3 (cookie/HSTS persistence, profiles), Phase 5 (localStorage/sessionStorage,
IndexedDB, Cache API, quota), encryption + eviction hardening later.
