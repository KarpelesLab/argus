//! Image decoding (Layer 1).
//!
//! Decodes image bytes into RGBA8 for the renderer, using the first-party oxideav
//! codecs: PNG (`oxideav-png`), GIF (`oxideav-gif`), **JPEG** (`oxideav-mjpeg` via
//! the `oxideav-core` registry, YUV→RGBA through `oxideav-pixfmt`), **WebP**
//! (`oxideav-webp`, lossless), **QOI** (`oxideav-qoi`), and **ICO/CUR favicons**
//! (`oxideav-ico`, largest sub-image) — plus uncompressed 24/32-bit BMP, **TGA**
//! (Truevision true-color + grayscale, uncompressed + RLE), **Netpbm** (PPM/PGM,
//! ASCII + binary), **PCX** (RLE 24-bit + 8-bit palette), all built in, and
//! `data:` URLs. AVIF, TIFF, and lossy-WebP
//! (VP8) decode here once that glue lands. See `docs/subsystems/media.md`.

/// A decoded image: `width * height * 4` straight-alpha RGBA bytes.
#[derive(Clone, Debug)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Decode image bytes, sniffing the format by signature. Returns `None` if the
/// format is unsupported or the data is malformed.
pub fn decode(bytes: &[u8]) -> Option<DecodedImage> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        let bmp = oxideav_png::decode_png_to_rgba(bytes).ok()?;
        return Some(DecodedImage {
            width: bmp.width,
            height: bmp.height,
            rgba: bmp.data,
        });
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        // Decode and compose the first frame to RGBA (lenient on minor spec quirks).
        let img = oxideav_gif::decode(bytes)
            .or_else(|_| oxideav_gif::decode_lenient(bytes))
            .ok()?;
        let frame = oxideav_gif::compose(&img).ok()?.into_iter().next()?;
        return Some(DecodedImage {
            width: frame.canvas.width as u32,
            height: frame.canvas.height as u32,
            rgba: frame.canvas.pixels,
        });
    }
    if bytes.starts_with(b"BM") {
        return decode_bmp(bytes);
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return decode_registry(bytes, "mjpeg", oxideav_mjpeg::register_codecs);
    }
    if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return decode_webp(bytes);
    }
    if bytes.starts_with(b"qoif") {
        return decode_qoi(bytes);
    }
    // ICO: reserved=0, type=1 (CUR is type 2, which we also accept — same layout).
    if bytes.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        return decode_ico(bytes);
    }
    // Netpbm: `P2`/`P3`/`P5`/`P6` magic.
    if bytes.len() > 1 && bytes[0] == b'P' && matches!(bytes[1], b'2' | b'3' | b'5' | b'6') {
        return decode_netpbm(bytes);
    }
    // PCX: manufacturer byte 0x0A, version byte 0..=5.
    if bytes.len() > 1 && bytes[0] == 0x0A && bytes[1] <= 5 {
        return decode_pcx(bytes);
    }
    // TGA has no leading signature; try it last (it validates structurally).
    decode_tga(bytes)
}

/// Decode a QOI image (lossless). `oxideav-qoi` returns tightly-packed RGB or
/// RGBA pixels with the true dimensions in the header.
fn decode_qoi(bytes: &[u8]) -> Option<DecodedImage> {
    use oxideav_qoi::QoiChannels;
    let img = oxideav_qoi::parse_qoi(bytes).ok()?;
    let (w, h) = (img.width, img.height);
    let rgba = match img.channels {
        QoiChannels::Rgba => img.pixels,
        QoiChannels::Rgb => rgb24_to_rgba(&img.pixels, w as usize, h as usize),
    };
    Some(DecodedImage {
        width: w,
        height: h,
        rgba,
    })
}

/// Decode an ICO/CUR (favicons), returning the largest sub-image. `oxideav-ico`
/// yields each entry as top-down, tightly-packed RGBA already.
fn decode_ico(bytes: &[u8]) -> Option<DecodedImage> {
    let (_ty, images) = oxideav_ico::read_ico(bytes).ok()?;
    // Pick the largest entry (best quality for rendering at any size).
    let best = images
        .into_iter()
        .max_by_key(|i| i.width as u64 * i.height as u64)?;
    if best.pixels.len() < (best.width as usize * best.height as usize * 4) {
        return None;
    }
    Some(DecodedImage {
        width: best.width,
        height: best.height,
        rgba: best.pixels,
    })
}

