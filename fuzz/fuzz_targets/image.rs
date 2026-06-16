//! Fuzz target: every image decoder must fail closed (return `None`, never panic)
//! on arbitrary bytes, and any image it *does* decode must be internally
//! consistent (the RGBA buffer is exactly `width * height * 4` bytes). This covers
//! the signature-dispatched still formats (PNG/GIF/JPEG/WebP/QOI/ICO/BMP/TGA/
//! Netpbm/PCX/TIFF) and the first-party container paths — AVIF (`ftyp avif`) and
//! video first-frame (`ftyp` MP4/MOV, EBML Matroska/WebM) through the oxideav
//! demux pipeline — all of which run on untrusted network bytes.
#![no_main]

use argus_image::decode;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(img) = decode(data) {
        let expected = (img.width as usize)
            .checked_mul(img.height as usize)
            .and_then(|p| p.checked_mul(4));
        assert_eq!(
            Some(img.rgba.len()),
            expected,
            "a decoded image's RGBA buffer must be width*height*4 bytes"
        );
    }
});
