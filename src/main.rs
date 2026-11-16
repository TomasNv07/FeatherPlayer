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
//!   sync    - cloud folder sync, phone (ClipSync) sync, starred copy, Recycle Bin
//!   window  - Win32: DWM frame, window rect, single instance, Start with Windows
//!   tray    - tray icon while the window is hidden

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
mod sync;
mod theme;
mod tray;
mod ui;
mod window;

use eframe::egui;
use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let mut file = None;
    let mut lib = None;
    let mut minimized = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--lib" => lib = args.next().map(PathBuf::from),
            "--minimized" => minimized = true, // started by "Start with Windows": go to the tray
            "--relaunch" => {}                 // Python flag, nothing to do here
            _ => file = Some(a),
        }
    }
    window::set_app_id();
    window::refresh_startup_entry();
    if !window::check_single_instance() {
        return Ok(()); // another instance was asked to show itself
    }
    // Python: geometry 1160x650, minsize 860x480; the native frame is replaced by our title bar
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1160.0, 650.0])
        .with_min_inner_size([860.0, 480.0])
        .with_title("Feather Player")
        .with_decorations(false)
        .with_visible(!minimized)
        .with_drag_and_drop(true);
    let (rgba, w, h) = tray::icon_rgba();
    if tray::pic_path("feather2.png").is_some() {
        viewport = viewport.with_icon(egui::IconData { rgba, width: w, height: h });
    }
    let options = eframe::NativeOptions { renderer: eframe::Renderer::Glow, viewport, ..Default::default() };
    eframe::run_native(
        "FeatherPlayer-rs",
        options,
        Box::new(move |cc| Ok(Box::new(app::FeatherApp::new(cc, lib, file, minimized)))),
    )
}