/// Decode an image through the oxideav codec registry: register the one codec,
/// feed the whole file as a single packet, and convert the decoded frame to RGBA.
/// This is the uniform path for the still-image codecs (JPEG/QOI/ICO/TIFF/TGA).
fn decode_registry(
    bytes: &[u8],
    codec: &str,
    register_codecs: fn(&mut oxideav_core::CodecRegistry),
) -> Option<DecodedImage> {
    use oxideav_core::{CodecId, CodecParameters, Packet, RuntimeContext, TimeBase};

    let mut ctx = RuntimeContext::new();
    register_codecs(&mut ctx.codecs);
    let params = CodecParameters::video(CodecId::new(codec));
    let mut dec = ctx.codecs.first_decoder(&params).ok()?;
    dec.send_packet(&Packet::new(0, TimeBase::SECONDS, bytes.to_vec()))
        .ok()?;
    let frame = dec.receive_arena_frame().ok()?;
    frame_to_rgba(&frame)
}

/// Decode a WebP (lossless; lossy VP8 is reported unsupported by the codec).
fn decode_webp(bytes: &[u8]) -> Option<DecodedImage> {
    let img = oxideav_webp::decode_webp(bytes).ok()?;
    let frame = img.frames.into_iter().next()?;
    Some(DecodedImage {
        width: frame.width,
        height: frame.height,
        rgba: frame.rgba,
    })
}

/// Convert a decoded video frame (the common still-image pixel formats) to RGBA8.
fn frame_to_rgba(frame: &oxideav_core::arena::sync::Frame) -> Option<DecodedImage> {
    use oxideav_core::format::PixelFormat;
    use oxideav_pixfmt::yuv::{yuv420_to_rgb24, yuv422_to_rgb24, yuv444_to_rgb24, YuvMatrix};

    let hdr = frame.header();
    let (w, h) = (hdr.width as usize, hdr.height as usize);
    if w == 0 || h == 0 || w * h > 64_000_000 {
        return None;
    }
    let plane = |i: usize| frame.plane(i);
    // JPEG YCbCr is full-range BT.601.
    let mat = YuvMatrix::BT601.with_range(false);

    let rgba = match hdr.pixel_format {
        PixelFormat::Rgba => plane(0)?.to_vec(),
        PixelFormat::Rgb24 => rgb24_to_rgba(plane(0)?, w, h),
        PixelFormat::Gray8 => {
            let g = plane(0)?;
            let mut out = vec![0u8; w * h * 4];
            oxideav_pixfmt::gray::gray8_to_rgba(g, &mut out, w * h);
            out
        }
        fmt @ (PixelFormat::Yuv420P | PixelFormat::Yuv422P | PixelFormat::Yuv444P) => {
            let (y, u, v) = (plane(0)?, plane(1)?, plane(2)?);
            let mut rgb = vec![0u8; w * h * 3];
            match fmt {
                PixelFormat::Yuv420P => yuv420_to_rgb24(y, u, v, &mut rgb, w, h, mat),
                PixelFormat::Yuv422P => yuv422_to_rgb24(y, u, v, &mut rgb, w, h, mat),
                _ => yuv444_to_rgb24(y, u, v, &mut rgb, w, h, mat),
            }
            rgb24_to_rgba(&rgb, w, h)
        }
        _ => return None, // uncommon formats (Cmyk, 10/12-bit, …) not handled yet
    };
    Some(DecodedImage {
        width: w as u32,
        height: h as u32,
        rgba,
    })
}

/// Expand packed RGB24 to opaque RGBA8.
fn rgb24_to_rgba(rgb: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h * 4);
    for px in rgb.chunks_exact(3).take(w * h) {
        out.extend_from_slice(&[px[0], px[1], px[2], 255]);
    }
    out
}

