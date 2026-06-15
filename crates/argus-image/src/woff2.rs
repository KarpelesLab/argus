//! WOFF2 → sfnt decoding (W3C WOFF2). WOFF2 wraps an sfnt (TrueType/OpenType)
//! font: its tables are concatenated, optionally transformed (the `glyf`/`loca`
//! pair uses a compact transform), and the whole block is Brotli-compressed.
//!
//! This reconstructs a plain sfnt byte stream that a TrueType/OpenType parser can
//! read: it parses the header + table directory, Brotli-decompresses the data,
//! reverses the `glyf`/`loca` transform when present (triplet-encoded points), and
//! rebuilds the offset table + directory with correct checksums. A malformed or
//! unsupported input returns `None`; the caller falls back to another `src`.
//!
//! The output is validated by the downstream font parser, so a reconstruction bug
//! degrades to "font rejected → fallback", never to corrupt rendering.

use compcol::vec::decompress_to_vec_capped;

/// The 63 “known” 4-byte table tags indexed by the directory flag's low 6 bits
/// (index 63 means an explicit tag follows). Order is normative (WOFF2 §5.2).
const KNOWN_TAGS: [&[u8; 4]; 63] = [
    b"cmap", b"head", b"hhea", b"hmtx", b"maxp", b"name", b"OS/2", b"post", b"cvt ", b"fpgm",
    b"glyf", b"loca", b"prep", b"CFF ", b"VORG", b"EBDT", b"EBLC", b"gasp", b"hdmx", b"kern",
    b"LTSH", b"PCLT", b"VDMX", b"vhea", b"vmtx", b"BASE", b"GDEF", b"GPOS", b"GSUB", b"EBSC",
    b"JSTF", b"MATH", b"CBDT", b"CBLC", b"COLR", b"CPAL", b"SVG ", b"sbix", b"acnt", b"avar",
    b"bdat", b"bloc", b"bsln", b"cvar", b"fdsc", b"feat", b"fmtx", b"fvar", b"gvar", b"hsty",
    b"just", b"lcar", b"mort", b"morx", b"opbd", b"prop", b"trak", b"Zapf", b"Silf", b"Glat",
    b"Gloc", b"Feat", b"Sill",
];

const TAG_GLYF: u32 = u32::from_be_bytes(*b"glyf");
const TAG_LOCA: u32 = u32::from_be_bytes(*b"loca");

/// A byte-stream cursor reading big-endian integers with bounds checks.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Reader<'a> {
        Reader { data, pos: 0 }
    }
    fn remaining(&self) -> usize {
        self.data.len() - self.pos.min(self.data.len())
    }
    fn u8(&mut self) -> Option<u8> {
        let b = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }
    fn u16(&mut self) -> Option<u16> {
        Some(((self.u8()? as u16) << 8) | self.u8()? as u16)
    }
    fn i16(&mut self) -> Option<i16> {
        Some(self.u16()? as i16)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(((self.u16()? as u32) << 16) | self.u16()? as u32)
    }
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.data.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(s)
    }
    /// UIntBase128 variable-length unsigned integer (WOFF2 §4.4).
    fn base128(&mut self) -> Option<u32> {
        let mut accum: u32 = 0;
        for i in 0..5 {
            let b = self.u8()?;
            if i == 0 && b == 0x80 {
                return None; // leading zero
            }
            if accum & 0xFE00_0000 != 0 {
                return None; // would overflow 32 bits
            }
            accum = (accum << 7) | (b & 0x7F) as u32;
            if b & 0x80 == 0 {
                return Some(accum);
            }
        }
        None // no terminating byte within 5
    }
    /// Read255UShort variable-length unsigned (WOFF2 §4.3).
    fn read255(&mut self) -> Option<u16> {
        match self.u8()? {
            253 => self.u16(),
            255 => Some(self.u8()? as u16 + 253),
            254 => Some(self.u8()? as u16 + 506),
            code => Some(code as u16),
        }
    }
}

