// Release builds are GUI-only on Windows: no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

fn main() -> eframe::Result {
    let paths: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    oxidrop::app::run(paths)
}
