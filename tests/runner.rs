//! The parallel runner with real files and real oxipng progress signals.

#![allow(clippy::unwrap_used, reason = "a test should fail loudly")]

mod common;

use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use oxidrop::{
    progress::Stage,
    runner::{Cancel, Event, Job, JobId, Runner, ZOPFLI_TIMED_OUT},
    settings::{Settings, available_cores},
};

const TIMEOUT: Duration = Duration::from_secs(60);

fn runner(parallel_files: usize) -> (Runner, mpsc::Receiver<Event>) {
    let (tx, rx) = mpsc::channel();
    (Runner::new(parallel_files, tx, Arc::new(|| {})), rx)
}

/// Collects events until `count` jobs have finished.
fn collect(rx: &mpsc::Receiver<Event>, count: usize) -> Vec<Event> {
    let mut events = Vec::new();
    let mut finished = 0;
    while finished < count {
        let event = rx.recv_timeout(TIMEOUT).expect("runner stalled");
        if matches!(event, Event::Finished { .. }) {
            finished += 1;
        }
        events.push(event);
    }
    events
}

fn write_fixtures(dir: &tempfile::TempDir, count: usize) -> Vec<PathBuf> {
    (0..count)
        .map(|i| {
            let path = dir.path().join(format!("image-{i}.png"));
            fs::write(
                &path,
                common::few_color_rgba_png(80 + 8 * u32::try_from(i).unwrap(), 60, 10),
            )
            .unwrap();
            path
        })
        .collect()
}

#[test]
fn optimizes_every_file_and_reports_progress_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let (runner, rx) = runner(2);
    let settings = Arc::new(Settings::default());
    for (id, path) in write_fixtures(&dir, 4).into_iter().enumerate() {
        runner.submit(id as JobId, path, Arc::clone(&settings));
    }

    let events = collect(&rx, 4);

    let mut seen: HashMap<JobId, Vec<&'static str>> = HashMap::new();
    let (mut running, mut max_running) = (0, 0);
    for event in &events {
        let (id, kind) = match event {
            Event::Started { id, .. } => {
                running += 1;
                max_running = max_running.max(running);
                (*id, "started")
            }
            Event::Progress { id, .. } => (*id, "progress"),
            Event::Finished {
                id, result, report, ..
            } => {
                running -= 1;
                let outcome = result.as_ref().unwrap();
                assert!(outcome.improved(), "job {id} did not shrink: {outcome:?}");
                // An equal share of the CPU's threads among the files running at once.
                let threads = report.threads.expect("the thread share");
                assert_eq!(threads.available, available_cores());
                assert!((1..=2).contains(&threads.sharing), "{threads:?}");
                assert_eq!(threads.used, (threads.available / threads.sharing).max(1));
                (*id, "finished")
            }
            Event::Stopped { id } => panic!("job {id} was stopped"),
        };
        seen.entry(id).or_default().push(kind);
    }
    assert!(max_running <= 2, "more than 2 files ran at once");
    assert_eq!(seen.len(), 4);
    for (id, kinds) in seen {
        assert_eq!(kinds.first(), Some(&"started"), "job {id}: {kinds:?}");
        assert_eq!(kinds.last(), Some(&"finished"), "job {id}: {kinds:?}");
        assert!(kinds.contains(&"progress"), "job {id} sent no progress");
    }
    assert_eq!(runner.running(), 0);
}

#[test]
fn files_submitted_together_split_the_cpu_threads() {
    let dir = tempfile::tempdir().unwrap();
    let (runner, rx) = runner(2);
    let settings = Arc::new(Settings::default());
    runner.submit_all(
        write_fixtures(&dir, 2)
            .into_iter()
            .enumerate()
            .map(|(id, path)| Job {
                id: id as JobId,
                path,
                output_name: None,
                replaces: None,
                settings: Arc::clone(&settings),
            }),
    );
    let shares: Vec<_> = collect(&rx, 2)
        .into_iter()
        .filter_map(|event| match event {
            Event::Started { threads, .. } => Some(threads),
            _ => None,
        })
        .collect();
    assert_eq!(shares.len(), 2);
    for threads in shares {
        assert_eq!(threads.sharing, 2, "{threads:?}");
        assert_eq!(threads.used, (available_cores() / 2).max(1));
    }
}

