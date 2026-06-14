# Process Model & Security Boundary

Argus is multi-process from the first commit. This document defines the process
topology, the trust boundary, the IPC design, and the sandbox. It is the
authoritative reference for "what is allowed to do what." The threat model and
web-facing security policy (origins, CSP, mixed content) live in
[`subsystems/security.md`](subsystems/security.md); this document is about the
*process* boundary that enforces them.

## 1. Processes and trust

| Process | Trust | Holds | Never holds |
|---------|-------|-------|-------------|
| **Browser** | trusted | window, UI, process table, navigation state, session/profile, IPC router | untrusted content in-process |
| **Content** | **untrusted** | DOM, CSS, layout, kataan VM, paint | OS handles, sockets, fd to disk, private keys |
| **Net service** | trusted | rsurl client, sockets, TLS (purecrypto), cookie jar, HTTP cache | DOM, script |
| **Storage service** | trusted | disk fds, profile dir, IndexedDB/Web Storage backends | sockets, script |
| **Crypto broker** | trusted | private keys, key store, purecrypto key material | DOM, script |
| **Media service** | semi-trusted | oxideav demux/decode | network, disk (receives bytes, returns frames) |
| **Compositor** | trusted | window surfaces, display-list rasterization | DOM, script |

The cut is simple: **content processes are the only place untrusted code (web
JS/WASM, attacker-controlled HTML/CSS) runs, and they are the only processes with
no OS authority.** Every capability a web page seems to have — load a URL, read
`localStorage`, decode a video, sign with WebCrypto — is actually a message to a
trusted process that performs the operation under policy and returns the result.

### Process-per-site-instance

One content process hosts one **site instance** (a `scheme://eTLD+1` grouping,
refined by COOP/COEP and `Cross-Origin-Opener-Policy`). Navigations that cross a
site boundary swap to a different content process. This bounds the blast radius of
a content-process compromise to a single site's data and mirrors the model Argus's
security policy assumes. Process reuse, limits, and spare-process pre-warming are
tuning details for the process manager (Phase 3).

## 2. The sandbox

Content, and to a lesser degree media, run inside an OS sandbox established by
`argus-platform` immediately after launch and before any untrusted byte is
touched. The platform crate provides a uniform `enter_sandbox(policy)` over
per-OS mechanisms:

| OS | Primary mechanism |
|----|-------------------|
| macOS (initial dev target) | Seatbelt (`sandbox_init`) profile: no network, no file write, restricted file read, no process spawn |
| Linux | namespaces + seccomp-bpf syscall filter + (optionally) Landlock for fs |
| Windows | restricted token + job object + AppContainer (later) |

The sandbox policy for a content process is, in spirit: **no `connect`, no
`open` for write, minimal `open` for read (only its own resources passed by fd),
no `fork`/`exec`, no raw device access.** It may only talk to the browser process
over the IPC channel handed to it at launch. A compromised renderer therefore
cannot exfiltrate to the network or persist to disk except by convincing a trusted
service to do so under policy.

> macOS is the initial development platform (the repo's host). Linux sandboxing is
> a Phase 3+ deliverable; Windows later. The `argus-platform` abstraction exists so
> the rest of the engine never sees these differences.

## 3. IPC

`argus-ipc` provides the transport and message discipline.

- **Transport.** A duplex byte channel per process pair (UNIX domain socket pair
  / Mach port on macOS, socketpair on Linux, named pipe on Windows), set up by the
  browser process at spawn and inherited by the child. Content processes get
  exactly one channel — to the browser process — and reach services only by having
  the browser route or by being handed a brokered sub-channel.
- **Framing.** Length-prefixed, versioned messages. A small `#[derive]`-based
  schema generates typed `Request`/`Response`/`Event` enums per service; no ad-hoc
  serialization. Messages are validated on receipt — the trusted side treats every
  field from a content process as hostile input.
- **Bulk data.** Large payloads (resource bodies, decoded frames, display lists,
  framebuffers) move through **shared-memory regions** with the small control
  message carrying only a handle + offset + length. This keeps copies out of the
  hot path (a 4K frame is never serialized field-by-field).
- **Capabilities are handles, not ambient.** A content process can act only on
  resources it was given a handle to. There is no "current directory," no global
  socket factory — authority is the set of handles it holds.

### Message taxonomy (illustrative)

```
Browser → Content:   Navigate, ResizeViewport, DispatchInput, ScriptEval,
                     CaptureSurface, FreezeLifecycle, Shutdown
Content → Browser:   FrameReady(displaylist_handle), TitleChanged, LoadState,
                     RequestResource, RequestStorage, RequestCrypto,
                     RequestMediaDecode, NavigationRequest, DialogRequest,
                     ConsoleMessage
Browser ↔ Service:   (routed) Load/Cookie/Cache · StorageRead/Write · Sign/Decrypt
                     · MediaOpen/DecodeFrame
```

All `Request*` from content carry the requesting **origin** stamped by the browser
process (not by content — content cannot forge its origin), so services apply
same-origin and permission policy against a trustworthy identity.

## 4. Brokered capabilities in detail

### Networking
Content emits `RequestResource{ url, method, headers, origin, mode, credentials }`.
The browser process validates it against the security policy (mixed content, CSP
connect-src, CORS mode), then hands it to the **net service**, which performs the
transfer with rsurl, applies the cookie jar and HSTS, consults the HTTP cache, and
streams `ResponseHead` + body chunks back into a shared-memory region the content
process can read. Content never sees a socket. See
[`subsystems/networking.md`](subsystems/networking.md).

### Storage
`localStorage`/`sessionStorage`/IndexedDB/cache-storage calls from script become
`RequestStorage` messages keyed by origin; the **storage service** owns the disk
and enforces per-origin quotas and partitioning. Profiles (on-disk user data) are
optionally encrypted with purecrypto via the crypto broker.

### Crypto
WebCrypto `SubtleCrypto` operations that touch non-extractable keys are performed
in the **crypto broker** over purecrypto, so private key material never enters an
untrusted address space. Pure-data crypto (hashing a buffer the page already has)
may run in-process for speed; key-bearing operations are brokered.

### Media
`<video>`/`<audio>`/image decode of untrusted bytes is risky, so it runs in the
**media service** (oxideav), isolated from both content and the browser process.
Content sends encoded bytes (via shared memory) and receives decoded frames into
another shared-memory surface. See [`subsystems/media.md`](subsystems/media.md).

## 5. Failure & lifecycle

- **Crash isolation.** A content process crash takes down one site instance's tabs,
  shown as a "this page crashed" surface; the browser process and other tabs
  survive. A service crash is restarted by the browser process; in-flight requests
  fail gracefully.
- **Resource limits.** The process manager caps content-process count and memory,
  reaping background/least-recently-used processes under pressure (ties into the
  page lifecycle / freeze states in [`subsystems/navigation.md`](subsystems/navigation.md)).
- **Spawn cost.** A pre-warmed spare content process hides launch latency on
  navigation (Phase 3 optimization).

## 6. Why this shape

- It matches the capability split Argus's libraries already imply: rsurl, disk, and
  purecrypto keys belong on the trusted side; only kataan executes untrusted code,
  and kataan is happy to run with **Argus-supplied host bindings** instead of its
  default OS-reaching `std` runtime.
- It makes the GUI and headless embeddings identical below the browser process —
  the sandbox and IPC are the same whether a human or an automation script is
  driving.
- It front-loads the hardest plumbing (multi-process, IPC, sandbox) per the user's
  explicit choice, so no subsystem is ever retrofitted across a security boundary
  it wasn't designed to cross.
