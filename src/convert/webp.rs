//! WebP decoding: still images (lossy or lossless) and animations.
//!
//! Still images and single animation frames are decoded by `image-webp`. Animations are
//! composited here, following libwebp: `image-webp` 0.2.4 also blends fully opaque pixels,
//! which darkens every color channel by one
//! (<https://github.com/image-rs/image-webp/issues/196>).

use std::io::{BufRead, Cursor, Seek};

use image_webp::WebPDecoder;

use super::{
    ConvertError, Converted, FileType, Options, Progress, Source, Step,
    animation::{MAX_CANVAS_BYTES, Planner},
    encode::{self, Metadata},
};

/// VP8X flag for an alpha channel.
const VP8X_ALPHA: u8 = 0x10;

pub(super) fn convert(
    data: &[u8],
    options: Options,
    progress: Progress<'_>,
) -> Result<Converted, ConvertError> {
    let mut decoder = WebPDecoder::new(Cursor::new(data))?;
    decoder.set_memory_limit(MAX_CANVAS_BYTES);
    let (width, height) = decoder.dimensions();
    super::check_size(width, height)?;
    let metadata = metadata(&mut decoder)?;

    let (png, frames, lossy, notes) = if decoder.is_animated() {
        let animation = Animation::parse(data, width, height)?;
        let total = u32::try_from(animation.frames.len()).unwrap_or(u32::MAX);

        // First pass: plan the frames.
        let mut planner = Planner::new(width, height, options.clean_transparent)?;
        let mut read = 0;
        let lossy = animation.composite(&mut |canvas, delay_ms| {
            planner.push(canvas, delay_ms);
            read += 1;
            progress(Step::Reading {
                done: read,
                total: Some(total),
            });
            Ok(())
        })?;

        // Second pass: write them.
        let mut png = Vec::new();
        let mut writer = planner.into_writer(&mut png, animation.plays, &metadata)?;
        if writer.needs_frames() {
            let mut written = 0;
            animation.composite(&mut |canvas, _| {
                writer.push(canvas)?;
                written += 1;
                progress(Step::Writing {
                    done: written,
                    total,
                });
                Ok(())
            })?;
        }
        let notes = writer.finish()?;
        (png, total, lossy, notes)
    } else {
        let lossy = decoder.is_lossy();
        let rgba = decode_image(&mut decoder)?;
        progress(Step::Reading {
            done: 1,
            total: Some(1),
        });
        (
            encode::still(width, height, &rgba, &metadata)?,
            1,
            lossy,
            Vec::new(),
        )
    };

    Ok(Converted {
        png,
        source: Source {
            file_type: FileType::WebP,
            lossy,
            width,
            height,
            frames,
            metadata: metadata.names(),
            notes,
            file_size: 0,
        },
    })
}

/// Describes a WebP without decoding its pixels: dimensions, lossiness, frame count, and
/// metadata.
pub(super) fn inspect(reader: impl BufRead + Seek) -> Result<Source, ConvertError> {
    let mut decoder = WebPDecoder::new(reader)?;
    let (width, height) = decoder.dimensions();
    Ok(Source {
        file_type: FileType::WebP,
        lossy: decoder.is_lossy(),
        width,
        height,
        frames: if decoder.is_animated() {
            decoder.num_frames()
        } else {
            1
        },
        metadata: metadata(&mut decoder)?.names(),
        notes: Vec::new(),
        file_size: 0,
    })
}

fn metadata<R: BufRead + Seek>(decoder: &mut WebPDecoder<R>) -> Result<Metadata, ConvertError> {
    Ok(Metadata {
        icc: decoder.icc_profile()?,
        exif: decoder.exif_metadata()?.map(Metadata::exif_from),
        xmp: decoder
            .xmp_metadata()?
            .and_then(|xmp| String::from_utf8(xmp).ok()),
    })
}

fn malformed(what: &str) -> ConvertError {
    ConvertError::WebP(what.to_owned())
}

