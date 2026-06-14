# Upstream Requirements: oxideav

**Consumer:** the **media service** behind [`argus-media`](../subsystems/media.md).
**Critical path:** gates **Phase 6** (`<video>`/`<audio>`); some image codecs touch
**Phase 4**.

## Baseline (what exists today)

oxideav is the media demux/decode stack for the audio/video pipeline. Unlike kataan
and rsurl it isn't on docs.rs in this analysis, so the items below are stated as
**requirements** to confirm against its actual API rather than diffs from a known one.

The governing constraint is the sandbox: media bytes are attacker-controlled and
decoded in an **isolated media-service process** ([process model](../PROCESS_MODEL.md)).
oxideav must therefore be a **pure compute** library — *bytes in, frames out* — with
no I/O of its own.

> Legend: **[require]** (must hold for Argus to use it), **[confirm]**, **[add]**.

---

## Tier 0 — non-negotiable for the sandbox

### 1. No ambient I/O — **[require]**
oxideav must **not** open files, sockets, or spawn threads behind Argus's back. It
receives encoded bytes (fetched by the net service, handed over shared memory) and
returns decoded frames/samples. All input is via a caller-provided byte source.

### 2. Push / custom-IO demux — **[require]**
Demux from an Argus-supplied source: either push (feed buffers, `need-more-data`
signalling) or a pull callback the media service backs with its shared-memory ring.
No URL/file opening inside oxideav.

### 3. Robust on malformed/truncated input — **[require]**
Never panic or over-allocate on hostile/partial streams; return errors. Bounded
memory. This is the primary attack surface and a fuzzing target on Argus's side.

---

## Tier 1 — Phase 6 (`<video>`/`<audio>`)

### 4. Demux API — **[confirm/require]**
Open a container from the byte source; enumerate tracks (codec id, timebase,
language/metadata, video dimensions / audio channel layout); read packets with
PTS/DTS and keyframe flags.

### 5. Containers — **[confirm]**
MP4/ISO-BMFF, WebM/Matroska, Ogg (and ideally fragmented MP4 for future MSE).

### 6. Decode API — **[confirm/require]**
Per-track decoder: feed packets, drain decoded outputs, flush on seek/EOS.
- **Video:** decoded frame with pixel format + colorspace/range, dimensions, PTS.
- **Audio:** PCM samples with format, sample rate, channel layout, PTS.

### 7. Codecs — **[confirm]**
Video: H.264/AVC, VP9, AV1 (and VP8 for WebM). Audio: AAC, Opus, Vorbis, FLAC, MP3.
Argus needs an **enumeration API** ("is codec X supported?") to back
`canPlayType`/`MediaCapabilities`.

### 8. Seeking — **[confirm/require]**
Keyframe-accurate seek by timestamp; report keyframe positions so the media element
can implement `currentTime` assignment and range requests (which flow back through the
net service for byte ranges).

### 9. Pixel-format / colorspace info — **[require]**
Enough metadata (YUV subsampling, primaries, transfer, range) for correct conversion
to the compositor's RGBA. Argus can do the YUV→RGB conversion in `argus-gfx` if oxideav
emits raw YUV; either way the colorspace must be reported.

### 10. Timestamp precision for A/V sync — **[require]**
Stable, high-resolution PTS per output so `argus-media`'s media clock can sync audio to
video and emit `timeupdate`/`seeked` correctly.

### 11. Streaming / progressive decode — **[require]**
Decode as data arrives and signal `need-more-data` (buffering, and the basis for MSE
later). No "whole file must be present" assumption.

---

## Tier 2 — images (some of this lands in Phase 4)

### 12. Still-image decode boundary — **[confirm]**
Decide which image formats oxideav decodes vs. `argus-image`. Natural split: oxideav
for codec-shared still formats (**AVIF** = AV1 still, **WebP**), `argus-image` for
**PNG/JPEG/GIF/BMP/ICO**. Confirm oxideav exposes a one-shot still-image decode path
(animated WebP/AVIF too).

### 13. Incremental/progressive image decode — **[add/confirm]**
For large images, decode progressively and downscale-on-decode to a target size (Argus
passes the layout's target box) to bound memory.

---

## Tier 3 — later

### 14. Audio resampling / format conversion — **[confirm]**
Either oxideav resamples to the platform sink's format, or it reports format and Argus
converts in the media service. Decide ownership.

### 15. Hardware-accelerated decode — **[add]**
A future hook to delegate decode to a platform decoder (via `argus-platform`), behind
the same packet-in/frame-out API. Not needed for v1.

### 16. WebCodecs alignment — **[confirm]**
Shape the packet/frame types so they map cleanly onto WebCodecs
(`EncodedVideoChunk`/`VideoFrame`/`AudioData`) if/when Argus exposes that API.

### 17. EME / encrypted media — **[add]**
Out of scope for v1; note whether the demux can at least surface encryption metadata
(common-encryption boxes) so a future EME path is possible.

---

## Boundary reminder

oxideav is the **codec engine** (demux + decode). Argus (`argus-media`) owns the
**HTML media-element semantics**: ready/network states, buffering policy, the playback
clock, the resource-selection algorithm, presentation as a compositor layer, and audio
routing to the platform sink. Keep oxideav a pure transform.

## Verification checklist (before Phase 6 design)

- [ ] Confirm Tier 0 (#1–#3) — these decide whether oxideav can live in the sandboxed
      media service at all.
- [ ] Pin the demux/decode API surface (#4, #6) and the supported-codec enumeration (#7).
- [ ] Agree the image-decode boundary with `argus-image` (#12) early, since AVIF/WebP
      may be wanted as soon as Phase 4 backgrounds/`<img>`.
