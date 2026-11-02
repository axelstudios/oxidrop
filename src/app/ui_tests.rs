//! UI tests: the real app driven through egui's accessibility tree with `egui_kittest`.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use egui_kittest::{Harness, kittest::Queryable};
use oxipng::{BitDepth, ColorType, Options, RawImage};

use super::{OxidropApp, Settings, Sort, SortKey, Status};
use crate::settings::OutputTarget;

/// A small RGBA image with few colors, stored without reductions, so oxipng shrinks it fast.
fn write_png(path: &Path, width: u32) {
    let mut data = Vec::new();
    for y in 0..32 {
        for x in 0..width {
            let c = u8::try_from((x / 5 + y / 3) % 8).unwrap_or(0) * 30;
            data.extend_from_slice(&[c, 255 - c, 40, 255]);
        }
    }
    let raw = RawImage::new(width, 32, ColorType::RGBA, BitDepth::Eight, data).expect("raw image");
    let mut opts = Options::from_preset(0);
    opts.color_type_reduction = false;
    opts.palette_reduction = false;
    opts.bit_depth_reduction = false;
    fs::write(path, raw.create_optimized_png(&opts).expect("encodable")).expect("write png");
}

/// The app with `paths` added. They start right away, whatever "Start when files are added"
/// defaults to; a test that wants them waiting turns it off and adds its files itself.
fn harness(paths: Vec<PathBuf>) -> Harness<'static, OxidropApp> {
    let mut harness = Harness::builder()
        .with_size([1120.0, 700.0])
        .build_eframe(|cc| OxidropApp::new(cc, Vec::new(), None));
    harness.state_mut().settings.auto_start = true;
    harness.state_mut().add_paths(paths);
    harness
}

/// Steps the app until `done` holds, while worker threads optimize in the background.
fn run_until(harness: &mut Harness<'_, OxidropApp>, done: impl Fn(&OxidropApp) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done(harness.state()) {
        assert!(Instant::now() < deadline, "the app never got there");
        harness.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    harness.run_steps(2);
}

fn all_finished(app: &OxidropApp) -> bool {
    let entries = app.files.entries();
    !entries.is_empty() && entries.iter().all(|e| e.status.is_finished())
}

/// Two finished files, `b.png` added before `a.png`.
fn finished_batch() -> (tempfile::TempDir, Harness<'static, OxidropApp>) {
    let dir = tempfile::tempdir().expect("temp dir");
    let (b, a) = (dir.path().join("b.png"), dir.path().join("a.png"));
    write_png(&b, 64);
    write_png(&a, 48);
    let mut harness = harness(vec![b, a]);
    run_until(&mut harness, all_finished);
    (dir, harness)
}

#[test]
fn empty_window_invites_dropping_files() {
    let mut harness = harness(Vec::new());
    harness.run_steps(2);
    harness.get_by_label("Drop PNG, APNG, GIF, or WebP files here");
    harness.get_by_label("No files yet");
    // Only the toolbar's: the drop area has no button of its own.
    harness.get_by_label("Add files…");
}

#[test]
fn optimizes_added_files_and_reports_the_savings() {
    let (_dir, harness) = finished_batch();
    for entry in harness.state().files.entries() {
        assert!(
            matches!(&entry.status, Status::Done { outcome, .. } if outcome.improved()),
            "{}: {:?}",
            entry.name,
            entry.status
        );
    }
    harness.get_by_label("a.png");
    harness.get_by_label("b.png");
    harness.get_by_label_contains("2 files · 2 done");
    harness.get_by_label_contains("Saved ");
}

#[test]
fn clicking_the_file_header_sorts_by_name() {
    let (_dir, mut harness) = finished_batch();
    let y = |harness: &Harness<'_, OxidropApp>, name: &str| {
        harness.get_by_label(name).rect().center().y
    };
    assert!(
        y(&harness, "b.png") < y(&harness, "a.png"),
        "rows start in the order added"
    );

    // "File" is also the menu; the column header is the lower one.
    harness
        .get_all_by_label("File")
        .max_by(|a, b| a.rect().center().y.total_cmp(&b.rect().center().y))
        .expect("the File header")
        .click();
    harness.run_steps(2);
    assert!(y(&harness, "a.png") < y(&harness, "b.png"), "sorted A to Z");
    assert_eq!(
        harness.state().sort,
        Some(Sort {
            key: SortKey::Name,
            ascending: true
        })
    );
}

