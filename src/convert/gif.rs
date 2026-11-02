//! GIF decoding, with frames composited the way browsers play them.

use std::{io::Read, num::NonZeroU64};

use gif::{ColorOutput, DecodeOptions, DisposalMethod, MemoryLimit, Repeat};

use super::{
    ConvertError, Converted, FileType, Progress, Source, Step,
    animation::{MAX_CANVAS_BYTES, Planner},
    encode::Metadata,
};

pub(super) fn convert(data: &[u8], progress: Progress<'_>) -> Result<Converted, ConvertError> {
    // First pass: plan the frames. The canvas size is only known at the first frame.
    let mut planner: Option<Planner> = None;
    let mut read = 0;
    let decoded = composite(data, &mut |width, height, canvas, delay_ms| {
        let plan = if let Some(plan) = &mut planner {
            plan
        } else {
            planner.insert(Planner::new(width, height, true)?)
        };
        plan.push(canvas, delay_ms);
        read += 1;
        progress(Step::Reading {
            done: read,
            total: None,
        });
        Ok(())
    })?;
    let planner = planner.ok_or(ConvertError::NoFrames)?;
    let (width, height) = planner.size();

    // Second pass: write them.
    let mut png = Vec::new();
    let mut writer = planner.into_writer(&mut png, decoded.plays, &decoded.metadata)?;
    if writer.needs_frames() {
        let mut written = 0;
        composite(data, &mut |_, _, canvas, _| {
            writer.push(canvas)?;
            written += 1;
            progress(Step::Writing {
                done: written,
                total: read,
            });
            Ok(())
        })?;
    }
    let notes = writer.finish()?;

    Ok(Converted {
        png,
        source: Source {
            file_type: FileType::Gif,
            lossy: false,
            width,
            height,
            frames: read,
            metadata: decoded.metadata.names(),
            notes,
            file_size: 0,
        },
    })
}

/// Describes a GIF without decoding its pixels: canvas size, frame count, and metadata.
pub(super) fn inspect(reader: impl Read) -> Result<Source, ConvertError> {
    let fail = |err: gif::DecodingError| ConvertError::Gif(err.to_string());
    let mut options = DecodeOptions::new();
    options.skip_frame_decoding(true);
    let mut decoder = options.read_info(reader).map_err(fail)?;
    let screen = (u32::from(decoder.width()), u32::from(decoder.height()));
    let mut size = None;
    let mut frames = 0_u32;
    while let Some(frame) = decoder.read_next_frame().map_err(fail)? {
        size.get_or_insert_with(|| canvas_size(screen, frame));
        frames += 1;
    }
    let (width, height) = size.ok_or(ConvertError::NoFrames)?;
    Ok(Source {
        file_type: FileType::Gif,
        lossy: false,
        width,
        height,
        frames,
        metadata: metadata(&decoder).names(),
        notes: Vec::new(),
        file_size: 0,
    })
}

/// Like Chrome, the canvas grows to fit a first frame that reaches past the declared screen
/// size, instead of cutting it off.
fn canvas_size(screen: (u32, u32), first: &gif::Frame<'_>) -> (u32, u32) {
    (
        screen.0.max(u32::from(first.left) + u32::from(first.width)),
        screen.1.max(u32::from(first.top) + u32::from(first.height)),
    )
}

/// ICC profile and XMP from application extensions; GIF has no EXIF.
fn metadata<R: Read>(decoder: &gif::Decoder<R>) -> Metadata {
    Metadata {
        icc: decoder.icc_profile().map(<[u8]>::to_vec),
        exif: None,
        xmp: decoder
            .xmp_metadata()
            .and_then(|xmp| String::from_utf8(xmp.to_vec()).ok()),
    }
}

/// What is only known once the whole file has been read.
struct Decoded {
    plays: u32,
    metadata: Metadata,
}

/// Receives the canvas size, a composited canvas (RGBA), and its delay in milliseconds.
type Show<'a> = &'a mut dyn FnMut(u32, u32, &[u8], u32) -> Result<(), ConvertError>;