/// Append `tables` (tag, data) as a complete sfnt: offset table + directory (tags
/// sorted) + 4-byte-aligned table data with per-table checksums.
fn build_sfnt(flavor: u32, mut tables: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    tables.sort_by_key(|t| t.0);
    let n = tables.len() as u16;
    let entry_selector = if n == 0 { 0 } else { 15 - n.leading_zeros() } as u16;
    let search_range = (1u16 << entry_selector).saturating_mul(16);
    let range_shift = n.wrapping_mul(16).wrapping_sub(search_range);

    let mut out = Vec::new();
    out.extend_from_slice(&flavor.to_be_bytes());
    out.extend_from_slice(&n.to_be_bytes());
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&range_shift.to_be_bytes());

    let mut offset = 12 + tables.len() * 16;
    let mut body = Vec::new();
    for (tag, data) in &tables {
        out.extend_from_slice(&tag.to_be_bytes());
        out.extend_from_slice(&table_checksum(data).to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        body.extend_from_slice(data);
        let padded = (data.len() + 3) & !3;
        body.resize(body.len() + (padded - data.len()), 0);
        offset += padded;
    }
    out.extend_from_slice(&body);
    out
}

/// sfnt table checksum: the sum of the table's big-endian u32 words (zero-padded).
fn table_checksum(data: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i < data.len() {
        let mut word = [0u8; 4];
        for (k, w) in word.iter_mut().enumerate() {
            if let Some(b) = data.get(i + k) {
                *w = *b;
            }
        }
        sum = sum.wrapping_add(u32::from_be_bytes(word));
        i += 4;
    }
    sum
}

/// Decode a WOFF2 font into a bare sfnt byte stream, or `None` if the bytes aren't
/// WOFF2 or can't be reconstructed.
pub fn woff2_to_sfnt(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut r = Reader::new(bytes);
    if r.u32()? != u32::from_be_bytes(*b"wOF2") {
        return None;
    }
    let flavor = r.u32()?;
    let _length = r.u32()?;
    let num_tables = r.u16()? as usize;
    let _reserved = r.u16()?;
    let total_sfnt_size = r.u32()? as usize;
    let total_compressed_size = r.u32()? as usize;
    let _major = r.u16()?;
    let _minor = r.u16()?;
    let _meta_off = r.u32()?;
    let _meta_len = r.u32()?;
    let _meta_orig = r.u32()?;
    let _priv_off = r.u32()?;
    let _priv_len = r.u32()?;
    if num_tables == 0 || total_sfnt_size > (1 << 28) {
        return None;
    }

    // Table directory: tag, transform flag, and lengths in the compressed stream.
    struct Entry {
        tag: u32,
        transformed: bool,
        // Length of this table's bytes inside the decompressed stream.
        src_len: usize,
        // Final (sfnt) length: origLength.
        orig_len: usize,
    }
    let mut dir: Vec<Entry> = Vec::with_capacity(num_tables);
    for _ in 0..num_tables {
        let flags = r.u8()?;
        let tag = if flags & 0x3F == 0x3F {
            u32::from_be_bytes(*r.bytes(4)?.first_chunk::<4>()?)
        } else {
            u32::from_be_bytes(*KNOWN_TAGS[(flags & 0x3F) as usize])
        };
        let transform = flags >> 6;
        let orig_len = r.base128()? as usize;
        // glyf/loca: transform 0 = transformed (transformLength follows); 3 = none.
        // Other tables: transform 0 = none; non-zero is unexpected.
        let transformed = (tag == TAG_GLYF || tag == TAG_LOCA) && transform == 0;
        let src_len = if transformed {
            r.base128()? as usize
        } else {
            orig_len
        };
        dir.push(Entry {
            tag,
            transformed,
            src_len,
            orig_len,
        });
    }

    // Brotli-decompress the single concatenated data block.
    let comp = r.bytes(total_compressed_size.min(r.remaining()))?;
    let data = decompress_to_vec_capped::<compcol::brotli::Brotli>(comp, total_sfnt_size as u64 * 2 + 4096)
        .ok()?;

    // Slice each table's bytes from the decompressed stream in directory order.
    let mut cur = 0usize;
    let mut raw: Vec<&[u8]> = Vec::with_capacity(dir.len());
    for e in &dir {
        let end = cur.checked_add(e.src_len)?;
        raw.push(data.get(cur..end)?);
        cur = end;
    }

    // Reconstruct: untransformed tables pass through; a transformed glyf is
    // rebuilt along with its loca (loca's own stream slice is empty/ignored).
    let mut glyf_loca: Option<(Vec<u8>, Vec<u8>)> = None;
    if let Some(gi) = dir.iter().position(|e| e.tag == TAG_GLYF && e.transformed) {
        glyf_loca = Some(reconstruct_glyf(raw[gi])?);
    }
    let mut tables: Vec<(u32, Vec<u8>)> = Vec::with_capacity(dir.len());
    for (i, e) in dir.iter().enumerate() {
        let bytes = if e.tag == TAG_GLYF && e.transformed {
            glyf_loca.as_ref()?.0.clone()
        } else if e.tag == TAG_LOCA && glyf_loca.is_some() {
            glyf_loca.as_ref()?.1.clone()
        } else {
            let b = raw[i];
            if b.len() != e.orig_len && !e.transformed {
                // Lengths must agree for untransformed tables.
                return None;
            }
            b.to_vec()
        };
        tables.push((e.tag, bytes));
    }
    Some(build_sfnt(flavor, tables))
}