#[test]
fn clicking_a_row_shows_its_details_and_optimization_report() {
    let (_dir, mut harness) = finished_batch();
    harness.get_by_label("a.png").click();
    harness.run_steps(2);
    harness.get_by_label("Details — a.png");
    harness.get_by_label("Optimization");
    harness.get_by_label("Overwritten in place");
}

#[test]
fn clicking_anywhere_in_a_header_cell_sorts() {
    let (_dir, mut harness) = finished_batch();
    // Left of the right-aligned title, still inside its column.
    let title = harness.get_by_label("Original").rect();
    let pos = egui::pos2(title.left() - 12.0, title.center().y);
    harness.event(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        });
    }
    harness.run_steps(2);
    assert_eq!(harness.state().sort.map(|s| s.key), Some(SortKey::Original));
}

#[test]
fn stop_cancels_a_running_file_and_its_menu_starts_it_again() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("slow.png");
    write_png(&path, 2000);
    let original = fs::read(&path).expect("read");
    let mut harness = harness(Vec::new());
    {
        // Seconds of Zopfli work, so it is still running when Stop is clicked.
        let app = harness.state_mut();
        app.settings.zopfli = true;
        app.settings.zopfli_iterations = Settings::MAX_ZOPFLI_ITERATIONS;
        app.add_paths(vec![path.clone()]);
    }
    let status = |app: &OxidropApp| app.files.entries()[0].status.clone();
    run_until(&mut harness, |app| {
        matches!(
            status(app),
            Status::Running {
                snapshot: Some(_),
                ..
            }
        )
    });
    harness.get_by_label("Stop").click();
    run_until(&mut harness, |app| matches!(status(app), Status::Stopped));
    assert_eq!(fs::read(&path).expect("read"), original, "nothing written");
    harness.get_by_label("Stopped");
    harness.get_by_label_contains("1 stopped");

    // The row menu starts it again, with whatever the settings are now.
    harness.state_mut().settings.zopfli = false;
    harness.get_by_label("slow.png").click_secondary();
    harness.run_steps(2);
    // The toolbar has a Start button too; the menu's is further down.
    harness
        .get_all_by_label("Start")
        .max_by(|a, b| a.rect().center().y.total_cmp(&b.rect().center().y))
        .expect("the menu's Start")
        .click();
    run_until(&mut harness, all_finished);
    assert!(matches!(
        status(harness.state()),
        Status::Done { outcome, .. } if outcome.improved()
    ));
}

#[test]
fn details_panel_keeps_its_height_whatever_it_shows() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (good, broken) = (dir.path().join("good.png"), dir.path().join("broken.png"));
    write_png(&good, 64);
    fs::write(&broken, b"\x89PNG\r\n\x1a\nnot really a PNG").expect("write broken png");
    let mut harness = harness(vec![good, broken]);
    run_until(&mut harness, all_finished);
    let top = |harness: &mut Harness<'_, OxidropApp>, name: &str| {
        harness.get_by_label(name).click();
        harness.run_steps(2);
        harness
            .get_by_label(&format!("Details — {name}"))
            .rect()
            .top()
    };
    // A failed file shows only a few rows, a finished one the whole optimization report.
    // Opened on the failed file, the panel used to shrink to its rows and then stay that
    // small for the finished one; it should keep its height either way.
    let failed = top(&mut harness, "broken.png");
    let finished = top(&mut harness, "good.png");
    assert!((finished - failed).abs() < 1.0, "{finished} vs {failed}");
}

#[test]
fn alt_and_a_menus_first_letter_open_it() {
    let mut harness = harness(Vec::new());
    harness.run_steps(2);
    for (key, entry) in [
        (egui::Key::F, "Quit"),
        (egui::Key::V, "Follow system"),
        (egui::Key::H, "About Oxidrop"),
    ] {
        assert!(
            harness.query_by_label(entry).is_none(),
            "{entry} shown early"
        );
        harness.key_press_modifiers(egui::Modifiers::ALT, key);
        harness.run_steps(3);
        harness.get_by_label(entry);
        harness.key_press(egui::Key::Escape);
        harness.run_steps(3);
        assert!(
            harness.query_by_label(entry).is_none(),
            "{entry} still shown"
        );
    }

    // The arrow keys move into an open menu, and Enter picks the entry.
    harness.key_press_modifiers(egui::Modifiers::ALT, egui::Key::H);
    harness.run_steps(3);
    harness.key_press(egui::Key::ArrowDown);
    harness.run_steps(3);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(3);
    assert!(harness.state().show_about);
}