/// Decodes the GIF and shows each composited canvas.
fn composite(data: &[u8], show: Show<'_>) -> Result<Decoded, ConvertError> {
    let fail = |err: gif::DecodingError| ConvertError::Gif(err.to_string());
    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::RGBA);
    options.set_memory_limit(MemoryLimit::Bytes(
        NonZeroU64::new(MAX_CANVAS_BYTES as u64).unwrap_or(NonZeroU64::MIN),
    ));
    let mut decoder = options.read_info(data).map_err(fail)?;
    let screen = (u32::from(decoder.width()), u32::from(decoder.height()));

    let mut canvas: Option<(u32, u32, Vec<u8>)> = None;
    let mut saved = Vec::new();
    while let Some(frame) = decoder.read_next_frame().map_err(fail)? {
        let (width, height, canvas) = if let Some(canvas) = &mut canvas {
            canvas
        } else {
            let (width, height) = canvas_size(screen, frame);
            let bytes = super::check_size(width, height)?;
            // The canvas starts transparent: browsers ignore the GIF background color.
            canvas.insert((width, height, vec![0; bytes]))
        };
        let (width, height) = (*width, *height);
        let area = Area::clipped(frame, width, height);
        if frame.dispose == DisposalMethod::Previous {
            saved.clear();
            area.copy_out(canvas, width, &mut saved);
        }
        area.paint(canvas, width, frame);

        // Delays are kept as stored, even 0 or 10 ms; viewers that slow those down (browsers
        // play them at 100 ms) do the same for APNG.
        show(width, height, canvas, u32::from(frame.delay) * 10)?;

        match frame.dispose {
            DisposalMethod::Background => area.clear(canvas, width),
            DisposalMethod::Previous => area.copy_in(canvas, width, &saved),
            DisposalMethod::Any | DisposalMethod::Keep => {}
        }
    }

    // A NETSCAPE loop count of n repeats the animation n more times; without one it plays once.
    let plays = match decoder.repeat() {
        Repeat::Infinite => 0,
        Repeat::Finite(n) => u32::from(n) + 1,
    };
    Ok(Decoded {
        plays,
        metadata: metadata(&decoder),
    })
}

/// The part of the canvas a frame covers; frames may reach past the canvas edges.
#[derive(Debug, Clone, Copy)]
struct Area {
    left: usize,
    top: usize,
    /// Columns and rows inside the canvas.
    width: usize,
    height: usize,
    /// The frame's full width, for indexing its pixels.
    stride: usize,
}

impl Area {
    fn clipped(frame: &gif::Frame<'_>, canvas_width: u32, canvas_height: u32) -> Self {
        let (left, top) = (usize::from(frame.left), usize::from(frame.top));
        let clip = |start: usize, len: u16, limit: u32| {
            (limit as usize).saturating_sub(start).min(usize::from(len))
        };
        Self {
            left,
            top,
            width: clip(left, frame.width, canvas_width),
            height: clip(top, frame.height, canvas_height),
            stride: usize::from(frame.width),
        }
    }

    /// Byte range of row `y` (relative to the area) in a canvas `canvas_width` wide.
    fn row(&self, y: usize, canvas_width: u32) -> std::ops::Range<usize> {
        let start = ((self.top + y) * canvas_width as usize + self.left) * 4;
        start..start + self.width * 4
    }

    /// Draws the frame's opaque pixels; transparent ones leave the canvas as it was.
    fn paint(&self, canvas: &mut [u8], canvas_width: u32, frame: &gif::Frame<'_>) {
        for y in 0..self.height {
            let source = &frame.buffer[y * self.stride * 4..][..self.width * 4];
            let target = &mut canvas[self.row(y, canvas_width)];
            for (to, from) in target
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(source.as_chunks::<4>().0)
            {
                if from[3] != 0 {
                    *to = *from;
                }
            }
        }
    }

    fn clear(&self, canvas: &mut [u8], canvas_width: u32) {
        for y in 0..self.height {
            canvas[self.row(y, canvas_width)].fill(0);
        }
    }

    fn copy_out(&self, canvas: &[u8], canvas_width: u32, out: &mut Vec<u8>) {
        for y in 0..self.height {
            out.extend_from_slice(&canvas[self.row(y, canvas_width)]);
        }
    }

    fn copy_in(&self, canvas: &mut [u8], canvas_width: u32, saved: &[u8]) {
        if self.width == 0 {
            return;
        }
        for (y, row) in saved.chunks_exact(self.width * 4).enumerate() {
            canvas[self.row(y, canvas_width)].copy_from_slice(row);
        }
    }
}

#[cfg(test)]
mod tests {
    use gif::DisposalMethod::{Background, Keep, Previous};
    use png::BlendOp;

    use super::*;
    use crate::{
        convert::{
            Options,
            testing::{self, canvas, gif_frame},
            to_png,
        },
        png_info,
    };

    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const WHITE: [u8; 4] = [255, 255, 255, 255];
    const CLEAR: [u8; 4] = [0, 0, 0, 0];

    fn convert_gif(data: &[u8]) -> Converted {
        to_png(data, FileType::Gif, Options::default(), &mut |_| {}).expect("converts")
    }

