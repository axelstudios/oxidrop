//! Oxidrop: a drag-and-drop PNG/APNG optimizer built on oxipng, which also converts GIF and
//! WebP files to PNG/APNG.
//!
//! The library holds everything that can be tested without a window; `main.rs` only starts
//! the eframe app.

pub mod app;
pub mod convert;
pub mod format;
pub mod icon;
pub mod inspect;
pub mod optimize;
pub mod png_info;
pub mod progress;
pub mod runner;
pub mod settings;
