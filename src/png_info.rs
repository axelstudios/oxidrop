//! Lightweight PNG/APNG inspection.
//!
//! Only chunk headers and a few small chunks are read; pixel data is never decoded. That is
//! enough to describe what oxipng changed (color type, bit depth, palette, interlacing,
//! and metadata chunks) without a full image decoder.

use std::{
    fmt,
    io::{Read, Seek, SeekFrom},
};

/// The 8-byte signature every PNG file starts with.
pub const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// Chunks that describe image structure rather than metadata. Changes to these are reported
/// through the format summary, so they are left out of the added/removed chunk lists.
const STRUCTURAL_CHUNKS: [&str; 8] = [
    "IHDR", "PLTE", "tRNS", "IDAT", "IEND", "acTL", "fcTL", "fdAT",
];

/// PNG color type, as stored in the IHDR chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorType {
    Grayscale,
    Rgb,
    Indexed,
    GrayscaleAlpha,
    Rgba,
}

impl ColorType {
    /// Decodes the color type code from an IHDR chunk.
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Grayscale),
            2 => Some(Self::Rgb),
            3 => Some(Self::Indexed),
            4 => Some(Self::GrayscaleAlpha),
            6 => Some(Self::Rgba),
            _ => None,
        }
    }

    /// Number of samples per pixel.
    #[must_use]
    pub const fn channels(self) -> u8 {
        match self {
            Self::Grayscale | Self::Indexed => 1,
            Self::GrayscaleAlpha => 2,
            Self::Rgb => 3,
            Self::Rgba => 4,
        }
    }

    /// Short human-readable name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Grayscale => "Grayscale",
            Self::Rgb => "RGB",
            Self::Indexed => "Indexed",
            Self::GrayscaleAlpha => "Grayscale+Alpha",
            Self::Rgba => "RGB+Alpha",
        }
    }

    const fn allows_bit_depth(self, depth: u8) -> bool {
        match self {
            Self::Grayscale => matches!(depth, 1 | 2 | 4 | 8 | 16),
            Self::Indexed => matches!(depth, 1 | 2 | 4 | 8),
            Self::Rgb | Self::GrayscaleAlpha | Self::Rgba => matches!(depth, 8 | 16),
        }
    }
}

/// A chunk type and how many times it occurs in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkCount {
    pub name: String,
    pub count: usize,
}

impl fmt::Display for ChunkCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.count == 1 {
            f.write_str(&self.name)
        } else {
            write!(
                f,
                "{}×{}",
                self.name,
                crate::format::grouped(self.count as u64)
            )
        }
    }
}

/// Structural facts about a PNG or APNG file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PngInfo {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub color_type: ColorType,
    pub interlaced: bool,
    /// Number of palette entries, when a PLTE chunk is present.
    pub palette_len: Option<usize>,
    /// Whether a tRNS (simple transparency) chunk is present.
    pub has_trns: bool,
    /// Number of frames declared by an acTL chunk; only set for APNG files.
    pub frame_count: Option<u32>,
    /// Chunk types in order of first appearance, with how often each occurs.
    pub chunks: Vec<ChunkCount>,
    /// Compressed image data: IDAT payloads plus APNG fdAT payloads (minus their sequence
    /// numbers). Everything else in the file is chunk overhead and metadata.
    pub image_data: u64,
    pub file_size: u64,
}

/// Why a file could not be inspected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InfoError {
    #[error("not a PNG or APNG file")]
    NotPng,
    #[error("the PNG file is truncated")]
    Truncated,
    #[error("the PNG file has no valid IHDR chunk")]
    BadHeader,
}

/// Returns true when `data` starts with the PNG signature.
#[must_use]
pub fn is_png(data: &[u8]) -> bool {
    data.starts_with(&SIGNATURE)
}

/// Inspects a complete PNG/APNG file held in memory.
///
/// # Errors
///
/// Returns an error when the data is not a PNG, is cut off mid-chunk, or lacks a valid IHDR.
pub fn parse(data: &[u8]) -> Result<PngInfo, InfoError> {
    let mut rest = data.strip_prefix(&SIGNATURE).ok_or(InfoError::NotPng)?;
    let mut walk = Walk::default();
    while !rest.is_empty() {
        let (len_bytes, after_len) = rest.split_first_chunk::<4>().ok_or(InfoError::Truncated)?;
        let (name, after_name) = after_len
            .split_first_chunk::<4>()
            .ok_or(InfoError::Truncated)?;
        let len =
            usize::try_from(u32::from_be_bytes(*len_bytes)).map_err(|_| InfoError::Truncated)?;
        // Chunk data is followed by a 4-byte CRC, which is not verified here.
        let body = after_name.get(..len).ok_or(InfoError::Truncated)?;
        rest = after_name.get(len + 4..).ok_or(InfoError::Truncated)?;
        if walk.chunk(*name, len, body)? {
            break;
        }
    }
    walk.finish(data.len() as u64)
}