    #[test]
    fn plays_like_the_gif_does_in_a_browser() {
        let data = testing::gif(
            6,
            4,
            Some(Repeat::Infinite),
            &[
                gif_frame((0, 0, 6, 4), &[0; 24], 10, Keep, None),
                // Shown with a zero delay, then cleared to transparent.
                gif_frame((1, 1, 2, 2), &[2; 4], 0, Background, None),
                // Index 3 is transparent; afterward the area is restored.
                gif_frame((3, 1, 2, 2), &[1, 3, 3, 1], 20, Previous, Some(3)),
                gif_frame((0, 0, 1, 1), &[0], 30, Keep, None),
                // Changes nothing: merged into the frame before.
                gif_frame((0, 0, 1, 1), &[0], 40, Keep, None),
            ],
        );
        let converted = convert_gif(&data);
        let played = testing::play(&converted.png);

        let blue = [(1, 1, BLUE), (2, 1, BLUE), (1, 2, BLUE), (2, 2, BLUE)];
        let hole = [(1, 1, CLEAR), (2, 1, CLEAR), (1, 2, CLEAR), (2, 2, CLEAR)];
        let green = [(3, 1, GREEN), (4, 2, GREEN)];
        assert_eq!(
            played.frames,
            [
                (canvas(6, 4, RED, &[]), 100),
                (canvas(6, 4, RED, &blue), 0),
                (canvas(6, 4, RED, &[&hole[..], &green[..]].concat()), 200),
                (canvas(6, 4, RED, &hole), 700),
            ]
        );
        assert_eq!(played.plays, 0);
        // Pixels turning transparent can't be blended over the previous frame.
        assert_eq!(
            played.blend_ops,
            [
                BlendOp::Source,
                BlendOp::Over,
                BlendOp::Source,
                BlendOp::Over
            ]
        );
        let info = png_info::parse(&converted.png).expect("png");
        assert_eq!(info.format_summary(), "Indexed 2-bit (4 colors) + tRNS");
        assert_eq!(converted.source.frames, 5);
        assert_eq!(
            converted.source.notes,
            ["1 frame was identical to the one before and was merged into it"]
        );
    }

    #[test]
    fn single_frames_become_still_pngs() {
        let still = testing::gif(
            2,
            2,
            None,
            &[gif_frame((0, 0, 2, 2), &[0, 1, 2, 3], 0, Keep, None)],
        );
        let converted = convert_gif(&still);
        assert!(!png_info::parse(&converted.png).expect("png").is_apng());
        assert_eq!(
            testing::play(&converted.png).frames[0].0,
            canvas(2, 2, RED, &[(1, 0, GREEN), (0, 1, BLUE), (1, 1, WHITE)])
        );
        assert_eq!(converted.source.notes, Vec::<String>::new());
    }

    #[test]
    fn keeps_the_loop_count() {
        let frames = [
            gif_frame((0, 0, 1, 1), &[0], 10, Keep, None),
            gif_frame((0, 0, 1, 1), &[1], 10, Keep, None),
        ];
        let plays =
            |repeat| testing::play(&convert_gif(&testing::gif(1, 1, repeat, &frames)).png).plays;
        assert_eq!(plays(None), 1);
        assert_eq!(plays(Some(Repeat::Finite(2))), 3);
        assert_eq!(plays(Some(Repeat::Infinite)), 0);
    }

    #[test]
    fn grows_the_canvas_to_fit_an_oversized_first_frame() {
        let data = testing::gif(
            2,
            2,
            None,
            &[
                gif_frame((1, 1, 3, 2), &[0, 1, 2, 3, 3, 3], 10, Keep, None),
                // Later frames are cut off at the canvas edge.
                gif_frame((3, 2, 2, 2), &[1; 4], 10, Keep, None),
            ],
        );
        let converted = convert_gif(&data);
        assert_eq!((converted.source.width, converted.source.height), (4, 3));
        let played = testing::play(&converted.png);
        let first = [
            (1, 1, RED),
            (2, 1, GREEN),
            (3, 1, BLUE),
            (1, 2, WHITE),
            (2, 2, WHITE),
            (3, 2, WHITE),
        ];
        assert_eq!(played.frames[0].0, canvas(4, 3, CLEAR, &first));
        assert_eq!(
            played.frames[1].0,
            canvas(4, 3, CLEAR, &[&first[..], &[(3, 2, GREEN)]].concat())
        );
    }

    #[test]
    fn clips_frames_that_reach_past_the_canvas() {
        let data = testing::gif(
            3,
            3,
            None,
            &[
                gif_frame((0, 0, 3, 3), &[3; 9], 10, Keep, None),
                gif_frame((2, 2, 2, 2), &[0; 4], 10, Background, None),
            ],
        );
        let played = testing::play(&convert_gif(&data).png);
        assert_eq!(played.frames[1].0, canvas(3, 3, WHITE, &[(2, 2, RED)]));
    }

    #[test]
    fn reports_broken_files() {
        let result = to_png(
            b"GIF89a\x10\x00",
            FileType::Gif,
            Options::default(),
            &mut |_| {},
        );
        assert!(matches!(result, Err(ConvertError::Gif(_))));
    }
}