/// The whole image as RGBA.
fn decode_image<R: BufRead + Seek>(decoder: &mut WebPDecoder<R>) -> Result<Vec<u8>, ConvertError> {
    let size = decoder.output_buffer_size().ok_or_else(|| {
        let (width, height) = decoder.dimensions();
        ConvertError::TooLarge { width, height }
    })?;
    let mut buffer = vec![0; size];
    decoder.read_image(&mut buffer)?;
    if decoder.has_alpha() {
        Ok(buffer)
    } else {
        Ok(buffer
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[r, g, b]| [r, g, b, 255])
            .collect())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl Rect {
    const fn contains(self, x: u32, y: u32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    const fn covers(self, width: u32, height: u32) -> bool {
        self.x == 0 && self.y == 0 && self.width == width && self.height == height
    }
}

/// What the previous frame left behind, for libwebp's key frame and blending rules.
#[derive(Debug, Clone, Copy)]
struct Shown {
    rect: Rect,
    dispose: bool,
    key: bool,
}

/// A decoded animation frame.
struct Decoded {
    /// RGBA pixels of the frame's rectangle.
    pixels: Vec<u8>,
    lossy: bool,
    has_alpha: bool,
}

/// How a frame is drawn onto the canvas.
#[derive(Debug, Clone, Copy)]
enum Blend {
    Replace,
    /// Alpha-blend, except inside the rectangle the previous frame just cleared: libwebp copies
    /// pixels there, which avoids a rounding error from blending over transparency.
    Over {
        except: Option<Rect>,
    },
}

/// libwebp's key frames: drawn onto a cleared canvas without blending. Blending over a fully
/// transparent canvas would give the same picture, but its integer arithmetic can be one off.
fn is_key_frame(
    frame: &Frame<'_>,
    has_alpha: bool,
    previous: Option<Shown>,
    width: u32,
    height: u32,
) -> bool {
    previous.is_none_or(|prev| {
        ((!has_alpha || !frame.blend) && frame.rect.covers(width, height))
            || (prev.dispose && (prev.rect.covers(width, height) || prev.key))
    })
}

/// Receives a composited canvas (RGBA) and its duration in milliseconds.
type Show<'a> = &'a mut dyn FnMut(&[u8], u32) -> Result<(), ConvertError>;

/// The frames of an animated WebP, still compressed.
struct Animation<'a> {
    width: u32,
    height: u32,
    plays: u32,
    frames: Vec<Frame<'a>>,
}

struct Frame<'a> {
    rect: Rect,
    duration_ms: u32,
    /// Alpha-blend over the canvas; otherwise the rectangle is replaced.
    blend: bool,
    /// Clear the rectangle once the frame has been shown.
    dispose: bool,
    /// The frame's image chunks (`ALPH`, `VP8 `, `VP8L`).
    chunks: Chunks<'a>,
}

impl<'a> Animation<'a> {
    fn parse(data: &'a [u8], width: u32, height: u32) -> Result<Self, ConvertError> {
        let body = data.get(12..).ok_or_else(|| malformed("truncated file"))?;
        let mut plays = 0;
        let mut frames = Vec::new();
        for (name, chunk) in riff_chunks(body)? {
            match &name {
                b"ANIM" => {
                    let loops = chunk.get(4..6).ok_or_else(|| malformed("bad ANIM chunk"))?;
                    plays = u32::from(u16::from_le_bytes([loops[0], loops[1]]));
                }
                b"ANMF" => {
                    let header = chunk.get(..16).ok_or_else(|| malformed("bad ANMF chunk"))?;
                    let rect = Rect {
                        x: u24(&header[0..3]) * 2,
                        y: u24(&header[3..6]) * 2,
                        width: u24(&header[6..9]) + 1,
                        height: u24(&header[9..12]) + 1,
                    };
                    if rect.x + rect.width > width || rect.y + rect.height > height {
                        return Err(malformed("a frame reaches outside the canvas"));
                    }
                    frames.push(Frame {
                        rect,
                        duration_ms: u24(&header[12..15]),
                        blend: header[15] & 0b10 == 0,
                        dispose: header[15] & 0b01 != 0,
                        chunks: riff_chunks(&chunk[16..])?,
                    });
                }
                _ => {}
            }
        }
        if frames.is_empty() {
            return Err(ConvertError::NoFrames);
        }
        Ok(Self {
            width,
            height,
            plays,
            frames,
        })
    }

