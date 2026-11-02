//! Turning a sequence of full canvases into APNG frames.
//!
//! Every frame after the first stores only the rectangle that changed. When all changed pixels
//! are opaque, the unchanged ones inside that rectangle can be made transparent and the frame
//! blended over the previous one, which compresses much better.
//!
//! The pixel format can only be chosen once every frame has been seen, and long animations
//! can have gigabytes of changed pixels. So the frames are played twice: a [`Planner`] looks at
//! them and keeps only their rectangles, delays, and colors; then a [`Writer`] writes each one
//! straight into the APNG as it comes by again. Memory stays at a few canvases.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    hash::{BuildHasherDefault, Hasher},
};

use super::{
    ConvertError,
    encode::{self, AnimationWriter, FrameData, Layout, Metadata},
};

/// Largest canvas accepted, as RGBA bytes (e.g. 16384 × 8192 pixels).
pub(super) const MAX_CANVAS_BYTES: usize = 512 * 1024 * 1024;

/// First pass: looks at every canvas without keeping its pixels.
#[derive(Debug)]
pub(super) struct Planner {
    width: u32,
    height: u32,
    clean_transparent: bool,
    previous: Vec<u8>,
    frames: Vec<Planned>,
    /// For every canvas: whether it starts a frame, or repeats the previous one.
    starts: Vec<bool>,
    /// Colors of the changed pixels, before any are made transparent.
    stats: ColorStats,
    /// Pixels inside changed rectangles that could be made transparent for blending.
    maskable: u64,
    /// Pixels of the rectangles of every frame after the first.
    later_pixels: u64,
}

/// What the first pass learned about one output frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Planned {
    rect: Rect,
    delay_ms: u32,
    /// Every changed pixel is opaque, so the frame can be blended over the previous one.
    blendable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl Rect {
    const fn pixels(self) -> u64 {
        self.width as u64 * self.height as u64
    }

    /// Byte ranges of the rectangle's rows in an RGBA canvas `canvas_width` wide.
    fn rows(self, canvas_width: u32) -> impl Iterator<Item = std::ops::Range<usize>> {
        (self.y..self.y + self.height).map(move |y| {
            let start = (y as usize * canvas_width as usize + self.x as usize) * 4;
            start..start + self.width as usize * 4
        })
    }
}

impl Planner {
    /// `clean_transparent` sets the hidden color of fully transparent pixels to black, so they
    /// all share one palette entry.
    pub(super) fn new(
        width: u32,
        height: u32,
        clean_transparent: bool,
    ) -> Result<Self, ConvertError> {
        let bytes = super::check_size(width, height)?;
        Ok(Self {
            width,
            height,
            clean_transparent,
            previous: vec![0; bytes],
            frames: Vec::new(),
            starts: Vec::new(),
            stats: ColorStats::default(),
            maskable: 0,
            later_pixels: 0,
        })
    }

    /// The canvas size, width then height.
    pub(super) const fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Looks at the next canvas: RGBA, `width × height`, shown for `delay_ms`.
    pub(super) fn push(&mut self, canvas: &[u8], delay_ms: u32) {
        debug_assert_eq!(canvas.len(), self.previous.len());
        let canvas = cleaned(canvas, self.clean_transparent);
        let rect = if self.frames.is_empty() {
            Some(Rect {
                x: 0,
                y: 0,
                width: self.width,
                height: self.height,
            })
        } else {
            changed_rect(&self.previous, &canvas, self.width)
        };
        if let Some(rect) = rect {
            let blendable = self.look_at(&canvas, rect);
            self.frames.push(Planned {
                rect,
                delay_ms,
                blendable,
            });
            self.starts.push(true);
        } else {
            // Nothing changed: the previous frame just stays up longer.
            if let Some(last) = self.frames.last_mut() {
                last.delay_ms = last.delay_ms.saturating_add(delay_ms);
            }
            self.starts.push(false);
        }
        self.previous.copy_from_slice(&canvas);
    }

