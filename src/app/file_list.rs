//! The list of files shown in the table, independent of any UI code.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::{
    convert::FileType,
    inspect::Facts,
    optimize::Outcome,
    progress::{Report, Snapshot, Threads},
    runner::{Event, JobId},
};

/// Where a file is in its life cycle.
#[derive(Debug, Clone)]
pub enum Status {
    /// Added but not handed to the runner (auto-start is off, or the user pressed Stop).
    Pending,
    /// Waiting for a free slot in the runner.
    Queued,
    Running {
        started: Instant,
        snapshot: Option<Snapshot>,
        /// Stop was pressed; the runner reports [`Event::Stopped`] shortly.
        stopping: bool,
        /// The file's share of the CPU.
        threads: Threads,
    },
    /// Stopped while it ran; nothing was written. Starts again like a pending file.
    Stopped,
    Done {
        outcome: Box<Outcome>,
        elapsed: Duration,
        report: Box<Report>,
    },
    Failed {
        error: String,
    },
}

impl Status {
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        matches!(self, Self::Done { .. } | Self::Failed { .. })
    }

    /// True while the runner has the file: queued or running.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        matches!(self, Self::Queued | Self::Running { .. })
    }
}

/// One row of the file table.
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub id: JobId,
    pub path: PathBuf,
    pub name: String,
    /// Where the file goes inside the output folder (folder mode): the path below a dropped
    /// folder, or the file name, made unique among all listed files.
    pub output_name: PathBuf,
    pub original_size: Option<u64>,
    pub status: Status,
    /// What the file was when it was added, once the inspector has read it.
    pub facts: Option<Facts>,
    /// The PNG a converted file was last written to, which running it again replaces.
    pub converted_to: Option<PathBuf>,
}

/// Table columns that can be sorted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Original,
    Optimized,
    Saved,
    Time,
}

impl SortKey {
    /// Direction used when the column is first clicked: names A to Z, numbers largest first.
    #[must_use]
    pub const fn default_ascending(self) -> bool {
        matches!(self, Self::Name)
    }
}

/// How the table is sorted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub key: SortKey,
    pub ascending: bool,
}

impl Sort {
    /// The sort after clicking `key`'s header: its default direction, then reversed, then
    /// back to the order the files were added.
    #[must_use]
    pub fn after_click(current: Option<Self>, key: SortKey) -> Option<Self> {
        match current {
            Some(sort) if sort.key == key && sort.ascending == key.default_ascending() => {
                Some(Self {
                    key,
                    ascending: !sort.ascending,
                })
            }
            Some(sort) if sort.key == key => None,
            _ => Some(Self {
                key,
                ascending: key.default_ascending(),
            }),
        }
    }
}

/// Counts and byte totals for the status bar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    pub files: usize,
    pub pending: usize,
    pub stopped: usize,
    pub queued: usize,
    pub running: usize,
    pub done: usize,
    pub failed: usize,
    /// Original size of the files that finished successfully.
    pub original_bytes: u64,
    /// Size of their results; more than the original when conversions grew.
    pub result_bytes: u64,
}

/// All files the user has added.
#[derive(Debug, Default)]
pub struct FileList {
    entries: Vec<FileEntry>,
    next_id: JobId,
}

impl FileList {
    #[must_use]
    pub fn entries(&self) -> &[FileEntry] {
        &self.entries
    }

    #[must_use]
    pub fn get(&self, id: JobId) -> Option<&FileEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    fn get_mut(&mut self, id: JobId) -> Option<&mut FileEntry> {
        self.entries.iter_mut().find(|e| e.id == id)
    }