    /// Decodes and composites every frame the way libwebp does, and calls `show` with each
    /// canvas (RGBA) and its duration. Returns whether any frame is lossy.
    fn composite(&self, show: Show<'_>) -> Result<bool, ConvertError> {
        let (width, height) = (self.width, self.height);
        let mut canvas = vec![0; width as usize * height as usize * 4];
        let mut lossy = false;
        let mut previous: Option<Shown> = None;
        for frame in &self.frames {
            let image = frame.decode()?;
            lossy |= image.lossy;
            let key = is_key_frame(frame, image.has_alpha, previous, width, height);
            // Disposed frames are cleared to transparent; like browsers, the background color
            // hint is ignored.
            let disposed = previous.filter(|p| p.dispose).map(|p| p.rect);
            let blend = if key {
                canvas.fill(0);
                Blend::Replace
            } else {
                if let Some(rect) = disposed {
                    fill(&mut canvas, width, rect, [0; 4]);
                }
                if frame.blend {
                    Blend::Over { except: disposed }
                } else {
                    Blend::Replace
                }
            };
            draw(&mut canvas, width, frame.rect, &image.pixels, blend);
            show(&canvas, frame.duration_ms)?;
            previous = Some(Shown {
                rect: frame.rect,
                dispose: frame.dispose,
                key,
            });
        }
        Ok(lossy)
    }
}

impl Frame<'_> {
    /// Decodes the frame to RGBA by wrapping its chunks in a still WebP file.
    fn decode(&self) -> Result<Decoded, ConvertError> {
        let find = |name: &[u8; 4]| self.chunks.iter().find(|(n, _)| n == name).map(|c| c.1);
        let (file, lossy) = if let Some(lossless) = find(b"VP8L") {
            (riff(&[(*b"VP8L", lossless)]), false)
        } else if let Some(lossy) = find(b"VP8 ") {
            let file = if let Some(alpha) = find(b"ALPH") {
                let mut vp8x = vec![VP8X_ALPHA, 0, 0, 0];
                vp8x.extend(u24_bytes(self.rect.width - 1));
                vp8x.extend(u24_bytes(self.rect.height - 1));
                riff(&[(*b"VP8X", &vp8x), (*b"ALPH", alpha), (*b"VP8 ", lossy)])
            } else {
                riff(&[(*b"VP8 ", lossy)])
            };
            (file, true)
        } else {
            return Err(malformed("a frame has no image data"));
        };
        let mut decoder = WebPDecoder::new(Cursor::new(file))?;
        if decoder.dimensions() != (self.rect.width, self.rect.height) {
            return Err(malformed("a frame's size doesn't match its image data"));
        }
        Ok(Decoded {
            has_alpha: decoder.has_alpha(),
            pixels: decode_image(&mut decoder)?,
            lossy,
        })
    }
}

/// RIFF chunks as `(name, payload)` pairs.
type Chunks<'a> = Vec<([u8; 4], &'a [u8])>;

/// Splits RIFF chunk data into `(name, payload)` pairs. Payloads are padded to even lengths.
fn riff_chunks(mut rest: &[u8]) -> Result<Chunks<'_>, ConvertError> {
    let mut chunks = Vec::new();
    while rest.len() >= 8 {
        let name = [rest[0], rest[1], rest[2], rest[3]];
        let len = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
        let payload = rest
            .get(8..8 + len)
            .ok_or_else(|| malformed("truncated chunk"))?;
        chunks.push((name, payload));
        rest = rest.get(8 + len + len % 2..).unwrap_or_default();
    }
    Ok(chunks)
}

/// A WebP file made of `chunks`.
fn riff(chunks: &[([u8; 4], &[u8])]) -> Vec<u8> {
    let mut body = b"WEBP".to_vec();
    for (name, payload) in chunks {
        body.extend_from_slice(name);
        body.extend_from_slice(
            &u32::try_from(payload.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        body.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            body.push(0);
        }
    }
    let mut file = b"RIFF".to_vec();
    file.extend_from_slice(&u32::try_from(body.len()).unwrap_or(u32::MAX).to_le_bytes());
    file.extend(body);
    file
}

fn u24(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0])
}