    /// Counts the colors in `rect` and, after the first frame, the pixels that didn't change.
    /// Returns whether the frame can be blended: every changed pixel is opaque.
    fn look_at(&mut self, canvas: &[u8], rect: Rect) -> bool {
        if self.frames.is_empty() {
            self.stats.add(canvas);
            return false;
        }
        let mut unchanged = 0;
        let mut all_opaque = true;
        for row in rect.rows(self.width) {
            self.stats.add(&canvas[row.clone()]);
            for (old, new) in self.previous[row.clone()]
                .as_chunks::<4>()
                .0
                .iter()
                .zip(canvas[row].as_chunks::<4>().0)
            {
                if old == new {
                    unchanged += 1;
                } else {
                    all_opaque &= new[3] == 255;
                }
            }
        }
        self.later_pixels += rect.pixels();
        if all_opaque {
            self.maskable += unchanged;
        }
        all_opaque
    }

    /// Whether unchanged pixels should be made transparent and frames blended, unless that
    /// costs more than it saves.
    fn blends(&self) -> bool {
        self.maskable > 0
            && match self.stats.color_count() {
                // Fine as long as the extra transparent color still fits a palette.
                Some(count) => count + usize::from(!self.stats.contains([0; 4])) <= 256,
                // An opaque image gains an alpha channel; worth it when much can be blanked.
                None => !self.stats.opaque || self.maskable * 4 >= self.later_pixels,
            }
    }

    /// Starts the second pass, which writes into `out` a PNG for one frame or an APNG that
    /// plays `plays` times (0 = forever). A single frame is written right away, from the
    /// canvas kept by this pass.
    ///
    /// # Errors
    ///
    /// Fails when no canvas was seen or the PNG encoder fails.
    pub(super) fn into_writer<'a>(
        mut self,
        out: &'a mut Vec<u8>,
        plays: u32,
        metadata: &Metadata,
    ) -> Result<Writer<'a>, ConvertError> {
        let merged = self.starts.iter().filter(|&&starts| !starts).count();
        let blend = self.blends();
        let png = match self.frames.len() {
            0 => return Err(ConvertError::NoFrames),
            1 => {
                *out = encode::still(self.width, self.height, &self.previous, metadata)?;
                None
            }
            count => {
                if blend {
                    self.stats.add(&[0; 4]);
                }
                let count = u32::try_from(count).map_err(|_| ConvertError::AnimationTooLarge)?;
                let layout = Layout::choose(&self.stats);
                Some(AnimationWriter::new(
                    out,
                    self.width,
                    self.height,
                    layout,
                    count,
                    plays,
                    metadata,
                )?)
            }
        };
        self.previous.fill(0);
        Ok(Writer {
            width: self.width,
            clean_transparent: self.clean_transparent,
            blend,
            frames: self.frames,
            starts: self.starts,
            canvases: 0,
            written: 0,
            merged: u32::try_from(merged).unwrap_or(u32::MAX),
            previous: self.previous,
            pixels: Vec::new(),
            png,
        })
    }
}

/// Second pass: writes each frame as its canvas comes by again.
pub(super) struct Writer<'a> {
    width: u32,
    clean_transparent: bool,
    blend: bool,
    frames: Vec<Planned>,
    starts: Vec<bool>,
    canvases: usize,
    written: usize,
    merged: u32,
    previous: Vec<u8>,
    pixels: Vec<u8>,
    /// `None` when the file was a single image, already written.
    png: Option<AnimationWriter<'a>>,
}

