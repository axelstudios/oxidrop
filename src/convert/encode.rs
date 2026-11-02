//! Writing decoded pixels as PNG or APNG with the `png` crate.
//!
//! Compression here is as fast as it gets, with one fixed row filter: oxipng refilters and
//! recompresses everything afterward. (Trying every filter on every row, the `png` crate's
//! usual choice, took 40% longer on long animations.)

use std::borrow::Cow;

use png::{BitDepth, BlendOp, ColorType, Compression, DisposeOp, text_metadata::ITXtChunk};

use super::{
    ConvertError,
    animation::{ColorMap, ColorStats},
};

/// Keyword of the iTXt chunk that holds XMP metadata.
const XMP_KEYWORD: &str = "XML:com.adobe.xmp";

/// Metadata carried over from the source file.
#[derive(Debug, Clone, Default)]
pub(super) struct Metadata {
    pub icc: Option<Vec<u8>>,
    /// EXIF data starting with the TIFF header, as PNG's eXIf chunk expects.
    pub exif: Option<Vec<u8>>,
    pub xmp: Option<String>,
}

impl Metadata {
    /// Names of the metadata present, for the details panel.
    pub(super) fn names(&self) -> Vec<&'static str> {
        [
            (self.icc.is_some(), "ICC profile"),
            (self.exif.is_some(), "EXIF"),
            (self.xmp.is_some(), "XMP"),
        ]
        .into_iter()
        .filter_map(|(present, name)| present.then_some(name))
        .collect()
    }

    /// Drops the "Exif\0\0" prefix some files put before the TIFF header.
    pub(super) fn exif_from(data: Vec<u8>) -> Vec<u8> {
        match data.strip_prefix(b"Exif\0\0") {
            Some(rest) => rest.to_vec(),
            None => data,
        }
    }
}

/// One APNG frame: RGBA pixels of a rectangle of the canvas.
#[derive(Debug, Clone)]
pub(super) struct FrameData<'a> {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub delay_ms: u32,
    /// Blend over the previous frame instead of replacing the rectangle.
    pub blend_over: bool,
    pub pixels: &'a [u8],
}

/// How pixels are stored in the PNG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Layout {
    /// Palette with non-opaque entries first, so the tRNS chunk stays short.
    Indexed {
        palette: Vec<[u8; 4]>,
        depth: u8,
    },
    Gray,
    GrayAlpha,
    Rgb,
    Rgba,
}

impl Layout {
    /// The smallest layout that holds every color exactly.
    pub(super) fn choose(stats: &ColorStats) -> Self {
        if let Some(mut palette) = stats.colors() {
            palette.sort_by_key(|&[r, g, b, a]| {
                let luma = 299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b);
                (a, luma, [r, g, b])
            });
            // Sorting by alpha puts transparent entries first; opaque ones follow by luma.
            let depth = match palette.len() {
                0..=2 => 1,
                3..=4 => 2,
                5..=16 => 4,
                _ => 8,
            };
            return Self::Indexed { palette, depth };
        }
        match (stats.gray, stats.opaque) {
            (true, true) => Self::Gray,
            (true, false) => Self::GrayAlpha,
            (false, true) => Self::Rgb,
            (false, false) => Self::Rgba,
        }
    }

    const fn color_type(&self) -> ColorType {
        match self {
            Self::Indexed { .. } => ColorType::Indexed,
            Self::Gray => ColorType::Grayscale,
            Self::GrayAlpha => ColorType::GrayscaleAlpha,
            Self::Rgb => ColorType::Rgb,
            Self::Rgba => ColorType::Rgba,
        }
    }

    const fn bit_depth(&self) -> BitDepth {
        match self {
            Self::Indexed { depth: 1, .. } => BitDepth::One,
            Self::Indexed { depth: 2, .. } => BitDepth::Two,
            Self::Indexed { depth: 4, .. } => BitDepth::Four,
            _ => BitDepth::Eight,
        }
    }

    /// Converts RGBA pixels of rows `width` wide to this layout's raw row data.
    fn pack(&self, rgba: &[u8], width: u32) -> Vec<u8> {
        let pixels = rgba.as_chunks::<4>().0.iter();
        match self {
            Self::Rgba => rgba.to_vec(),
            Self::Rgb => pixels.flat_map(|p| [p[0], p[1], p[2]]).collect(),
            Self::GrayAlpha => pixels.flat_map(|p| [p[0], p[3]]).collect(),
            Self::Gray => pixels.map(|p| p[0]).collect(),
            Self::Indexed { palette, depth } => {
                let index: ColorMap<u8> = palette
                    .iter()
                    .enumerate()
                    .map(|(i, &color)| {
                        (
                            u32::from_le_bytes(color),
                            u8::try_from(i).unwrap_or(u8::MAX),
                        )
                    })
                    .collect();
                // Runs of one color are common; look each run up once.
                let mut last = None;
                let indices = pixels.map(|&pixel| {
                    let color = u32::from_le_bytes(pixel);
                    match last {
                        Some((previous, found)) if previous == color => found,
                        _ => {
                            let found = index.get(&color).copied().unwrap_or(0);
                            last = Some((color, found));
                            found
                        }
                    }
                });
                pack_bits(indices, width as usize, *depth)
            }
        }
    }
}

