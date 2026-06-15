//! Image decoding (Layer 1).
//!
//! Decodes image bytes into RGBA8 for the renderer, using the first-party oxideav
//! codecs: PNG (`oxideav-png`), GIF (`oxideav-gif`), **JPEG** (`oxideav-mjpeg` via
//! the `oxideav-core` registry, YUV→RGBA through `oxideav-pixfmt`), **WebP**
//! (`oxideav-webp`, lossless), **QOI** (`oxideav-qoi`), and **ICO/CUR favicons**
//! (`oxideav-ico`, largest sub-image) — plus uncompressed 24/32-bit BMP (built in)
//! and `data:` URLs. AVIF, TIFF, TGA, and lossy-WebP (VP8) decode here once that
//! glue lands. See `docs/subsystems/media.md`.

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
    None
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
    fn decodes_a_tiny_png_data_url() {
        // 1x1 red PNG.
        let url = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
        let img = decode_data_url(url).expect("decode 1x1 png");
        assert_eq!((img.width, img.height), (1, 1));
        assert_eq!(img.rgba.len(), 4);
        assert_eq!(img.rgba[3], 255); // opaque
    }
}