impl Writer<'_> {
    /// False when there is nothing left to write, so the second pass can be skipped.
    pub(super) const fn needs_frames(&self) -> bool {
        self.png.is_some()
    }

    /// Takes the next canvas, the same one the planner saw at this point.
    ///
    /// # Errors
    ///
    /// Fails when the PNG encoder fails.
    pub(super) fn push(&mut self, canvas: &[u8]) -> Result<(), ConvertError> {
        let Some(png) = &mut self.png else {
            return Ok(());
        };
        let canvas = cleaned(canvas, self.clean_transparent);
        let starts = self.starts.get(self.canvases).copied().unwrap_or(false);
        self.canvases += 1;
        if starts {
            let frame = *self
                .frames
                .get(self.written)
                .ok_or(ConvertError::Internal("the second pass saw more frames"))?;
            self.written += 1;
            let blend = self.blend && frame.blendable;
            self.pixels.clear();
            for row in frame.rect.rows(self.width) {
                let old = self.previous[row.clone()].as_chunks::<4>().0;
                for (old, new) in old.iter().zip(canvas[row].as_chunks::<4>().0) {
                    self.pixels
                        .extend_from_slice(if blend && old == new { &[0; 4] } else { new });
                }
            }
            png.write(&FrameData {
                x: frame.rect.x,
                y: frame.rect.y,
                width: frame.rect.width,
                height: frame.rect.height,
                delay_ms: frame.delay_ms,
                blend_over: blend,
                pixels: &self.pixels,
            })?;
        }
        self.previous.copy_from_slice(&canvas);
        Ok(())
    }

    /// Finishes the file. Returns notes for the details panel.
    ///
    /// # Errors
    ///
    /// Fails when the second pass didn't see every frame, or the PNG encoder fails.
    pub(super) fn finish(self) -> Result<Vec<String>, ConvertError> {
        if let Some(png) = self.png {
            if self.written != self.frames.len() {
                return Err(ConvertError::Internal("the second pass saw fewer frames"));
            }
            png.finish()?;
        }
        Ok(match self.merged {
            0 => Vec::new(),
            1 => vec!["1 frame was identical to the one before and was merged into it".to_owned()],
            n => vec![format!(
                "{} frames were identical to the one before and were merged into it",
                crate::format::grouped(u64::from(n))
            )],
        })
    }
}

/// The canvas with the hidden color of fully transparent pixels set to black, if asked.
fn cleaned(canvas: &[u8], clean: bool) -> Cow<'_, [u8]> {
    let pixels = canvas.as_chunks::<4>().0;
    if !clean || !pixels.iter().any(|p| p[3] == 0 && *p != [0; 4]) {
        return Cow::Borrowed(canvas);
    }
    let mut owned = canvas.to_vec();
    for pixel in owned.as_chunks_mut::<4>().0 {
        if pixel[3] == 0 {
            *pixel = [0; 4];
        }
    }
    Cow::Owned(owned)
}

/// The smallest rectangle containing every pixel that differs, or `None` if none does.
fn changed_rect(previous: &[u8], current: &[u8], width: u32) -> Option<Rect> {
    let row_bytes = width as usize * 4;
    let rows = || {
        previous
            .chunks_exact(row_bytes)
            .zip(current.chunks_exact(row_bytes))
    };
    let top = rows().position(|(a, b)| a != b)?;
    let bottom = rows().rposition(|(a, b)| a != b)?;
    let (mut left, mut right) = (usize::MAX, 0);
    for (a, b) in rows().skip(top).take(bottom - top + 1) {
        let pixels = || a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0);
        if let Some(first) = pixels().position(|(p, q)| p != q) {
            left = left.min(first);
            right = right.max(pixels().rposition(|(p, q)| p != q).unwrap_or(first));
        }
    }
    let to_u32 = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    Some(Rect {
        x: to_u32(left),
        y: to_u32(top),
        width: to_u32(right - left + 1),
        height: to_u32(bottom - top + 1),
    })
}

/// A fast hash for RGBA colors packed into a `u32`; the default hasher is several times
/// slower, which shows when every pixel of a long animation is looked up.
#[derive(Debug, Default)]
pub(super) struct ColorHasher(u64);