/// Packs palette indices into rows of `depth` bits each, most significant bits first, with
/// every row starting on a new byte.
fn pack_bits(indices: impl Iterator<Item = u8>, width: usize, depth: u8) -> Vec<u8> {
    if depth == 8 {
        return indices.collect();
    }
    let per_byte = usize::from(8 / depth);
    let mut out = Vec::new();
    for (i, index) in indices.enumerate() {
        let column = i % width;
        if column.is_multiple_of(per_byte) {
            out.push(0);
        }
        let shift = 8 - depth * (u8::try_from(column % per_byte).unwrap_or(0) + 1);
        if let Some(byte) = out.last_mut() {
            *byte |= index << shift;
        }
    }
    out
}

/// A PNG `fcTL` delay for `ms` milliseconds, in the finest unit that fits.
fn delay_fraction(ms: u32) -> (u16, u16) {
    if let Ok(ms) = u16::try_from(ms) {
        (ms, 1000)
    } else if let Ok(centiseconds) = u16::try_from(ms / 10) {
        (centiseconds, 100)
    } else {
        (u16::try_from(ms / 1000).unwrap_or(u16::MAX), 1)
    }
}

fn header<'a>(width: u32, height: u32, layout: &Layout, metadata: &'a Metadata) -> png::Info<'a> {
    let mut info = png::Info::with_size(width, height);
    info.color_type = layout.color_type();
    info.bit_depth = layout.bit_depth();
    if let Layout::Indexed { palette, .. } = layout {
        info.palette = Some(Cow::Owned(
            palette.iter().flat_map(|c| [c[0], c[1], c[2]]).collect(),
        ));
        let opaque_from = palette
            .iter()
            .position(|c| c[3] == 255)
            .unwrap_or(palette.len());
        if opaque_from > 0 {
            info.trns = Some(Cow::Owned(
                palette[..opaque_from].iter().map(|c| c[3]).collect(),
            ));
        }
    }
    info.icc_profile = metadata.icc.as_deref().map(Cow::Borrowed);
    info.exif_metadata = metadata.exif.as_deref().map(Cow::Borrowed);
    if let Some(xmp) = &metadata.xmp {
        info.utf8_text
            .push(ITXtChunk::new(XMP_KEYWORD.to_owned(), xmp.clone()));
    }
    info
}

/// Writes a single image from RGBA pixels, as RGB when every pixel is opaque. oxipng picks
/// the best pixel format for still images itself.
pub(super) fn still(
    width: u32,
    height: u32,
    rgba: &[u8],
    metadata: &Metadata,
) -> Result<Vec<u8>, ConvertError> {
    let layout = if rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255) {
        Layout::Rgb
    } else {
        Layout::Rgba
    };
    let mut out = Vec::new();
    let mut encoder = png::Encoder::with_info(&mut out, header(width, height, &layout, metadata))?;
    encoder.set_compression(Compression::Fastest);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&layout.pack(rgba, width))?;
    writer.finish()?;
    Ok(out)
}

/// Writes an APNG frame by frame, so only one frame needs to be in memory.
pub(super) struct AnimationWriter<'a> {
    png: png::Writer<&'a mut Vec<u8>>,
    layout: Layout,
}

