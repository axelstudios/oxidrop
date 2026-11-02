//! Optimizing one file: read it (converting GIF and WebP to PNG/APNG), run oxipng, and decide
//! whether and where to write the result.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::{
    convert::{self, ConvertError, FileType, Source},
    png_info::{self, Change, InfoError, PngInfo},
    settings::{OutputTarget, Settings},
};

/// Why optimizing a file failed.
#[derive(Debug, thiserror::Error)]
pub enum OptimizeError {
    #[error("could not read the file: {0}")]
    Read(#[source] io::Error),
    #[error("not a PNG, APNG, GIF, or WebP file")]
    UnknownFormat,
    #[error(transparent)]
    Format(#[from] InfoError),
    #[error(transparent)]
    Convert(#[from] ConvertError),
    #[error("oxipng failed: {0}")]
    Oxipng(String),
    #[error("no output folder is selected")]
    NoOutputFolder,
    #[error("could not write {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// A file that has been read and recognized, as PNG/APNG data for oxipng.
#[derive(Debug, Clone)]
pub struct Input {
    pub path: PathBuf,
    /// The file itself, or the PNG/APNG it was converted to.
    pub data: Vec<u8>,
    pub info: PngInfo,
    pub modified: Option<SystemTime>,
    /// Relative path inside the output folder in folder mode. Defaults to the file name; the
    /// app sets it so files with the same name from different folders don't overwrite each
    /// other. Converted files get a `.png` name.
    pub output_name: PathBuf,
    /// What the file was before it was converted; `None` for PNG/APNG files.
    pub source: Option<Source>,
    /// The PNG an earlier run converted this file to. Running it again replaces that PNG
    /// instead of adding `name (2).png` next to it.
    pub replaces: Option<PathBuf>,
}

/// The result of optimizing one file.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub before: PngInfo,
    /// Structure of the best version (the original when oxipng found nothing smaller).
    pub after: PngInfo,
    pub original_size: u64,
    /// Size of the best version.
    pub optimized_size: u64,
    /// Where the result was written, if anything was written.
    pub written_to: Option<PathBuf>,
    /// True when the original file itself was replaced.
    pub replaced_original: bool,
    pub changes: Vec<Change>,
    /// What a converted file was before; `None` for PNG/APNG files.
    pub source: Option<Source>,
}

impl Outcome {
    /// True when the result is smaller than the original file.
    #[must_use]
    pub const fn improved(&self) -> bool {
        self.optimized_size < self.original_size
    }

    /// True when the file was converted from another format.
    #[must_use]
    pub const fn converted(&self) -> bool {
        self.source.is_some()
    }

    /// Bytes saved; zero when nothing improved.
    #[must_use]
    pub const fn saved_bytes(&self) -> u64 {
        self.original_size.saturating_sub(self.optimized_size)
    }
}

/// Reads a PNG/APNG file, or converts a GIF or WebP file, with default conversion options.
///
/// # Errors
///
/// See [`read_input_with`].
pub fn read_input(path: &Path) -> Result<Input, OptimizeError> {
    read_input_with(path, convert::Options::default(), &mut |_| {})
}

/// Reads a PNG/APNG file, or converts a GIF or WebP file to PNG/APNG, reporting decoded frames
/// to `progress`.
///
/// # Errors
///
/// Fails when the file can't be read, isn't a supported format, or can't be decoded.
pub fn read_input_with(
    path: &Path,
    options: convert::Options,
    progress: convert::Progress<'_>,
) -> Result<Input, OptimizeError> {
    let data = fs::read(path).map_err(OptimizeError::Read)?;
    let modified = fs::metadata(path).and_then(|m| m.modified()).ok();
    let file_name = path.file_name().map(PathBuf::from).unwrap_or_default();
    let (data, source, output_name) = match FileType::detect(&data) {
        Some(FileType::Png) => (data, None, file_name),
        Some(file_type) => {
            let converted = convert::to_png(&data, file_type, options, progress)?;
            (
                converted.png,
                Some(converted.source),
                file_name.with_extension("png"),
            )
        }
        None => return Err(OptimizeError::UnknownFormat),
    };
    Ok(Input {
        info: png_info::parse(&data)?,
        path: path.to_path_buf(),
        data,
        modified,
        output_name,
        source,
        replaces: None,
    })
}

/// Runs oxipng on the input. Returns the original bytes when nothing smaller was found.
///
/// # Errors
///
/// Fails when oxipng can't decode or optimize the image.
pub fn optimize(input: &Input, opts: &oxipng::Options) -> Result<Vec<u8>, OptimizeError> {
    oxipng::optimize_from_memory(&input.data, opts)
        .map_err(|e| OptimizeError::Oxipng(e.to_string()))
}

/// Writes the result according to the output settings and describes what changed.
///
/// In place, the original is only replaced when the result is smaller. In folder mode the
/// best version is always written, so the folder ends up with a complete set of files.
/// Converted files always get a PNG; next to the original it takes a free name, so nothing
/// that is already there is replaced.
///
/// # Errors
///
/// Fails when no output folder is configured or the file can't be written.
pub fn write_output(
    input: &Input,
    optimized: &[u8],
    settings: &Settings,
) -> Result<Outcome, OptimizeError> {
    let improved = optimized.len() < input.data.len();
    let best = if improved { optimized } else { &input.data };
    let after = if improved {
        png_info::parse(best)?
    } else {
        input.info.clone()
    };
    let modified = settings
        .preserve_modified_time
        .then_some(input.modified)
        .flatten();

    if let Some(source) = &input.source {
        let target = match output_path(input, settings)? {
            Some(target) if target != input.path => target,
            _ => input.replaces.clone().unwrap_or_else(|| {
                free_path(
                    &input
                        .path
                        .with_file_name(input.output_name.file_name().unwrap_or_default()),
                )
            }),
        };
        write_file(&target, best, modified)?;
        return Ok(Outcome {
            changes: png_info::conversion_changes(source.format_name(), &input.info, &after),
            before: input.info.clone(),
            after,
            original_size: source.file_size,
            optimized_size: best.len() as u64,
            written_to: Some(target),
            replaced_original: false,
            source: Some(source.clone()),
        });
    }

    let written_to = match output_path(input, settings)? {
        Some(target) if target != input.path => {
            write_file(&target, best, modified)?;
            Some(target)
        }
        _ if improved => {
            write_file(&input.path, best, modified)?;
            Some(input.path.clone())
        }
        _ => None,
    };

    Ok(Outcome {
        changes: png_info::changes(&input.info, &after),
        before: input.info.clone(),
        after,
        original_size: input.data.len() as u64,
        optimized_size: best.len() as u64,
        replaced_original: written_to.as_deref() == Some(input.path.as_path()),
        written_to,
        source: None,
    })
}

/// `wanted`, or `name (2).png`, `name (3).png`, … if a file by that name exists.
fn free_path(wanted: &Path) -> PathBuf {
    if !wanted.exists() {
        return wanted.to_path_buf();
    }
    let stem = wanted.file_stem().unwrap_or_default().to_string_lossy();
    let extension = wanted
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..=u32::MAX)
        .map(|n| wanted.with_file_name(format!("{stem} ({n}){extension}")))
        .find(|candidate| !candidate.exists())
        .unwrap_or_else(|| wanted.to_path_buf())
}

/// Reads (converting if needed), optimizes, and writes one file.
///
/// # Errors
///
/// See [`read_input`], [`optimize`] and [`write_output`].
pub fn optimize_path(path: &Path, settings: &Settings) -> Result<Outcome, OptimizeError> {
    let input = read_input(path)?;
    let optimized = optimize(&input, &settings.oxipng_options())?;
    write_output(&input, &optimized, settings)
}

/// The folder-mode destination for `input`, or `None` when writing in place.
fn output_path(input: &Input, settings: &Settings) -> Result<Option<PathBuf>, OptimizeError> {
    match settings.output {
        OutputTarget::InPlace => Ok(None),
        OutputTarget::Folder => {
            if settings.output_dir.as_os_str().is_empty() {
                return Err(OptimizeError::NoOutputFolder);
            }
            // Only plain relative names may pick a place inside the output folder; anything
            // that could point outside it falls back to the file name.
            let name = if is_plain_relative(&input.output_name) {
                input.output_name.as_path()
            } else {
                input
                    .path
                    .file_name()
                    .map(Path::new)
                    .ok_or(OptimizeError::NoOutputFolder)?
            };
            Ok(Some(settings.output_dir.join(name)))
        }
    }
}

fn is_plain_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Writes `bytes` to `path` through a temporary file in the same folder, so a crash or full
/// disk never leaves a half-written image behind.
fn write_file(
    path: &Path,
    bytes: &[u8],
    modified: Option<SystemTime>,
) -> Result<(), OptimizeError> {
    let fail = |source| OptimizeError::Write {
        path: path.to_path_buf(),
        source,
    };
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(dir).map_err(fail)?;

    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(path.file_name().unwrap_or_default());
    temp_name.push(".oxidrop-tmp");
    let temp = dir.join(temp_name);

    let result = replace_via(&temp, path, bytes, modified);
    if result.is_err() {
        // Best effort: the temp file is hidden and harmless if this fails too.
        let _ = fs::remove_file(&temp);
    }
    result.map_err(fail)
}

fn replace_via(
    temp: &Path,
    path: &Path,
    bytes: &[u8],
    modified: Option<SystemTime>,
) -> io::Result<()> {
    fs::write(temp, bytes)?;
    if let Some(time) = modified {
        fs::File::options()
            .write(true)
            .open(temp)?
            .set_modified(time)?;
    }
    // Carry over permission bits (mainly Unix modes). Skipped for read-only files so that on
    // Windows the temp file stays deletable when renaming over the read-only original fails.
    if let Ok(meta) = fs::metadata(path)
        && !meta.permissions().readonly()
    {
        fs::set_permissions(temp, meta.permissions())?;
    }
    fs::rename(temp, path)
}

#[cfg(test)]
mod tests {
    use gif::DisposalMethod;

    use super::*;
    use crate::{
        convert::testing::{gif, gif_frame},
        png_info::tests::{build_png, ihdr},
    };

    fn input_at(path: &Path, data: &[u8]) -> Input {
        fs::write(path, data).expect("write input");
        read_input(path).expect("read input")
    }

    fn png(extra: usize) -> Vec<u8> {
        build_png(&[
            (b"IHDR", &ihdr(4, 4, 8, 2, 0)),
            (b"IDAT", &vec![0; 10 + extra]),
            (b"IEND", &[]),
        ])
    }

    #[test]
    fn replaces_original_only_when_smaller() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("a.png");
        let input = input_at(&path, &png(20));

        let outcome = write_output(&input, &png(0), &Settings::default()).expect("write");
        assert!(outcome.improved());
        assert_eq!(outcome.saved_bytes(), 20);
        assert_eq!(outcome.written_to.as_deref(), Some(path.as_path()));
        assert!(outcome.replaced_original);
        assert_eq!(fs::read(&path).expect("read back"), png(0));
        assert!(!dir.path().join(".a.png.oxidrop-tmp").exists());
    }

    #[test]
    fn leaves_original_untouched_when_not_smaller() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("a.png");
        let input = input_at(&path, &png(0));

        let outcome = write_output(&input, &png(5), &Settings::default()).expect("write");
        assert!(!outcome.improved());
        assert_eq!(outcome.written_to, None);
        assert_eq!(outcome.optimized_size, outcome.original_size);
        assert_eq!(fs::read(&path).expect("read back"), png(0));
    }

