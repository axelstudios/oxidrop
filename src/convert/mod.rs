//! Converting GIF and WebP files to PNG/APNG, so oxipng can optimize them.
//!
//! The source is decoded to full RGBA frames, composited the way browsers show them, and
//! written as a quickly compressed PNG (one frame) or APNG (several). oxipng then does the
//! real work. oxipng never reduces the colors of an APNG, so for animations the pixel format
//! is chosen here: a palette when all frames together use at most 256 colors.

mod animation;
mod encode;
mod gif;
#[cfg(test)]
pub(crate) mod testing;
mod webp;

use std::fmt;

use crate::png_info;

/// Image formats Oxidrop reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    /// PNG or APNG: optimized as it is.
    Png,
    Gif,
    WebP,
}

impl FileType {
    /// Bytes needed from the start of a file to tell its type.
    pub const SNIFF_LEN: usize = 12;

    /// Extensions (lowercase) searched for in dropped folders and offered in file dialogs.
    pub const EXTENSIONS: [&'static str; 4] = ["png", "apng", "gif", "webp"];

    /// Recognizes a file from its first [`Self::SNIFF_LEN`] bytes.
    #[must_use]
    pub fn detect(head: &[u8]) -> Option<Self> {
        if png_info::is_png(head) {
            Some(Self::Png)
        } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else if head.get(..4) == Some(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
            Some(Self::WebP)
        } else {
            None
        }
    }

    /// True for formats that are converted to PNG/APNG first.
    #[must_use]
    pub const fn is_converted(self) -> bool {
        !matches!(self, Self::Png)
    }
}

/// What a converted file was, for the details panel and the change summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub file_type: FileType,
    /// WebP only: at least one frame uses lossy compression.
    pub lossy: bool,
    pub width: u32,
    pub height: u32,
    /// Frames in the source file; 1 for a still image.
    pub frames: u32,
    /// Metadata carried over into the PNG, e.g. `ICC profile`. oxipng may strip it again,
    /// depending on the metadata setting.
    pub metadata: Vec<&'static str>,
    /// What the conversion changed that isn't visible in the result, e.g. merged frames.
    pub notes: Vec<String>,
    pub file_size: u64,
}

impl Source {
    /// Short format name, e.g. `GIF` or `WebP (lossy)`.
    #[must_use]
    pub const fn format_name(&self) -> &'static str {
        match (self.file_type, self.lossy) {
            (FileType::Png, _) => "PNG",
            (FileType::Gif, _) => "GIF",
            (FileType::WebP, true) => "WebP (lossy)",
            (FileType::WebP, false) => "WebP (lossless)",
        }
    }

    /// True for animations with more than one frame.
    #[must_use]
    pub const fn is_animated(&self) -> bool {
        self.frames > 1
    }
}

impl fmt::Display for Source {
    /// e.g. `GIF · 24 frames` or `WebP (lossy)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.format_name())?;
        if self.is_animated() {
            write!(
                f,
                " · {} frames",
                crate::format::grouped(u64::from(self.frames))
            )?;
        }
        Ok(())
    }
}

/// A converted file: PNG/APNG bytes for oxipng, and what they came from.
#[derive(Debug, Clone)]
pub struct Converted {
    pub png: Vec<u8>,
    pub source: Source,
}

/// Why a file could not be converted.
#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    #[error("could not decode the GIF: {0}")]
    Gif(String),
    #[error("could not decode the WebP: {0}")]
    WebP(String),
    #[error("could not decode the WebP: {0}")]
    WebPDecoding(#[from] image_webp::DecodingError),
    #[error(
        "{} × {} is too large to convert",
        crate::format::grouped(u64::from(*.width)),
        crate::format::grouped(u64::from(*.height))
    )]
    TooLarge { width: u32, height: u32 },
    #[error("the animation is too large to convert")]
    AnimationTooLarge,
    #[error("the file has no image data")]
    NoFrames,
    #[error("could not write the PNG: {0}")]
    Encode(#[from] png::EncodingError),
    #[error("internal error: {0}")]
    Internal(&'static str),
}

/// Conversion settings.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Fully transparent pixels in WebP animations may lose their hidden color, as with
    /// oxipng's "optimize alpha". GIF transparency has no color, so it is always cleaned.
    pub clean_transparent: bool,
}