    /// Adds dropped or picked paths. Folders are searched recursively for PNG, APNG, GIF,
    /// and WebP files. A file that is already listed is reset to pending if it had finished, and
    /// otherwise left alone. Returns the ids that are now pending.
    pub fn add_paths(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> Vec<JobId> {
        // Each file with the name it should get inside the output folder.
        let mut files: Vec<(PathBuf, PathBuf)> = Vec::new();
        for path in paths {
            if path.is_dir() {
                // Keep the dropped folder's own name and structure: `icons/small/a.png`.
                let base = path.parent().unwrap_or(&path).to_path_buf();
                let mut found = Vec::new();
                collect_images(&path, &mut found);
                files.extend(found.into_iter().map(|file| {
                    let relative = file.strip_prefix(&base).map_or_else(
                        |_| file.file_name().map(PathBuf::from).unwrap_or_default(),
                        Path::to_path_buf,
                    );
                    (file, relative)
                }));
            } else {
                let name = path.file_name().map(PathBuf::from).unwrap_or_default();
                files.push((path, name));
            }
        }

        let mut pending = Vec::new();
        for (path, output_name) in files {
            if let Some(existing) = self.entries.iter_mut().find(|e| e.path == path) {
                if existing.status.is_finished() {
                    existing.original_size = file_size(&path);
                    existing.status = Status::Pending;
                    // Optimizing in place may have changed it.
                    existing.facts = None;
                    pending.push(existing.id);
                }
                continue;
            }
            let id = self.next_id;
            self.next_id += 1;
            let (status, output_name) = match sniff(&path) {
                Ok(file_type) => {
                    pending.push(id);
                    // Converted files are written as PNG.
                    let output_name = if file_type.is_converted() {
                        output_name.with_extension("png")
                    } else {
                        output_name
                    };
                    (Status::Pending, output_name)
                }
                Err(error) => (Status::Failed { error }, output_name),
            };
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            let output_name = self.unique_output_name(&output_name);
            self.entries.push(FileEntry {
                id,
                original_size: file_size(&path),
                name,
                output_name,
                path,
                status,
                facts: None,
                converted_to: None,
            });
        }
        pending
    }

    /// `wanted`, or `name (2).png`, `name (3).png`, … when another listed file already uses
    /// that output name. Compared case-insensitively, as on Windows and macOS file systems.
    fn unique_output_name(&self, wanted: &Path) -> PathBuf {
        let taken = |candidate: &Path| {
            let candidate = candidate.to_string_lossy().to_lowercase();
            self.entries
                .iter()
                .any(|e| e.output_name.to_string_lossy().to_lowercase() == candidate)
        };
        if !taken(wanted) {
            return wanted.to_path_buf();
        }
        let stem = wanted.file_stem().unwrap_or_default().to_string_lossy();
        let extension = wanted
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        // With n files listed, one of the first n + 1 suffixes is always free.
        (2..=self.entries.len() + 2)
            .map(|n| wanted.with_file_name(format!("{stem} ({n}){extension}")))
            .find(|candidate| !taken(candidate))
            .unwrap_or_else(|| wanted.to_path_buf())
    }

    /// Stores what the inspector read about a file.
    pub fn set_facts(&mut self, id: JobId, facts: Facts) {
        if let Some(entry) = self.get_mut(id) {
            entry.facts = Some(facts);
        }
    }

    /// Ids of files waiting to be started, including stopped ones.
    #[must_use]
    pub fn pending_ids(&self) -> Vec<JobId> {
        self.entries
            .iter()
            .filter(|e| matches!(e.status, Status::Pending | Status::Stopped))
            .map(|e| e.id)
            .collect()
    }

    /// Makes a file that isn't queued or running ready to start again with the current
    /// settings: finished files are reset as if added again. Returns false for queued and
    /// running files.
    pub fn prepare_restart(&mut self, id: JobId) -> bool {
        let Some(entry) = self.get_mut(id) else {
            return false;
        };
        if entry.status.is_active() {
            return false;
        }
        if entry.status.is_finished() {
            entry.original_size = file_size(&entry.path);
            // Optimizing in place may have changed it.
            entry.facts = None;
        }
        entry.status = Status::Pending;
        true
    }

    /// Marks running files whose stop was requested.
    pub fn mark_stopping(&mut self, ids: &[JobId]) {
        for entry in self.entries.iter_mut().filter(|e| ids.contains(&e.id)) {
            if let Status::Running { stopping, .. } = &mut entry.status {
                *stopping = true;
            }
        }
    }

    /// Marks files as handed to the runner.
    pub fn mark_queued(&mut self, ids: &[JobId]) {
        self.set_status(ids, || Status::Queued);
    }

    /// Marks files as no longer queued, e.g. after Stop.
    pub fn mark_pending(&mut self, ids: &[JobId]) {
        self.set_status(ids, || Status::Pending);
    }

    fn set_status(&mut self, ids: &[JobId], status: impl Fn() -> Status) {
        for entry in self.entries.iter_mut().filter(|e| ids.contains(&e.id)) {
            entry.status = status();
        }
    }

    /// Applies a runner event. Events for removed files are ignored.
    pub fn apply(&mut self, event: Event, now: Instant) {
        match event {
            Event::Started { id, threads } => {
                if let Some(entry) = self.get_mut(id) {
                    entry.status = Status::Running {
                        started: now,
                        snapshot: None,
                        stopping: false,
                        threads,
                    };
                }
            }
            Event::Progress { id, snapshot } => {
                if let Some(FileEntry {
                    status:
                        Status::Running {
                            snapshot: current, ..
                        },
                    ..
                }) = self.get_mut(id)
                {
                    *current = Some(snapshot);
                }
            }
            Event::Finished {
                id,
                result,
                elapsed,
                report,
            } => {
                if let Some(entry) = self.get_mut(id) {
                    entry.status = match result {
                        Ok(outcome) => {
                            entry.original_size = Some(outcome.original_size);
                            if outcome.converted() {
                                entry.converted_to.clone_from(&outcome.written_to);
                            }
                            Status::Done {
                                outcome,
                                elapsed,
                                report,
                            }
                        }
                        Err(error) => Status::Failed { error },
                    };
                }
            }
            Event::Stopped { id } => {
                if let Some(entry) = self.get_mut(id) {
                    entry.status = Status::Stopped;
                }
            }
        }
    }

    /// Removes finished files from the list.
    pub fn clear_finished(&mut self) {
        self.entries.retain(|e| !e.status.is_finished());
    }

    /// Removes every file that is not running right now.
    pub fn clear_idle(&mut self) {
        self.entries.retain(|e| e.status.is_active());
    }

    /// Removes one file if it is not running or queued.
    pub fn remove(&mut self, id: JobId) {
        self.entries.retain(|e| e.id != id || e.status.is_active());
    }

    /// Row order for the table, as indices into [`Self::entries`]. Files without a value for
    /// the sorted column (e.g. not finished yet) come last, in the order they were added.
    #[must_use]
    pub fn sorted_indices(&self, sort: Option<Sort>, now: Instant) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.entries.len()).collect();
        let Some(Sort { key, ascending }) = sort else {
            return order;
        };
        let directed = |ordering: std::cmp::Ordering| {
            if ascending {
                ordering
            } else {
                ordering.reverse()
            }
        };
        if key == SortKey::Name {
            let names: Vec<String> = self.entries.iter().map(|e| e.name.to_lowercase()).collect();
            order.sort_by(|&a, &b| directed(names[a].cmp(&names[b])));
            return order;
        }
        let values: Vec<Option<i128>> = self
            .entries
            .iter()
            .map(|e| sort_value(e, key, now))
            .collect();
        order.sort_by(|&a, &b| match (values[a], values[b]) {
            (Some(x), Some(y)) => directed(x.cmp(&y)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
        order
    }

    #[must_use]
    pub fn totals(&self) -> Totals {
        let mut totals = Totals {
            files: self.entries.len(),
            ..Totals::default()
        };
        for entry in &self.entries {
            match &entry.status {
                Status::Pending => totals.pending += 1,
                Status::Stopped => totals.stopped += 1,
                Status::Queued => totals.queued += 1,
                Status::Running { .. } => totals.running += 1,
                Status::Done { outcome, .. } => {
                    totals.done += 1;
                    totals.original_bytes += outcome.original_size;
                    totals.result_bytes += outcome.optimized_size;
                }
                Status::Failed { .. } => totals.failed += 1,
            }
        }
        totals
    }
}

/// A file's value in a numeric column, if it has one yet.
fn sort_value(entry: &FileEntry, key: SortKey, now: Instant) -> Option<i128> {
    let micros = |d: Duration| i128::try_from(d.as_micros()).unwrap_or(i128::MAX);
    match (key, &entry.status) {
        (SortKey::Original, _) => entry.original_size.map(i128::from),
        (SortKey::Optimized, Status::Done { outcome, .. }) => {
            Some(i128::from(outcome.optimized_size))
        }
        // Savings as a fraction of the original, in millionths, so it sorts as an integer.
        // Conversions that grew are negative.
        (SortKey::Saved, Status::Done { outcome, .. }) => Some(
            (i128::from(outcome.original_size) - i128::from(outcome.optimized_size)) * 1_000_000
                / i128::from(outcome.original_size.max(1)),
        ),
        (SortKey::Time, Status::Done { elapsed, .. }) => Some(micros(*elapsed)),
        (SortKey::Time, Status::Running { started, .. }) => {
            Some(micros(now.saturating_duration_since(*started)))
        }
        _ => None,
    }
}

fn file_size(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().map(|m| m.len())
}

/// Tells the file type from the first bytes, without reading the whole file.
fn sniff(path: &Path) -> Result<FileType, String> {
    let mut head = Vec::with_capacity(FileType::SNIFF_LEN);
    fs::File::open(path)
        .and_then(|file| file.take(FileType::SNIFF_LEN as u64).read_to_end(&mut head))
        .map_err(|err| format!("could not read the file: {err}"))?;
    FileType::detect(&head).ok_or_else(|| "not a PNG, APNG, GIF, or WebP file".to_owned())
}

fn has_image_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            FileType::EXTENSIONS
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

/// Recursively collects image files in sorted order. Symlinked folders are skipped to avoid
/// cycles.
fn collect_images(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<_> = read_dir.filter_map(Result::ok).collect();
    children.sort_by_key(fs::DirEntry::file_name);
    for child in children {
        let Ok(kind) = child.file_type() else {
            continue;
        };
        let path = child.path();
        if kind.is_dir() {
            collect_images(&path, out);
        } else if kind.is_file() && has_image_extension(&path) {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        png_info::{
            self,
            tests::{build_png, ihdr},
        },
        progress::Stage,
    };

    const THREADS: Threads = Threads {
        used: 4,
        available: 8,
        sharing: 2,
    };

    fn png() -> Vec<u8> {
        build_png(&[
            (b"IHDR", &ihdr(1, 1, 8, 2, 0)),
            (b"IDAT", &[0]),
            (b"IEND", &[]),
        ])
    }

    fn sample_outcome(original: u64, optimized: u64) -> Box<Outcome> {
        let info = png_info::parse(&png()).expect("valid png");
        Box::new(Outcome {
            before: info.clone(),
            after: info,
            original_size: original,
            optimized_size: optimized,
            written_to: None,
            replaced_original: false,
            changes: Vec::new(),
            source: None,
        })
    }

    #[test]
    fn adds_files_and_searches_folders_recursively() {
        let dir = tempfile::tempdir().expect("temp dir");
        let nested = dir.path().join("icons").join("small");
        fs::create_dir_all(&nested).expect("folders");
        fs::write(dir.path().join("b.png"), png()).expect("write");
        fs::write(dir.path().join("a.PNG"), png()).expect("write");
        fs::write(nested.join("anim.apng"), png()).expect("write");
        fs::write(dir.path().join("clip.GIF"), b"GIF89a\x01\x00").expect("write");
        fs::write(dir.path().join("still.webp"), b"RIFF\x04\x00\x00\x00WEBP").expect("write");
        fs::write(dir.path().join("notes.txt"), b"skip me").expect("write");
        let photo = dir.path().join("photo.jpg");
        fs::write(&photo, b"\xff\xd8\xff\xe0 jpeg").expect("write");

        let mut list = FileList::default();
        let pending = list.add_paths([dir.path().to_path_buf(), photo]);

        let names: Vec<_> = list.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "a.PNG",
                "b.png",
                "clip.GIF",
                "anim.apng",
                "still.webp",
                "photo.jpg"
            ]
        );
        assert_eq!(pending.len(), 5);
        assert!(matches!(
            &list.entries()[5].status,
            Status::Failed { error } if error == "not a PNG, APNG, GIF, or WebP file"
        ));
        assert_eq!(list.entries()[0].original_size, Some(png().len() as u64));
        // Converted files are written as PNG.
        let output = |i: usize| list.entries()[i].output_name.file_name().map(PathBuf::from);
        assert_eq!(output(2), Some(PathBuf::from("clip.png")));
        assert_eq!(output(4), Some(PathBuf::from("still.png")));
        assert_eq!(output(1), Some(PathBuf::from("b.png")));
    }

    #[test]
    fn sorts_by_column_with_unfinished_files_last() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut list = FileList::default();
        let ids = list.add_paths(["b.png", "C.png", "a.png", "d.png"].map(|name| {
            let path = dir.path().join(name);
            fs::write(&path, png()).expect("write");
            path
        }));
        let now = Instant::now();
        // b: 1000 → 600 (40%), C: 2000 → 1800 (10%), a: 500 → 100 (80%), d: not finished.
        for (id, original, optimized) in [
            (ids[0], 1000, 600),
            (ids[1], 2000, 1800),
            (ids[2], 500, 100),
        ] {
            list.apply(
                Event::Started {
                    id,
                    threads: THREADS,
                },
                now,
            );
            list.apply(
                Event::Finished {
                    id,
                    result: Ok(sample_outcome(original, optimized)),
                    elapsed: Duration::from_millis(original),
                    report: Box::default(),
                },
                now,
            );
        }
        let names = |sort| -> Vec<String> {
            list.sorted_indices(sort, now)
                .into_iter()
                .map(|i| list.entries()[i].name.clone())
                .collect()
        };
        let by = |key, ascending| Some(Sort { key, ascending });

        assert_eq!(names(None), ["b.png", "C.png", "a.png", "d.png"]);
        assert_eq!(
            names(by(SortKey::Name, true)),
            ["a.png", "b.png", "C.png", "d.png"]
        );
        assert_eq!(
            names(by(SortKey::Saved, false)),
            ["a.png", "b.png", "C.png", "d.png"]
        );
        assert_eq!(
            names(by(SortKey::Optimized, true)),
            ["a.png", "b.png", "C.png", "d.png"]
        );
        assert_eq!(
            names(by(SortKey::Time, false)),
            ["C.png", "b.png", "a.png", "d.png"]
        );
    }