/// Decode an uncompressed 24- or 32-bit Windows BMP (BITMAPINFOHEADER) to RGBA.
/// Rows are bottom-up for positive height, top-down for negative; pixels are
/// BGR(A) and each row is padded to a 4-byte boundary. Returns `None` for
/// compressed, paletted, or malformed files.
fn decode_bmp(bytes: &[u8]) -> Option<DecodedImage> {
    let rd_u16 = |o: usize| -> Option<u16> {
        Some(u16::from_le_bytes([*bytes.get(o)?, *bytes.get(o + 1)?]))
    };
    let rd_u32 = |o: usize| -> Option<u32> {
        Some(u32::from_le_bytes([
            *bytes.get(o)?,
            *bytes.get(o + 1)?,
            *bytes.get(o + 2)?,
            *bytes.get(o + 3)?,
        ]))
    };
    let rd_i32 = |o: usize| -> Option<i32> { rd_u32(o).map(|v| v as i32) };

    let data_offset = rd_u32(10)? as usize;
    let dib_size = rd_u32(14)?;
    if dib_size < 40 {
        return None; // only BITMAPINFOHEADER and later
    }
    let width = rd_i32(18)?;
    let height_raw = rd_i32(22)?;
    let bpp = rd_u16(28)?;
    let compression = rd_u32(30)?;
    if compression != 0 || (bpp != 24 && bpp != 32) || width <= 0 || height_raw == 0 {
        return None;
    }
    // Guard against absurd dimensions before allocating (malicious headers).
    if width as i64 * height_raw.unsigned_abs() as i64 > 64_000_000 {
        return None;
    }
    let top_down = height_raw < 0;
    let width = width as usize;
    let height = height_raw.unsigned_abs() as usize;
    let bytes_pp = (bpp / 8) as usize;
    let row_size = (width * bytes_pp).div_ceil(4) * 4; // padded to 4 bytes

    let mut rgba = vec![0u8; width * height * 4];
    for row in 0..height {
        // Source row: bottom-up files store the last image row first.
        let src_row = if top_down { row } else { height - 1 - row };
        let row_start = data_offset + src_row * row_size;
        for col in 0..width {
            let p = row_start + col * bytes_pp;
            let b = *bytes.get(p)?;
            let g = *bytes.get(p + 1)?;
            let r = *bytes.get(p + 2)?;
            // 32bpp in a BITMAPINFOHEADER is BGRX (the 4th byte is padding, not
            // alpha — that needs V4/V5 headers), so treat every pixel as opaque.
            let dst = (row * width + col) * 4;
            rgba[dst] = r;
            rgba[dst + 1] = g;
            rgba[dst + 2] = b;
            rgba[dst + 3] = 0xFF;
        }
    }
    Some(DecodedImage {
        width: width as u32,
        height: height as u32,
        rgba,
    })
}

/// Decode a TGA (Truevision Targa) image: uncompressed (type 2) or RLE (type 10)
/// true-color (24-bit BGR / 32-bit BGRA), or uncompressed (type 3) / RLE (type 11)
/// 8-bit grayscale. Color-mapped variants are not handled. TGA has no leading
/// magic, so this is tried last and validates the header structurally (bounded
/// reads fail closed on malformed data).
fn decode_tga(bytes: &[u8]) -> Option<DecodedImage> {
    if bytes.len() < 18 {
        return None;
    }
    let id_len = bytes[0] as usize;
    let cmap_type = bytes[1];
    let img_type = bytes[2];
    let cmap_len = u16::from_le_bytes([bytes[5], bytes[6]]) as usize;
    let cmap_entry_bits = bytes[7] as usize;
    let width = u16::from_le_bytes([bytes[12], bytes[13]]) as usize;
    let height = u16::from_le_bytes([bytes[14], bytes[15]]) as usize;
    let depth = bytes[16] as usize;
    let descriptor = bytes[17];

    // Uncompressed (2) / RLE (10) true-color at 24/32-bit, or uncompressed (3) /
    // RLE (11) grayscale at 8-bit. (Color-mapped variants aren't handled.)
    let truecolor = (img_type == 2 || img_type == 10) && (depth == 24 || depth == 32);
    let grayscale = (img_type == 3 || img_type == 11) && depth == 8;
    let rle = img_type == 10 || img_type == 11;
    if (!truecolor && !grayscale) || cmap_type > 1 {
        return None;
    }
    if width == 0 || height == 0 || (width as u64) * (height as u64) > 64_000_000 {
        return None;
    }
    let bpp = depth / 8;
    let cmap_bytes = if cmap_type == 1 {
        cmap_len * cmap_entry_bits.div_ceil(8)
    } else {
        0
    };
    let mut p = 18 + id_len + cmap_bytes;
    let npx = width * height;
    // Pixels in stored order (first stored pixel first); flipped to top-down below.
    let mut px = vec![0u8; npx * 4];
    let read_pixel = |bytes: &[u8], p: &mut usize, out: &mut [u8]| -> Option<()> {
        let (r, g, b, a) = if bpp == 1 {
            // 8-bit grayscale: one luminance byte, opaque.
            let l = *bytes.get(*p)?;
            (l, l, l, 0xFF)
        } else {
            let b = *bytes.get(*p)?;
            let g = *bytes.get(*p + 1)?;
            let r = *bytes.get(*p + 2)?;
            let a = if bpp == 4 { *bytes.get(*p + 3)? } else { 0xFF };
            (r, g, b, a)
        };
        out[0] = r;
        out[1] = g;
        out[2] = b;
        out[3] = a;
        *p += bpp;
        Some(())
    };

    let mut idx = 0;
    if !rle {
        while idx < npx {
            read_pixel(bytes, &mut p, &mut px[idx * 4..idx * 4 + 4])?;
            idx += 1;
        }
    } else {
        // RLE: each packet is a 1-byte header; high bit set → run of `count` copies
        // of one pixel, else `count` raw pixels. `count` = low 7 bits + 1.
        while idx < npx {
            let header = *bytes.get(p)?;
            p += 1;
            let count = (header & 0x7F) as usize + 1;
            if header & 0x80 != 0 {
                let mut pix = [0u8; 4];
                read_pixel(bytes, &mut p, &mut pix)?;
                for _ in 0..count {
                    if idx >= npx {
                        break;
                    }
                    px[idx * 4..idx * 4 + 4].copy_from_slice(&pix);
                    idx += 1;
                }
            } else {
                for _ in 0..count {
                    if idx >= npx {
                        break;
                    }
                    read_pixel(bytes, &mut p, &mut px[idx * 4..idx * 4 + 4])?;
                    idx += 1;
                }
            }
        }
    }

    // Rows are stored bottom-to-top unless descriptor bit 5 (top-down) is set.
    let top_down = descriptor & 0x20 != 0;
    let rgba = if top_down {
        px
    } else {
        let mut flipped = vec![0u8; npx * 4];
        let stride = width * 4;
        for row in 0..height {
            let src = (height - 1 - row) * stride;
            let dst = row * stride;
            flipped[dst..dst + stride].copy_from_slice(&px[src..src + stride]);
        }
        flipped
    };
    Some(DecodedImage {
        width: width as u32,
        height: height as u32,
        rgba,
    })
}

