//! Test fixtures generated in code, so the repository needs no binary test images.

#![allow(dead_code, reason = "each test crate uses part of it")]
#![allow(
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    reason = "fixtures fail loudly, and wrap pixel values on purpose"
)]

use std::borrow::Cow;

use oxipng::{BitDepth, ColorType, Options, RawImage};

/// An animated RGBA PNG with `frames` full-size frames, written by hand because oxipng can't
/// create APNGs. The image data is stored uncompressed, so oxipng has plenty to recompress.
pub(crate) fn apng(width: u32, height: u32, frames: u32) -> Vec<u8> {
    let mut out = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    write_chunk(&mut out, *b"IHDR", &ihdr);

    let mut actl = frames.to_be_bytes().to_vec();
    actl.extend_from_slice(&0u32.to_be_bytes()); // loop forever
    write_chunk(&mut out, *b"acTL", &actl);

    let mut sequence = 0u32;
    for frame in 0..frames {
        let mut fctl = sequence.to_be_bytes().to_vec();
        for value in [width, height, 0, 0] {
            fctl.extend_from_slice(&value.to_be_bytes());
        }
        fctl.extend_from_slice(&[0, 1, 0, 10, 0, 0]); // 1/10 s, no dispose, no blend
        write_chunk(&mut out, *b"fcTL", &fctl);
        sequence += 1;

        let mut rows = Vec::new();
        for y in 0..height {
            rows.push(0); // filter type None
            for x in 0..width {
                let shade = ((x + y + frame) % 4 * 60) as u8;
                rows.extend_from_slice(&[(x * 8 + frame * 40) as u8, (y * 8) as u8, shade, 255]);
            }
        }
        let data = zlib_stored(&rows);
        if frame == 0 {
            write_chunk(&mut out, *b"IDAT", &data);
        } else {
            let mut fdat = sequence.to_be_bytes().to_vec();
            fdat.extend_from_slice(&data);
            write_chunk(&mut out, *b"fdAT", &fdat);
            sequence += 1;
        }
    }
    write_chunk(&mut out, *b"IEND", &[]);
    out
}

/// `png` with `chunks` added right after its IHDR, where metadata usually is.
pub(crate) fn with_chunks(png: &[u8], chunks: &[([u8; 4], &[u8])]) -> Vec<u8> {
    // The signature, then IHDR: length, name, 13 bytes of data, and CRC.
    let (head, rest) = png.split_at(8 + 4 + 4 + 13 + 4);
    let mut out = head.to_vec();
    for (name, data) in chunks {
        write_chunk(&mut out, *name, data);
    }
    out.extend_from_slice(rest);
    out
}

fn write_chunk(out: &mut Vec<u8>, name: [u8; 4], data: &[u8]) {
    out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
    let start = out.len();
    out.extend_from_slice(&name);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// zlib stream made of stored (uncompressed) deflate blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = data.chunks(65_535).collect();
    for (i, block) in blocks.iter().enumerate() {
        let len = u16::try_from(block.len()).unwrap();
        out.push(u8::from(i + 1 == blocks.len()));
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// An RGBA image that uses only `colors` distinct opaque colors, stored without any reductions
/// and with a `tEXt` chunk. oxipng should turn it into an indexed PNG and (with safe
/// stripping) drop the text.
pub(crate) fn few_color_rgba_png(width: u32, height: u32, colors: u8) -> Vec<u8> {
    encode_without_reductions(&few_color_rgba(width, height, colors))
}

/// [`few_color_rgba_png`], but Adam7-interlaced, as some tools save them.
pub(crate) fn interlaced_png(width: u32, height: u32, colors: u8) -> Vec<u8> {
    let mut opts = Options::from_preset(0);
    opts.bit_depth_reduction = false;
    opts.color_type_reduction = false;
    opts.palette_reduction = false;
    opts.grayscale_reduction = false;
    opts.interlace = Some(true);
    // oxipng only interlaces when that makes the file smaller, unless forced.
    opts.force = true;
    few_color_rgba(width, height, colors)
        .create_optimized_png(&opts)
        .unwrap()
}

fn few_color_rgba(width: u32, height: u32, colors: u8) -> RawImage {
    let mut data = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let c = ((x / 7 + y / 5) % u32::from(colors)) as u8;
            data.extend_from_slice(&[c.wrapping_mul(37), 255 - c.wrapping_mul(11), c, 255]);
        }
    }
    let mut raw = RawImage::new(width, height, ColorType::RGBA, BitDepth::Eight, data).unwrap();
    raw.add_png_chunk(
        *b"tEXt",
        b"Comment\0generated by the oxidrop tests".to_vec(),
    );
    raw
}

/// A noisy RGB image with many colors; the slowest kind of input to compress.
pub(crate) fn noisy_rgb_png(width: u32, height: u32) -> Vec<u8> {
    let mut seed = 0x1234_5678_u32;
    let mut data = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 28) as u8;
            data.extend_from_slice(&[(x as u8) ^ noise, y as u8, ((x + y) % 64) as u8]);
        }
    }
    let raw = RawImage::new(
        width,
        height,
        ColorType::RGB {
            transparent_color: None,
        },
        BitDepth::Eight,
        data,
    )
    .unwrap();
    encode_without_reductions(&raw)
}

fn encode_without_reductions(raw: &RawImage) -> Vec<u8> {
    let mut opts = Options::from_preset(0);
    opts.bit_depth_reduction = false;
    opts.color_type_reduction = false;
    opts.palette_reduction = false;
    opts.grayscale_reduction = false;
    raw.create_optimized_png(&opts).unwrap()
}

/// A looping GIF of a square moving across a light background. Every frame is stored in full,
/// as simple GIF encoders do, so the APNG it converts to is much smaller.
pub(crate) fn moving_square_gif(width: u16, height: u16, frames: u16) -> Vec<u8> {
    let palette = [240, 240, 240, 200, 30, 30];
    let mut out = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut out, width, height, &palette).unwrap();
        encoder.set_repeat(gif::Repeat::Infinite).unwrap();
        for i in 0..frames {
            let mut pixels = vec![0_u8; usize::from(width) * usize::from(height)];
            let left = usize::from(i * 4 % (width - 6));
            for y in 4..10 {
                let row = y * usize::from(width);
                pixels[row + left..row + left + 6].fill(1);
            }
            let frame = gif::Frame {
                width,
                height,
                delay: 5,
                buffer: Cow::Owned(pixels),
                ..gif::Frame::default()
            };
            encoder.write_frame(&frame).unwrap();
        }
    }
    out
}

/// A lossless WebP with a smooth gradient.
pub(crate) fn gradient_webp(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            rgba.extend_from_slice(&[(x * 255 / width) as u8, (y * 255 / height) as u8, 90, 255]);
        }
    }
    let mut out = Vec::new();
    image_webp::WebPEncoder::new(&mut out)
        .encode(&rgba, width, height, image_webp::ColorType::Rgba8)
        .unwrap();
    out
}
