//! Clipboard image conversion between Windows DIBs and PNG.

/// Convert a packed DIB (CF_DIB / CF_DIBV5 contents) to PNG.
pub fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
    let u32_at = |o: usize| dib.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let i32_at = |o: usize| u32_at(o).map(|v| v as i32);
    let u16_at = |o: usize| dib.get(o..o + 2).map(|b| u16::from_le_bytes(b.try_into().unwrap()));

    let header = u32_at(0)? as usize;
    let width = i32_at(4)?;
    let height = i32_at(8)?;
    let bpp = u16_at(14)?;
    let compression = u32_at(16)?;
    let colors_used = u32_at(32)? as usize;
    if width <= 0 || height == 0 || !(bpp == 24 || bpp == 32) || !(compression == 0 || compression == 3) {
        return None;
    }
    let (w, h) = (width as usize, height.unsigned_abs() as usize);
    if w > 32768 || h > 32768 {
        return None;
    }
    let mut offset = header + colors_used * 4;
    if compression == 3 && header == 40 {
        offset += 12;
    }
    let stride = (w * bpp as usize).div_ceil(32) * 4;
    let pixels = dib.get(offset..offset + stride * h)?;
    let bottom_up = height > 0;
    let step = bpp as usize / 8;

    let mut rgba = vec![0u8; w * h * 4];
    let mut any_alpha = false;
    for row in 0..h {
        let src_row = if bottom_up { h - 1 - row } else { row };
        let src = &pixels[src_row * stride..src_row * stride + w * step];
        let dst = &mut rgba[row * w * 4..(row + 1) * w * 4];
        for (s, d) in src.chunks_exact(step).zip(dst.chunks_exact_mut(4)) {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = if step == 4 { s[3] } else { 255 };
            any_alpha |= step == 4 && s[3] != 0;
        }
    }
    // Most apps leave the alpha byte at zero in 32-bit DIBs.
    if bpp == 32 && !any_alpha {
        rgba.chunks_exact_mut(4).for_each(|p| p[3] = 255);
    }
    encode_png(w as u32, h as u32, &rgba)
}

pub fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(rgba).ok()?;
    }
    Some(out)
}

/// Decode PNG to (width, height, RGBA8).
pub fn decode_png(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(data));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width, info.height);
    let px = (w * h) as usize;
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf[..px * 4].to_vec(),
        png::ColorType::Rgb => buf[..px * 3].chunks_exact(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf[..px * 2].chunks_exact(2).flat_map(|c| [c[0], c[0], c[0], c[1]]).collect(),
        png::ColorType::Grayscale => buf[..px].iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return None,
    };
    Some((w, h, rgba))
}

/// Packed bottom-up 32-bit DIB for CF_DIB.
pub fn png_to_dib(data: &[u8]) -> Option<Vec<u8>> {
    let (w, h, rgba) = decode_png(data)?;
    let (wu, hu) = (w as usize, h as usize);
    let mut dib = Vec::with_capacity(40 + wu * hu * 4);
    dib.extend_from_slice(&40u32.to_le_bytes());
    dib.extend_from_slice(&(w as i32).to_le_bytes());
    dib.extend_from_slice(&(h as i32).to_le_bytes());
    dib.extend_from_slice(&1u16.to_le_bytes());
    dib.extend_from_slice(&32u16.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    dib.extend_from_slice(&((wu * hu * 4) as u32).to_le_bytes());
    dib.extend_from_slice(&2835i32.to_le_bytes());
    dib.extend_from_slice(&2835i32.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes());
    for row in (0..hu).rev() {
        for p in rgba[row * wu * 4..(row + 1) * wu * 4].chunks_exact(4) {
            dib.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }
    Some(dib)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_dib_roundtrip() {
        let rgba: Vec<u8> = (0..3 * 2).flat_map(|i| [i as u8 * 40, 10, 200, 255]).collect();
        let png = encode_png(3, 2, &rgba).unwrap();
        let dib = png_to_dib(&png).unwrap();
        let back = dib_to_png(&dib).unwrap();
        let (w, h, px) = decode_png(&back).unwrap();
        assert_eq!((w, h), (3, 2));
        assert_eq!(px, rgba);
    }
}