    #[test]
    fn header_clicks_cycle_through_directions() {
        let first = Sort::after_click(None, SortKey::Saved);
        assert_eq!(
            first,
            Some(Sort {
                key: SortKey::Saved,
                ascending: false
            })
        );
        let second = Sort::after_click(first, SortKey::Saved);
        assert_eq!(
            second,
            Some(Sort {
                key: SortKey::Saved,
                ascending: true
            })
        );
        assert_eq!(Sort::after_click(second, SortKey::Saved), None);
        assert_eq!(
            Sort::after_click(second, SortKey::Name),
            Some(Sort {
                key: SortKey::Name,
                ascending: true
            })
        );
    }

    #[test]
    fn gives_every_file_a_unique_output_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (left, right) = (dir.path().join("left"), dir.path().join("right"));
        let small = dir.path().join("icons").join("small");
        for folder in [&left, &right, &small] {
            fs::create_dir_all(folder).expect("folder");
        }
        fs::write(left.join("logo.png"), png()).expect("write");
        fs::write(right.join("LOGO.png"), png()).expect("write");
        fs::write(small.join("logo.png"), png()).expect("write");
        fs::write(dir.path().join("icons").join("logo.png"), png()).expect("write");

        let mut list = FileList::default();
        list.add_paths([left.join("logo.png"), right.join("LOGO.png")]);
        list.add_paths([dir.path().join("icons")]);
        list.add_paths([left.join("logo.png")]);

