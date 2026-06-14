//! Image decoding (Layer 1).
//!
//! Decodes image bytes into RGBA8 for the renderer, using the first-party oxideav
//! codecs. Phase 1 covers PNG (via `oxideav-png`) and `data:` URLs; JPEG/GIF/WebP/
//! AVIF (the other oxideav codecs) plug into [`decode`] as they're wired up. See
//! `docs/subsystems/media.md`.

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
    // JPEG (FF D8) and WebP (RIFF…WEBP) decode here as those codecs are wired in.
    None
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