/// How far a conversion is. Animations are decoded twice: once to plan the output, then again
/// to write it, so that only a few frames are ever in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Frames read in the first pass, and how many there are when the format says (WebP
    /// does, GIF doesn't).
    Reading { done: u32, total: Option<u32> },
    /// Frames written in the second pass.
    Writing { done: u32, total: u32 },
}

/// Called as a conversion moves on.
pub type Progress<'a> = &'a mut dyn FnMut(Step);

/// Decodes `data` (a GIF or WebP file) and writes it as PNG or APNG.
///
/// # Errors
///
/// Fails when the file can't be decoded, is too large, or `file_type` is PNG.
pub fn to_png(
    data: &[u8],
    file_type: FileType,
    options: Options,
    progress: Progress<'_>,
) -> Result<Converted, ConvertError> {
    progress(Step::Reading {
        done: 0,
        total: None,
    });
    let mut converted = match file_type {
        FileType::Gif => gif::convert(data, progress)?,
        FileType::WebP => webp::convert(data, options, progress)?,
        FileType::Png => return Err(ConvertError::NoFrames),
    };
    converted.source.file_size = data.len() as u64;
    Ok(converted)
}

/// Describes a GIF or WebP file without decoding its pixels, as [`to_png`] would describe its
/// source.
///
/// # Errors
///
/// Fails when the file can't be read or parsed, or `file_type` is PNG.
pub fn inspect(file_type: FileType, file: std::fs::File) -> Result<Source, ConvertError> {
    let file_size = file.metadata().map_or(0, |m| m.len());
    let reader = std::io::BufReader::new(file);
    let mut source = match file_type {
        FileType::Gif => gif::inspect(reader)?,
        FileType::WebP => webp::inspect(reader)?,
        FileType::Png => return Err(ConvertError::NoFrames),
    };
    source.file_size = file_size;
    Ok(source)
}

/// Fails for canvases whose RGBA buffer would take more than [`animation::MAX_CANVAS_BYTES`].
fn check_size(width: u32, height: u32) -> Result<usize, ConvertError> {
    let bytes = u64::from(width) * u64::from(height) * 4;
    if width == 0 || height == 0 {
        return Err(ConvertError::NoFrames);
    }
    usize::try_from(bytes)
        .ok()
        .filter(|&b| b <= animation::MAX_CANVAS_BYTES)
        .ok_or(ConvertError::TooLarge { width, height })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_file_types_from_their_first_bytes() {
        assert_eq!(FileType::detect(&png_info::SIGNATURE), Some(FileType::Png));
        assert_eq!(FileType::detect(b"GIF89a\x01\x00"), Some(FileType::Gif));
        assert_eq!(FileType::detect(b"GIF87a"), Some(FileType::Gif));
        assert_eq!(
            FileType::detect(b"RIFF\x24\x00\x00\x00WEBPVP8L"),
            Some(FileType::WebP)
        );
        assert_eq!(FileType::detect(b"RIFF\x24\x00\x00\x00WAVE"), None);
        assert_eq!(FileType::detect(b"\xff\xd8\xff\xe0"), None);
        assert_eq!(FileType::detect(b""), None);
    }

    #[test]
    fn describes_sources() {
        let mut source = Source {
            file_type: FileType::WebP,
            lossy: true,
            width: 10,
            height: 10,
            frames: 1,
            metadata: Vec::new(),
            notes: Vec::new(),
            file_size: 100,
        };
        assert_eq!(source.to_string(), "WebP (lossy)");
        source.file_type = FileType::Gif;
        source.frames = 24;
        assert_eq!(source.to_string(), "GIF · 24 frames");
    }

    #[test]
    fn refuses_huge_canvases() {
        assert!(check_size(1000, 1000).is_ok());
        let too_large = check_size(65_535, 65_535).expect_err("too large");
        assert_eq!(
            too_large.to_string(),
            "65,535 × 65,535 is too large to convert"
        );
        assert!(matches!(check_size(0, 10), Err(ConvertError::NoFrames)));
    }
}