/// Inspects a PNG/APNG file by reading its chunk headers and the few small chunks that
/// matter, seeking past everything else, so even large files are described quickly.
///
/// # Errors
///
/// Like [`parse`]; a file that can't be read counts as truncated.
pub fn parse_file(file: &mut (impl Read + Seek)) -> Result<PngInfo, InfoError> {
    let truncated = |_: std::io::Error| InfoError::Truncated;
    let file_size = file.seek(SeekFrom::End(0)).map_err(truncated)?;
    file.seek(SeekFrom::Start(0)).map_err(truncated)?;
    let mut signature = [0; 8];
    file.read_exact(&mut signature)
        .map_err(|_| InfoError::NotPng)?;
    if signature != SIGNATURE {
        return Err(InfoError::NotPng);
    }

    let mut walk = Walk::default();
    let mut position = SIGNATURE.len() as u64;
    let mut body = Vec::new();
    while position < file_size {
        let mut head = [0; 8];
        file.read_exact(&mut head).map_err(truncated)?;
        let [l0, l1, l2, l3, n0, n1, n2, n3] = head;
        let len = u32::from_be_bytes([l0, l1, l2, l3]);
        let name = [n0, n1, n2, n3];
        // Length, name, data, CRC.
        let next = position + 8 + u64::from(len) + 4;
        if next > file_size {
            return Err(InfoError::Truncated);
        }
        body.clear();
        if Walk::reads_body(name) {
            // Both chunks are tiny; a longer one is malformed and only its start is needed.
            body.resize(len.min(64) as usize, 0);
            file.read_exact(&mut body).map_err(truncated)?;
        }
        file.seek(SeekFrom::Start(next)).map_err(truncated)?;
        position = next;
        let len = usize::try_from(len).map_err(|_| InfoError::Truncated)?;
        if walk.chunk(name, len, &body)? {
            break;
        }
    }
    walk.finish(file_size)
}

/// What [`parse`] and [`parse_file`] collect, one chunk at a time.
#[derive(Debug, Default)]
struct Walk {
    header: Option<(u32, u32, u8, ColorType, bool)>,
    palette_len: Option<usize>,
    has_trns: bool,
    frame_count: Option<u32>,
    image_data: u64,
    chunks: Vec<ChunkCount>,
}

impl Walk {
    /// Chunks whose contents matter; for all others only the length does.
    fn reads_body(name: [u8; 4]) -> bool {
        matches!(&name, b"IHDR" | b"acTL")
    }

    /// Takes one chunk; `body` holds at least the start of IHDR and acTL. Returns true at the
    /// IEND chunk, after which nothing belongs to the image.
    fn chunk(&mut self, name: [u8; 4], len: usize, body: &[u8]) -> Result<bool, InfoError> {
        match &name {
            b"IHDR" => self.header = Some(parse_ihdr(body)?),
            b"PLTE" => self.palette_len = Some(len / 3),
            b"tRNS" => self.has_trns = true,
            b"acTL" => {
                self.frame_count = body.first_chunk::<4>().map(|n| u32::from_be_bytes(*n));
            }
            b"IDAT" => self.image_data += len as u64,
            b"fdAT" => self.image_data += len.saturating_sub(4) as u64,
            _ => {}
        }
        let name = String::from_utf8_lossy(&name).into_owned();
        let end = name == "IEND";
        match self.chunks.iter_mut().find(|c| c.name == name) {
            Some(existing) => existing.count += 1,
            None => self.chunks.push(ChunkCount { name, count: 1 }),
        }
        Ok(end)
    }

    fn finish(self, file_size: u64) -> Result<PngInfo, InfoError> {
        let (width, height, bit_depth, color_type, interlaced) =
            self.header.ok_or(InfoError::BadHeader)?;
        Ok(PngInfo {
            width,
            height,
            bit_depth,
            color_type,
            interlaced,
            palette_len: self.palette_len,
            has_trns: self.has_trns,
            frame_count: self.frame_count,
            chunks: self.chunks,
            image_data: self.image_data,
            file_size,
        })
    }
}