/// Reverse the WOFF2 transformed `glyf` table into `(glyf, loca)` sfnt tables.
fn reconstruct_glyf(data: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut h = Reader::new(data);
    let _reserved = h.u16()?;
    let option_flags = h.u16()?;
    let num_glyphs = h.u16()? as usize;
    let index_format = h.u16()?; // 0 = short loca, 1 = long loca
    let n_contour_sz = h.u32()? as usize;
    let n_points_sz = h.u32()? as usize;
    let flag_sz = h.u32()? as usize;
    let glyph_sz = h.u32()? as usize;
    let composite_sz = h.u32()? as usize;
    let bbox_sz = h.u32()? as usize;
    let instruction_sz = h.u32()? as usize;

    // The substreams follow the header in fixed order.
    let mut n_contour = Reader::new(h.bytes(n_contour_sz)?);
    let mut n_points = Reader::new(h.bytes(n_points_sz)?);
    let mut flags = Reader::new(h.bytes(flag_sz)?);
    let mut glyph = Reader::new(h.bytes(glyph_sz)?);
    let mut composite = Reader::new(h.bytes(composite_sz)?);
    let bbox_block = h.bytes(bbox_sz)?;
    let mut instr = Reader::new(h.bytes(instruction_sz)?);
    // The bbox block is a 1-bit-per-glyph bitmap followed by the explicit bboxes.
    let bitmap_len = num_glyphs.div_ceil(8);
    let bbox_bitmap = bbox_block.get(..bitmap_len)?;
    let mut bbox = Reader::new(bbox_block.get(bitmap_len..)?);
    let overlap = option_flags & 1 != 0;
    let mut overlap_bits = if overlap {
        Some(Reader::new(h.bytes(num_glyphs.div_ceil(8))?))
    } else {
        None
    };

    let mut glyf = Vec::new();
    let mut offsets: Vec<u32> = Vec::with_capacity(num_glyphs + 1);
    offsets.push(0);
    for gid in 0..num_glyphs {
        let nc = n_contour.i16()?;
        let has_bbox = bbox_bitmap[gid / 8] & (0x80 >> (gid % 8)) != 0;
        let glyph_start = glyf.len();
        if nc == 0 {
            // Empty glyph: no data, loca offset unchanged.
        } else if nc > 0 {
            emit_simple_glyph(
                nc as usize,
                has_bbox,
                gid == 0 && overlap,
                &mut n_points,
                &mut flags,
                &mut glyph,
                &mut bbox,
                &mut instr,
                &mut overlap_bits,
                &mut glyf,
            )?;
        } else {
            // Composite: bbox is always explicit.
            if !has_bbox {
                return None;
            }
            emit_composite_glyph(&mut composite, &mut glyph, &mut bbox, &mut instr, &mut glyf)?;
        }
        // Pad each glyph to an even length (loca offsets must be representable).
        if glyf.len() % 2 != 0 {
            glyf.push(0);
        }
        let _ = glyph_start;
        offsets.push(glyf.len() as u32);
    }

    // Build loca from the glyph offsets in the requested index format.
    let mut loca = Vec::new();
    if index_format == 0 {
        for off in &offsets {
            // Short loca stores offset/2; every offset is even by construction.
            loca.extend_from_slice(&((off / 2) as u16).to_be_bytes());
        }
    } else {
        for off in &offsets {
            loca.extend_from_slice(&off.to_be_bytes());
        }
    }
    Some((glyf, loca))
}

