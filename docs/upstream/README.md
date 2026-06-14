# Upstream Requirements

Argus is built on first-party libraries that predate it. This folder tracks what
Argus needs **from** those libraries — features to add, APIs to expose or stabilize,
and behaviors to make controllable — as living checklists that the roadmap's
"upstream feedback" track feeds.

Each doc tiers its asks by when Argus needs them and maps them to roadmap phases.
These are integration requirements, not bug reports: many items may already exist
(the published docs are sparse) and just need confirming or stabilizing — those are
marked accordingly.

| Library | Role in Argus | Doc | Critical path |
|---------|---------------|-----|---------------|
| [kataan](kataan.md) | JS/WASM engine → `argus-script` bindings | [kataan.md](kataan.md) | **gates Phase 2** (scripting) |
| [rsurl](rsurl.md) | transfer layer → net service | [rsurl.md](rsurl.md) | gates Phase 1 (loads) / Phase 3 (policy) |
| [oxideav](oxideav.md) | media demux/decode → media service | [oxideav.md](oxideav.md) | gates Phase 6 (media) |

## purecrypto

No structural changes are currently anticipated. purecrypto already covers what
Argus needs: TLS 1.2/1.3 + X.509 (via rsurl, and for the trust-store/validation
policy in [`../subsystems/security.md`](../subsystems/security.md)), and the
primitives behind WebCrypto. The only foreseeable asks are **coverage**, surfaced
as Argus implements `SubtleCrypto` in Phase 5:

- Confirm the WebCrypto algorithm set is reachable through a clean API for the
  **crypto broker** (`../subsystems/security.md`): AES-GCM/CBC/CTR, HMAC, SHA-1/2,
  PBKDF2, HKDF, RSA-OAEP/PSS/PKCS1, ECDSA/ECDH (P-256/384/521), Ed25519/X25519.
- Key import/export in the WebCrypto key formats (raw, PKCS8, SPKI, JWK).
- A handle/opaque-key model so non-extractable keys can live in the broker and
  never enter a content process's address space.

A dedicated `purecrypto.md` will be added if Phase 5 turns up real gaps.

## How to use these docs

- When a phase starts consuming a library, walk its tier(s) for that phase and
  file the concrete gaps upstream.
- When an item ships upstream, check it off here and note the version.
- Promote resolved "open questions" into the relevant subsystem doc as decisions.
