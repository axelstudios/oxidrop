//! Test helpers: GIF and WebP files built in code, and an APNG player to check conversions.

use std::{borrow::Cow, io::Cursor};

use gif::{DisposalMethod, Repeat};
use png::{BlendOp, ColorType, DisposeOp, Transformations};

/// Red, green, blue, and white, as a GIF palette.
pub(crate) const PALETTE: [u8; 12] = [255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];

/// A GIF frame drawing palette `indices` into a rectangle.
pub(crate) fn gif_frame(
    rect: (u16, u16, u16, u16),
    indices: &[u8],
    delay_cs: u16,
    dispose: DisposalMethod,
    transparent: Option<u8>,
) -> gif::Frame<'static> {
    let (left, top, width, height) = rect;
    gif::Frame {
        left,
        top,
        width,
        height,
        buffer: Cow::Owned(indices.to_vec()),
        delay: delay_cs,
        dispose,
        transparent,
        ..gif::Frame::default()
    }
}

/// A GIF file using [`PALETTE`].
pub(crate) fn gif(
    width: u16,
    height: u16,
    repeat: Option<Repeat>,
    frames: &[gif::Frame<'_>],
) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut out, width, height, &PALETTE).expect("encoder");
        if let Some(repeat) = repeat {
            encoder.set_repeat(repeat).expect("repeat");
        }
        for frame in frames {
            encoder.write_frame(frame).expect("frame");
        }
    }
    out
}

/// A lossless WebP file with RGBA pixels.
pub(crate) fn webp(width: u32, height: u32, rgba: &[u8], icc: Option<Vec<u8>>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = image_webp::WebPEncoder::new(&mut out);
    if let Some(icc) = icc {
        encoder.set_icc_profile(icc);
    }
    encoder
        .encode(rgba, width, height, image_webp::ColorType::Rgba8)
        .expect("encodable");
    out
}

/// One frame of an animated WebP.
pub(crate) struct WebPFrame<'a> {
    pub rect: (u32, u32, u32, u32),
    pub rgba: &'a [u8],
    pub duration_ms: u32,
    /// Alpha-blend over the canvas instead of replacing the rectangle.
    pub blend: bool,
    /// Clear the rectangle to the background once the frame has been shown.
    pub dispose: bool,
}

/// An animated WebP with lossless frames that loops `loops` times (0 = forever).
pub(crate) fn animated_webp(
    width: u32,
    height: u32,
    loops: u16,
    frames: &[WebPFrame<'_>],
) -> Vec<u8> {
    fn chunk(name: [u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = name.to_vec();
        out.extend_from_slice(&u32::try_from(data.len()).expect("small").to_le_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(0);
        }
        out
    }
    fn u24(value: u32) -> [u8; 3] {
        let [a, b, c, _] = value.to_le_bytes();
        [a, b, c]
    }

    let mut vp8x = vec![0x12, 0, 0, 0]; // alpha + animation
    vp8x.extend(u24(width - 1));
    vp8x.extend(u24(height - 1));
    let mut anim = vec![0, 0, 0, 0];
    anim.extend(loops.to_le_bytes());

    let mut body = b"WEBP".to_vec();
    body.extend(chunk(*b"VP8X", &vp8x));
    body.extend(chunk(*b"ANIM", &anim));
    for frame in frames {
        let (x, y, w, h) = frame.rect;
        let encoded = webp(w, h, frame.rgba, None);
        assert_eq!(&encoded[12..16], b"VP8L", "a simple lossless WebP");
        let mut anmf = Vec::new();
        for value in [x / 2, y / 2, w - 1, h - 1, frame.duration_ms] {
            anmf.extend(u24(value));
        }
        anmf.push(u8::from(!frame.blend) << 1 | u8::from(frame.dispose));
        anmf.extend_from_slice(&encoded[12..]);
        body.extend(chunk(*b"ANMF", &anmf));
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&u32::try_from(body.len()).expect("small").to_le_bytes());
    out.extend(body);
    out
}

/// A PNG or APNG as a viewer plays it.
#[derive(Debug)]
pub(crate) struct Played {
    /// Times to play; 0 means forever.
    pub plays: u32,
    /// The full canvas after each frame, with how long it shows.
    pub frames: Vec<(Vec<u8>, u32)>,
    pub blend_ops: Vec<BlendOp>,
}

/// Plays a PNG or APNG made by the converter (which never disposes frames).
pub(crate) fn play(png: &[u8]) -> Played {
    let mut decoder = png::Decoder::new(Cursor::new(png));
    decoder.set_transformations(Transformations::EXPAND);
    let mut reader = decoder.read_info().expect("valid png");
    let (width, height) = (reader.info().width, reader.info().height);
    let (count, plays) = reader
        .info()
        .animation_control
        .map_or((1, 1), |a| (a.num_frames, a.num_plays));
    let mut canvas = vec![0; width as usize * height as usize * 4];
    let mut buffer = vec![0; reader.output_buffer_size().expect("size")];
    let mut played = Played {
        plays,
        frames: Vec::new(),
        blend_ops: Vec::new(),
    };
    for _ in 0..count {
        let output = reader.next_frame(&mut buffer).expect("frame");
        let control = reader.info().frame_control;
        let (x, y, delay_ms, blend) = control.map_or((0, 0, 0, BlendOp::Source), |c| {
            assert_eq!(c.dispose_op, DisposeOp::None);
            let den = if c.delay_den == 0 { 100 } else { c.delay_den };
            (
                c.x_offset,
                c.y_offset,
                u32::from(c.delay_num) * 1000 / u32::from(den),
                c.blend_op,
            )
        });
        let rows =
            buffer[..output.line_size * output.height as usize].chunks_exact(output.line_size);
        for (row, line) in rows.enumerate() {
            let pixels: Vec<[u8; 4]> = match output.color_type {
                ColorType::Rgba => line.as_chunks::<4>().0.to_vec(),
                ColorType::Rgb => line
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|&[r, g, b]| [r, g, b, 255])
                    .collect(),
                ColorType::GrayscaleAlpha => line
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&[g, a]| [g, g, g, a])
                    .collect(),
                ColorType::Grayscale => line.iter().map(|&g| [g, g, g, 255]).collect(),
                ColorType::Indexed => panic!("EXPAND turns palettes into RGB(A)"),
            };
            for (column, pixel) in pixels.into_iter().take(output.width as usize).enumerate() {
                if blend == BlendOp::Over && pixel[3] == 0 {
                    continue;
                }
                assert!(
                    blend == BlendOp::Source || pixel[3] == 255,
                    "only opaque pixels blend"
                );
                let at = ((y as usize + row) * width as usize + x as usize + column) * 4;
                canvas[at..at + 4].copy_from_slice(&pixel);
            }
        }
        played.frames.push((canvas.clone(), delay_ms));
        played.blend_ops.push(blend);
    }
    played
}

/// A canvas filled with `color`, with some pixels changed.
pub(crate) fn canvas(
    width: u32,
    height: u32,
    fill: [u8; 4],
    pixels: &[(u32, u32, [u8; 4])],
) -> Vec<u8> {
    let mut canvas = fill.repeat(width as usize * height as usize);
    for &(x, y, color) in pixels {
        let at = ((y * width + x) * 4) as usize;
        canvas[at..at + 4].copy_from_slice(&color);
    }
    canvas
}