fn u24_bytes(value: u32) -> [u8; 3] {
    let [a, b, c, _] = value.to_le_bytes();
    [a, b, c]
}

fn fill(canvas: &mut [u8], canvas_width: u32, rect: Rect, color: [u8; 4]) {
    for y in rect.y..rect.y + rect.height {
        let start = (y as usize * canvas_width as usize + rect.x as usize) * 4;
        for pixel in canvas[start..start + rect.width as usize * 4]
            .as_chunks_mut::<4>()
            .0
        {
            *pixel = color;
        }
    }
}

/// Draws a decoded frame's pixels into `rect` on the canvas.
fn draw(canvas: &mut [u8], canvas_width: u32, rect: Rect, pixels: &[u8], blend: Blend) {
    for (y, source) in (rect.y..).zip(pixels.chunks_exact(rect.width as usize * 4)) {
        let start = (y as usize * canvas_width as usize + rect.x as usize) * 4;
        let target = &mut canvas[start..start + rect.width as usize * 4];
        let Blend::Over { except } = blend else {
            target.copy_from_slice(source);
            continue;
        };
        for ((x, dst), src) in (rect.x..)
            .zip(target.as_chunks_mut::<4>().0)
            .zip(source.as_chunks::<4>().0)
        {
            *dst = if except.is_some_and(|r| r.contains(x, y)) {
                *src
            } else {
                blend_pixel(*src, *dst)
            };
        }
    }
}