    #[test]
    fn folder_mode_always_writes_the_best_version() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().join("a.png");
        let out_dir = dir.path().join("out");
        let settings = Settings {
            output: OutputTarget::Folder,
            output_dir: out_dir.clone(),
            ..Settings::default()
        };

        let input = input_at(&source, &png(0));
        let outcome = write_output(&input, &png(5), &settings).expect("write");
        assert_eq!(outcome.written_to, Some(out_dir.join("a.png")));
        assert!(!outcome.replaced_original);
        assert_eq!(fs::read(out_dir.join("a.png")).expect("read copy"), png(0));
        assert_eq!(fs::read(&source).expect("read source"), png(0));
    }

    #[test]
    fn folder_mode_uses_the_output_name_and_never_leaves_the_folder() {
        let dir = tempfile::tempdir().expect("temp dir");
        let out_dir = dir.path().join("out");
        let settings = Settings {
            output: OutputTarget::Folder,
            output_dir: out_dir.clone(),
            ..Settings::default()
        };
        let mut input = input_at(&dir.path().join("a.png"), &png(5));

        input.output_name = PathBuf::from("icons").join("small").join("a (2).png");
        let outcome = write_output(&input, &png(0), &settings).expect("write");
        let nested = out_dir.join("icons").join("small").join("a (2).png");
        assert_eq!(outcome.written_to.as_deref(), Some(nested.as_path()));
        assert!(nested.exists());

        input.output_name = PathBuf::from("..").join("escaped.png");
        let outcome = write_output(&input, &png(0), &settings).expect("write");
        assert_eq!(outcome.written_to, Some(out_dir.join("a.png")));
        assert!(!dir.path().join("escaped.png").exists());
    }

    #[test]
    fn folder_mode_requires_a_folder() {
        let dir = tempfile::tempdir().expect("temp dir");
        let input = input_at(&dir.path().join("a.png"), &png(0));
        let settings = Settings {
            output: OutputTarget::Folder,
            ..Settings::default()
        };
        assert!(matches!(
            write_output(&input, &png(0), &settings),
            Err(OptimizeError::NoOutputFolder)
        ));
    }

    #[test]
    fn preserves_modified_time_when_asked() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("a.png");
        let mut input = input_at(&path, &png(20));
        let old = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        input.modified = Some(old);

        write_output(&input, &png(0), &Settings::default()).expect("write");
        let modified = fs::metadata(&path)
            .and_then(|m| m.modified())
            .expect("mtime");
        assert_eq!(modified, old);
    }

    /// A 2×2 GIF, converted. Its PNG is larger than the GIF.
    fn converted_input(dir: &Path, name: &str) -> Input {
        let path = dir.join(name);
        let frame = gif_frame((0, 0, 2, 2), &[0, 1, 2, 3], 0, DisposalMethod::Keep, None);
        fs::write(&path, gif(2, 2, None, &[frame])).expect("write gif");
        read_input(&path).expect("converts")
    }

    #[test]
    fn converted_files_get_a_new_png_without_replacing_anything() {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(dir.path().join("a.png"), b"someone else's file").expect("write");
        let input = converted_input(dir.path(), "a.gif");
        assert_eq!(input.output_name, PathBuf::from("a.png"));

        let outcome = write_output(&input, &input.data, &Settings::default()).expect("write");
        assert_eq!(outcome.written_to, Some(dir.path().join("a (2).png")));
        assert!(!outcome.replaced_original);
        // Written even though it is larger than the GIF.
        assert!(!outcome.improved());
        assert_eq!(
            fs::read(dir.path().join("a.png")).expect("read"),
            b"someone else's file"
        );
        assert!(
            FileType::detect(&fs::read(dir.path().join("a.gif")).expect("read"))
                .is_some_and(|t| t == FileType::Gif)
        );
        assert!(matches!(
            &outcome.changes[0],
            Change::Converted { from, to, .. } if from == "GIF" && to == "PNG"
        ));
    }

    #[test]
    fn converting_again_replaces_the_earlier_png() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut input = converted_input(dir.path(), "c.gif");
        let first = write_output(&input, &input.data, &Settings::default()).expect("write");
        assert_eq!(first.written_to, Some(dir.path().join("c.png")));

        input.replaces = first.written_to;
        let again = write_output(&input, &input.data, &Settings::default()).expect("write");
        assert_eq!(again.written_to, Some(dir.path().join("c.png")));
        assert!(!dir.path().join("c (2).png").exists());
    }

    #[test]
    fn converted_files_go_to_the_output_folder_as_png() {
        let dir = tempfile::tempdir().expect("temp dir");
        let out_dir = dir.path().join("out");
        let settings = Settings {
            output: OutputTarget::Folder,
            output_dir: out_dir.clone(),
            ..Settings::default()
        };
        let input = converted_input(dir.path(), "b.gif");
        let outcome = write_output(&input, &input.data, &settings).expect("write");
        assert_eq!(outcome.written_to, Some(out_dir.join("b.png")));
        assert_eq!(
            outcome.original_size,
            fs::metadata(dir.path().join("b.gif")).expect("gif").len()
        );
    }

    #[test]
    fn rejects_unsupported_files() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("photo.jpg");
        fs::write(&path, b"\xff\xd8\xff\xe0 not a png").expect("write");
        assert!(matches!(
            read_input(&path),
            Err(OptimizeError::UnknownFormat)
        ));
    }
}