fn parse_ihdr(body: &[u8]) -> Result<(u32, u32, u8, ColorType, bool), InfoError> {
    let &[
        w0,
        w1,
        w2,
        w3,
        h0,
        h1,
        h2,
        h3,
        bit_depth,
        color,
        _compression,
        _filter,
        interlace,
    ] = body
    else {
        return Err(InfoError::BadHeader);
    };
    let color_type = ColorType::from_code(color).ok_or(InfoError::BadHeader)?;
    if !color_type.allows_bit_depth(bit_depth) {
        return Err(InfoError::BadHeader);
    }
    Ok((
        u32::from_be_bytes([w0, w1, w2, w3]),
        u32::from_be_bytes([h0, h1, h2, h3]),
        bit_depth,
        color_type,
        interlace == 1,
    ))
}

impl PngInfo {
    /// True for animated PNGs.
    #[must_use]
    pub const fn is_apng(&self) -> bool {
        self.frame_count.is_some()
    }

    /// Bits used by one pixel in the raw image data.
    #[must_use]
    pub fn bits_per_pixel(&self) -> u32 {
        u32::from(self.color_type.channels()) * u32::from(self.bit_depth)
    }

    /// Compact description of the pixel format, e.g. `RGB+Alpha 8-bit` or
    /// `Indexed 4-bit (12 colors) + tRNS`.
    #[must_use]
    pub fn format_summary(&self) -> String {
        let palette = match (self.color_type, self.palette_len) {
            (ColorType::Indexed, Some(colors)) => format!(" ({colors} colors)"),
            _ => String::new(),
        };
        let trns = if self.has_trns { " + tRNS" } else { "" };
        format!(
            "{} {}-bit{palette}{trns}",
            self.color_type.name(),
            self.bit_depth
        )
    }

