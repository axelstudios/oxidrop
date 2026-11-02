//! The eframe application: window layout, and the wiring between UI, file list, and runner.
//!
//! Each panel is drawn by a module of its own (`menu_bar`, `toolbar`, `settings_panel`,
//! `file_table`, `details_panel`, `status_bar`, and `about_window`), in an `impl OxidropApp`
//! block there; [`OxidropApp`]'s `ui` lays them out.

mod about_window;
mod crash;
mod details_panel;
mod file_list;
mod file_table;
mod fonts;
mod graphics;
mod menu_bar;
mod reveal;
mod screenshot;
mod settings_panel;
mod status_bar;
mod style;
mod toolbar;
#[cfg(test)]
mod ui_tests;
mod widgets;

use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use egui::{Key, KeyboardShortcut, Modifiers, ScrollArea, Ui};

pub use file_list::{FileEntry, FileList, Sort, SortKey, Status, Totals};
use screenshot::Screenshot;

use crate::{
    convert::FileType,
    inspect::{Facts, Inspector},
    runner::{Cancel, Event, Job, JobId, Runner},
    settings::{Settings, available_cores},
};

/// Names the settings folder (`%APPDATA%\oxidrop\data` on Windows), and the app to the desktop.
const APP_ID: &str = "oxidrop";
const SETTINGS_KEY: &str = "settings";
const ADD_FILES: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const START: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter);
/// Window and taskbar icon size; the OS scales it down where needed.
const WINDOW_ICON_SIZE: u32 = 64;

/// Opens the main window. `paths` (e.g. from the command line) are added right away. A panic on
/// the UI thread is written to a crash log and shown in a dialog before the app exits (see
/// `crash`).
///
/// # Errors
///
/// Fails when no window or graphics context can be created.
pub fn run(paths: Vec<PathBuf>) -> eframe::Result {
    let screenshot = Screenshot::from_env();
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Oxidrop")
            .with_app_id(APP_ID)
            .with_inner_size([1120.0, 700.0])
            .with_min_inner_size([640.0, 420.0])
            .with_icon(egui::IconData {
                rgba: crate::icon::rgba(WINDOW_ICON_SIZE),
                width: WINDOW_ICON_SIZE,
                height: WINDOW_ICON_SIZE,
            })
            .with_drag_and_drop(true),
        wgpu_options: graphics::wgpu_options(),
        ..Default::default()
    };
    if let Some(screenshot) = &screenshot {
        // Screenshots start from a clean slate and never touch the user's saved state.
        options.persistence_path = Some(Screenshot::fresh_storage_file());
        if let Some(size) = screenshot.window_size() {
            options.viewport = options.viewport.with_inner_size(size);
        }
    }
    crash::guard(|| {
        eframe::run_native(
            "Oxidrop",
            options,
            Box::new(|cc| Ok(Box::new(OxidropApp::new(cc, paths, screenshot)))),
        )
    })
}

/// Application state.
pub struct OxidropApp {
    settings: Settings,
    files: FileList,
    runner: Runner,
    events: mpsc::Receiver<Event>,
    inspector: Inspector,
    inspected: mpsc::Receiver<(JobId, Result<Facts, String>)>,
    selected: Option<JobId>,
    sort: Option<Sort>,
    output_dir_text: String,
    batch_started: Option<Instant>,
    batch_finished: Option<Instant>,
    show_about: bool,
    /// A text box had the keyboard at the end of the last frame (see `close_about_on_escape`).
    was_typing: bool,
    /// A menu was opened from the keyboard and is still open, so access keys are shown.
    menu_from_keyboard: bool,
    /// GPU backend and adapter, shown in the About window for bug reports.
    graphics: String,
    frame_stats: Option<graphics::FrameStats>,
    screenshot: Option<Screenshot>,
}