impl Hasher for ColorHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.write_u32(u32::from(byte));
        }
    }

    fn write_u32(&mut self, n: u32) {
        let mixed = (self.0 ^ u64::from(n)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        // Fold the well-mixed high bits down: tables pick buckets from the low bits.
        self.0 = mixed ^ (mixed >> 32);
    }
}

pub(super) type ColorMap<V> = HashMap<u32, V, BuildHasherDefault<ColorHasher>>;
type ColorSet = HashSet<u32, BuildHasherDefault<ColorHasher>>;

/// What the pixels of an image need: at most 256 colors (listed), alpha, color.
#[derive(Debug, Clone)]
pub(super) struct ColorStats {
    /// Distinct colors as little-endian `u32`s, until there are more than 256.
    colors: ColorSet,
    too_many: bool,
    pub opaque: bool,
    pub gray: bool,
}

impl Default for ColorStats {
    fn default() -> Self {
        Self {
            colors: ColorSet::default(),
            too_many: false,
            opaque: true,
            gray: true,
        }
    }
}

impl ColorStats {
    #[cfg(test)]
    pub(super) fn of<'a>(buffers: impl IntoIterator<Item = &'a [u8]>) -> Self {
        let mut stats = Self::default();
        for buffer in buffers {
            stats.add(buffer);
        }
        stats
    }

    /// Counts the colors of RGBA pixels.
    pub(super) fn add(&mut self, rgba: &[u8]) {
        let mut last = None;
        for &pixel in rgba.as_chunks::<4>().0 {
            // Runs of one color are common; skip the set lookup for them.
            if last == Some(pixel) {
                continue;
            }
            last = Some(pixel);
            self.opaque &= pixel[3] == 255;
            self.gray &= pixel[0] == pixel[1] && pixel[1] == pixel[2];
            if !self.too_many {
                self.colors.insert(u32::from_le_bytes(pixel));
                self.too_many = self.colors.len() > 256;
            }
        }
    }

    /// The distinct colors, or `None` when there are more than 256.
    pub(super) fn colors(&self) -> Option<Vec<[u8; 4]>> {
        (!self.too_many).then(|| self.colors.iter().map(|c| c.to_le_bytes()).collect())
    }

    fn color_count(&self) -> Option<usize> {
        (!self.too_many).then_some(self.colors.len())
    }

    fn contains(&self, color: [u8; 4]) -> bool {
        self.colors.contains(&u32::from_le_bytes(color))
    }
}

#[cfg(test)]
mod tests {
    use png::BlendOp;

    use super::*;
    use crate::convert::testing::{self, canvas};

    const W: u32 = 4;
    const H: u32 = 3;
    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const CLEAR: [u8; 4] = [0, 0, 0, 0];

    fn planner(canvases: &[(Vec<u8>, u32)], clean: bool) -> Planner {
        let mut planner = Planner::new(W, H, clean).expect("size");
        for (canvas, delay) in canvases {
            planner.push(canvas, *delay);
        }
        planner
    }

    /// Both passes, as the converters run them. Returns the file and the notes.
    fn convert(canvases: &[(Vec<u8>, u32)], clean: bool) -> (Vec<u8>, Vec<String>) {
        let mut png = Vec::new();
        let mut writer = planner(canvases, clean)
            .into_writer(&mut png, 0, &Metadata::default())
            .expect("writer");
        if writer.needs_frames() {
            for (canvas, _) in canvases {
                writer.push(canvas).expect("push");
            }
        }
        let notes = writer.finish().expect("finish");
        (png, notes)
    }

    #[test]
    fn finds_the_changed_rectangle() {
        let before = canvas(W, H, RED, &[]);
        assert_eq!(changed_rect(&before, &before, W), None);
        let after = canvas(W, H, RED, &[(1, 1, BLUE), (2, 2, BLUE)]);
        assert_eq!(
            changed_rect(&before, &after, W),
            Some(Rect {
                x: 1,
                y: 1,
                width: 2,
                height: 2
            })
        );
    }