/// Decode a Netpbm image: `P2`/`P5` (grayscale) and `P3`/`P6` (RGB), in ASCII
/// (`P2`/`P3`) or binary (`P5`/`P6`) form, with 8-bit samples (`maxval ≤ 255`).
/// Samples are scaled to 0–255; the result is opaque RGBA.
fn decode_netpbm(bytes: &[u8]) -> Option<DecodedImage> {
    if bytes.len() < 2 || bytes[0] != b'P' {
        return None;
    }
    let kind = bytes[1];
    if !matches!(kind, b'2' | b'3' | b'5' | b'6') {
        return None;
    }
    // Read one whitespace-separated header token, skipping `#` comments.
    let token = |pos: &mut usize| -> Option<&[u8]> {
        loop {
            while *pos < bytes.len() && bytes[*pos].is_ascii_whitespace() {
                *pos += 1;
            }
            if *pos < bytes.len() && bytes[*pos] == b'#' {
                while *pos < bytes.len() && bytes[*pos] != b'\n' {
                    *pos += 1;
                }
            } else {
                break;
            }
        }
        let start = *pos;
        while *pos < bytes.len() && !bytes[*pos].is_ascii_whitespace() {
            *pos += 1;
        }
        (*pos > start).then(|| &bytes[start..*pos])
    };
    let num = |t: &[u8]| -> Option<usize> { std::str::from_utf8(t).ok()?.parse().ok() };

    let mut pos = 2;
    let width = num(token(&mut pos)?)?;
    let height = num(token(&mut pos)?)?;
    let maxval = num(token(&mut pos)?)?;
    if maxval == 0 || maxval > 255 || width == 0 || height == 0 {
        return None;
    }
    if (width as u64) * (height as u64) > 64_000_000 {
        return None;
    }
    let npx = width * height;
    let channels = if matches!(kind, b'3' | b'6') { 3 } else { 1 };
    let scale = |v: usize| (v * 255 / maxval).min(255) as u8;
    let mut rgba = vec![0u8; npx * 4];

    if matches!(kind, b'5' | b'6') {
        // Binary: exactly one whitespace follows maxval, then raw samples.
        pos += 1;
        if bytes.len() < pos + npx * channels {
            return None;
        }
        for i in 0..npx {
            let p = pos + i * channels;
            let (r, g, b) = if channels == 3 {
                (
                    scale(bytes[p] as usize),
                    scale(bytes[p + 1] as usize),
                    scale(bytes[p + 2] as usize),
                )
            } else {
                let v = scale(bytes[p] as usize);
                (v, v, v)
            };
            rgba[i * 4] = r;
            rgba[i * 4 + 1] = g;
            rgba[i * 4 + 2] = b;
            rgba[i * 4 + 3] = 0xFF;
        }
    } else {
        // ASCII: read width×height×channels decimal samples.
        for i in 0..npx {
            let (r, g, b) = if channels == 3 {
                (
                    scale(num(token(&mut pos)?)?),
                    scale(num(token(&mut pos)?)?),
                    scale(num(token(&mut pos)?)?),
                )
            } else {
                let v = scale(num(token(&mut pos)?)?);
                (v, v, v)
            };
            rgba[i * 4] = r;
            rgba[i * 4 + 1] = g;
            rgba[i * 4 + 2] = b;
            rgba[i * 4 + 3] = 0xFF;
        }
    }
    Some(DecodedImage {
        width: width as u32,
        height: height as u32,
        rgba,
    })
}