impl OxidropApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        paths: Vec<PathBuf>,
        screenshot: Option<Screenshot>,
    ) -> Self {
        cc.egui_ctx.set_fonts(fonts::definitions());
        style::apply(&cc.egui_ctx);
        if let Some(screenshot) = &screenshot {
            cc.egui_ctx.set_theme(screenshot.theme());
        }
        let mut settings: Settings = if screenshot.is_some() {
            Screenshot::settings()
        } else {
            cc.storage
                .and_then(|s| eframe::get_value(s, SETTINGS_KEY))
                .unwrap_or_default()
        };
        settings.parallel_files = settings.parallel_files.clamp(1, available_cores());
        let graphics = cc.wgpu_render_state.as_ref().map_or_else(
            || "unknown".to_owned(),
            |state| {
                let info = state.adapter.get_info();
                let driver = if info.driver_info.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", info.driver_info)
                };
                format!("{:?} on {}{driver}", info.backend, info.name)
            },
        );
        if screenshot.is_some() {
            eprintln!("graphics: {graphics}");
        }

        let (tx, rx) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        // Progress events can arrive hundreds of times a second (zopfli passes, filter
        // trials); batch them into at most one repaint per animation frame.
        let runner = Runner::new(
            settings.parallel_files(),
            tx,
            Arc::new(move || ctx.request_repaint_after(widgets::ANIMATION_INTERVAL)),
        );
        let (facts_tx, facts_rx) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        let inspector = Inspector::new(facts_tx, Arc::new(move || ctx.request_repaint()));
        let mut app = Self {
            output_dir_text: settings.output_dir.display().to_string(),
            settings,
            files: FileList::default(),
            runner,
            events: rx,
            inspector,
            inspected: facts_rx,
            selected: None,
            sort: None,
            batch_started: None,
            batch_finished: None,
            show_about: false,
            was_typing: false,
            menu_from_keyboard: false,
            graphics,
            frame_stats: graphics::FrameStats::from_env(),
            screenshot,
        };
        app.add_paths(paths);
        app
    }

    fn add_paths(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let pending = self.files.add_paths(paths);
        for &id in &pending {
            if let Some(entry) = self.files.get(id) {
                self.inspector.request(id, entry.path.clone());
            }
        }
        if self.settings.auto_start && !pending.is_empty() {
            self.start(&pending);
        }
    }

    fn pick_files(&mut self) {
        let picked = rfd::FileDialog::new()
            .set_title("Add images")
            .add_filter("PNG, APNG, GIF, and WebP images", &FileType::EXTENSIONS)
            .add_filter("All files", &["*"])
            .pick_files();
        self.add_paths(picked.unwrap_or_default());
    }

    fn pick_folder(&mut self) {
        if let Some(folder) = rfd::FileDialog::new()
            .set_title("Add a folder of images")
            .pick_folder()
        {
            self.add_paths(vec![folder]);
        }
    }

    /// Hands files to the runner and starts the elapsed-time clock if nothing was running.
    fn start(&mut self, ids: &[JobId]) {
        if !self.busy() {
            self.batch_started = Some(Instant::now());
            self.batch_finished = None;
        }
        self.submit(ids);
    }

    fn submit(&mut self, ids: &[JobId]) {
        let settings = Arc::new(self.settings.clone());
        let jobs: Vec<Job> = ids
            .iter()
            .filter_map(|&id| self.files.get(id))
            .map(|entry| Job {
                id: entry.id,
                path: entry.path.clone(),
                output_name: Some(entry.output_name.clone()),
                replaces: entry.converted_to.clone(),
                settings: Arc::clone(&settings),
            })
            .collect();
        // All at once, so they split the CPU's threads evenly.
        self.runner.submit_all(jobs);
        self.files.mark_queued(ids);
    }

    fn start_pending(&mut self) {
        let ids = self.files.pending_ids();
        if !ids.is_empty() {
            self.start(&ids);
        }
    }

    /// Stops everything: queued files go back to waiting, and running ones are cancelled
    /// without writing anything.
    fn stop(&mut self) {
        let ids = self.runner.cancel_queued();
        self.files.mark_pending(&ids);
        let running = self.runner.cancel_running();
        self.files.mark_stopping(&running);
    }

    /// Starts one file from its row menu with the current settings, again if it already ran.
    fn restart(&mut self, id: JobId) {
        let finished = self.files.get(id).is_some_and(|e| e.status.is_finished());
        if !self.files.prepare_restart(id) {
            return;
        }
        if finished && let Some(entry) = self.files.get(id) {
            // Optimizing in place changed the file; read what it is now.
            self.inspector.request(id, entry.path.clone());
        }
        self.start(&[id]);
    }

    /// Stops one file from its row menu, whether it is queued or running.
    fn stop_one(&mut self, id: JobId) {
        match self.runner.cancel(id) {
            Cancel::Dequeued => self.files.mark_pending(&[id]),
            Cancel::Stopping => self.files.mark_stopping(&[id]),
            Cancel::NotFound => {}
        }
    }

    fn busy(&self) -> bool {
        let totals = self.files.totals();
        totals.running + totals.queued > 0
    }

    fn drain_events(&mut self) {
        let now = Instant::now();
        while let Ok(event) = self.events.try_recv() {
            self.files.apply(event, now);
        }
        while let Ok((id, facts)) = self.inspected.try_recv() {
            // A file that can't be inspected is reported when it is optimized.
            if let Ok(facts) = facts {
                self.files.set_facts(id, facts);
            }
        }
        if self.batch_started.is_some() && self.batch_finished.is_none() && !self.busy() {
            self.batch_finished = Some(now);
        }
    }
}