#[test]
fn cancelling_removes_only_files_that_have_not_started() {
    let dir = tempfile::tempdir().unwrap();
    let (runner, rx) = runner(1);
    let settings = Arc::new(Settings::default());
    for (id, path) in write_fixtures(&dir, 3).into_iter().enumerate() {
        runner.submit(id as JobId, path, Arc::clone(&settings));
    }

    assert_eq!(runner.cancel_queued(), vec![1, 2]);
    assert_eq!(runner.queued(), 0);

    let events = collect(&rx, 1);
    assert!(
        events
            .iter()
            .all(|e| !matches!(e, Event::Started { id, .. } if *id != 0))
    );
}

#[test]
fn the_time_limit_cuts_zopfli_short_and_keeps_what_finished() {
    // oxipng checks its time limit only before starting a trial; one Zopfli compression of
    // this image with 1,000 iterations takes far longer than the 1 s limit.
    for level in [2u8, 6] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.png");
        let original = common::few_color_rgba_png(320, 240, 40);
        fs::write(&path, &original).unwrap();
        let (runner, rx) = runner(1);
        let settings = Settings {
            level,
            zopfli: true,
            zopfli_iterations: 1000,
            time_limit: true,
            time_limit_secs: 1,
            ..Settings::default()
        };
        let started = Instant::now();
        runner.submit(1, path.clone(), Arc::new(settings));
        let Some(Event::Finished { result, report, .. }) = collect(&rx, 1).pop() else {
            panic!("level {level}: the last event finishes the job");
        };
        let took = started.elapsed();
        assert!(
            took < Duration::from_secs(10),
            "level {level}: took {took:?}"
        );
        let outcome = result.unwrap();
        assert!(
            report.notes.iter().any(|n| n == ZOPFLI_TIMED_OUT),
            "level {level}: {:?}",
            report.notes
        );
        if level == 2 {
            // The final Zopfli compression is the only one that makes a result, and it
            // didn't finish: the file stays as it was.
            assert!(!outcome.improved(), "{outcome:?}");
            assert_eq!(fs::read(&path).unwrap(), original);
        } else if let Some(chosen) = &report.chosen {
            // Each filter is a Zopfli trial of its own; one finished in time and was kept.
            assert!(chosen.deflater.starts_with("zopfli"), "{chosen:?}");
        }
    }
}

#[test]
fn stopping_a_running_file_cancels_oxipng_and_writes_nothing() {
    // Level 2 compresses once with zopfli; level 6 compresses every filter with zopfli at
    // once, on several threads, which all have to unwind.
    for level in [2, 6] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.png");
        let original = common::few_color_rgba_png(320, 240, 40);
        fs::write(&path, &original).unwrap();
        let (runner, rx) = runner(1);
        // Far more work than the test waits for.
        let settings = Settings {
            level,
            zopfli: true,
            zopfli_iterations: 5000,
            ..Settings::default()
        };
        runner.submit(1, path.clone(), Arc::new(settings));
        let queued = dir.path().join("queued.png");
        fs::write(&queued, &original).unwrap();
        runner.submit(2, queued, Arc::new(Settings::default()));

        // Wait until zopfli is at work.
        loop {
            match rx.recv_timeout(TIMEOUT).expect("runner stalled") {
                Event::Progress { snapshot, .. } if snapshot.detail.starts_with("Zopfli") => break,
                Event::Finished { .. } | Event::Stopped { .. } => panic!("ended too soon"),
                _ => {}
            }
        }
        assert_eq!(runner.cancel(2), Cancel::Dequeued, "level {level}");
        let asked = Instant::now();
        assert_eq!(runner.cancel(1), Cancel::Stopping, "level {level}");
        loop {
            match rx.recv_timeout(TIMEOUT).expect("runner stalled") {
                Event::Stopped { id } => {
                    assert_eq!(id, 1);
                    break;
                }
                Event::Finished { .. } => panic!("level {level}: finished instead of stopping"),
                _ => {}
            }
        }
        let took = asked.elapsed();
        assert!(
            took < Duration::from_secs(2),
            "level {level}: took {took:?}"
        );
        assert_eq!(fs::read(&path).unwrap(), original, "level {level}");
        // The dequeued file never started.
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
        assert_eq!(runner.cancel(1), Cancel::NotFound);
        assert_eq!(runner.running(), 0);
    }
}