/// Decode a PCX (ZSoft PC Paintbrush) image: RLE-encoded, 8 bits-per-plane, with
/// 3 planes (24-bit RGB) or 1 plane + a trailing 256-color palette (indexed).
/// Other bit depths are not handled.
fn decode_pcx(bytes: &[u8]) -> Option<DecodedImage> {
    if bytes.len() < 128 || bytes[0] != 0x0A {
        return None;
    }
    let rd_u16 = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
    let encoding = bytes[2];
    let bpp = bytes[3]; // bits per pixel per plane
    let xmin = rd_u16(4) as i32;
    let ymin = rd_u16(6) as i32;
    let xmax = rd_u16(8) as i32;
    let ymax = rd_u16(10) as i32;
    let planes = bytes[65];
    let bytes_per_line = rd_u16(66) as usize;
    if encoding != 1 || bpp != 8 || !matches!(planes, 1 | 3) {
        return None;
    }
    let width = (xmax - xmin + 1).max(0) as usize;
    let height = (ymax - ymin + 1).max(0) as usize;
    if width == 0 || height == 0 || (width as u64) * (height as u64) > 64_000_000 {
        return None;
    }
    let nplanes = planes as usize;
    if bytes_per_line < width {
        return None;
    }

    // RLE-decode each scanline into `nplanes * bytes_per_line` bytes per row.
    let mut p = 128usize;
    let row_len = bytes_per_line * nplanes;
    let mut scan = vec![0u8; height * row_len];
    let mut i = 0usize;
    let end = height * row_len;
    while i < end {
        let b = *bytes.get(p)?;
        p += 1;
        if b & 0xC0 == 0xC0 {
            let count = (b & 0x3F) as usize;
            let val = *bytes.get(p)?;
            p += 1;
            for _ in 0..count {
                if i >= end {
                    break;
                }
                scan[i] = val;
                i += 1;
            }
        } else {
            scan[i] = b;
            i += 1;
        }
    }

    let mut rgba = vec![0u8; width * height * 4];
    if nplanes == 3 {
        // Planar RGB: each row is [R…][G…][B…].
        for y in 0..height {
            let base = y * row_len;
            for x in 0..width {
                let r = scan[base + x];
                let g = scan[base + bytes_per_line + x];
                let b = scan[base + 2 * bytes_per_line + x];
                let d = (y * width + x) * 4;
                rgba[d] = r;
                rgba[d + 1] = g;
                rgba[d + 2] = b;
                rgba[d + 3] = 0xFF;
            }
        }
    } else {
        // 1 plane + a 256-color palette in the trailing 769 bytes (0x0C marker).
        let pal_start = bytes.len().checked_sub(769)?;
        if bytes[pal_start] != 0x0C {
            return None;
        }
        let pal = &bytes[pal_start + 1..];
        for y in 0..height {
            let base = y * row_len;
            for x in 0..width {
                let idx = scan[base + x] as usize * 3;
                let d = (y * width + x) * 4;
                rgba[d] = *pal.get(idx)?;
                rgba[d + 1] = *pal.get(idx + 1)?;
                rgba[d + 2] = *pal.get(idx + 2)?;
                rgba[d + 3] = 0xFF;
            }
        }
    }
    Some(DecodedImage {
        width: width as u32,
        height: height as u32,
        rgba,
    })
}

/// Decode a `data:` URL (`data:[<mime>][;base64],<payload>`).
pub fn decode_data_url(url: &str) -> Option<DecodedImage> {
    let rest = url.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    let bytes = if meta.contains(";base64") {
        base64_decode(payload.trim())?
    } else {
        // Percent/plain text data URLs aren't images we handle here.
        payload.as_bytes().to_vec()
    };
    decode(&bytes)
}