        let names: Vec<String> = list
            .entries()
            .iter()
            .map(|e| e.output_name.to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(
            names,
            [
                "logo.png",
                "LOGO (2).png",
                "icons/logo.png",
                "icons/small/logo.png"
            ]
        );
    }

    #[test]
    fn re_adding_a_finished_file_makes_it_pending_again() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("a.png");
        fs::write(&path, png()).expect("write");
        let mut list = FileList::default();
        let ids = list.add_paths([path.clone()]);
        assert_eq!(
            list.add_paths([path.clone()]),
            [],
            "pending file is not added twice"
        );

        list.mark_queued(&ids);
        list.apply(
            Event::Started {
                id: ids[0],
                threads: THREADS,
            },
            Instant::now(),
        );
        list.apply(
            Event::Finished {
                id: ids[0],
                result: Ok(sample_outcome(100, 80)),
                elapsed: Duration::from_millis(5),
                report: Box::default(),
            },
            Instant::now(),
        );
        assert_eq!(list.add_paths([path]), ids);
        assert_eq!(list.entries().len(), 1);
        assert!(matches!(list.entries()[0].status, Status::Pending));
    }

    #[test]
    fn follows_runner_events_and_totals() {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths: Vec<_> = (0..4)
            .map(|i| {
                let path = dir.path().join(format!("{i}.png"));
                fs::write(&path, png()).expect("write");
                path
            })
            .collect();
        let mut list = FileList::default();
        let ids = list.add_paths(paths);
        list.mark_queued(&ids[..3]);
        let now = Instant::now();

        list.apply(
            Event::Started {
                id: ids[0],
                threads: THREADS,
            },
            now,
        );
        list.apply(
            Event::Progress {
                id: ids[0],
                snapshot: Snapshot {
                    stage: Stage::Filtering,
                    fraction: Some(0.5),
                    estimated: false,
                    detail: String::new(),
                    projection: None,
                    taken: now,
                },
            },
            now,
        );
        assert!(matches!(
            &list.entries()[0].status,
            Status::Running { snapshot: Some(s), .. } if s.fraction == Some(0.5)
        ));

        list.apply(
            Event::Started {
                id: ids[1],
                threads: THREADS,
            },
            now,
        );
        for (id, result) in [
            (ids[0], Ok(sample_outcome(1000, 600))),
            (ids[1], Err("boom".to_owned())),
        ] {
            list.apply(
                Event::Finished {
                    id,
                    result,
                    elapsed: Duration::ZERO,
                    report: Box::default(),
                },
                now,
            );
        }

        let totals = list.totals();
        assert_eq!(
            totals,
            Totals {
                files: 4,
                pending: 1,
                stopped: 0,
                queued: 1,
                running: 0,
                done: 1,
                failed: 1,
                original_bytes: 1000,
                result_bytes: 600,
            }
        );

        list.mark_pending(&[ids[2]]);
        assert_eq!(list.pending_ids(), [ids[2], ids[3]]);
        list.clear_finished();
        assert_eq!(list.entries().len(), 2);
        list.clear_idle();
        assert_eq!(list.entries().len(), 0);
    }

    #[test]
    fn running_files_cannot_be_removed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("a.png");
        fs::write(&path, png()).expect("write");
        let mut list = FileList::default();
        let ids = list.add_paths([path]);
        list.apply(
            Event::Started {
                id: ids[0],
                threads: THREADS,
            },
            Instant::now(),
        );
        list.remove(ids[0]);
        list.clear_idle();
        assert_eq!(list.entries().len(), 1);
    }
}