#[test]
fn about_window_links_oxipng_beside_the_version_built() {
    // The build script reads it from Cargo.lock, or says "unknown".
    let version = env!("OXIPNG_VERSION");
    let parts: Vec<&str> = version.split('.').collect();
    assert!(
        parts.len() == 3 && parts.iter().all(|part| part.parse::<u32>().is_ok()),
        "{version}"
    );
    let mut harness = harness(Vec::new());
    harness.state_mut().show_about = true;
    harness.run_steps(3);
    // "Optimization via oxipng 10.2.1" on one line, with "oxipng" the link.
    let rects = [
        harness.get_by_label("Optimization via").rect(),
        harness
            .get_by_role_and_label(egui::accesskit::Role::Link, "oxipng")
            .rect(),
        harness.get_by_label(version).rect(),
    ];
    for pair in rects.windows(2) {
        assert!(pair[0].max.x < pair[1].min.x, "{rects:?}");
        assert!(
            (pair[0].center().y - pair[1].center().y).abs() < 1.0,
            "{rects:?}"
        );
    }
}

#[test]
fn an_open_menus_items_have_access_keys() {
    let mut harness = harness(Vec::new());
    harness.run_steps(2);
    for (item, theme) in [
        (egui::Key::D, egui::ThemePreference::Dark),
        (egui::Key::L, egui::ThemePreference::Light),
        (egui::Key::F, egui::ThemePreference::System),
    ] {
        harness.key_press_modifiers(egui::Modifiers::ALT, egui::Key::V);
        harness.run_steps(3);
        harness.key_press(item);
        harness.run_steps(3);
        assert!(
            harness.query_by_label("Follow system").is_none(),
            "the menu closes"
        );
        assert_eq!(harness.ctx.options(|o| o.theme_preference), theme);
    }
    harness.key_press_modifiers(egui::Modifiers::ALT, egui::Key::H);
    harness.run_steps(3);
    harness.key_press(egui::Key::A);
    harness.run_steps(3);
    assert!(harness.state().show_about);
}

#[test]
fn on_macos_option_letters_open_menus_unless_typing() {
    let mut harness = harness(Vec::new());
    harness.ctx.set_os(egui::os::OperatingSystem::Mac);
    harness.state_mut().settings.output = OutputTarget::Folder;
    harness.run_steps(2);
    let menu_open =
        |harness: &Harness<'_, OxidropApp>| harness.query_by_label("Follow system").is_some();

    // Again and again: the menu button a keyboard-opened menu leaves focused isn't a text box.
    for (item, theme) in [
        (egui::Key::D, egui::ThemePreference::Dark),
        (egui::Key::L, egui::ThemePreference::Light),
    ] {
        harness.key_press_modifiers(egui::Modifiers::ALT, egui::Key::V);
        harness.run_steps(3);
        assert!(menu_open(&harness), "Option+V opens the View menu");
        harness.key_press(item);
        harness.run_steps(3);
        assert_eq!(harness.ctx.options(|o| o.theme_preference), theme);
    }

    // In a text box, Option with a letter types a character instead.
    harness
        .get_by_role(egui::accesskit::Role::TextInput)
        .focus();
    harness.run_steps(2);
    harness.key_press_modifiers(egui::Modifiers::ALT, egui::Key::V);
    harness.run_steps(3);
    assert!(!menu_open(&harness), "the text box keeps the key");
}

#[test]
fn delete_removes_the_selected_file_unless_typing() {
    let (_dir, mut harness) = finished_batch();
    let count = |harness: &Harness<'_, OxidropApp>| harness.state().files.entries().len();
    let first = harness.state().files.entries()[0].id;
    harness.state_mut().selected = Some(first);
    harness.state_mut().settings.output = OutputTarget::Folder;
    harness.run_steps(2);

    // In the output folder's text box, Delete edits the text.
    harness
        .get_by_role(egui::accesskit::Role::TextInput)
        .focus();
    harness.run_steps(2);
    harness.key_press(egui::Key::Delete);
    harness.run_steps(2);
    assert_eq!(count(&harness), 2, "the text box got the key");

    // A menu used from the keyboard leaves its button focused, which isn't typing.
    harness.key_press_modifiers(egui::Modifiers::ALT, egui::Key::V);
    harness.run_steps(3);
    harness.key_press(egui::Key::D);
    harness.run_steps(3);
    harness.key_press(egui::Key::Delete);
    harness.run_steps(3);
    assert_eq!(count(&harness), 1, "the selected file is removed");
}