    /// Chunk list such as `IHDR tEXt×3 IDAT×4 IEND`.
    #[must_use]
    pub fn chunk_summary(&self) -> String {
        self.chunks
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn count_of(&self, name: &str) -> usize {
        self.chunks
            .iter()
            .find(|c| c.name == name)
            .map_or(0, |c| c.count)
    }
}

/// One difference between an input file and its optimized version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The file was converted from another format, e.g. `GIF` to `APNG`, and now stores its
    /// pixels as `pixels`.
    Converted {
        from: String,
        to: String,
        pixels: String,
    },
    /// Color type, bit depth, palette, or transparency changed.
    Format { from: String, to: String },
    /// Interlacing was turned on or off.
    Interlacing { now_interlaced: bool },
    /// Metadata chunks that are gone from the output.
    Removed(Vec<ChunkCount>),
    /// Metadata chunks that only exist in the output (e.g. iCCP replaced by sRGB).
    Added(Vec<ChunkCount>),
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let join = |chunks: &[ChunkCount]| {
            chunks
                .iter()
                .map(|c| {
                    if c.count == 1 {
                        c.name.clone()
                    } else {
                        format!("{} ×{}", c.name, crate::format::grouped(c.count as u64))
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        match self {
            Self::Converted { from, to, pixels } => write!(f, "{from} to {to} · {pixels}"),
            // "to" rather than an arrow: egui's bundled font has no arrow glyphs.
            Self::Format { from, to } => write!(f, "{from} to {to}"),
            Self::Interlacing {
                now_interlaced: true,
            } => f.write_str("interlaced"),
            Self::Interlacing {
                now_interlaced: false,
            } => f.write_str("de-interlaced"),
            Self::Removed(chunks) => write!(f, "stripped {}", join(chunks)),
            Self::Added(chunks) => write!(f, "added {}", join(chunks)),
        }
    }
}

/// Lists what differs between `before` and `after`, in a stable order.
#[must_use]
pub fn changes(before: &PngInfo, after: &PngInfo) -> Vec<Change> {
    let mut changes = Vec::new();
    let (from, to) = (before.format_summary(), after.format_summary());
    if from != to {
        changes.push(Change::Format { from, to });
    }
    if before.interlaced != after.interlaced {
        changes.push(Change::Interlacing {
            now_interlaced: after.interlaced,
        });
    }
    let removed = chunk_difference(before, after);
    if !removed.is_empty() {
        changes.push(Change::Removed(removed));
    }
    let added = chunk_difference(after, before);
    if !added.is_empty() {
        changes.push(Change::Added(added));
    }
    changes
}

/// What changed for a file converted from `from` (e.g. `GIF`): the conversion itself, then
/// what oxipng changed in the converted PNG, except its pixel format, which only says how the
/// conversion stored the pixels before oxipng picked the final one.
#[must_use]
pub fn conversion_changes(from: &str, converted: &PngInfo, after: &PngInfo) -> Vec<Change> {
    let mut changes = vec![Change::Converted {
        from: from.to_owned(),
        to: if after.is_apng() { "APNG" } else { "PNG" }.to_owned(),
        pixels: after.format_summary(),
    }];
    changes.extend(
        self::changes(converted, after)
            .into_iter()
            .filter(|c| !matches!(c, Change::Format { .. })),
    );
    changes
}

/// Metadata chunks present in `a` more often than in `b`.
fn chunk_difference(a: &PngInfo, b: &PngInfo) -> Vec<ChunkCount> {
    a.chunks
        .iter()
        .filter(|c| !STRUCTURAL_CHUNKS.contains(&c.name.as_str()))
        .filter_map(|c| {
            let count = c.count.saturating_sub(b.count_of(&c.name));
            (count > 0).then(|| ChunkCount {
                name: c.name.clone(),
                count,
            })
        })
        .collect()
}

/// Joins changes into one line for the file table, e.g.
/// `RGB+Alpha 8-bit to Indexed 8-bit (182 colors) · stripped tEXt ×3`.
#[must_use]
pub fn summarize(changes: &[Change]) -> String {
    changes
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a syntactically valid PNG from chunks, with dummy CRCs.
    pub(crate) fn build_png(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut out = SIGNATURE.to_vec();
        for (name, body) in chunks {
            let len = u32::try_from(body.len()).expect("chunk too large");
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(*name);
            out.extend_from_slice(body);
            out.extend_from_slice(&[0, 0, 0, 0]);
        }
        out
    }

    pub(crate) fn ihdr(width: u32, height: u32, depth: u8, color: u8, interlace: u8) -> Vec<u8> {
        let mut body = Vec::with_capacity(13);
        body.extend_from_slice(&width.to_be_bytes());
        body.extend_from_slice(&height.to_be_bytes());
        body.extend_from_slice(&[depth, color, 0, 0, interlace]);
        body
    }

    #[test]
    fn rejects_non_png_data() {
        assert_eq!(parse(b"\xff\xd8\xff\xe0 JFIF"), Err(InfoError::NotPng));
        assert!(!is_png(b"GIF89a"));
    }

    #[test]
    fn parses_rgba_header_and_chunk_counts() {
        let png = build_png(&[
            (b"IHDR", &ihdr(512, 256, 8, 6, 1)),
            (b"tEXt", b"Title\0Logo"),
            (b"tEXt", b"Author\0Me"),
            (b"IDAT", &[1, 2, 3]),
            (b"IDAT", &[4, 5]),
            (b"IEND", &[]),
        ]);
        let info = parse(&png).expect("valid png");
        assert_eq!((info.width, info.height), (512, 256));
        assert_eq!(info.color_type, ColorType::Rgba);
        assert_eq!(info.bit_depth, 8);
        assert!(info.interlaced);
        assert_eq!(info.bits_per_pixel(), 32);
        assert_eq!(info.chunk_summary(), "IHDR tEXt×2 IDAT×2 IEND");
        let many = ChunkCount {
            name: "IDAT".to_owned(),
            count: 1_234,
        };
        assert_eq!(many.to_string(), "IDAT×1,234");
        assert_eq!(info.image_data, 5);
        assert_eq!(info.file_size, png.len() as u64);
        assert!(!info.is_apng());
    }

    #[test]
    fn reads_palette_transparency_and_animation() {
        let palette = [0u8; 12 * 3];
        let png = build_png(&[
            (b"IHDR", &ihdr(16, 16, 4, 3, 0)),
            (b"acTL", &[0, 0, 0, 24, 0, 0, 0, 0]),
            (b"PLTE", &palette),
            (b"tRNS", &[0]),
            (b"IDAT", &[0]),
            (b"IEND", &[]),
        ]);
        let info = parse(&png).expect("valid png");
        assert_eq!(info.palette_len, Some(12));
        assert!(info.has_trns);
        assert_eq!(info.frame_count, Some(24));
        assert_eq!(info.image_data, 1);
        assert_eq!(info.format_summary(), "Indexed 4-bit (12 colors) + tRNS");
    }

    #[test]
    fn detects_truncation_and_bad_headers() {
        let mut png = build_png(&[(b"IHDR", &ihdr(1, 1, 8, 2, 0)), (b"IDAT", &[0; 10])]);
        png.truncate(png.len() - 6);
        assert_eq!(parse(&png), Err(InfoError::Truncated));

        let no_header = build_png(&[(b"IDAT", &[0]), (b"IEND", &[])]);
        assert_eq!(parse(&no_header), Err(InfoError::BadHeader));

        let bad_depth = build_png(&[(b"IHDR", &ihdr(1, 1, 4, 2, 0)), (b"IEND", &[])]);
        assert_eq!(parse(&bad_depth), Err(InfoError::BadHeader));
    }

    #[test]
    fn describes_reduction_to_palette_and_stripped_metadata() {
        let before = parse(&build_png(&[
            (b"IHDR", &ihdr(64, 64, 8, 6, 1)),
            (b"iCCP", b"profile"),
            (b"tEXt", b"a\0b"),
            (b"tEXt", b"c\0d"),
            (b"tIME", &[0; 7]),
            (b"IDAT", &[0]),
            (b"IEND", &[]),
        ]))
        .expect("valid png");
        let after = parse(&build_png(&[
            (b"IHDR", &ihdr(64, 64, 8, 3, 0)),
            (b"sRGB", &[0]),
            (b"PLTE", &[0; 182 * 3]),
            (b"tRNS", &[0; 10]),
            (b"IDAT", &[0]),
            (b"IEND", &[]),
        ]))
        .expect("valid png");

        let found = changes(&before, &after);
        assert_eq!(
            summarize(&found),
            "RGB+Alpha 8-bit to Indexed 8-bit (182 colors) + tRNS · de-interlaced · \
             stripped iCCP, tEXt ×2, tIME · added sRGB"
        );
    }

    /// Counts the bytes actually read, to check that image data is skipped.
    struct Counting {
        inner: std::io::Cursor<Vec<u8>>,
        read: usize,
    }

    impl Read for Counting {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.inner.read(buf)?;
            self.read += n;
            Ok(n)
        }
    }

    impl Seek for Counting {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    #[test]
    fn reading_a_file_finds_the_same_while_skipping_image_data() {
        let mut truncated = build_png(&[(b"IHDR", &ihdr(1, 1, 8, 2, 0)), (b"IDAT", &[0; 10])]);
        truncated.truncate(truncated.len() - 6);
        let mut trailing = build_png(&[
            (b"IHDR", &ihdr(9, 9, 8, 2, 0)),
            (b"IDAT", &[0; 3]),
            (b"IEND", &[]),
        ]);
        trailing.extend_from_slice(b"junk after the end");
        let cases = [
            build_png(&[
                (b"IHDR", &ihdr(512, 256, 8, 6, 1)),
                (b"tEXt", b"Title\0Logo"),
                (b"IDAT", &vec![1; 100_000]),
                (b"IDAT", &vec![2; 100_000]),
                (b"IEND", &[]),
            ]),
            build_png(&[
                (b"IHDR", &ihdr(16, 16, 4, 3, 0)),
                (b"acTL", &[0, 0, 0, 24, 0, 0, 0, 0]),
                (b"PLTE", &[0; 36]),
                (b"tRNS", &[0]),
                (b"IDAT", &[0]),
                (b"fdAT", &[0; 9]),
                (b"IEND", &[]),
            ]),
            truncated,
            trailing,
            build_png(&[(b"IDAT", &[0]), (b"IEND", &[])]),
            b"\xff\xd8\xff\xe0 JFIF".to_vec(),
        ];
        for data in cases {
            let mut file = Counting {
                inner: std::io::Cursor::new(data.clone()),
                read: 0,
            };
            assert_eq!(parse_file(&mut file), parse(&data));
            assert!(file.read < 200, "read {} bytes", file.read);
        }
    }

    #[test]
    fn identical_structure_has_no_changes() {
        let png = build_png(&[
            (b"IHDR", &ihdr(8, 8, 8, 2, 0)),
            (b"IDAT", &[0]),
            (b"IEND", &[]),
        ]);
        let info = parse(&png).expect("valid png");
        assert_eq!(changes(&info, &info), []);
    }
}