impl eframe::App for OxidropApp {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Switches the swapchain, created without vsync, to vsync (see `graphics`). Cheap
        // once it is set.
        frame.set_wgpu_surface_config(graphics::surface_config());
        if let Some(stats) = &mut self.frame_stats {
            if let Some(line) = stats.frame(Instant::now()) {
                eprintln!("{line}");
            }
            ctx.request_repaint();
        }
        self.drain_events();
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        self.add_paths(dropped);
        if ctx.input_mut(|i| i.consume_shortcut(&ADD_FILES)) {
            self.pick_files();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&START)) {
            self.start_pending();
        }
        if self.screenshot.as_ref().is_some_and(Screenshot::is_due) && self.selected.is_none() {
            // Show the details panel in screenshots, for the largest finished file.
            self.selected = self
                .files
                .entries()
                .iter()
                .filter_map(|e| match &e.status {
                    Status::Done { outcome, .. } if outcome.improved() => {
                        Some((outcome.original_size, e.id))
                    }
                    _ => None,
                })
                .max()
                .map(|(_, id)| id);
        }
        if let Some(screenshot) = &mut self.screenshot {
            screenshot.tick(ctx);
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.close_about_on_escape(ui.ctx());
        egui::Panel::top("menu_bar").show(ui, |ui| self.menu_bar(ui));
        // The same margin above and below the buttons. (Spacing added inside the panel
        // isn't even: egui puts its usual gap between items after the button row only.)
        let toolbar_frame =
            egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(8, 4));
        egui::Panel::top("toolbar")
            .frame(toolbar_frame)
            .show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status_bar").show(ui, |ui| self.status_bar(ui));
        // Fixed width: the settings fit it, and wider would only add empty space.
        egui::Panel::left("settings")
            .resizable(false)
            .exact_size(280.0)
            .show(ui, |ui| {
                ScrollArea::vertical().show(ui, |ui| self.settings_panel(ui));
            });
        if self.selected.is_some_and(|id| self.files.get(id).is_some()) {
            egui::Panel::bottom("details")
                .resizable(true)
                .default_size(440.0)
                .show(ui, |ui| self.details_panel(ui));
        }
        egui::CentralPanel::default().show(ui, |ui| {
            if self.files.entries().is_empty() {
                Self::empty_state(ui);
            } else {
                self.file_table(ui);
            }
        });
        self.about_window(ui.ctx());
        self.was_typing = ui.ctx().text_edit_focused();

        let hovered = ui.input(|i| i.raw.hovered_files.clone());
        widgets::drop_overlay(ui.ctx(), &hovered);
        if self.busy() {
            // Keeps elapsed times ticking; progress events also trigger repaints.
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, SETTINGS_KEY, &self.settings);
    }
}