/// Minimal standard-alphabet base64 decoder (ignores whitespace and padding).
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for &c in s.as_bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = val(c)?;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_24bit_bmp() {
        // A 2x2 24-bit BMP, bottom-up. Stored rows (bottom first):
        //   row0 (image bottom): red, green   row1 (image top): blue, white
        let mut b: Vec<u8> = Vec::new();
        b.extend_from_slice(b"BM");
        b.extend_from_slice(&70u32.to_le_bytes()); // file size
        b.extend_from_slice(&0u32.to_le_bytes()); // reserved
        b.extend_from_slice(&54u32.to_le_bytes()); // pixel data offset
        b.extend_from_slice(&40u32.to_le_bytes()); // DIB header size
        b.extend_from_slice(&2i32.to_le_bytes()); // width
        b.extend_from_slice(&2i32.to_le_bytes()); // height (bottom-up)
        b.extend_from_slice(&1u16.to_le_bytes()); // planes
        b.extend_from_slice(&24u16.to_le_bytes()); // bpp
        b.extend_from_slice(&0u32.to_le_bytes()); // compression (BI_RGB)
        b.extend_from_slice(&0u32.to_le_bytes()); // image size
        b.extend_from_slice(&0u32.to_le_bytes()); // x ppm
        b.extend_from_slice(&0u32.to_le_bytes()); // y ppm
        b.extend_from_slice(&0u32.to_le_bytes()); // colors used
        b.extend_from_slice(&0u32.to_le_bytes()); // colors important
                                                  // Pixel data is BGR, each row padded to 4 bytes.
        b.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]); // bottom: red, green + pad
        b.extend_from_slice(&[255, 0, 0, 255, 255, 255, 0, 0]); // top: blue, white + pad

        let img = decode(&b).expect("decode bmp");
        assert_eq!((img.width, img.height), (2, 2));
        // Top-left is blue, top-right white, bottom-left red, bottom-right green.
        assert_eq!(&img.rgba[0..4], &[0, 0, 255, 255]); // (0,0) blue
        assert_eq!(&img.rgba[4..8], &[255, 255, 255, 255]); // (1,0) white
        assert_eq!(&img.rgba[8..12], &[255, 0, 0, 255]); // (0,1) red
        assert_eq!(&img.rgba[12..16], &[0, 255, 0, 255]); // (1,1) green
    }

    // Build an 18-byte TGA header for a true-color image.
    fn tga_header(img_type: u8, w: u16, h: u16, depth: u8, descriptor: u8) -> Vec<u8> {
        let mut b = vec![0u8; 18];
        b[2] = img_type;
        b[12..14].copy_from_slice(&w.to_le_bytes());
        b[14..16].copy_from_slice(&h.to_le_bytes());
        b[16] = depth;
        b[17] = descriptor;
        b
    }

    #[test]
    fn decodes_uncompressed_tga_with_vertical_flip() {
        // 2x2 uncompressed 24-bit TGA, bottom-up (descriptor 0). Stored bottom row
        // first: row0 (image bottom) = red,green; row1 (image top) = blue,white.
        let mut b = tga_header(2, 2, 2, 24, 0);
        // BGR pixels.
        b.extend_from_slice(&[0, 0, 255, 0, 255, 0]); // bottom: red, green
        b.extend_from_slice(&[255, 0, 0, 255, 255, 255]); // top: blue, white
        let img = decode(&b).expect("decode tga");
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(&img.rgba[0..4], &[0, 0, 255, 255], "(0,0) blue");
        assert_eq!(&img.rgba[4..8], &[255, 255, 255, 255], "(1,0) white");
        assert_eq!(&img.rgba[8..12], &[255, 0, 0, 255], "(0,1) red");
        assert_eq!(&img.rgba[12..16], &[0, 255, 0, 255], "(1,1) green");
    }

    #[test]
    fn decodes_rgb_pcx() {
        // 2x2 RLE 8bpp 3-plane PCX: row0 = red,green; row1 = blue,white.
        let mut b = vec![0u8; 128];
        b[0] = 0x0A; // manufacturer
        b[1] = 5; // version
        b[2] = 1; // RLE
        b[3] = 8; // bpp/plane
        b[8..10].copy_from_slice(&1u16.to_le_bytes()); // xmax
        b[10..12].copy_from_slice(&1u16.to_le_bytes()); // ymax
        b[65] = 3; // planes
        b[66..68].copy_from_slice(&2u16.to_le_bytes()); // bytes/line
        // A byte is a literal if < 0xC0; 0xFF is encoded as a run-of-1 to be safe.
        let lit = |v: &mut Vec<u8>, x: u8| {
            if x & 0xC0 == 0xC0 {
                v.push(0xC1);
            }
            v.push(x);
        };
        // Row 0: R[255,0] G[0,255] B[0,0]; Row 1: R[0,255] G[0,255] B[255,255].
        for plane in [[255u8, 0], [0, 255], [0, 0], [0, 255], [0, 255], [255, 255]] {
            for x in plane {
                lit(&mut b, x);
            }
        }
        let img = decode(&b).expect("decode pcx");
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(&img.rgba[0..4], &[255, 0, 0, 255], "(0,0) red");
        assert_eq!(&img.rgba[4..8], &[0, 255, 0, 255], "(1,0) green");
        assert_eq!(&img.rgba[8..12], &[0, 0, 255, 255], "(0,1) blue");
        assert_eq!(&img.rgba[12..16], &[255, 255, 255, 255], "(1,1) white");
    }

    #[test]
    fn decodes_binary_ppm_p6() {
        // 2x1 binary RGB PPM: red, green.
        let mut b = b"P6\n2 1\n255\n".to_vec();
        b.extend_from_slice(&[255, 0, 0, 0, 255, 0]);
        let img = decode(&b).expect("decode P6");
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(&img.rgba[0..4], &[255, 0, 0, 255], "red");
        assert_eq!(&img.rgba[4..8], &[0, 255, 0, 255], "green");
    }

    #[test]
    fn decodes_ascii_pgm_p2_with_comment_and_scaling() {
        // 2x1 ASCII grayscale with a comment and maxval 100 → samples scaled to 255.
        let b = b"P2\n# a comment\n2 1\n100\n0 100\n".to_vec();
        let img = decode(&b).expect("decode P2");
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(&img.rgba[0..4], &[0, 0, 0, 255], "black");
        assert_eq!(&img.rgba[4..8], &[255, 255, 255, 255], "white (100/100→255)");
    }

    #[test]
    fn decodes_grayscale_tga() {
        // 2x1 uncompressed 8-bit grayscale TGA, top-down. Two luminance bytes 0 and
        // 200 → black then mid-gray, each opaque with R=G=B=L.
        let mut b = tga_header(3, 2, 1, 8, 0x20);
        b.extend_from_slice(&[0, 200]);
        let img = decode(&b).expect("decode gray tga");
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(&img.rgba[0..4], &[0, 0, 0, 255], "black");
        assert_eq!(&img.rgba[4..8], &[200, 200, 200, 255], "mid-gray");
    }

    #[test]
    fn decodes_rle_tga_top_down_32bit() {
        // 4x1 RLE 32-bit TGA, top-down (descriptor 0x20). One run packet of 3 blue
        // pixels, then a raw packet of 1 red pixel.
        let mut b = tga_header(10, 4, 1, 32, 0x20);
        b.push(0x80 | 2); // run packet, count = 3
        b.extend_from_slice(&[255, 0, 0, 255]); // BGRA blue
        b.push(0x00); // raw packet, count = 1
        b.extend_from_slice(&[0, 0, 255, 255]); // BGRA red
        let img = decode(&b).expect("decode rle tga");
        assert_eq!((img.width, img.height), (4, 1));
        for i in 0..3 {
            assert_eq!(&img.rgba[i * 4..i * 4 + 4], &[0, 0, 255, 255], "pixel {i} blue");
        }
        assert_eq!(&img.rgba[12..16], &[255, 0, 0, 255], "pixel 3 red");
    }

    #[test]
    fn base64_roundtrip_known() {
        // "Man" → "TWFu"
        assert_eq!(base64_decode("TWFu").unwrap(), b"Man");
        // "hello" → "aGVsbG8="
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
    }

    #[test]
    fn rejects_non_png() {
        assert!(decode(b"not an image").is_none());
    }

    #[test]
    fn decodes_a_lossless_webp_roundtrip() {
        // Encode a 2x2 RGBA image losslessly, then decode it back exactly.
        let (w, h) = (2u32, 2u32);
        let src: Vec<u8> = vec![
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
            0, 0, 255, 255, // blue
            255, 255, 255, 255, // white
        ];
        let Ok(encoded) = oxideav_webp::encode_webp_lossless(&src, w, h) else {
            eprintln!("no webp lossless encoder; skipping");
            return;
        };
        let img = decode(&encoded).expect("decode webp");
        assert_eq!((img.width, img.height), (2, 2));
        // Lossless → exact pixels.
        assert_eq!(img.rgba, src);
    }

    #[test]
    fn decodes_a_jpeg_roundtrip() {
        // Encode a solid 16x16 mid-gray YUV420P frame to JPEG via the oxideav
        // registry, then decode it back through our sniff+convert path.
        use oxideav_core::format::PixelFormat;
        use oxideav_core::{
            CodecId, CodecParameters, Frame, RuntimeContext, VideoFrame, VideoPlane,
        };
        let (w, h) = (16usize, 16usize);
        let frame = VideoFrame {
            pts: Some(0),
            planes: vec![
                VideoPlane {
                    stride: w,
                    data: vec![128u8; w * h],
                },
                VideoPlane {
                    stride: w / 2,
                    data: vec![128u8; (w / 2) * (h / 2)],
                },
                VideoPlane {
                    stride: w / 2,
                    data: vec![128u8; (w / 2) * (h / 2)],
                },
            ],
        };
        let mut ctx = RuntimeContext::new();
        oxideav_mjpeg::register(&mut ctx);
        let mut params = CodecParameters::video(CodecId::new("mjpeg"));
        params.width = Some(w as u32);
        params.height = Some(h as u32);
        params.pixel_format = Some(PixelFormat::Yuv420P);
        let Ok(mut enc) = ctx.codecs.first_encoder(&params) else {
            eprintln!("no mjpeg encoder available; skipping");
            return;
        };
        enc.send_frame(&Frame::Video(frame)).expect("send_frame");
        let pkt = enc.receive_packet().expect("receive_packet");

        let img = decode(&pkt.data).expect("decode jpeg");
        assert_eq!((img.width, img.height), (16, 16));
        assert_eq!(img.rgba.len(), 16 * 16 * 4);
        // A solid gray survives JPEG nearly exactly; alpha is opaque.
        assert!((img.rgba[0] as i32 - 128).abs() < 16, "r={}", img.rgba[0]);
        assert_eq!(img.rgba[3], 255);
    }

    #[test]
    fn decodes_a_qoi_roundtrip() {
        // QOI is lossless: encode a 2x2 RGBA image, decode it back exactly.
        let src: Vec<u8> = vec![
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
            0, 0, 255, 255, // blue
            10, 20, 30, 255, // dark
        ];
        let encoded = oxideav_qoi::encode_qoi(2, 2, 4, &src);
        assert!(encoded.starts_with(b"qoif"), "qoi magic");
        let img = decode(&encoded).expect("decode qoi");
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(img.rgba, src);
    }

    #[test]
    fn decodes_an_ico_roundtrip() {
        // A favicon: encode a 2x2 RGBA icon, decode the best-fit image back.
        let src: Vec<u8> = vec![
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
            0, 0, 255, 255, // blue
            255, 255, 0, 255, // yellow
        ];
        let icon = oxideav_ico::IconImage::from_rgba(2, 2, src.clone());
        let Ok(encoded) = oxideav_ico::write_ico(
            oxideav_ico::IconType::Ico,
            &[icon],
            oxideav_ico::WriteOptions::default(),
        ) else {
            eprintln!("no ico encoder; skipping");
            return;
        };
        assert!(encoded.starts_with(&[0, 0, 1, 0]), "ico magic");
        let img = decode(&encoded).expect("decode ico");
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(img.rgba.len(), 2 * 2 * 4);
        // Opaque red top-left survives losslessly.
        assert_eq!(&img.rgba[0..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn decodes_a_gif() {
        // Minimal 1x1 red GIF (GIF87a).
        let url = "data:image/gif;base64,R0lGODdhAQABAIAAAP8AAAAA/ywAAAAAAQABAAACAkQBADs=";
        let img = decode_data_url(url).expect("decode gif");
        assert_eq!((img.width, img.height), (1, 1));
        assert_eq!(img.rgba.len(), 4);
        assert_eq!(img.rgba[3], 255); // opaque
    }

    #[test]
    fn decoders_never_panic_on_malformed_input() {
        // Image bytes come from the network — every decoder must fail closed (return
        // None) on truncated/hostile input, never panic. Fuzz each format by prefixing
        // its magic onto pseudo-random bodies, plus the magic alone (truncated header).
        let magics: &[&[u8]] = &[
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A], // PNG
            b"GIF89a",
            b"GIF87a",
            b"BM",
            &[0xFF, 0xD8, 0xFF],         // JPEG
            b"RIFF\0\0\0\0WEBPVP8L",     // WebP
            b"qoif",                     // QOI
            &[0x00, 0x00, 0x01, 0x00],   // ICO
        ];
        let mut seed = 0x9E3779B97F4A7C15u64;
        let mut byte = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed & 0xff) as u8
        };
        for magic in magics {
            // Magic alone (header truncated to nothing).
            let _ = decode(magic);
            for _ in 0..120 {
                let len = byte() as usize * 2;
                let mut buf = magic.to_vec();
                buf.extend((0..len).map(|_| byte()));
                let _ = decode(&buf); // must not panic
            }
        }
        // Raw random bodies (no magic) exercise the signature-less TGA fallback,
        // including plausible-looking TGA headers, which must still fail closed.
        for _ in 0..400 {
            let len = byte() as usize * 4;
            let mut buf: Vec<u8> = (0..len).map(|_| byte()).collect();
            // Occasionally force a true-color TGA image-type byte to hit the decoder.
            let n = buf.len();
            if n > 0 {
                buf[2.min(n - 1)] = if byte() & 1 == 0 { 2 } else { 10 };
                if n > 16 {
                    buf[16] = if byte() & 1 == 0 { 24 } else { 32 };
                }
            }
            let _ = decode(&buf); // must not panic
        }
    }

    #[test]
    fn decodes_a_tiny_png_data_url() {
        // 1x1 red PNG.
        let url = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
        let img = decode_data_url(url).expect("decode 1x1 png");
        assert_eq!((img.width, img.height), (1, 1));
        assert_eq!(img.rgba.len(), 4);
        assert_eq!(img.rgba[3], 255); // opaque
    }
}