#[test]
fn reports_unreadable_and_non_png_files_as_errors() {
    let dir = tempfile::tempdir().unwrap();
    let not_png = dir.path().join("photo.jpg");
    fs::write(&not_png, b"\xff\xd8\xff\xe0 jpeg data").unwrap();
    let (runner, rx) = runner(2);
    let settings = Arc::new(Settings::default());
    runner.submit(7, not_png, Arc::clone(&settings));
    runner.submit(8, dir.path().join("missing.png"), settings);

    let errors: HashMap<JobId, String> = collect(&rx, 2)
        .into_iter()
        .filter_map(|event| match event {
            Event::Finished {
                id,
                result: Err(err),
                ..
            } => Some((id, err)),
            _ => None,
        })
        .collect();
    assert_eq!(errors[&7], "not a PNG, APNG, GIF, or WebP file");
    assert!(
        errors[&8].starts_with("could not read the file"),
        "{}",
        errors[&8]
    );
}

#[test]
fn zopfli_compression_reports_determinate_progress() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("noise.png");
    fs::write(&path, common::noisy_rgb_png(300, 200)).unwrap();
    let (runner, rx) = runner(1);
    let settings = Settings {
        zopfli: true,
        zopfli_iterations: 3,
        ..Settings::default()
    };
    runner.submit(1, path, Arc::new(settings));

    let events = collect(&rx, 1);

    let snapshots: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::Progress { snapshot, .. } => Some(snapshot),
            _ => None,
        })
        .collect();
    let stages: Vec<Stage> = snapshots.iter().map(|s| s.stage).collect();
    for stage in [
        Stage::Reducing,
        Stage::Filtering,
        Stage::Compressing,
        Stage::Writing,
    ] {
        assert!(
            stages.contains(&stage),
            "never reached {stage:?}: {stages:?}"
        );
    }
    let zopfli: Vec<f32> = snapshots
        .iter()
        .filter(|s| s.stage == Stage::Compressing)
        .filter_map(|s| s.fraction)
        .collect();
    assert!(
        zopfli.iter().any(|&f| f > 0.5),
        "zopfli progress never passed 50%: {zopfli:?}"
    );
    assert!(
        zopfli.windows(2).all(|w| w[0] <= w[1]),
        "progress went backwards: {zopfli:?}"
    );

    let Some(Event::Finished {
        result: Ok(outcome),
        report,
        ..
    }) = events.last()
    else {
        panic!("last event should be a successful finish");
    };
    assert!(outcome.improved());
    assert!(
        report
            .stages
            .iter()
            .any(|(stage, _)| *stage == Stage::Compressing)
    );

    // The report is built from the same log stream, so check it against real oxipng too.
    let chosen = report.chosen.as_ref().expect("oxipng logged its choice");
    assert_eq!(chosen.compressor(), "Zopfli ×3");
    let filters = report.filters();
    assert!(filters.len() >= 2, "filter trials: {filters:?}");
    assert_eq!(
        filters[0].label, chosen.filter,
        "the smallest trial is the chosen filter"
    );
    let zopfli = report.zopfli().expect("zopfli passes were logged");
    assert_eq!(zopfli.totals.len(), 3, "one entry per configured iteration");
    assert_eq!(
        zopfli.gains[0], zopfli.blocks,
        "every block counts as a gain on the first pass"
    );
    // The last total is the compressed image data, give or take zlib's 6 bytes of framing.
    let image_data = outcome.after.image_data;
    let last = *zopfli.totals.last().unwrap();
    assert!(
        image_data.abs_diff(last) <= 8,
        "total {last} vs image data {image_data}"
    );
}

#[test]
fn reports_reduction_candidates_for_few_color_images() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("logo.png");
    fs::write(&path, common::few_color_rgba_png(120, 80, 12)).unwrap();
    let (runner, rx) = runner(1);
    runner.submit(1, path, Arc::new(Settings::default()));

    let Some(Event::Finished {
        result: Ok(outcome),
        report,
        ..
    }) = collect(&rx, 1).pop()
    else {
        panic!("expected a successful finish");
    };

    let reductions = report.reductions();
    assert!(
        reductions
            .first()
            .is_some_and(|c| c.label.contains("Indexed")),
        "best reduction should be a palette: {reductions:?}"
    );
    assert!(outcome.after.image_data < outcome.before.image_data);
    assert!(report.zopfli().is_none(), "libdeflate was used");
}