/// `src` over `dst`, neither premultiplied, with libwebp's integer arithmetic. Opaque and fully
/// transparent sources are exact.
fn blend_pixel(src: [u8; 4], dst: [u8; 4]) -> [u8; 4] {
    match src[3] {
        255 => src,
        0 => dst,
        src_a => {
            let src_a = u64::from(src_a);
            let dst_a = (u64::from(dst[3]) * (256 - src_a)) >> 8;
            let blend_a = src_a + dst_a;
            let scale = (1 << 24) / blend_a;
            let channel = |i: usize| {
                let mixed = u64::from(src[i]) * src_a + u64::from(dst[i]) * dst_a;
                u8::try_from((mixed * scale) >> 24).unwrap_or(u8::MAX)
            };
            [
                channel(0),
                channel(1),
                channel(2),
                u8::try_from(blend_a).unwrap_or(u8::MAX),
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        convert::{
            testing::{self, WebPFrame, canvas},
            to_png,
        },
        png_info,
    };

    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const CLEAR: [u8; 4] = [0, 0, 0, 0];

    /// A 10 ms frame of an animated test WebP.
    fn frame(rect: (u32, u32, u32, u32), rgba: &[u8], blend: bool, dispose: bool) -> WebPFrame<'_> {
        WebPFrame {
            rect,
            rgba,
            duration_ms: 10,
            blend,
            dispose,
        }
    }

    fn convert_webp(data: &[u8]) -> Converted {
        to_png(data, FileType::WebP, Options::default(), &mut |_| {}).expect("converts")
    }

    #[test]
    fn converts_still_images_exactly_with_their_icc_profile() {
        let rgba = canvas(
            5,
            3,
            [10, 20, 30, 255],
            &[(0, 0, [1, 2, 3, 0]), (4, 2, [200, 100, 50, 128])],
        );
        let data = testing::webp(5, 3, &rgba, Some(b"not really a profile".to_vec()));
        let converted = convert_webp(&data);

        assert_eq!(testing::play(&converted.png).frames[0].0, rgba);
        let source = &converted.source;
        assert_eq!(source.format_name(), "WebP (lossless)");
        assert_eq!((source.frames, source.file_size), (1, data.len() as u64));
        assert_eq!(source.metadata, ["ICC profile"]);
        let info = png_info::parse(&converted.png).expect("png");
        assert!(info.chunks.iter().any(|c| c.name == "iCCP"));
    }

    #[test]
    fn plays_animations_with_blending_and_disposal() {
        let semi_green = [0, 255, 0, 128];
        let data = testing::animated_webp(
            4,
            4,
            3,
            &[
                WebPFrame {
                    rect: (0, 0, 4, 4),
                    rgba: &RED.repeat(16),
                    duration_ms: 100,
                    blend: false,
                    dispose: false,
                },
                WebPFrame {
                    rect: (2, 2, 2, 2),
                    rgba: &BLUE.repeat(4),
                    duration_ms: 50,
                    blend: true,
                    dispose: true,
                },
                WebPFrame {
                    rect: (0, 0, 2, 2),
                    rgba: &semi_green.repeat(4),
                    duration_ms: 70,
                    blend: false,
                    dispose: false,
                },
            ],
        );
        let converted = convert_webp(&data);
        let played = testing::play(&converted.png);

        let square = |color| [(2, 2, color), (3, 2, color), (2, 3, color), (3, 3, color)];
        let corner = [
            (0, 0, semi_green),
            (1, 0, semi_green),
            (0, 1, semi_green),
            (1, 1, semi_green),
        ];
        assert_eq!(
            played.frames,
            [
                (canvas(4, 4, RED, &[]), 100),
                (canvas(4, 4, RED, &square(BLUE)), 50),
                (
                    canvas(4, 4, RED, &[&square(CLEAR)[..], &corner[..]].concat()),
                    70
                ),
            ]
        );
        assert_eq!(played.plays, 3);
        assert_eq!(converted.source.frames, 3);
        assert!(!converted.source.lossy);
    }

    #[test]
    fn blends_like_libwebp_and_keeps_opaque_pixels_exact() {
        assert_eq!(
            blend_pixel([12, 34, 56, 255], [1, 2, 3, 255]),
            [12, 34, 56, 255]
        );
        assert_eq!(blend_pixel([12, 34, 56, 0], [1, 2, 3, 255]), [1, 2, 3, 255]);
        assert_eq!(
            blend_pixel([255, 0, 0, 128], [0, 0, 255, 255]),
            [127, 0, 126, 255]
        );
        assert_eq!(
            blend_pixel([255, 0, 0, 128], [0, 0, 0, 0]),
            [255, 0, 0, 128]
        );
    }

    #[test]
    fn copies_key_frames_and_cleared_areas_instead_of_blending() {
        // Blending this over transparency with libwebp's arithmetic would give 199, 99, 49.
        let translucent = [200, 100, 50, 200];
        let (translucent_4, translucent_8) = (translucent.repeat(4), translucent.repeat(8));
        let (red_8, blue_4) = (RED.repeat(8), BLUE.repeat(4));
        let key = testing::animated_webp(
            2,
            2,
            0,
            &[
                frame((0, 0, 2, 2), &translucent_4, true, false),
                frame((0, 0, 2, 2), &blue_4, false, false),
            ],
        );
        let played = testing::play(&convert_webp(&key).png);
        assert_eq!(played.frames[0].0, canvas(2, 2, translucent, &[]));

        let cleared = testing::animated_webp(
            4,
            2,
            0,
            &[
                frame((0, 0, 4, 2), &red_8, false, false),
                frame((0, 0, 2, 2), &blue_4, false, true),
                frame((0, 0, 4, 2), &translucent_8, true, false),
            ],
        );
        let played = testing::play(&convert_webp(&cleared).png);
        let over_red = blend_pixel(translucent, RED);
        let row = |y| {
            [
                (0, y, translucent),
                (1, y, translucent),
                (2, y, over_red),
                (3, y, over_red),
            ]
        };
        assert_eq!(
            played.frames[2].0,
            canvas(4, 2, RED, &[row(0), row(1)].concat())
        );
    }

    #[test]
    fn reports_broken_files() {
        let result = to_png(
            b"RIFF\x04\x00\x00\x00WEBPVP8 ",
            FileType::WebP,
            Options::default(),
            &mut |_| {},
        );
        assert!(matches!(
            result,
            Err(ConvertError::WebP(_) | ConvertError::WebPDecoding(_))
        ));
    }
}