    #[test]
    fn stores_only_changes_and_merges_repeated_frames() {
        let first = canvas(W, H, RED, &[]);
        let second = canvas(W, H, RED, &[(3, 0, BLUE)]);
        let canvases = [(first, 100), (second.clone(), 100), (second.clone(), 50)];

        let planned = planner(&canvases, true);
        assert_eq!(planned.frames.len(), 2);
        assert_eq!(
            planned.frames[1].rect,
            Rect {
                x: 3,
                y: 0,
                width: 1,
                height: 1
            }
        );
        let (png, notes) = convert(&canvases, true);
        let played = testing::play(&png);
        assert_eq!(played.frames[1], (second, 150));
        assert_eq!(
            notes,
            ["1 frame was identical to the one before and was merged into it"]
        );
    }

    #[test]
    fn blends_over_only_when_no_pixel_turns_transparent() {
        let first = canvas(W, H, RED, &[]);
        let opaque_change = canvas(W, H, RED, &[(0, 0, BLUE), (2, 0, BLUE)]);
        let cleared = canvas(W, H, RED, &[(0, 0, CLEAR), (2, 0, BLUE)]);
        let canvases = [
            (first, 100),
            (opaque_change.clone(), 100),
            (cleared.clone(), 100),
        ];
        let (png, _) = convert(&canvases, true);
        let played = testing::play(&png);
        assert_eq!(
            played.blend_ops,
            [BlendOp::Source, BlendOp::Over, BlendOp::Source]
        );
        assert_eq!(played.frames[1].0, opaque_change);
        assert_eq!(played.frames[2].0, cleared);
    }

    #[test]
    fn keeps_unchanged_pixels_when_blanking_them_would_not_pay() {
        // 300 opaque colors, then a frame that changes all but one pixel of its rectangle:
        // blanking that one pixel would cost an alpha channel for the whole animation.
        let mut planner = Planner::new(300, 1, true).expect("size");
        let first: Vec<u8> = (0..300_u32)
            .flat_map(|i| {
                let [low, high, ..] = i.to_le_bytes();
                [low, high, 9, 255]
            })
            .collect();
        let mut second = first.clone();
        for (i, pixel) in second.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            if i != 1 {
                pixel[2] = 10;
            }
        }
        planner.push(&first, 10);
        planner.push(&second, 10);
        assert!(!planner.blends());
    }

    #[test]
    fn cleans_the_color_of_transparent_pixels_when_asked() {
        let hidden = canvas(W, H, [9, 9, 9, 0], &[]);
        let (png, _) = convert(&[(hidden.clone(), 100)], true);
        assert_eq!(testing::play(&png).frames[0].0, canvas(W, H, CLEAR, &[]));
        let (png, _) = convert(&[(hidden.clone(), 100)], false);
        assert_eq!(testing::play(&png).frames[0].0, hidden);
    }

    #[test]
    fn counts_colors_up_to_a_palette() {
        let gray: Vec<u8> = (0..=255).flat_map(|v| [v, v, v, 255]).collect();
        let stats = ColorStats::of([gray.as_slice()]);
        assert_eq!(stats.colors().map(|c| c.len()), Some(256));
        assert!(stats.opaque && stats.gray);

        let more = ColorStats::of([gray.as_slice(), &[1, 2, 3, 0]]);
        assert_eq!(more.colors(), None);
        assert!(!more.opaque && !more.gray);
    }

    #[test]
    fn color_hashes_spread_over_the_low_bits() {
        // Colors that differ only in green or blue must not all land in one bucket.
        let buckets: HashSet<u64> = (0..64_u8)
            .map(|i| {
                let mut hasher = ColorHasher::default();
                hasher.write_u32(u32::from_le_bytes([0, i * 4, i * 3, 255]));
                hasher.finish() & 63
            })
            .collect();
        assert!(buckets.len() > 32, "{} of 64 buckets used", buckets.len());
    }
}