/// Reconstruct one simple glyph into standard `glyf` format and append it.
#[allow(clippy::too_many_arguments)]
fn emit_simple_glyph(
    n_contours: usize,
    has_bbox: bool,
    overlap_first: bool,
    n_points: &mut Reader,
    flags: &mut Reader,
    glyph: &mut Reader,
    bbox: &mut Reader,
    instr: &mut Reader,
    overlap_bits: &mut Option<Reader>,
    out: &mut Vec<u8>,
) -> Option<()> {
    // Points-per-contour, accumulated to end-point indices.
    let mut end_pts: Vec<u16> = Vec::with_capacity(n_contours);
    let mut total = 0usize;
    for _ in 0..n_contours {
        total += n_points.read255()? as usize;
        end_pts.push((total.checked_sub(1)?) as u16);
    }
    // Decode the triplet-encoded points into per-point (on_curve, dx, dy) deltas.
    let mut on_curve = Vec::with_capacity(total);
    let mut dxs = Vec::with_capacity(total);
    let mut dys = Vec::with_capacity(total);
    let (mut x, mut y, mut xmin, mut ymin, mut xmax, mut ymax) = (0i32, 0i32, i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for _ in 0..total {
        let flag = flags.u8()?;
        let on = flag & 0x80 == 0;
        let (dx, dy) = decode_triplet((flag & 0x7F) as usize, glyph)?;
        on_curve.push(on);
        dxs.push(dx);
        dys.push(dy);
        x += dx;
        y += dy;
        xmin = xmin.min(x);
        ymin = ymin.min(y);
        xmax = xmax.max(x);
        ymax = ymax.max(y);
    }
    let instr_len = glyph.read255()? as usize;
    let instructions = instr.bytes(instr_len)?;

    // Explicit or computed bounding box.
    let (xmin, ymin, xmax, ymax) = if has_bbox {
        (bbox.i16()? as i32, bbox.i16()? as i32, bbox.i16()? as i32, bbox.i16()? as i32)
    } else if total == 0 {
        (0, 0, 0, 0)
    } else {
        (xmin, ymin, xmax, ymax)
    };

    out.extend_from_slice(&(n_contours as i16).to_be_bytes());
    out.extend_from_slice(&(xmin as i16).to_be_bytes());
    out.extend_from_slice(&(ymin as i16).to_be_bytes());
    out.extend_from_slice(&(xmax as i16).to_be_bytes());
    out.extend_from_slice(&(ymax as i16).to_be_bytes());
    for e in &end_pts {
        out.extend_from_slice(&e.to_be_bytes());
    }
    out.extend_from_slice(&(instr_len as u16).to_be_bytes());
    out.extend_from_slice(instructions);
    // Flags: emit one per point (no run-length optimization). on-curve = bit 0;
    // bit 6 = OVERLAP_SIMPLE on the first point when the bitmap requests it.
    for (i, &on) in on_curve.iter().enumerate() {
        let mut f = if on { 0x01u8 } else { 0x00 };
        if i == 0 {
            let bit = match overlap_bits {
                Some(b) => b.u8().map(|byte| byte & 0x80 != 0).unwrap_or(false),
                None => overlap_first,
            };
            if bit {
                f |= 0x40;
            }
        }
        out.push(f);
    }
    // X then Y as int16 deltas (flags left their short/same bits clear).
    for d in &dxs {
        out.extend_from_slice(&(*d as i16).to_be_bytes());
    }
    for d in &dys {
        out.extend_from_slice(&(*d as i16).to_be_bytes());
    }
    Some(())
}

/// Copy one composite glyph's verbatim component records (and optional
/// instructions) into standard `glyf` format and append it.
fn emit_composite_glyph(
    composite: &mut Reader,
    glyph: &mut Reader,
    bbox: &mut Reader,
    instr: &mut Reader,
    out: &mut Vec<u8>,
) -> Option<()> {
    let (xmin, ymin, xmax, ymax) = (bbox.i16()?, bbox.i16()?, bbox.i16()?, bbox.i16()?);
    out.extend_from_slice(&(-1i16).to_be_bytes()); // numberOfContours
    out.extend_from_slice(&xmin.to_be_bytes());
    out.extend_from_slice(&ymin.to_be_bytes());
    out.extend_from_slice(&xmax.to_be_bytes());
    out.extend_from_slice(&ymax.to_be_bytes());

    // Copy component records verbatim, tracking WE_HAVE_INSTRUCTIONS / MORE.
    let mut have_instructions = false;
    loop {
        let flags = composite.u16()?;
        let _glyph_index = composite.u16()?;
        out.extend_from_slice(&flags.to_be_bytes());
        out.extend_from_slice(&_glyph_index.to_be_bytes());
        // Argument bytes: words (4) or bytes (2).
        let arg_len = if flags & 0x0001 != 0 { 4 } else { 2 };
        out.extend_from_slice(composite.bytes(arg_len)?);
        // Optional transform.
        let xform = if flags & 0x0008 != 0 {
            2 // WE_HAVE_A_SCALE (one F2Dot14)
        } else if flags & 0x0040 != 0 {
            4 // X_AND_Y_SCALE
        } else if flags & 0x0080 != 0 {
            8 // TWO_BY_TWO
        } else {
            0
        };
        if xform > 0 {
            out.extend_from_slice(composite.bytes(xform)?);
        }
        if flags & 0x0100 != 0 {
            have_instructions = true;
        }
        if flags & 0x0020 == 0 {
            break; // no MORE_COMPONENTS
        }
    }
    if have_instructions {
        let n = glyph.read255()? as usize;
        let bytes = instr.bytes(n)?;
        out.extend_from_slice(&(n as u16).to_be_bytes());
        out.extend_from_slice(bytes);
    }
    Some(())
}

/// Decode a coordinate triplet given the flag's low-7-bit index, reading the
/// data bytes from `glyph`. Returns the signed `(dx, dy)` delta (WOFF2 §5.2).
fn decode_triplet(index: usize, glyph: &mut Reader) -> Option<(i32, i32)> {
    // Sign helper: the 2-bit code maps to (x_positive, y_positive).
    let sign = |s: usize| ((s & 1) == 1, (s & 2) == 2);
    if index < 10 {
        // y only, 8-bit magnitude, base 256*(index/2).
        let (_, yp) = (false, index & 1 == 1);
        let dy = 256 * (index as i32 / 2) + glyph.u8()? as i32;
        Some((0, if yp { dy } else { -dy }))
    } else if index < 20 {
        let i = index - 10;
        let xp = i & 1 == 1;
        let dx = 256 * (i as i32 / 2) + glyph.u8()? as i32;
        Some((if xp { dx } else { -dx }, 0))
    } else if index < 84 {
        // x,y 4-bit nibbles packed in one byte; bases from {1,17,33,49}.
        let i = index - 20;
        let bases = [1i32, 17, 33, 49];
        let (xp, yp) = sign(i % 4);
        let dxb = bases[i / 16];
        let dyb = bases[(i / 4) % 4];
        let byte = glyph.u8()? as i32;
        let dx = dxb + (byte >> 4);
        let dy = dyb + (byte & 0x0F);
        Some((if xp { dx } else { -dx }, if yp { dy } else { -dy }))
    } else if index < 120 {
        // x,y 8-bit each (two data bytes); bases from {1,257,513}.
        let i = index - 84;
        let bases = [1i32, 257, 513];
        let (xp, yp) = sign(i % 4);
        let dxb = bases[i / 12];
        let dyb = bases[(i / 4) % 3];
        let dx = dxb + glyph.u8()? as i32;
        let dy = dyb + glyph.u8()? as i32;
        Some((if xp { dx } else { -dx }, if yp { dy } else { -dy }))
    } else if index < 124 {
        // 12-bit x and y packed into three bytes.
        let (xp, yp) = sign(index - 120);
        let b0 = glyph.u8()? as i32;
        let b1 = glyph.u8()? as i32;
        let b2 = glyph.u8()? as i32;
        let dx = (b0 << 4) | (b1 >> 4);
        let dy = ((b1 & 0x0F) << 8) | b2;
        Some((if xp { dx } else { -dx }, if yp { dy } else { -dy }))
    } else if index < 128 {
        // 16-bit x and y (four data bytes).
        let (xp, yp) = sign(index - 124);
        let dx = glyph.u16()? as i32;
        let dy = glyph.u16()? as i32;
        Some((if xp { dx } else { -dx }, if yp { dy } else { -dy }))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base128_and_255_readers() {
        // base128: 0x3F -> 63; 0x8F 0x00 -> (0xF<<7)|0 = 1920.
        assert_eq!(Reader::new(&[0x3F]).base128(), Some(63));
        assert_eq!(Reader::new(&[0x8F, 0x00]).base128(), Some(1920));
        assert_eq!(Reader::new(&[0x80, 0x00]).base128(), None); // leading zero
        // read255: <253 direct; 255,b -> b+253; 254,b -> b+506; 253,hi,lo -> word.
        assert_eq!(Reader::new(&[200]).read255(), Some(200));
        assert_eq!(Reader::new(&[255, 10]).read255(), Some(263));
        assert_eq!(Reader::new(&[254, 10]).read255(), Some(516));
        assert_eq!(Reader::new(&[253, 0x12, 0x34]).read255(), Some(0x1234));
    }

    #[test]
    fn triplet_decode_known_cases() {
        // Index 1: y only, +, 8-bit value 5 -> (0, 5).
        assert_eq!(decode_triplet(1, &mut Reader::new(&[5])), Some((0, 5)));
        // Index 0: y only, -, value 5 -> (0, -5).
        assert_eq!(decode_triplet(0, &mut Reader::new(&[5])), Some((0, -5)));
        // Index 11: x only, +, value 3 -> (3, 0).
        assert_eq!(decode_triplet(11, &mut Reader::new(&[3])), Some((3, 0)));
        // Index 23: 4-bit nibbles, (+,+), base (1,1); byte 0x21 -> x=1+2, y=1+1.
        assert_eq!(decode_triplet(23, &mut Reader::new(&[0x21])), Some((3, 2)));
        // Index 87: 8-bit each, (+,+), base (1,1); bytes 4,9 -> (5, 10).
        assert_eq!(decode_triplet(87, &mut Reader::new(&[4, 9])), Some((5, 10)));
        // Index 125: 16-bit each, (+,-); 0x0102, 0x0304 -> (258, -772).
        assert_eq!(
            decode_triplet(125, &mut Reader::new(&[1, 2, 3, 4])),
            Some((258, -772))
        );
    }

    #[test]
    fn rejects_non_woff2() {
        assert!(woff2_to_sfnt(b"\x00\x01\x00\x00rest of an sfnt").is_none());
        assert!(woff2_to_sfnt(b"wOFFnot-v2").is_none());
        assert!(woff2_to_sfnt(b"short").is_none());
    }
}