#[test]
fn escape_closes_the_about_window_once_menus_and_text_boxes_are_done() {
    let mut harness = harness(Vec::new());
    harness.state_mut().settings.output = OutputTarget::Folder;
    harness.state_mut().show_about = true;
    harness.run_steps(3);

    // An open menu closes first.
    harness.key_press_modifiers(egui::Modifiers::ALT, egui::Key::H);
    harness.run_steps(3);
    assert!(
        egui::Popup::is_any_open(&harness.ctx),
        "the Help menu opens"
    );
    harness.key_press(egui::Key::Escape);
    harness.run_steps(3);
    assert!(!egui::Popup::is_any_open(&harness.ctx), "the menu closes");
    assert!(harness.state().show_about, "the About window stays");

    // A text box gives up the keyboard first.
    harness
        .get_by_role(egui::accesskit::Role::TextInput)
        .focus();
    harness.run_steps(2);
    assert!(harness.ctx.text_edit_focused());
    harness.key_press(egui::Key::Escape);
    harness.run_steps(3);
    assert!(!harness.ctx.text_edit_focused(), "the text box lets go");
    assert!(harness.state().show_about, "the About window stays");

    harness.key_press(egui::Key::Escape);
    harness.run_steps(3);
    assert!(
        !harness.state().show_about,
        "Escape closes the About window"
    );
}

#[test]
fn about_window_fits_its_lines_unwrapped() {
    let mut harness = harness(Vec::new());
    // As long as a real adapter's description gets.
    let graphics = "Vulkan on NVIDIA RTX A5000 Laptop GPU (NVIDIA 566.36)";
    harness.state_mut().graphics = graphics.to_owned();
    harness.state_mut().show_about = true;
    harness.run_steps(3);
    let height = harness
        .get_by_label("Drag-and-drop PNG and APNG optimizer")
        .rect()
        .height();
    let rect = harness
        .get_by_label(&format!("Graphics: {graphics}"))
        .rect();
    assert!(rect.height() < height * 1.5, "the graphics line wrapped");
}

#[test]
fn zopfli_checkbox_updates_the_settings() {
    let mut harness = harness(Vec::new());
    harness.run_steps(2);
    assert!(!harness.state().settings.zopfli);
    harness.get_by_label("Use Zopfli").click();
    harness.run_steps(2);
    assert!(harness.state().settings.zopfli);
}

#[test]
fn converts_dropped_gifs_to_new_pngs() {
    use crate::convert::testing::{gif, gif_frame};
    use gif::{DisposalMethod::Keep, Repeat};

    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("anim.gif");
    let frames = [
        gif_frame((0, 0, 4, 4), &[0; 16], 10, Keep, None),
        gif_frame((1, 1, 2, 2), &[2; 4], 10, Keep, None),
    ];
    fs::write(&path, gif(4, 4, Some(Repeat::Infinite), &frames)).expect("write gif");

    let mut harness = harness(vec![path]);
    run_until(&mut harness, all_finished);
    harness.get_by_label_contains("GIF to APNG");
    assert!(dir.path().join("anim.png").exists());

    harness.get_by_label("anim.gif").click();
    harness.run_steps(2);
    harness.get_by_label("Format");
    harness.get_by_label("GIF");
    // The row's change summary and the details panel's format.
    assert_eq!(
        harness.get_all_by_label_contains("APNG · Indexed").count(),
        2
    );
    harness.get_by_label_contains("New file");
}

#[test]
fn shows_what_a_file_is_before_it_is_optimized() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("waiting.png");
    write_png(&path, 64);
    let mut harness = harness(Vec::new());
    harness.state_mut().settings.auto_start = false;
    harness.state_mut().add_paths(vec![path]);
    run_until(&mut harness, |app| {
        app.files.entries().iter().all(|e| e.facts.is_some())
    });

    harness.get_by_label("waiting.png").click();
    harness.run_steps(2);
    harness.get_by_label("Details — waiting.png");
    harness.get_by_label("Dimensions");
    harness.get_by_label("64 × 32");
    harness.get_by_label("RGB+Alpha 8-bit");
    // The settings panel has an "Interlacing" option too.
    assert_eq!(harness.get_all_by_label("Interlacing").count(), 2);
    harness.get_by_label("None");
    // The row and the details' Status line.
    assert_eq!(harness.get_all_by_label("Not started").count(), 2);
    assert!(matches!(
        harness.state().files.entries()[0].status,
        Status::Pending
    ));
}
