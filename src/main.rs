//! FeatherPlayer-rs: Rust port of FeatherPlayer (see CLAUDE.md).
//!
//! Modules:
//!   app     - eframe app shell
//!   mpv     - libmpv loaded at runtime, OpenGL render API
//!   player  - playback state/control, audio tracks, video GL blit
//!   ui      - widgets and views
//!   theme   - colors and style
//!   prefs   - settings and data folder (step 2)
//!   scanner - clip folder scanning and thumbnails (step 3)

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod mpv;
mod player;
#[allow(dead_code)]
mod prefs;
#[allow(dead_code)]
mod scanner;
mod theme;
mod ui;

use eframe::egui;
use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let mut file = None;
    let mut lib = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--lib" {
            lib = args.next().map(PathBuf::from);
        } else {
            file = Some(a);
        }
    }
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 720.0])
            .with_min_inner_size([640.0, 400.0])
            .with_title("FeatherPlayer")
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "FeatherPlayer-rs",
        options,
        Box::new(move |cc| Ok(Box::new(app::FeatherApp::new(cc, lib, file)))),
    )
}
