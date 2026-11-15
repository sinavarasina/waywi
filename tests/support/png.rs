fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn push_chunk(out: &mut Vec<u8>, tag: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    let crc_start = out.len();
    out.extend_from_slice(tag);
    out.extend_from_slice(payload);
    let c = crc32(&out[crc_start..]);
    out.extend_from_slice(&c.to_be_bytes());
}

/// Encodes an uncompressed 32-bit RGBA buffer into a valid PNG byte stream for tests.
pub fn make_rgba_png(width: u32, height: u32, rgba_pixels: &[u8]) -> Vec<u8> {
    assert_eq!(rgba_pixels.len(), (width * height * 4) as usize);
    let mut raw = Vec::with_capacity(((width * 4 + 1) * height) as usize);
    for y in 0..height as usize {
        raw.push(0);
        let start = y * (width as usize) * 4;
        raw.extend_from_slice(&rgba_pixels[start..start + (width as usize) * 4]);
    }

    let mut zlib = Vec::with_capacity(raw.len() + 11);
    zlib.extend_from_slice(&[0x78, 0x01, 0x01]);
    let len = raw.len() as u16;
    zlib.extend_from_slice(&len.to_le_bytes());
    zlib.extend_from_slice(&(!len).to_le_bytes());
    zlib.extend_from_slice(&raw);
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = Vec::new();
    out.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    push_chunk(&mut out, b"IHDR", &ihdr);
    push_chunk(&mut out, b"IDAT", &zlib);
    push_chunk(&mut out, b"IEND", &[]);
    out
}
