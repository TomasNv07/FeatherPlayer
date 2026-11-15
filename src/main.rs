//! FeatherPlayer-rs: Rust port of FeatherPlayer (see CLAUDE.md).
//!
//! Modules:
//!   app     - eframe app shell
//!   mpv     - libmpv loaded at runtime, OpenGL render API
//!   player  - playback state/control, audio tracks, video GL blit
//!   ui      - widgets and views
//!   theme   - colors and style
//!   prefs   - data folder, settings.json, clip prefs with backups
//!   scanner - clip folder scanning, ffprobe durations, ffmpeg thumbnails
//!   library - the clip list: sort, filter, visible count, selection
//!   playback - trim mode / loop / seeking rules on top of the player
//!   media   - ffmpeg jobs: apply trim, compress

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod library;
mod media;
mod playback;
mod mpv;
mod player;
#[allow(dead_code)] // API used by later porting steps
mod prefs;
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
            .with_title("Feather Player")
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "FeatherPlayer-rs",
        options,
        Box::new(move |cc| Ok(Box::new(app::FeatherApp::new(cc, lib, file)))),
    )
}
