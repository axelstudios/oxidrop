//! Compares the progress the UI would show with the true share of time spent, to tune the
//! estimates in `src/progress/`:
//!
//! ```text
//! cargo run --release --example progress_accuracy -- FILE.png [--zopfli ITERATIONS] [--level N]
//! ```
//!
//! The file is copied and optimized twice through the real runner: the first run starts with no
//! calibration, the second uses what the first one measured.

use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use oxidrop::{
    progress::Snapshot,
    runner::{Event, Runner},
    settings::Settings,
};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut settings = Settings::default();
    let mut file = None;
    while let Some(arg) = args.next() {
        let mut value = || args.next().and_then(|v| v.parse().ok()).expect("a number");
        match arg.as_str() {
            "--zopfli" => {
                settings.zopfli = true;
                settings.zopfli_iterations = value();
            }
            "--level" => settings.level = u8::try_from(value()).expect("level 0-6"),
            path => file = Some(PathBuf::from(path)),
        }
    }
    let source = file.expect("usage: progress_accuracy FILE.png [--zopfli N] [--level N]");
    let dir = std::env::temp_dir().join("oxidrop-progress-accuracy");
    std::fs::create_dir_all(&dir).expect("temp folder");

    let (tx, rx) = mpsc::channel();
    let runner = Runner::new(1, tx, Arc::new(|| {}));
    let settings = Arc::new(settings);
    for run in 1..=2 {
        let copy = dir.join(format!("run{run}.png"));
        std::fs::copy(&source, &copy).expect("copy the file");
        runner.submit(run, copy, Arc::clone(&settings));
        let samples = sample(&rx);
        report(run, &samples);
    }
}

/// (time since start, stage label, fraction the UI would draw)
type Sample = (Duration, String, Option<f32>);

fn sample(rx: &mpsc::Receiver<Event>) -> Vec<Sample> {
    let start = Instant::now();
    let mut latest: Option<Snapshot> = None;
    let mut samples = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Event::Progress { snapshot, .. }) => latest = Some(snapshot),
            Ok(Event::Finished { result, .. }) => {
                result.expect("optimization succeeded");
                samples.push((start.elapsed(), "done".to_owned(), Some(1.0)));
                return samples;
            }
            Ok(Event::Started { .. } | Event::Stopped { .. })
            | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return samples,
        }
        let now = Instant::now();
        if samples
            .last()
            .is_none_or(|(t, _, _): &Sample| now - start >= *t + Duration::from_millis(100))
            && let Some(snapshot) = &latest
        {
            samples.push((
                now - start,
                format!("{:?}", snapshot.stage),
                snapshot.fraction_at(now),
            ));
        }
    }
}

fn report(run: u64, samples: &[Sample]) {
    let total = samples.last().map_or(Duration::ZERO, |s| s.0).as_secs_f64();
    println!("== run {run}: {total:.2} s");
    println!(" time %  stage         shown %");
    let mut next_mark = 0.0;
    for (at, stage, fraction) in samples {
        let share = at.as_secs_f64() / total * 100.0;
        if share + 1e-9 < next_mark {
            continue;
        }
        next_mark = share + 10.0;
        let shown = fraction.map_or_else(|| "  —".to_owned(), |f| format!("{:5.1}", f * 100.0));
        println!(" {share:5.1}  {stage:12}  {shown}");
    }
}
