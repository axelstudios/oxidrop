//! The app icon, drawn in code: a water drop. The window icon and the Windows `.exe` icon
//! (built by `build.rs`, which includes this file) come from this one source, so no image
//! files need to be checked in. This module uses only `std` for that reason.

/// Sizes stored in the Windows `.ico` file.
pub const ICO_SIZES: [u32; 6] = [16, 24, 32, 48, 64, 256];

/// Samples per pixel along each axis, for anti-aliased edges.
const SUPERSAMPLING: u32 = 4;

/// The icon as straight (not premultiplied) RGBA, `size` × `size` pixels, rows top to bottom.
#[must_use]
pub fn rgba(size: u32) -> Vec<u8> {
    let size = size.max(1);
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    let step = 1.0 / f64::from(size * SUPERSAMPLING);
    for py in 0..size {
        for px in 0..size {
            let (mut drop, mut shine, mut shade) = (0.0, 0.0, 0.0);
            for sy in 0..SUPERSAMPLING {
                for sx in 0..SUPERSAMPLING {
                    let x = (f64::from(px * SUPERSAMPLING + sx) + 0.5) * step;
                    let y = (f64::from(py * SUPERSAMPLING + sy) + 0.5) * step;
                    if in_drop(x, y) {
                        drop += 1.0;
                        shade += y;
                        if in_highlight(x, y) {
                            shine += 1.0;
                        }
                    }
                }
            }
            let samples = f64::from(SUPERSAMPLING * SUPERSAMPLING);
            if drop == 0.0 {
                pixels.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            // Light blue at the top to a deeper blue at the bottom, with a soft white shine.
            let t = shade / drop;
            let base = [
                lerp(110.0, 11.0, t),
                lerp(193.0, 111.0, t),
                lerp(255.0, 201.0, t),
            ];
            let white = 0.6 * shine / drop;
            let channel = |c: f64| to_byte(c + (255.0 - c) * white);
            pixels.extend_from_slice(&[
                channel(base[0]),
                channel(base[1]),
                channel(base[2]),
                to_byte(255.0 * drop / samples),
            ]);
        }
    }
    pixels
}

/// Teardrop: a circle plus the cone from the tip to where it touches the circle.
fn in_drop(x: f64, y: f64) -> bool {
    const TIP_Y: f64 = 0.05;
    const CENTER: (f64, f64) = (0.5, 0.61);
    const RADIUS: f64 = 0.35;
    let (dx, dy) = (x - CENTER.0, y - CENTER.1);
    if dx * dx + dy * dy <= RADIUS * RADIUS {
        return true;
    }
    let axis = CENTER.1 - TIP_Y;
    let sin = RADIUS / axis;
    let cos = (1.0 - sin * sin).sqrt();
    // The cone's sides touch the circle this far below the tip.
    let touch = axis * cos * cos;
    let depth = y - TIP_Y;
    depth >= 0.0 && depth <= touch && dx.abs() <= depth * sin / cos
}

/// An oval shine on the upper left of the drop.
fn in_highlight(x: f64, y: f64) -> bool {
    let (dx, dy) = ((x - 0.37) / 0.075, (y - 0.55) / 0.15);
    dx * dx + dy * dy <= 1.0
}

fn lerp(from: f64, to: f64, t: f64) -> f64 {
    from + (to - from) * t.clamp(0.0, 1.0)
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=255 first"
)]
fn to_byte(value: f64) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

/// A Windows `.ico` file with the icon at every size in [`ICO_SIZES`], stored as 32-bit
/// bitmaps (supported by every Windows version, no image encoder needed).
#[must_use]
pub fn ico() -> Vec<u8> {
    let images: Vec<(u32, Vec<u8>)> = ICO_SIZES.iter().map(|&s| (s, bitmap(s))).collect();
    let count = u16::try_from(images.len()).unwrap_or(u16::MAX);
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]); // reserved, type 1 = icon
    out.extend_from_slice(&count.to_le_bytes());
    let mut offset = 6 + 16 * images.len();
    for (size, data) in &images {
        // Width and height bytes of 0 mean 256.
        let dimension = u8::try_from(*size).unwrap_or(0);
        out.extend_from_slice(&[dimension, dimension, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // color planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_le_bytes());
        out.extend_from_slice(&u32::try_from(offset).unwrap_or(u32::MAX).to_le_bytes());
        offset += data.len();
    }
    for (_, data) in images {
        out.extend_from_slice(&data);
    }
    out
}

/// One `.ico` image: a BITMAPINFOHEADER, bottom-up BGRA rows, then an all-zero AND mask (the
/// alpha channel already says what is transparent).
fn bitmap(size: u32) -> Vec<u8> {
    let rgba = rgba(size);
    let width = size as usize;
    let mask_row = size.div_ceil(32) as usize * 4;
    let mut out = Vec::new();
    out.extend_from_slice(&40u32.to_le_bytes()); // header size
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&(size * 2).to_le_bytes()); // color rows plus mask rows
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
    out.extend_from_slice(&[0; 24]); // no compression, sizes and palette left to defaults
    for row in rgba.chunks_exact(width * 4).rev() {
        for &[r, g, b, a] in row.as_chunks::<4>().0 {
            out.extend_from_slice(&[b, g, r, a]);
        }
    }
    out.resize(out.len() + mask_row * width, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(pixels: &[u8], size: u32, x: u32, y: u32) -> u8 {
        pixels[((y * size + x) * 4 + 3) as usize]
    }

    #[test]
    fn draws_an_opaque_drop_on_a_transparent_background() {
        let pixels = rgba(64);
        assert_eq!(pixels.len(), 64 * 64 * 4);
        assert_eq!(alpha(&pixels, 64, 0, 0), 0, "corners are transparent");
        assert_eq!(alpha(&pixels, 64, 63, 63), 0);
        assert_eq!(
            alpha(&pixels, 64, 32, 40),
            255,
            "the middle of the drop is opaque"
        );
        assert_eq!(alpha(&pixels, 64, 32, 5), 255, "the tip reaches the top");
        assert_eq!(alpha(&pixels, 64, 10, 8), 0, "beside the tip is empty");
        let edge = alpha(&pixels, 64, 32, 61);
        assert!(edge > 0 && edge < 255, "edges are anti-aliased, got {edge}");
    }

    #[test]
    fn writes_a_well_formed_ico() {
        let ico = ico();
        assert_eq!(&ico[..4], &[0, 0, 1, 0]);
        assert_eq!(u16::from_le_bytes([ico[4], ico[5]]), 6);
        let mut expected_offset = 6 + 16 * ICO_SIZES.len();
        for (i, &size) in ICO_SIZES.iter().enumerate() {
            let entry = &ico[6 + 16 * i..6 + 16 * (i + 1)];
            assert_eq!(u32::from(entry[0]), size % 256);
            let length = u32::from_le_bytes(entry[8..12].try_into().unwrap_or_default()) as usize;
            let offset = u32::from_le_bytes(entry[12..16].try_into().unwrap_or_default()) as usize;
            assert_eq!(offset, expected_offset);
            let mask = size.div_ceil(32) as usize * 4 * size as usize;
            assert_eq!(length, 40 + (size * size * 4) as usize + mask);
            expected_offset += length;
        }
        assert_eq!(ico.len(), expected_offset);
    }
}