#[test]
fn apng_frames_report_progress_and_oxipngs_warning() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sprite.apng");
    fs::write(&path, common::apng(64, 48, 4)).unwrap();
    let (runner, rx) = runner(1);
    runner.submit(1, path, Arc::new(Settings::default()));

    let events = collect(&rx, 1);

    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::Progress { snapshot, .. } if snapshot.stage == Stage::Frames
        )),
        "frame recompression should show up as its own stage"
    );
    let Some(Event::Finished {
        result: Ok(outcome),
        report,
        ..
    }) = events.last()
    else {
        panic!("expected a successful finish");
    };
    assert_eq!(outcome.after.frame_count, Some(4));
    assert!(
        report.notes.iter().any(|n| n.contains("APNG detected")),
        "notes: {:?}",
        report.notes
    );
}

#[test]
fn converts_gifs_and_reports_conversion_progress_first() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("anim.gif");
    fs::write(&path, common::moving_square_gif(48, 16, 6)).unwrap();
    let (runner, rx) = runner(1);
    runner.submit(1, path, Arc::new(Settings::default()));

    let events = collect(&rx, 1);
    let stages: Vec<Stage> = events
        .iter()
        .filter_map(|event| match event {
            Event::Progress { snapshot, .. } => Some(snapshot.stage),
            _ => None,
        })
        .collect();
    assert_eq!(stages.first(), Some(&Stage::Converting), "{stages:?}");
    assert!(stages.iter().any(|&s| s != Stage::Converting), "{stages:?}");
    let Some(Event::Finished { result, report, .. }) = events.last() else {
        panic!("the last event finishes the job");
    };
    let outcome = result.as_ref().unwrap();
    assert!(outcome.converted());
    assert_eq!(outcome.written_to, Some(dir.path().join("anim.png")));
    assert_eq!(
        report.stages.first().map(|(s, _)| *s),
        Some(Stage::Converting)
    );
}

#[test]
fn keeps_the_chosen_filters_zopfli_passes_when_trials_run_side_by_side() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image.png");
    fs::write(&path, common::few_color_rgba_png(96, 64, 12)).unwrap();
    let (runner, rx) = runner(1);
    // Level 6 compresses every filter with zopfli at once instead of one final run.
    let settings = Settings {
        level: 6,
        zopfli: true,
        zopfli_iterations: 3,
        ..Settings::default()
    };
    runner.submit(1, path, Arc::new(settings));

    let Some(Event::Finished { result, report, .. }) = collect(&rx, 1).pop() else {
        panic!("the last event finishes the job");
    };
    result.unwrap();
    assert!(report.filter_trials.len() > 1, "{:?}", report.filter_trials);
    let chosen = report.chosen.as_ref().expect("a chosen filter");
    assert!(chosen.deflater.starts_with("zopfli"), "{chosen:?}");
    let summary = report.zopfli().expect("the chosen trial's zopfli passes");
    assert_ne!(summary.totals.len(), 0);
}

#[test]
fn stopping_after_idle_passes_points_to_stop_after() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image.png");
    fs::write(&path, common::few_color_rgba_png(96, 64, 12)).unwrap();
    let (runner, rx) = runner(1);
    // Every block stops 20 passes after its own last gain, so blocks end at different
    // passes and none comes near 1,000.
    let settings = Settings {
        level: 6,
        zopfli: true,
        zopfli_iterations: 1000,
        zopfli_stop_early: true,
        zopfli_stop_after: 20,
        ..Settings::default()
    };
    runner.submit(1, path, Arc::new(settings));

    let Some(Event::Finished { result, report, .. }) = collect(&rx, 1).pop() else {
        panic!("the last event finishes the job");
    };
    result.unwrap();
    let summary = report.zopfli().expect("the chosen trial's zopfli passes");
    assert_eq!(summary.limit, Some(1000));
    assert!(summary.block_ends.iter().all(|&(passes, _)| passes < 1000));
    // Zopfli stops a block exactly "Stop after" passes after its last gain.
    assert!(
        summary
            .block_ends
            .iter()
            .all(|&(passes, last_gain)| passes - last_gain == 20),
        "{:?}",
        summary.block_ends
    );
    assert_eq!(
        summary.hint().as_deref(),
        Some(
            "Every block stopped after 20 passes without a gain, before reaching 1,000 \
             iterations. More iterations alone won't change the result, but a higher \
             \"Stop after\" might: zopfli can still improve after a run like that."
        )
    );
}
