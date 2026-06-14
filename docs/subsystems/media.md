# Subsystem: Media Stack

**Crates:** `argus-media` (orchestration), `argus-image` (image decode), service over `oxideav`
**Layer:** 3 (integration); the media **service** is a process (Layer 4)
**Depends on:** `oxideav`, `argus-gfx` (surfaces), `argus-net` (byte sources), `argus-script` (element APIs)
**Consumed by:** `argus-engine`, `argus-paint`/compositor (frames), `argus-webapi` (media element APIs)

## Purpose

Decode and present audio/video and images for `<img>`, `<picture>`, `<video>`,
`<audio>`, and `<canvas>` image sources — running untrusted-byte decoding in an
isolated **media service process** (oxideav), feeding decoded frames to the
compositor through shared memory.

## Why a separate process

Media demuxers/decoders parse complex, attacker-controlled binary formats — a
classic memory-safety attack surface. Even in Rust, decode runs in its own
sandboxed **media service**: it receives encoded bytes (already fetched by the net
service) and returns decoded frames/samples into shared-memory surfaces. A decoder
crash never takes down a content process or the browser process.

## Responsibilities

- **Image decoding** (`argus-image`) — PNG, JPEG, GIF (animated), WebP, AVIF, BMP,
  ICO; progressive decode, color-profile handling, downscale-on-decode; feeds the
  raster image cache and `<canvas>` `drawImage`.
- **Container demux** (oxideav) — MP4/ISO-BMFF, WebM/Matroska, Ogg; track
  selection, timestamps, seeking.
- **A/V decode** (oxideav) — common codecs (H.264/AVC, VP9, AV1; AAC, Opus, Vorbis,
  FLAC) subject to oxideav's support; produce decoded video frames + audio samples.
- **Media element pipeline** — the `HTMLMediaElement` machinery: ready/network
  states, buffering, playback clock, `currentTime`/seeking, rate, looping, the
  resource selection algorithm; Media Source Extensions later.
- **Audio output** — route decoded samples to the platform audio sink
  (`argus-platform`); A/V sync against the media clock.
- **Video frame presentation** — hand decoded frames to the compositor as a
  dedicated layer (video is layerized; frames recomposite without repainting the
  page).
- **Canvas/bitmap interop** — `createImageBitmap`, `<canvas>` image sources, image
  `decode()`.

## Key data structures

- **Decoded frame surfaces** — shared-memory RGBA/YUV buffers handed to the
  compositor; ring-buffered for video.
- **Demux packet queue** — encoded packets with timestamps between demux and decode.
- **Media clock** — the playback timeline driving A/V sync and `timeupdate` events.
- **Image decode cache** — decoded bitmaps keyed by (url, target size), shared with
  the raster image cache in [`rendering.md`](rendering.md).

## Design decisions

1. **Decode is isolated.** Untrusted media bytes are decoded out-of-process; the
   blast radius of a decoder bug is the media service.
2. **oxideav is the codec engine; Argus owns element semantics.** oxideav does
   demux/decode; `argus-media` implements the HTML media element state machines,
   buffering policy, and presentation.
3. **Video as a compositor layer.** Frames go straight to a compositor layer, so
   playback doesn't churn layout/paint.
4. **Net fetches, media decodes.** The media service does not open connections; it
   receives bytes from the net service (range requests for seeking flow back
   through the loader).

## Boundaries

- No networking or disk in the media service — bytes in, frames out.
- Element DOM/JS API shapes live in `argus-webapi`/`argus-dom`; this subsystem
  provides the decode/playback engine they drive.

## Spec references

HTML media elements, Media Source Extensions, Encrypted Media Extensions (much
later, if ever), WebCodecs (candidate), Image-related specs, Canvas.

## Open questions

- Exact oxideav API surface (demux/decode entry points, supported codecs) — pin
  down with the media-service design; may drive upstream feature requests.
- Hardware-accelerated decode path (later; needs platform hooks).
- MSE/EME scope and timing (likely post-v1).

## Roadmap mapping

Phase 4 (image decode for `<img>`/backgrounds via `argus-image`), Phase 6 (the
oxideav `<video>`/`<audio>` pipeline, audio output, video layers), MSE/EME later.
