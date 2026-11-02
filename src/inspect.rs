//! What a file is, read without decoding its pixels: format, dimensions, colors, and chunks.
//! The details panel shows this before a file has been optimized.

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
};

use crate::{
    convert::{self, FileType, Source},
    png_info::{self, PngInfo},
    runner::{JobId, Waker},
};

/// A file as it was before optimizing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Facts {
    Png(PngInfo),
    /// A GIF or WebP file, which is converted to PNG/APNG first.
    Converted(Source),
}

/// Reads what `path` is. PNG files are read chunk header by chunk header, skipping the image
/// data; GIF and WebP files are scanned without decoding any frames.
///
/// # Errors
///
/// Fails with a message when the file can't be read or isn't a supported format.
pub fn inspect(path: &Path) -> Result<Facts, String> {
    let mut file = File::open(path).map_err(|err| format!("could not read the file: {err}"))?;
    let mut head = Vec::with_capacity(FileType::SNIFF_LEN);
    (&mut file)
        .take(FileType::SNIFF_LEN as u64)
        .read_to_end(&mut head)
        .and_then(|_| file.seek(SeekFrom::Start(0)))
        .map_err(|err| format!("could not read the file: {err}"))?;
    match FileType::detect(&head) {
        Some(FileType::Png) => png_info::parse_file(&mut file)
            .map(Facts::Png)
            .map_err(|err| err.to_string()),
        Some(file_type) => convert::inspect(file_type, file)
            .map(Facts::Converted)
            .map_err(|err| err.to_string()),
        None => Err("not a PNG, APNG, GIF, or WebP file".to_owned()),
    }
}

/// Inspects files one at a time on a background thread, so large files never hold up the
/// window. Results arrive on the channel given to [`Inspector::new`].
pub struct Inspector {
    requests: mpsc::Sender<(JobId, PathBuf)>,
}

impl Inspector {
    /// Starts the thread. It stops when the inspector is dropped.
    #[must_use]
    pub fn new(results: mpsc::Sender<(JobId, Result<Facts, String>)>, waker: Waker) -> Self {
        let (requests, queue) = mpsc::channel::<(JobId, PathBuf)>();
        let spawned = thread::Builder::new()
            .name("oxidrop-inspector".to_owned())
            .spawn(move || {
                for (id, path) in queue {
                    if results.send((id, inspect(&path))).is_err() {
                        break;
                    }
                    waker();
                }
            });
        if let Err(err) = spawned {
            // Without the thread, files just show no facts until they are optimized.
            log::warn!("could not start the file inspector: {err}");
        }
        Self { requests }
    }

    /// Asks for `path` to be inspected; the result comes back tagged with `id`.
    pub fn request(&self, id: JobId, path: PathBuf) {
        // Fails only when the thread is gone, and then there is no one to tell.
        let _ = self.requests.send((id, path));
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc, time::Duration};

    use gif::DisposalMethod::Keep;

    use super::*;
    use crate::{
        convert::testing::{gif, gif_frame, webp},
        png_info::tests::{build_png, ihdr},
    };

    #[test]
    fn describes_each_format_without_decoding() {
        let dir = tempfile::tempdir().expect("temp dir");
        let png_path = dir.path().join("a.png");
        let png = build_png(&[
            (b"IHDR", &ihdr(512, 512, 8, 6, 1)),
            (b"IDAT", &[0; 30]),
            (b"IDAT", &[0; 30]),
            (b"IEND", &[]),
        ]);
        fs::write(&png_path, &png).expect("write");
        let Ok(Facts::Png(info)) = inspect(&png_path) else {
            panic!("a PNG");
        };
        assert_eq!((info.width, info.height, info.interlaced), (512, 512, true));
        assert_eq!(info.chunk_summary(), "IHDR IDAT×2 IEND");

        let gif_path = dir.path().join("b.gif");
        let frames = [
            gif_frame((0, 0, 3, 2), &[0; 6], 10, Keep, None),
            gif_frame((0, 0, 1, 1), &[1], 10, Keep, None),
        ];
        fs::write(&gif_path, gif(3, 2, None, &frames)).expect("write");
        let Ok(Facts::Converted(source)) = inspect(&gif_path) else {
            panic!("a GIF");
        };
        assert_eq!((source.width, source.height, source.frames), (3, 2, 2));
        assert_eq!(
            source.file_size,
            fs::metadata(&gif_path).expect("size").len()
        );

        let webp_path = dir.path().join("c.webp");
        fs::write(&webp_path, webp(5, 4, &[9; 80], Some(b"icc".to_vec()))).expect("write");
        let Ok(Facts::Converted(source)) = inspect(&webp_path) else {
            panic!("a WebP");
        };
        assert_eq!((source.width, source.height, source.frames), (5, 4, 1));
        assert_eq!(source.format_name(), "WebP (lossless)");
        assert_eq!(source.metadata, ["ICC profile"]);

        let text = dir.path().join("notes.txt");
        fs::write(&text, b"not an image").expect("write");
        assert_eq!(
            inspect(&text),
            Err("not a PNG, APNG, GIF, or WebP file".to_owned())
        );
    }

    #[test]
    fn inspector_answers_in_the_background() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("a.png");
        let png = build_png(&[(b"IHDR", &ihdr(7, 3, 8, 2, 0)), (b"IEND", &[])]);
        fs::write(&path, png).expect("write");
        let (tx, rx) = mpsc::channel();
        let inspector = Inspector::new(tx, Arc::new(|| {}));
        inspector.request(4, path);
        let (id, facts) = rx.recv_timeout(Duration::from_secs(10)).expect("an answer");
        assert_eq!(id, 4);
        assert!(matches!(facts, Ok(Facts::Png(info)) if info.width == 7));
    }
}