impl<'a> AnimationWriter<'a> {
    /// Starts an APNG of `count` frames in `out`; the first frame must cover the whole canvas.
    pub(super) fn new(
        out: &'a mut Vec<u8>,
        width: u32,
        height: u32,
        layout: Layout,
        count: u32,
        plays: u32,
        metadata: &Metadata,
    ) -> Result<Self, ConvertError> {
        out.clear();
        let mut encoder = png::Encoder::with_info(out, header(width, height, &layout, metadata))?;
        encoder.set_compression(Compression::Fastest);
        encoder.set_animated(count, plays)?;
        Ok(Self {
            png: encoder.write_header()?,
            layout,
        })
    }

    pub(super) fn write(&mut self, frame: &FrameData<'_>) -> Result<(), ConvertError> {
        let (num, den) = delay_fraction(frame.delay_ms);
        let png = &mut self.png;
        // Position and size are checked against each other: move to the corner first so the
        // new size always fits, then to the frame's place.
        png.reset_frame_position()?;
        png.set_frame_dimension(frame.width, frame.height)?;
        png.set_frame_position(frame.x, frame.y)?;
        png.set_frame_delay(num, den)?;
        png.set_blend_op(if frame.blend_over {
            BlendOp::Over
        } else {
            BlendOp::Source
        })?;
        png.set_dispose_op(DisposeOp::None)?;
        png.write_image_data(&self.layout.pack(frame.pixels, frame.width))?;
        Ok(())
    }

    pub(super) fn finish(self) -> Result<(), ConvertError> {
        self.png.finish()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_low_bit_depths_row_by_row() {
        // Two rows of three 2-bit indices: 1 2 3 | 3 0 1.
        let packed = pack_bits([1, 2, 3, 3, 0, 1].into_iter(), 3, 2);
        assert_eq!(packed, [0b0110_1100, 0b1100_0100]);
        assert_eq!(pack_bits([1, 0, 1].into_iter(), 3, 1), [0b1010_0000]);
        assert_eq!(pack_bits([7, 9].into_iter(), 2, 8), [7, 9]);
    }

    #[test]
    fn picks_the_smallest_layout() {
        let three = [[255, 255, 255, 255], [0, 0, 0, 0], [0, 0, 0, 255]].concat();
        let Layout::Indexed { palette, depth } = Layout::choose(&ColorStats::of([&three[..]]))
        else {
            panic!("three colors fit a palette");
        };
        assert_eq!(depth, 2);
        assert_eq!(
            palette,
            [[0, 0, 0, 0], [0, 0, 0, 255], [255, 255, 255, 255]]
        );

        // More than 256 colors.
        let many = |alpha: u8, gray: bool| -> Vec<u8> {
            (0..300_u32)
                .flat_map(|i| {
                    let [v, high, ..] = i.to_le_bytes();
                    let other = if gray { v } else { high + 1 };
                    [v, v, other, if i == 0 { alpha } else { 255 }]
                })
                .collect()
        };
        let layout = |pixels: Vec<u8>| Layout::choose(&ColorStats::of([&pixels[..]]));
        assert_eq!(layout(many(255, false)), Layout::Rgb);
        assert_eq!(layout(many(0, false)), Layout::Rgba);
        let gray_alpha: Vec<u8> = (0..300_u32)
            .flat_map(|i| {
                let v = i.to_le_bytes()[0];
                [v, v, v, if i < 256 { 255 } else { 128 }]
            })
            .collect();
        assert_eq!(layout(gray_alpha), Layout::GrayAlpha);
    }

    #[test]
    fn delays_use_the_finest_unit_that_fits() {
        assert_eq!(delay_fraction(40), (40, 1000));
        assert_eq!(delay_fraction(65_535), (65_535, 1000));
        assert_eq!(delay_fraction(100_000), (10_000, 100));
        assert_eq!(delay_fraction(u32::MAX), (u16::MAX, 1));
    }

    #[test]
    fn strips_the_exif_prefix() {
        assert_eq!(Metadata::exif_from(b"Exif\0\0II*\0".to_vec()), b"II*\0");
        assert_eq!(Metadata::exif_from(b"MM\0*".to_vec()), b"MM\0*");
    }
}
