//! App shell: owns the player, prefs, clip library, scanner and the views.

use crate::library::{self, Library};
use crate::media::{self, CompressCtl, MediaMsg, TrimJob};
use crate::playback::Playback;
use crate::player::{Player, Start};
use crate::prefs::{self, JsonGet, Prefs};
use crate::scanner::{self, Msg, Scanner};
use crate::sync::{self, Cloud, Phone, PhoneSnap};
use crate::theme::{self, c};
use crate::ui::clip_grid::{Badges, ClipGrid, GridAction};
use crate::ui::dialogs::{CategorizeDialog, CompressAction, CompressDialog, Confirm, ConfirmDialog, RenameDialog, ShareDialog};
use crate::ui::header::{Header, HeaderAction};
use crate::ui::player_view::{ClipState, PlayerView, ViewAction, SPEEDS};
use crate::ui::settings::{SettingsAction, SettingsPage, SettingsValues};
use crate::tray::{Tray, TrayMsg};
use crate::ui::titlebar::{self, TitleAction, TitleBar};
use crate::window;
use crate::tracks::AudioTracks;
use eframe::{egui, glow};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

/// Python `_SEE_MORE` choices for "Load more".
const SEE_MORE_STEPS: [usize; 6] = [25, 50, 100, 200, 500, 1000];
/// New durations are written to prefs at most this often (Python saved after every single one).
const DUR_SAVE_DELAY: Duration = Duration::from_secs(2);

const GREEN: egui::Color32 = egui::Color32::from_rgb(0x7e, 0xc9, 0x7e);
const YELLOW: egui::Color32 = egui::Color32::from_rgb(0xc8, 0xc8, 0x64);
const RED: egui::Color32 = egui::Color32::from_rgb(0xe0, 0x55, 0x55);

pub struct FeatherApp {
    player: Result<Player, String>,
    /// Second mpv instance that preloads the clip under the mouse (paused at its start point,
    /// tracks applied); clicking that clip swaps it in, so it shows at once.
    standby: Option<Player>,
    /// What the standby player was opened with: (clip, start point).
    preload: Option<(String, Start)>,
    /// Clip card under the mouse and since when.
    hover: Option<(String, Instant)>,
    /// Audio track layout of every clip mpv has opened (this session), so its audio graph can
    /// be set up before the file opens next time.
    track_cache: HashMap<String, Vec<crate::player::audio::Track>>,
    /// Video area size in pixels (off-screen renders use it, so a swap needs no re-allocate).
    video_px: (i32, i32),
    /// `track_cache` has new entries to save (at most every 2 s).
    tracks_dirty: Option<Instant>,
    pb: Playback,
    prefs: Prefs,
    lib: Library,
    scanner: Scanner,
    view: PlayerView,
    grid: ClipGrid,
    header: Header,
    /// Audio tracks editor (Python `_at_*`).
    tracks: AudioTracks,
    start_always: bool,
    start_zero: HashSet<String>,
    /// Saved playback speed per clip (settings `clip_speeds`), 1x clips are not listed.
    clip_speed: HashMap<String, f64>,
    trim_gpu: bool,
    dur_dirty: Option<Instant>,
    /// Scan ids started as silent refreshes (keep the open clip).
    auto_scans: HashSet<u64>,
    rename: Option<RenameDialog>,
    categorize: Option<CategorizeDialog>,
    /// "Apply trim" confirmation for this clip path.
    confirm_trim: Option<(ConfirmDialog, String)>,
    trim_busy: bool,
    compress: Option<(CompressDialog, CompressCtl)>,
    media_tx: Sender<MediaMsg>,
    media_rx: Receiver<MediaMsg>,
    egui_ctx: egui::Context,
    cloud: Cloud,
    phone: Phone,
    settings: SettingsPage,
    settings_open: bool,
    settings_seen: bool,
    recycle_bin: bool,
    /// Delete question: (dialog, clips, clip -> its cloud copy).
    confirm_delete: Option<(ConfirmDialog, Vec<String>, HashMap<String, Option<String>>)>,
    confirm_cloud_delete: Option<ConfirmDialog>,
    error_dialog: Option<ConfirmDialog>,
    win: Win,
    /// Settings `auto_refresh` (Python `_folder_watch_tick`).
    auto_refresh: bool,
    /// A folder scan has finished at least once (Python `_loaded_once`).
    loaded_once: bool,
    watch: Watch,
    bg_tx: Sender<BgMsg>,
    bg_rx: Receiver<BgMsg>,
    /// Settings "Thumbnail cache" line and how many files it counted.
    cache_info: String,
    cache_count: usize,
    confirm_cache: Option<ConfirmDialog>,
    share: Option<ShareDialog>,
    fit: Fit,
    /// Thumbnails were cleared: reload them when Settings closes (Python `_thumbs_dirty`).
    thumbs_dirty: bool,
}

/// egui id of the clip sidebar (its width lives in egui's panel state).
const CLIPS_PANEL: &str = "clips";
/// Python `SIDEBAR_MAX_W` and the pane minimums (left 400, sidebar 220).
const SIDEBAR_MAX_W: f32 = 440.0;
const LEFT_MIN: f32 = 400.0;
const RIGHT_MIN: f32 = 220.0;

/// Python `_sync_sash_16_9` / `_fit_video_16_9` state.
struct Fit {
    start: Instant,
    /// The remembered window size wins over the startup fit.
    had_saved_size: bool,
    startup_done: bool,
    /// Window size the sidebar was last fitted for (Python `_sash_last_state`).
    last_state: Option<egui::Vec2>,
    /// A new window size and since when it has been stable (fitted once it settles, like
    /// Python fits once the window edge is released).
    pending: Option<(egui::Vec2, Instant)>,
    /// Sidebar width to force for one frame.
    sidebar_w: Option<f32>,
}

/// Results from background threads.
enum BgMsg {
    /// Folder watch scan: (path, size) of every clip file.
    Watch(Vec<(String, u64)>),
    /// Cache info text + file count.
    CacheInfo(String, usize),
    CacheCleared,
}

/// Python `_folder_watch_tick` / `_watch_apply` state.
struct Watch {
    next: Instant,
    busy: bool,
    prev: HashMap<String, u64>,
    last_trigger: Option<std::collections::BTreeSet<String>>,
}

/// Main-window state (step 8): title bar, tray, close/quit, remembered size.
struct Win {
    hwnd: windows_sys::Win32::Foundation::HWND,
    titlebar: TitleBar,
    tray: Tray,
    restore_rx: Receiver<()>,
    /// Hidden in the tray (Python `_hidden_to_tray`).
    hidden: bool,
    /// Quitting for real: let the close through.
    quitting: bool,
    close_to_tray: bool,
    remember_size: bool,
    startup_on: bool,
    /// settings `window`: (w, h, max).
    saved: Option<(i64, i64, bool)>,
    /// The saved size is applied on the first frame; until then (and 800 ms after) nothing is
    /// written, so the saved size isn't overwritten before it was restored.
    restore_pending: bool,
    size_ready_at: Option<Instant>,
    last_rect: Option<(i32, i32, i32, i32)>,
    size_save_due: Option<Instant>,
    /// eframe shows the window after its first frame whatever the builder said: a --minimized`r
    /// start hides it again for a few frames.
    rehide_frames: u8,
}

impl Win {
    fn size_ready(&self) -> bool {
        self.size_ready_at.is_some_and(|t| Instant::now() >= t)
    }
}

/// Folder from a dialog, written like Tk's askdirectory ("Z:/All clips/Clips"), since folder
/// strings are part of every clip path key in prefs.
fn tk_dir(p: PathBuf) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Audio track layouts per clip (ffprobe / mpv), so a clip's audio mix can be set up before
/// mpv opens it. Only the Rust app uses this file.
fn track_cache_file() -> std::path::PathBuf {
    prefs::data_dir().join("rs_audio_tracks.json")
}

fn load_track_cache() -> HashMap<String, Vec<crate::player::audio::Track>> {
    let Ok(text) = std::fs::read_to_string(track_cache_file()) else { return HashMap::new() };
    let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&text) else { return HashMap::new() };
    m.into_iter()
        .filter(|(p, _)| std::path::Path::new(p).exists())
        .filter_map(|(p, v)| {
            let tracks = v
                .as_array()?
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let s = |k: usize| t.get(k).and_then(|x| x.as_str()).unwrap_or("").to_owned();
                    crate::player::audio::Track { id: i as i64 + 1, title: s(0), handler: s(1) }
                })
                .collect();
            Some((p, tracks))
        })
        .collect()
}

fn save_track_cache(cache: &HashMap<String, Vec<crate::player::audio::Track>>) {
    let m: serde_json::Map<String, Value> = cache.iter().map(|(p, tr)| (p.clone(), Value::Array(tr.iter().map(|t| serde_json::json!([t.title, t.handler])).collect()))).collect();
    let tmp = track_cache_file().with_extension("json.tmp");
    if std::fs::write(&tmp, Value::Object(m).to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, track_cache_file());
    }
}

/// Play these track items unless they are already what the player has (set up before the
/// file opened, or by the preload). None = mpv's first track at 100 %.
fn apply_if_changed(p: &mut Player, items: Option<Vec<(i64, f64)>>) {
    if items == p.applied {
        return;
    }
    match items {
        Some(i) => p.apply_tracks(&i),
        None => {
            if let Some(first) = p.tracks.first().map(|t| t.id) {
                p.apply_tracks(&[(first, 1.0)]);
            }
            p.applied = None;
        }
    }
}

impl FeatherApp {
    pub fn new(cc: &eframe::CreationContext<'_>, lib: Option<PathBuf>, file: Option<String>, minimized: bool) -> Self {
        theme::apply(&cc.egui_ctx);
        let hwnd = window::hwnd_of(cc);
        window::apply_dark_frame(hwnd, theme::bg());
        let (restore_tx, restore_rx) = std::sync::mpsc::channel();
        window::start_restore_listener(restore_tx, cc.egui_ctx.clone());
        let prefs = Prefs::load();
        let st = prefs::read_settings();
        let step = st.get_i64("see_more_step", 50);
        let step = SEE_MORE_STEPS.iter().copied().find(|s| *s as i64 == step).unwrap_or(50);
        let clip_speed = st
            .get("clip_speeds")
            .and_then(|v| v.as_object())
            .map(|m| m.iter().filter_map(|(k, v)| v.as_f64().filter(|s| SPEEDS.contains(s) && *s != 1.0).map(|s| (k.clone(), s))).collect())
            .unwrap_or_default();
        let standby = Player::start(cc, lib.clone(), prefs.volume as f64).ok();
        let mut player = Player::start(cc, lib, prefs.volume as f64);
        let mut view = PlayerView::default();
        if let (Ok(p), Some(f)) = (&mut player, file) {
            p.open(&f);
            view.now_playing = scanner::basename(&f).to_owned();
        }
        let (media_tx, media_rx) = std::sync::mpsc::channel();
        let (bg_tx, bg_rx) = std::sync::mpsc::channel();
        let mut app = FeatherApp {
            player,
            standby,
            preload: None,
            hover: None,
            track_cache: load_track_cache(),
            video_px: (1280, 720),
            tracks_dirty: None,
            pb: Playback::new(st.get_bool("loop_clip", false)),
            lib: Library::new(step),
            scanner: Scanner::new(cc.egui_ctx.clone()),
            view,
            grid: ClipGrid::default(),
            header: Header::default(),
            tracks: AudioTracks::load(&st),
            start_always: st.get_bool("always_start_beginning", false),
            start_zero: st.get_str_list("start_from_beginning_clips").into_iter().collect(),
            clip_speed,
            trim_gpu: st.get_bool("trim_use_gpu", true),
            dur_dirty: None,
            auto_scans: HashSet::new(),
            rename: None,
            categorize: None,
            confirm_trim: None,
            trim_busy: false,
            compress: None,
            media_tx,
            media_rx,
            egui_ctx: cc.egui_ctx.clone(),
            cloud: Cloud::new(cc.egui_ctx.clone()),
            phone: Phone::new(),
            settings: SettingsPage::default(),
            settings_open: false,
            settings_seen: false,
            recycle_bin: st.get_bool("recycle_bin", true),
            confirm_delete: None,
            confirm_cloud_delete: None,
            error_dialog: None,
            prefs,
            auto_refresh: st.get_bool("auto_refresh", true),
            loaded_once: false,
            // Python: first check 4 s after start, then every 3 s
            watch: Watch { next: Instant::now() + Duration::from_secs(4), busy: false, prev: HashMap::new(), last_trigger: None },
            bg_tx,
            bg_rx,
            cache_info: "Calculating\u{2026}".into(),
            cache_count: 0,
            confirm_cache: None,
            share: None,
            fit: Fit {
                start: Instant::now(),
                had_saved_size: st.get_bool("remember_window_size", true) && saved_window(&st).is_some(),
                startup_done: false,
                last_state: None,
                pending: None,
                sidebar_w: None,
            },
            thumbs_dirty: false,
            win: Win {
                hwnd,
                titlebar: TitleBar::default(),
                tray: Tray::new(cc.egui_ctx.clone()),
                restore_rx,
                hidden: false,
                quitting: false,
                close_to_tray: st.get_bool("close_to_tray", false),
                remember_size: st.get_bool("remember_window_size", true),
                startup_on: window::startup_enabled(),
                saved: saved_window(&st),
                restore_pending: true,
                size_ready_at: None,
                last_rect: None,
                size_save_due: None,
                rehide_frames: 0,
            },
        };
        app.load_folders(false);
        if minimized {
            // Python `_start_hidden` (Start with Windows): straight to the tray
            app.win.hidden = true;
            app.win.rehide_frames = 3;
            if !app.win.tray.show() {
                app.win.hidden = false;
                cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            }
        }
        app
    }

    /// Python `_restore_saved_size` (first frame): saved size clamped to the work area, kept
    /// on screen, then maximised if it was.
    fn restore_saved_size(&mut self, ctx: &egui::Context) {
        self.win.restore_pending = false;
        let ready = Instant::now() + Duration::from_millis(800);
        let Some((w, h, max)) = self.win.saved.filter(|_| self.win.remember_size) else {
            self.win.size_ready_at = Some(Instant::now());
            return;
        };
        let hwnd = self.win.hwnd;
        if let (Some((wx, wy, ww, wh)), Some((x, y, _, _))) = (window::work_area(hwnd), window::window_rect(hwnd)) {
            let w = (w as i32).min(ww).max(600);
            let h = (h as i32).min(wh).max(400);
            let x = x.max(wx).min(wx + ww - w);
            let y = y.max(wy).min(wy + wh - h);
            window::set_outer_rect(hwnd, x, y, w, h);
            if max {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
        }
        self.win.size_ready_at = Some(ready);
    }

    /// Python `_save_window_size`: the outer size (the normal size when maximised) + max flag.
    fn save_window_size(&mut self, ctx: &egui::Context) {
        self.win.size_save_due = None;
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        let hwnd = self.win.hwnd;
        if !(self.win.remember_size && self.win.size_ready()) || fullscreen || self.win.hidden || window::is_iconic(hwnd) {
            return;
        }
        let data = if window::is_zoomed(hwnd) {
            window::normal_size(hwnd).map(|(w, h)| (w as i64, h as i64, true))
        } else {
            window::window_rect(hwnd).map(|(_, _, w, h)| (w as i64, h as i64, false))
        };
        if let Some(d @ (w, h, max)) = data {
            if w > 0 && h > 0 && Some(d) != self.win.saved {
                self.win.saved = Some(d);
                prefs::save_setting("window", serde_json::json!({"w": w, "h": h, "max": max}));
            }
        }
    }

    /// Python `on_close` (the X button / Alt+F4): hide to the tray, or quit if that is off.
    fn on_close(&mut self, ctx: &egui::Context) {
        self.save_window_size(ctx);
        if !self.win.close_to_tray {
            self.real_quit(ctx);
            return;
        }
        self.hide_to_tray(ctx);
    }

    /// Python `_real_quit` (also Ctrl+Q and the tray's Quit). Prefs are saved in `on_exit`.
    fn real_quit(&mut self, ctx: &egui::Context) {
        self.save_window_size(ctx);
        self.win.quitting = true;
        self.win.tray.hide();
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Python `on_close` with close-to-tray + `_restore_window`'s reset: the clip is closed,
    /// the window hidden and the tray icon shown.
    fn hide_to_tray(&mut self, ctx: &egui::Context) {
        if let Ok(p) = &mut self.player {
            self.prefs.volume = p.volume.round() as i64;
            p.close();
        }
        self.release_preload(None);
        self.prefs.save();
        self.lib.current = None;
        self.tracks.clip_reset(None);
        self.view.close_audio_menu();
        self.view.now_playing.clear();
        self.view.now_playing_mb.clear();
        self.pb.set_trim_mode(false, None);
        if ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
        if !self.win.tray.show() {
            // Python: no tray icon possible -> quit
            self.real_quit(ctx);
            return;
        }
        self.win.hidden = true;
        window::hide_to_tray(self.win.hwnd);
    }

    /// Python `_restore_window` / the restore signal from a second launch.
    fn show_window(&mut self, ctx: &egui::Context) {
        if self.win.hidden {
            self.win.hidden = false;
            self.win.rehide_frames = 0;
            self.win.tray.hide();
            window::restore_from_tray(self.win.hwnd);
            window::apply_dark_frame(self.win.hwnd, theme::bg());
            ctx.request_repaint();
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// Tray / restore messages, close requests, Ctrl+Q and the size watch. Every frame.
    fn window_tick(&mut self, ctx: &egui::Context) {
        while let Ok(m) = self.win.tray.rx.try_recv() {
            match m {
                TrayMsg::Open => self.show_window(ctx),
                TrayMsg::Quit => self.real_quit(ctx),
            }
        }
        while self.win.restore_rx.try_recv().is_ok() {
            self.show_window(ctx);
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.win.quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.on_close(ctx);
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Q)) {
            self.real_quit(ctx);
        }
        if self.win.hidden {
            return;
        }
        if self.win.restore_pending {
            self.restore_saved_size(ctx);
        }
        // Python `_queue_size_save`: 500 ms after the window stops changing
        if self.win.remember_size && self.win.size_ready() {
            let r = window::window_rect(self.win.hwnd);
            if r != self.win.last_rect {
                self.win.last_rect = r;
                self.win.size_save_due = Some(Instant::now() + Duration::from_millis(500));
            }
        }
        if let Some(due) = self.win.size_save_due {
            let now = Instant::now();
            if now >= due {
                self.save_window_size(ctx);
            } else {
                ctx.request_repaint_after(due - now);
            }
        }
        if let Some(t) = self.win.size_ready_at {
            if Instant::now() < t {
                ctx.request_repaint_after(t - Instant::now());
            }
        }
    }

    /// Python `_load_folders(self._checked_folders, auto)`: active folder first, scan in the
    /// background. `auto` = silent refresh that keeps the open clip.
    fn load_folders(&mut self, auto: bool) {
        let paths: Vec<String> = self.prefs.checked_folders.iter().filter(|p| std::path::Path::new(p).is_dir()).cloned().collect();
        if paths.is_empty() {
            return;
        }
        let active = if paths.contains(&self.prefs.active_folder) { self.prefs.active_folder.clone() } else { paths[0].clone() };
        self.prefs.folder = active.clone();
        let mut ordered = vec![active.clone()];
        ordered.extend(paths.into_iter().filter(|p| *p != active));
        let id = self.scanner.scan(ordered);
        if auto {
            self.auto_scans.insert(id);
        }
    }

    /// Queue thumbnails/durations for the clips that are listed. `new` (clips that appeared since
    /// the last scan) go first; `None` = everything at full speed.
    fn load_thumbs(&mut self, new: Option<&HashSet<String>>) {
        let visible: Vec<String> = self.lib.filtered(&self.prefs).into_iter().take(self.lib.visible_count).cloned().collect();
        let todo: Vec<String> = visible.into_iter().filter(|v| !self.grid.thumbs.contains_key(v) || !self.prefs.has_duration(v) || !self.track_cache.contains_key(v)).collect();
        let (prio, rest) = match new {
            Some(n) => todo.into_iter().partition(|v| n.contains(v)),
            None => (todo, Vec::new()),
        };
        let (prefs, cache) = (&self.prefs, &self.track_cache);
        self.scanner.load_thumbs(prio, rest, |p| !prefs.has_duration(p), |p| !cache.contains_key(p));
    }

    /// Python `_folder_watch_tick`: every 3 s look for clips added to / removed from the
    /// folders (in the background), only while the window is shown.
    fn folder_watch_tick(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if now < self.watch.next {
            ctx.request_repaint_after(self.watch.next - now);
            return;
        }
        self.watch.next = now + Duration::from_secs(3);
        ctx.request_repaint_after(Duration::from_secs(3));
        if !(self.auto_refresh && self.loaded_once && !self.watch.busy) || self.win.hidden || window::is_iconic(self.win.hwnd) {
            return;
        }
        let paths: Vec<String> = self.prefs.checked_folders.iter().filter(|p| std::path::Path::new(p).is_dir()).cloned().collect();
        if paths.is_empty() {
            return;
        }
        self.watch.busy = true;
        let (tx, ctx) = (self.bg_tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let sig = sync::scan_sig(&paths, "").into_iter().map(|(p, size, _)| (p, size)).collect();
            let _ = tx.send(BgMsg::Watch(sig));
            ctx.request_repaint();
        });
    }

    /// Python `_watch_apply`: refresh once the set of clips changed and every new clip has
    /// stopped growing (a recording that is still being written keeps getting bigger).
    fn watch_apply(&mut self, sig: Vec<(String, u64)>) {
        self.watch.busy = false;
        let sig: HashMap<String, u64> = sig.into_iter().collect();
        let prev = std::mem::replace(&mut self.watch.prev, sig.clone());
        let cur: std::collections::BTreeSet<String> = sig.keys().cloned().collect();
        let known: std::collections::BTreeSet<String> = self.lib.videos.iter().cloned().collect();
        if cur == known {
            self.watch.last_trigger = None;
            return;
        }
        if cur.difference(&known).any(|p| prev.get(p) != sig.get(p) || sig[p] == 0) {
            return;
        }
        if self.watch.last_trigger.as_ref() == Some(&cur) {
            return; // already refreshed for exactly this state
        }
        self.watch.last_trigger = Some(cur);
        self.load_folders(true);
    }

    /// Python `_refresh_cache_info` (background): thumbnails of the loaded clips.
    fn refresh_cache_info(&mut self) {
        self.cache_info = "Calculating\u{2026}".into();
        let videos = self.lib.videos.clone();
        let (tx, ctx) = (self.bg_tx.clone(), self.egui_ctx.clone());
        std::thread::spawn(move || {
            let dir = scanner::cache_dir();
            let files: Vec<u64> = videos.iter().filter_map(|v| scanner::cache_key(v)).filter_map(|k| std::fs::metadata(dir.join(k)).ok()).filter(|m| m.is_file()).map(|m| m.len()).collect();
            let total: u64 = files.iter().sum();
            let n = files.len();
            let txt = if videos.is_empty() {
                "No folder loaded".to_owned()
            } else if n == 0 {
                "No cached thumbnails for the current folder".to_owned()
            } else {
                let size = if total >= 1 << 30 {
                    format!("{:.2} GB", total as f64 / (1u64 << 30) as f64)
                } else if total >= 1 << 20 {
                    format!("{:.1} MB", total as f64 / (1u64 << 20) as f64)
                } else {
                    format!("{} KB", (total / 1024).max(1))
                };
                format!("{n} thumbnail{} \u{2022} {size} for the current folder", if n != 1 { "s" } else { "" })
            };
            let _ = tx.send(BgMsg::CacheInfo(txt, n));
            ctx.request_repaint();
        });
    }

    /// Python `_clear_thumbnail_cache` (after the question): delete them in the background.
    fn clear_thumbnail_cache(&mut self) {
        let videos = self.lib.videos.clone();
        let (tx, ctx) = (self.bg_tx.clone(), self.egui_ctx.clone());
        std::thread::spawn(move || {
            let dir = scanner::cache_dir();
            for k in videos.iter().filter_map(|v| scanner::cache_key(v)) {
                let _ = std::fs::remove_file(dir.join(k));
            }
            let _ = tx.send(BgMsg::CacheCleared);
            ctx.request_repaint();
        });
    }

    /// Python `_nav_clip`: Left = newer, Right = older clip in the current filter; with no
    /// clip open (or the open one filtered out) the first one.
    fn nav_clip(&mut self, step: i32) {
        let list: Vec<String> = self.lib.filtered(&self.prefs).into_iter().cloned().collect();
        if list.is_empty() {
            return;
        }
        let pos = self.lib.current.as_ref().and_then(|c| list.iter().position(|v| v == c));
        match pos {
            None => self.play(list[0].clone()),
            Some(i) => {
                let j = i as i64 + step as i64;
                if j >= 0 && (j as usize) < list.len() {
                    self.play(list[j as usize].clone());
                }
            }
        }
    }

    fn handle_bg(&mut self) {
        while let Ok(m) = self.bg_rx.try_recv() {
            match m {
                BgMsg::Watch(sig) => self.watch_apply(sig),
                BgMsg::CacheInfo(t, n) => {
                    self.cache_info = t;
                    self.cache_count = n;
                }
                BgMsg::CacheCleared => {
                    self.grid.thumbs.clear();
                    self.thumbs_dirty = true; // reloaded when Settings closes
                    self.refresh_cache_info();
                }
            }
        }
    }

    fn handle_messages(&mut self, ctx: &egui::Context) {
        self.handle_bg();
        while let Ok(m) = self.scanner.rx.try_recv() {
            match m {
                Msg::Scan(r) => {
                    if r.scan_id != self.scanner.current_scan() {
                        continue; // superseded by a newer refresh
                    }
                    let auto = self.auto_scans.remove(&r.scan_id);
                    self.loaded_once = true;
                    let new: HashSet<String> = self.lib.apply_scan(r, &mut self.prefs, auto).into_iter().collect();
                    let current: HashSet<&String> = self.lib.videos.iter().collect();
                    self.grid.thumbs.retain(|k, _| current.contains(k));
                    if !auto {
                        self.view.now_playing.clear();
                        self.view.now_playing_mb.clear();
                        self.pb.set_trim_mode(false, None);
                    }
                    self.load_thumbs(Some(&new));
                    self.prefs.save();
                }
                Msg::Thumb(t) => {
                    if let Some(tr) = t.tracks {
                        self.track_cache.insert(t.path.clone(), tr);
                        self.tracks_dirty.get_or_insert_with(Instant::now);
                    }
                    if let Some(d) = &t.dur {
                        if !self.prefs.has_duration(&t.path) {
                            self.prefs.set_duration(&t.path, d);
                            self.dur_dirty.get_or_insert_with(Instant::now);
                        }
                    }
                    if let Some(img) = t.image {
                        let tex = ctx.load_texture(format!("thumb:{}", t.path), img, egui::TextureOptions::LINEAR);
                        self.grid.thumbs.insert(t.path, tex);
                    }
                }
            }
        }
        if let Some(since) = self.dur_dirty {
            if since.elapsed() >= DUR_SAVE_DELAY {
                self.dur_dirty = None;
                self.prefs.save();
            } else {
                ctx.request_repaint_after(DUR_SAVE_DELAY - since.elapsed());
            }
        }
        if let Some(since) = self.tracks_dirty {
            if since.elapsed() >= DUR_SAVE_DELAY {
                self.tracks_dirty = None;
                save_track_cache(&self.track_cache);
            } else {
                ctx.request_repaint_after(DUR_SAVE_DELAY - since.elapsed());
            }
        }
        while let Ok(m) = self.media_rx.try_recv() {
            self.media_message(m);
        }
    }

    fn media_message(&mut self, m: MediaMsg) {
        match m {
            MediaMsg::Flash(t, [r, g, b]) => self.view.flash(t, egui::Color32::from_rgb(r, g, b)),
            MediaMsg::ReleaseFile(path) => {
                if let Ok(p) = &mut self.player {
                    if p.path.as_deref() == Some(path.as_str()) {
                        p.stop();
                    }
                }
                self.release_preload(Some(&path));
            }
            MediaMsg::TrimCopy(new, err) => {
                // Python `_finish_trim_copy`
                self.trim_busy = false;
                match new {
                    Some(n) => {
                        self.view.flash(format!("Saved copy: {}", scanner::basename(&n)), GREEN);
                        self.mark_start_zero(&n);
                        self.load_folders(true);
                    }
                    None => self.view.flash(cut60(&format!("Copy failed: {err}")), RED),
                }
            }
            MediaMsg::TrimCut(path, ok, err) => {
                // Python `_finish_trim_cut`
                self.trim_busy = false;
                if ok {
                    // the trim is now baked in: the whole clip is the trim
                    self.mark_start_zero(&path);
                    self.prefs.remove_trim(&path);
                    self.prefs.trim_enabled.remove(&path);
                    self.prefs.remove_duration(&path);
                    self.grid.thumbs.remove(&path);
                    self.prefs.save();
                    self.view.flash("Trim applied", GREEN);
                } else {
                    self.view.flash(cut60(&format!("Trim failed: {err}")), RED);
                }
                if self.lib.current.as_deref() == Some(path.as_str()) {
                    self.update_trim_mode();
                    if let Ok(p) = &mut self.player {
                        p.open_clip(&path, Start::Beginning, None, true, None);
                    }
                    if let Ok(m) = std::fs::metadata(&path) {
                        self.view.now_playing_mb = format!("{:.1} MB", m.len() as f64 / (1024.0 * 1024.0));
                    }
                }
                self.load_folders(true);
            }
            MediaMsg::Progress(f, text) => {
                if let Some((d, _)) = &mut self.compress {
                    d.progress = Some(f);
                    d.status = text;
                    d.status_color = theme::text_muted();
                }
            }
            MediaMsg::CompressedOne(src, ok, dst) => {
                if let (true, Some(dst)) = (ok, &dst) {
                    self.copy_clip_settings(&src, dst);
                }
                if let Some((d, _)) = &mut self.compress {
                    d.done.insert(src, ok);
                }
            }
            MediaMsg::CompressDone(done, failed, cancelled) => {
                if let Some((d, _)) = &mut self.compress {
                    d.encoding = false;
                    let n = d.targets.len();
                    if !cancelled {
                        if !failed.is_empty() {
                            d.status = format!("Done \u{2014} {done}/{n} compressed, {} failed", failed.len());
                            d.status_color = c("#e0a05f");
                        } else {
                            let suffix = if n == 1 { "saved next to the original" } else { "saved next to the originals" };
                            d.status = format!("Done  \u{2022}  {done}/{n} compressed  \u{2022}  {suffix}");
                            d.status_color = c("#FFFFFF");
                        }
                        d.progress = Some(1.0);
                    }
                }
                if !cancelled {
                    self.load_folders(false);
                    self.lib.selected.clear();
                }
            }
        }
    }

    /// Python `_mark_start_zero`: this clip opens at 0:00 from now on.
    fn mark_start_zero(&mut self, path: &str) {
        self.start_zero.insert(path.to_owned());
        let mut v: Vec<&String> = self.start_zero.iter().collect();
        v.sort();
        prefs::save_setting("start_from_beginning_clips", Value::Array(v.into_iter().map(|s| Value::String(s.clone())).collect()));
    }

    /// Python `_copy_clip_settings`: a compressed copy keeps the source's tracks, volumes, speed.
    fn copy_clip_settings(&mut self, src: &str, dst: &str) {
        self.tracks.copy_clip(src, dst);
        if let Some(s) = self.clip_speed.get(src).copied() {
            self.clip_speed.insert(dst.to_owned(), s);
            let m: serde_json::Map<String, Value> = self.clip_speed.iter().map(|(k, v)| (k.clone(), Value::from(*v))).collect();
            prefs::save_setting("clip_speeds", Value::Object(m));
        }
    }

    /// Python `_update_trim_mode_for_current`.
    fn update_trim_mode(&mut self) {
        match &self.lib.current {
            Some(path) => {
                let enabled = self.prefs.trim_enabled.contains(path);
                let saved = self.prefs.trim_raw(path).unwrap_or((0.0, 10000.0));
                self.pb.set_trim_mode(enabled, Some(saved));
            }
            None => self.pb.set_trim_mode(false, None),
        }
    }

    /// Where a clip opens: trim start, the beginning, or the middle (Python `_play_index`).
    fn start_for(&self, path: &str) -> Start {
        match self.prefs.trim(path) {
            Some((s, _)) if self.prefs.trim_enabled.contains(path) => Start::Trim(s),
            _ if self.start_always || self.start_zero.contains(path) => Start::Beginning,
            _ => Start::Middle,
        }
    }

    /// The track items for `path` when its track layout is known (empty = mpv's first track).
    fn preset_for(&self, path: &str) -> Option<Vec<(i64, f64)>> {
        let tracks = self.track_cache.get(path)?;
        let mut t = self.tracks.clone();
        Some(t.on_tracks(Some(path), tracks).unwrap_or_default())
    }

    /// Preload the clip card that has been under the mouse for a moment into the standby
    /// player, and keep that player's state up to date.
    fn preload_tick(&mut self, ctx: &egui::Context) {
        if self.standby.is_none() {
            return;
        }
        let now = Instant::now();
        match (&self.grid.hovered, &self.hover) {
            (Some(h), Some((cur, _))) if h == cur => {}
            (h, _) => self.hover = h.clone().map(|h| (h, now)),
        }
        const DELAY: Duration = Duration::from_millis(120);
        let mut open = None;
        if let Some((h, since)) = &self.hover {
            if self.lib.current.as_deref() != Some(h.as_str()) {
                if now - *since < DELAY {
                    ctx.request_repaint_after(DELAY - (now - *since));
                } else {
                    let start = self.start_for(h);
                    if self.preload.as_ref() != Some(&(h.clone(), start)) {
                        let dur = self.prefs.duration(h).and_then(scanner::parse_dur).filter(|d| *d > 0.0);
                        open = Some((h.clone(), start, dur, self.preset_for(h)));
                    }
                }
            }
        }
        let Some(sb) = &mut self.standby else { return };
        if let Some((h, start, dur, preset)) = open {
            sb.open_clip(&h, start, dur, false, preset.as_deref());
            self.preload = Some((h, start));
        }
        sb.poll();
        if sb.tracks_new {
            // set up its tracks now (on a copy of the track state: the open clip keeps its own)
            sb.tracks_new = false;
            if let Some(path) = sb.path.clone() {
                if self.track_cache.insert(path, sb.tracks.clone()).is_none() {
                    self.tracks_dirty.get_or_insert_with(Instant::now);
                }
            }
            let mut t = self.tracks.clone();
            let items = t.on_tracks(sb.path.as_deref(), &sb.tracks);
            apply_if_changed(sb, items);
            sb.tracks_handled = true;
        }
        if self.preload.is_some() && !sb.tracks_handled {
            ctx.request_repaint_after(Duration::from_millis(20)); // keep polling while it loads
        }
    }

    /// The standby player lets go of `path` (file about to be changed or deleted).
    fn release_preload(&mut self, path: Option<&str>) {
        if let Some(sb) = &mut self.standby {
            if path.is_none() || sb.path.as_deref() == path {
                sb.stop();
                self.preload = None;
            }
        }
    }

    /// Python `_play_index`.
    fn play(&mut self, path: String) {
        let start = self.start_for(&path);
        let Ok(p) = &mut self.player else { return };
        if self.lib.current.as_deref() == Some(path.as_str()) && p.playing() {
            return;
        }
        self.pb.reset_for_clip();
        self.tracks.clip_reset(Some(&path));
        self.view.close_audio_menu();
        // speed belongs to the clip: its own saved speed, otherwise 1x
        let speed = self.clip_speed.get(&path).copied().unwrap_or(1.0);
        let preloaded = self.preload.as_ref().is_some_and(|(pp, s)| *pp == path && *s == start)
            && self.standby.as_ref().is_some_and(|s| s.path.as_deref() == Some(path.as_str()));
        if let (true, Some(mut sb)) = (preloaded, self.standby.take()) {
            // swap the preloaded player in; the old one lets go of its clip and waits
            sb.set_volume(p.volume);
            sb.set_speed(speed);
            p.pause();
            p.stop();
            std::mem::swap(p, &mut sb);
            self.standby = Some(sb);
            self.preload = None;
            if p.tracks_handled {
                // the app's track state follows the new clip (same rules as the preload used)
                let items = self.tracks.on_tracks(Some(&path), &p.tracks);
                apply_if_changed(p, items);
            }
            p.start_when_ready();
        } else {
            p.set_speed(speed);
            let known_dur = self.prefs.duration(&path).and_then(scanner::parse_dur).filter(|d| *d > 0.0);
            let preset = self.track_cache.get(&path).map(|tr| self.tracks.clone().on_tracks(Some(&path), tr).unwrap_or_default());
            let Ok(p) = &mut self.player else { return };
            p.open_clip(&path, start, known_dur, true, preset.as_deref());
        }
        let name = scanner::basename(&path);
        let info = self.lib.info.get(&path);
        let ts = info.and_then(|i| i.clip_dt).map(|d| d.format("%d.%m.%Y %H:%M").to_string()).unwrap_or_default();
        // category label (keyword group / rename) first, like Python `_format_now_playing`
        let game = library::category_key_for_video(&path, &self.prefs.keyword_groups)
            .and_then(|k| self.lib.cats.norm_map.get(&k).cloned())
            .unwrap_or_else(|| scanner::game_name_from_filename(name));
        self.view.now_playing = if ts.is_empty() { game } else { format!("{game}  {ts}") };
        self.view.now_playing_mb = info.map(|i| format!("{:.1} MB", i.size as f64 / (1024.0 * 1024.0))).unwrap_or_default();
        self.lib.current = Some(path);
        self.update_trim_mode();
    }

    fn view_action(&mut self, a: ViewAction) {
        let items = match a {
            ViewAction::AudioToggle(n) => Some(self.tracks.toggle_track(n)),
            ViewAction::AudioVol(n, v, fin) => Some(self.tracks.set_vol(n, v, fin)),
            ViewAction::AudioReset => Some(self.tracks.reset()),
            _ => None,
        };
        if let Some(items) = items {
            if let (Some(items), Ok(p)) = (items, &mut self.player) {
                p.apply_tracks(&items);
            }
            return;
        }
        let Some(path) = self.lib.current.clone() else {
            match a {
                ViewAction::SetSpeed(s) => {
                    if let Ok(p) = &mut self.player {
                        p.set_speed(s);
                    }
                }
                ViewAction::ChooseStarFolder => {
                    self.choose_star_folder(true);
                }
                _ => {}
            }
            return;
        };
        match a {
            ViewAction::AudioToggle(_) | ViewAction::AudioVol(..) | ViewAction::AudioReset => {} // handled above
            ViewAction::ToggleStar => {
                // Python `_toggle_star`: save first, then copy to the starred-clips folder
                let newly = !self.prefs.starred.remove(&path);
                if newly {
                    self.prefs.starred.insert(path.clone());
                }
                self.prefs.save();
                if newly && self.prefs.star_copy_enabled {
                    let folder = self.choose_star_folder(false);
                    if !folder.is_empty() {
                        let trim = if self.prefs.trim_enabled.contains(&path) { self.prefs.trim_raw(&path) } else { None };
                        sync::copy_starred(path, folder, trim, scanner::find_ffmpeg(), self.media_tx.clone(), self.egui_ctx.clone());
                    }
                }
            }
            ViewAction::ChooseStarFolder => {
                self.choose_star_folder(true);
            }
            ViewAction::ToggleLike => {
                if !self.prefs.liked.remove(&path) {
                    self.prefs.liked.insert(path);
                }
                self.prefs.save();
            }
            ViewAction::ToggleTrim => {
                // Python `_toggle_trim`: turning trim off forgets the trim range
                if self.prefs.trim_enabled.remove(&path) {
                    self.prefs.remove_trim(&path);
                } else {
                    self.prefs.trim_enabled.insert(path);
                }
                self.prefs.save();
                self.update_trim_mode();
            }
            ViewAction::SetSpeed(s) => {
                if let Ok(p) = &mut self.player {
                    p.set_speed(s);
                }
                if s != 1.0 {
                    self.clip_speed.insert(path, s);
                } else {
                    self.clip_speed.remove(&path);
                }
                let m: serde_json::Map<String, Value> = self.clip_speed.iter().map(|(k, v)| (k.clone(), Value::from(*v))).collect();
                prefs::save_setting("clip_speeds", Value::Object(m));
            }
            ViewAction::TrimSaved(s, e) => {
                // Python `_apply_trim`
                if s == 0.0 && e == 10000.0 {
                    self.prefs.remove_trim(&path);
                } else {
                    self.prefs.set_trim(&path, s, e);
                }
                self.prefs.save();
            }
        }
    }

    /// Python `_apply_trim_cut` (the checks and the question).
    fn ask_apply_trim(&mut self) {
        let Some(path) = self.lib.current.clone() else { return };
        if self.trim_busy {
            return;
        }
        let rng = self.prefs.trim_raw(&path);
        let has = self.prefs.trim_enabled.contains(&path) && rng.is_some_and(|(s, e)| !(s <= 0.0 && e >= 10000.0));
        if !has {
            self.view.flash("Nothing to trim - move a trim handle first", YELLOW);
            return;
        }
        if scanner::find_ffmpeg().is_none() {
            self.view.flash("ffmpeg not found", RED);
            return;
        }
        let d = ConfirmDialog {
            title: "Apply trim".into(),
            head: "Cut this clip down to the trimmed part?".into(),
            details: vec![scanner::basename(&path).to_owned()],
            note: "Apply trim deletes the parts outside the trim from the file and can't be undone. Save as copy keeps the original clip and makes a new trimmed clip next to it.".into(),
            yes_text: "Apply trim".into(),
            extra2_text: Some("Save as copy".into()),
            extra2_neutral: true,
            ..Default::default()
        };
        self.confirm_trim = Some((d, path));
    }

    fn start_trim(&mut self, ctx: &egui::Context, path: String, as_copy: bool) {
        let (Some(range), Some(ff)) = (self.prefs.trim_raw(&path), scanner::find_ffmpeg()) else { return };
        self.trim_busy = true;
        self.view.flash(if as_copy { "Saving trimmed copy\u{2026}" } else { "Cutting clip\u{2026}" }, YELLOW);
        media::start_trim(TrimJob { path, range, as_copy, use_gpu: self.trim_gpu, ffmpeg: ff }, self.media_tx.clone(), ctx.clone());
    }

    /// Python `_open_compress_dialog`: the selected clips, else the open one.
    fn open_compress(&mut self) {
        let targets: Vec<String> = if !self.lib.selected.is_empty() {
            self.lib.videos.iter().filter(|v| self.lib.selected.contains(*v)).cloned().collect()
        } else if let Some(c) = &self.lib.current {
            vec![c.clone()]
        } else {
            return;
        };
        if let Ok(p) = &mut self.player {
            if p.playing() {
                p.pause();
            }
        }
        let thumb = if targets.len() == 1 { self.grid.thumbs.get(&targets[0]).cloned() } else { None };
        self.compress = Some((CompressDialog::new(targets, thumb), CompressCtl::new()));
    }

    /// Python `_fit_video_16_9` (once, 0.5 s after start: window height so the video area is
    /// 16:9, unless a remembered size was restored) and `_sync_sash_16_9` (after the window
    /// size changes, not in the first 3 s: the clip sidebar gets whatever width makes the
    /// video exactly 16:9, within its limits).
    fn fit_16_9(&mut self, ctx: &egui::Context, video: egui::Rect, maximized: bool) {
        if video.width() < 50.0 || video.height() < 50.0 {
            return;
        }
        let now = Instant::now();
        let ar = 16.0 / 9.0;
        if !self.fit.startup_done {
            if now - self.fit.start < Duration::from_millis(500) {
                ctx.request_repaint_after(Duration::from_millis(500) - (now - self.fit.start));
            } else {
                self.fit.startup_done = true;
                if !maximized && !self.fit.had_saved_size && !window::is_iconic(self.win.hwnd) {
                    if let Some((x, y, w, h)) = window::window_rect(self.win.hwnd) {
                        let ppp = ctx.pixels_per_point();
                        let fixed_h = h as f32 - video.height() * ppp;
                        let target_h = (video.width() * ppp / ar).round() as i32 + fixed_h.round() as i32;
                        if (target_h - h).abs() > 1 {
                            window::set_outer_rect(self.win.hwnd, x, y, w, target_h);
                        }
                    }
                }
            }
        }
        let state = ctx.content_rect().size();
        if self.fit.last_state == Some(state) {
            self.fit.pending = None;
            return;
        }
        if now - self.fit.start < Duration::from_secs(3) {
            self.fit.last_state = Some(state); // startup: leave the layout as it is
            return;
        }
        match self.fit.pending {
            Some((s, since)) if s == state => {
                if now - since < Duration::from_millis(150) {
                    ctx.request_repaint_after(Duration::from_millis(150) - (now - since));
                    return;
                }
            }
            _ => {
                self.fit.pending = Some((state, now));
                ctx.request_repaint_after(Duration::from_millis(150));
                return;
            }
        }
        self.fit.pending = None;
        self.fit.last_state = Some(state);
        let pane_w = state.x;
        let mut want = (video.height() * ar).round();
        want = want.max(pane_w - SIDEBAR_MAX_W);
        want = want.min(pane_w - RIGHT_MIN).max(LEFT_MIN);
        let sidebar = pane_w - want;
        let cur = egui::containers::panel::PanelState::load(ctx, egui::Id::new(CLIPS_PANEL)).map(|s| s.rect.width()).unwrap_or(300.0);
        if (cur - sidebar).abs() > 1.0 {
            self.fit.sidebar_w = Some(sidebar.clamp(RIGHT_MIN, SIDEBAR_MAX_W));
            ctx.request_repaint();
        }
    }

    /// Python `_open_share_dialog`: the open clip, cut to the trim slider's range, playback
    /// paused while the dialog is open.
    fn open_share(&mut self, ctx: &egui::Context) {
        let Some(src) = self.lib.current.clone() else { return };
        // no ffmpeg: the dialog opens and says "Encoding failed", like Python
        let ff = scanner::find_ffmpeg().unwrap_or_else(|| PathBuf::from("ffmpeg"));
        let Ok(p) = &mut self.player else { return };
        if p.playing() {
            p.pause();
        }
        let dur = p.dur;
        let (s, e) = (self.pb.bar.trim_start, self.pb.bar.trim_end);
        let range = if dur > 0.0 { (s / 10000.0 * dur, e / 10000.0 * dur) } else { (0.0, dur) };
        let snap = self.tracks.export_snapshot(&src);
        let thumb = self.grid.thumbs.get(&src).cloned();
        self.share = Some(ShareDialog::new(ctx, src, range, ff, snap, thumb));
    }

    fn header_action(&mut self, ctx: &egui::Context, a: HeaderAction) {
        let pick = || rfd::FileDialog::new().set_title("Select your video folder").pick_folder().map(tk_dir);
        match a {
            HeaderAction::PickFolder => {
                if let Some(path) = pick() {
                    self.lib.visible_count = library::VISIBLE_START;
                    if !self.prefs.saved_folders.contains(&path) {
                        self.prefs.saved_folders.push(path.clone());
                    }
                    self.prefs.checked_folders = vec![path.clone()];
                    self.prefs.active_folder = path;
                    self.prefs.save();
                    self.load_folders(false);
                }
            }
            HeaderAction::AddFolder => {
                if let Some(path) = pick() {
                    if !self.prefs.saved_folders.contains(&path) {
                        self.prefs.saved_folders.push(path.clone());
                    }
                    if !self.prefs.checked_folders.contains(&path) {
                        self.prefs.checked_folders.push(path.clone());
                    }
                    self.prefs.active_folder = path;
                    self.prefs.save();
                    self.load_folders(false);
                }
            }
            HeaderAction::ToggleFolder(p) => {
                let checked = &mut self.prefs.checked_folders;
                if checked.contains(&p) {
                    // the last folder can't be unchecked
                    if checked.len() > 1 {
                        checked.retain(|f| *f != p);
                        self.prefs.active_folder = checked[0].clone();
                    }
                } else {
                    checked.retain(|f| *f != p);
                    checked.insert(0, p.clone());
                    self.prefs.active_folder = p;
                }
                self.prefs.save();
                self.load_folders(false);
            }
            HeaderAction::Refresh => self.load_folders(false),
            HeaderAction::Categorize => self.categorize = Some(CategorizeDialog::new(self.lib.current.as_deref())),
            HeaderAction::ShowInFolder => {
                if let Some(p) = &self.lib.current {
                    // raw_arg: `.arg()` would quote the whole "/select,path" (paths with spaces),
                    // which explorer doesn't understand and opens Documents instead
                    use std::os::windows::process::CommandExt;
                    let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", p.replace('/', "\\"))).spawn();
                }
            }
            HeaderAction::Compress => self.open_compress(),
            HeaderAction::Share => self.open_share(ctx),
            HeaderAction::ApplyTrim => self.ask_apply_trim(),
            HeaderAction::Delete => self.ask_delete(),
            HeaderAction::Settings => {
                if !self.settings_seen {
                    // Python pauses when the page is first built
                    self.settings_seen = true;
                    if let Ok(p) = &mut self.player {
                        if p.playing() {
                            p.pause();
                        }
                    }
                }
                self.settings_open = true;
                self.win.startup_on = window::startup_enabled();
                self.refresh_cache_info();
            }
        }
        let _ = ctx;
    }

    /// Python `_choose_star_folder`: the starred-clips folder, asking for it when none is set
    /// (or it is gone). `force` always asks (right click on the star).
    fn choose_star_folder(&mut self, force: bool) -> String {
        let cur = self.prefs.star_copy_folder.clone();
        if !cur.is_empty() && !force {
            if std::path::Path::new(&cur).is_dir() {
                return cur;
            }
            self.view.flash("Folder missing \u{2014} choose it again", RED);
        }
        let mut dlg = rfd::FileDialog::new().set_title("Choose a folder for starred clips");
        if std::path::Path::new(&cur).is_dir() {
            dlg = dlg.set_directory(&cur);
        }
        if let Some(p) = dlg.pick_folder() {
            let path = p.to_string_lossy().into_owned();
            self.prefs.star_copy_folder = path.clone();
            self.prefs.save();
            self.view.flash(format!("Starred clips folder: {}", scanner::basename(&path)), GREEN);
        }
        self.prefs.star_copy_folder.clone()
    }

    /// Python `_delete_selected` (the question).
    fn ask_delete(&mut self) {
        let names: Vec<String> = if !self.lib.selected.is_empty() {
            self.lib.selected.iter().cloned().collect()
        } else if let Some(c) = &self.lib.current {
            vec![c.clone()]
        } else {
            self.view.flash("Open or select a clip to delete first", YELLOW);
            return;
        };
        let n = names.len();
        let noun = if n == 1 { "clip" } else { "clips" };
        let total: u64 = names.iter().filter_map(|x| std::fs::metadata(x).ok()).map(|m| m.len()).sum();
        let size_txt = if total >= 1 << 30 { format!("{:.2} GB", total as f64 / (1u64 << 30) as f64) } else { format!("{:.1} MB", total as f64 / (1u64 << 20) as f64) };
        let (head, mut note) = if self.recycle_bin {
            let n_perm = names.iter().filter(|x| !sync::can_recycle(x)).count();
            let note = if n_perm > 0 {
                format!("{n_perm} of them {} on a drive without a Recycle Bin and will be deleted permanently.", if n_perm == 1 { "is" } else { "are" })
            } else {
                "You can restore them from the Recycle Bin.".to_owned()
            };
            (format!("Move {n} {noun} ({size_txt}) to the Recycle Bin?"), note)
        } else {
            (format!("Permanently delete {n} {noun} ({size_txt})?"), "This can't be undone.".to_owned())
        };
        let cloud_map: HashMap<String, Option<String>> = names.iter().map(|x| (x.clone(), self.cloud.copy_of(x))).collect();
        let n_cloud = cloud_map.values().filter(|v| v.is_some()).count();
        if n_cloud > 0 {
            let extra = if n_cloud == n {
                format!("{} a copy in your cloud folder.", if n == 1 { "It also has" } else { "They also have" })
            } else {
                format!("{n_cloud} of them also {} a copy in your cloud folder.", if n_cloud == 1 { "has" } else { "have" })
            };
            note = format!("{note}\n{extra}");
        }
        let d = ConfirmDialog {
            title: "Delete clips".into(),
            head,
            details: names.iter().take(8).map(|x| scanner::basename(x).to_owned()).collect(),
            more: n.saturating_sub(8),
            note,
            yes_text: "Delete".into(),
            extra_text: (n_cloud > 0).then(|| "Delete + from cloud".into()),
            extra2_text: (n_cloud > 0).then(|| "Only from cloud".into()),
            ..Default::default()
        };
        self.confirm_delete = Some((d, names, cloud_map));
    }

    /// Python `_delete_selected` (after the answer) / `_delete_cloud_copies_only`.
    fn do_delete(&mut self, choice: Confirm, names: Vec<String>, cloud_map: HashMap<String, Option<String>>) {
        let mut errors = Vec::new();
        if choice == Confirm::Extra2 {
            let mut done = 0;
            for path in &names {
                let Some(Some(target)) = cloud_map.get(path) else { continue };
                match std::fs::remove_file(target) {
                    Ok(()) => {
                        self.cloud.mark_removed(path);
                        done += 1;
                    }
                    Err(e) => errors.push(format!("{} (cloud copy): {e}", scanner::basename(path))),
                }
            }
            self.lib.selected.clear();
            if errors.is_empty() {
                self.view.flash(format!("Removed {done} cloud cop{}", if done == 1 { "y" } else { "ies" }), GREEN);
            } else {
                self.error_dialog = Some(error_dialog("Some cloud copies couldn't be deleted.", errors));
            }
            self.load_folders(false);
            return;
        }
        let with_cloud = choice == Confirm::Extra;
        // mpv keeps the open clip locked: let go of it before deleting
        if let (Ok(p), Some(cur)) = (&mut self.player, &self.lib.current) {
            if names.contains(cur) {
                p.close();
                std::thread::sleep(Duration::from_millis(400));
            }
        }
        // the preloaded clip is locked too
        if self.standby.as_ref().and_then(|s| s.path.as_ref()).is_some_and(|sp| names.contains(sp)) {
            self.release_preload(None);
            std::thread::sleep(Duration::from_millis(200));
        }
        for path in &names {
            // the thumbnail name depends on size/date: work it out before the file goes
            let thumb = scanner::cache_key(path).map(|k| scanner::cache_dir().join(k));
            let res = if self.recycle_bin && sync::can_recycle(path) {
                if sync::recycle_file(path) { Ok(()) } else { Err("could not move it to the Recycle Bin".to_owned()) }
            } else {
                std::fs::remove_file(path).map_err(|e| e.to_string())
            };
            if let Err(e) = res {
                errors.push(format!("{}: {e}", scanner::basename(path)));
                continue;
            }
            if let Some(t) = thumb {
                let _ = std::fs::remove_file(t);
            }
            if let (true, Some(Some(target))) = (with_cloud, cloud_map.get(path)) {
                match std::fs::remove_file(target) {
                    Ok(()) => self.cloud.forget(path),
                    Err(e) => errors.push(format!("{} (cloud copy): {e}", scanner::basename(path))),
                }
            }
        }
        self.lib.selected.clear();
        if !errors.is_empty() {
            self.error_dialog = Some(error_dialog("Some clips couldn't be deleted.", errors));
        }
        self.load_folders(false);
    }

    fn settings_action(&mut self, ctx: &egui::Context, a: SettingsAction) {
        match a {
            SettingsAction::Close => {
                self.settings_open = false;
                if std::mem::take(&mut self.thumbs_dirty) {
                    self.load_thumbs(None);
                }
            }
            SettingsAction::Theme(name) => {
                theme::switch_theme(ctx, &name);
                window::apply_dark_frame(self.win.hwnd, theme::bg());
            }
            SettingsAction::Accent(color) => {
                theme::set_custom_accent(ctx, &theme::active_name(), color.as_deref());
                window::apply_dark_frame(self.win.hwnd, theme::bg());
            }
            SettingsAction::AutoRefresh(on) => {
                // Python `_on_auto_refresh_toggle`
                self.auto_refresh = on;
                prefs::save_setting("auto_refresh", Value::Bool(on));
                self.watch.last_trigger = None;
            }
            SettingsAction::SeeMore(dir) => {
                // Python `_change_see_more`: nearest step, then one up / down
                let cur = self.lib.see_more_step as i64;
                let i = (0..SEE_MORE_STEPS.len()).min_by_key(|k| (SEE_MORE_STEPS[*k] as i64 - cur).abs()).unwrap_or(1) as i32;
                let i = (i + dir).clamp(0, SEE_MORE_STEPS.len() as i32 - 1) as usize;
                self.lib.see_more_step = SEE_MORE_STEPS[i];
                prefs::save_setting("see_more_step", Value::from(SEE_MORE_STEPS[i]));
            }
            SettingsAction::ClearCache => {
                if !self.lib.videos.is_empty() && self.cache_count > 0 {
                    self.confirm_cache = Some(ConfirmDialog {
                        title: "Clear thumbnail cache".into(),
                        head: format!("Are you sure you want to delete the cached thumbnails for the {} clips in the current folder?", self.lib.videos.len()),
                        note: "They're recreated automatically the next time they're needed. Your clips aren't touched.".into(),
                        yes_text: "Clear".into(),
                        ..Default::default()
                    });
                }
            }
            SettingsAction::StarCopy(on) => {
                self.prefs.star_copy_enabled = on;
                self.prefs.save();
            }
            SettingsAction::ChooseStarFolder => {
                let mut dlg = rfd::FileDialog::new().set_title("Choose a folder for starred clips");
                if std::path::Path::new(&self.prefs.star_copy_folder).is_dir() {
                    dlg = dlg.set_directory(&self.prefs.star_copy_folder);
                }
                if let Some(p) = dlg.pick_folder() {
                    self.prefs.star_copy_folder = p.to_string_lossy().into_owned();
                    self.prefs.save();
                }
            }
            SettingsAction::CloudToggle(on) => {
                // turning it on needs a folder first
                if on && !std::path::Path::new(&self.cloud.folder).is_dir() && !self.choose_cloud_folder() {
                    return;
                }
                self.cloud.set_enabled(on);
            }
            SettingsAction::ChooseCloudFolder => {
                self.choose_cloud_folder();
            }
            SettingsAction::CloudMode(m) => self.cloud.set_mode(m),
            SettingsAction::CloudGame(k) => self.cloud.pick_game(&k),
            SettingsAction::CloudSync => {
                let folders = self.prefs.checked_folders.clone();
                if !self.cloud.sync_now(&folders, &self.prefs.keyword_groups) && self.choose_cloud_folder() {
                    self.cloud.sync_now(&folders, &self.prefs.keyword_groups);
                }
            }
            SettingsAction::CloudDeleteAll => match self.cloud.count_cloud_clips() {
                Err(e) => self.cloud.set_status(&e, true),
                Ok(0) => self.cloud.set_status("No clips in the cloud folder", true),
                Ok(n) => {
                    self.confirm_cloud_delete = Some(ConfirmDialog {
                        title: "Delete cloud clips".into(),
                        head: format!("Delete all {n} clip{} in the cloud folder?", if n == 1 { "" } else { "s" }),
                        note: "Your original clips are not touched. This can't be undone.".into(),
                        ..Default::default()
                    });
                }
            },
            SettingsAction::PhoneToggle(on) => self.phone.set_enabled(on),
            SettingsAction::PhoneKind(k) => self.phone.toggle_kind(k),
            SettingsAction::PhoneSync => {
                let p = &self.prefs;
                let trims = p.trim_enabled.iter().filter_map(|t| p.trim_raw(t).map(|r| (t.clone(), r))).collect();
                let snap = PhoneSnap {
                    kinds: self.phone.kinds.clone(),
                    starred: p.starred.clone(),
                    liked: p.liked.clone(),
                    trim_enabled: p.trim_enabled.clone(),
                    trims,
                    folders: p.checked_folders.clone(),
                    groups: p.keyword_groups.clone(),
                    labels: self.lib.cats.norm_map.clone(),
                };
                self.phone.sync_now(snap, scanner::find_ffmpeg(), ctx.clone());
            }
            SettingsAction::Loop(on) => {
                self.pb.loop_clip = on;
                prefs::save_setting("loop_clip", Value::Bool(on));
            }
            SettingsAction::StartAlways(on) => {
                self.start_always = on;
                prefs::save_setting("always_start_beginning", Value::Bool(on));
            }
            SettingsAction::TrimGpu(on) => {
                self.trim_gpu = on;
                prefs::save_setting("trim_use_gpu", Value::Bool(on));
            }
            SettingsAction::RecycleBin(on) => {
                self.recycle_bin = on;
                prefs::save_setting("recycle_bin", Value::Bool(on));
            }
            SettingsAction::RememberSize(on) => {
                // Python `_on_remember_size_toggle`
                self.win.remember_size = on;
                prefs::save_setting("remember_window_size", Value::Bool(on));
                if on {
                    self.win.size_ready_at = Some(Instant::now());
                    self.save_window_size(ctx);
                }
            }
            SettingsAction::CloseToTray(on) => {
                self.win.close_to_tray = on;
                prefs::save_setting("close_to_tray", Value::Bool(on));
            }
            SettingsAction::AudioEnabled(on) => {
                self.tracks.set_enabled(on);
                if !on {
                    self.view.close_audio_menu();
                }
            }
            SettingsAction::SteelFirst(on) => self.tracks.set_steel_first(on),
            SettingsAction::DefaultTrack(n) => self.tracks.default_tick(n),
            SettingsAction::DefaultVol(n, v, fin) => {
                if let (Some(items), Ok(p)) = (self.tracks.default_vol(n, v, fin), &mut self.player) {
                    p.apply_tracks(&items);
                }
            }
            SettingsAction::StartWithWindows(on) => {
                // Python `_on_startup_toggle`: if the registry can't be written the switch
                // shows the real state again
                window::set_startup(on);
                self.win.startup_on = window::startup_enabled();
            }
        }
    }

    /// Python `_choose_cloud_folder`. False if cancelled.
    fn choose_cloud_folder(&mut self) -> bool {
        let mut dlg = rfd::FileDialog::new().set_title("Choose your cloud folder (e.g. Google Drive)");
        if std::path::Path::new(&self.cloud.folder).is_dir() {
            dlg = dlg.set_directory(&self.cloud.folder);
        }
        match dlg.pick_folder() {
            Some(p) => {
                self.cloud.set_folder(p.to_string_lossy().into_owned());
                true
            }
            None => false,
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(d) = &mut self.rename {
            if !d.show(ctx, &mut self.prefs, &mut self.lib) {
                self.rename = None;
            }
        }
        if let Some(d) = &mut self.categorize {
            let (open, changed) = d.show(ctx, &mut self.prefs, &mut self.lib);
            if changed {
                self.load_thumbs(None);
            }
            if !open {
                self.categorize = None;
            }
        }
        if let Some((d, path)) = &self.confirm_trim {
            if let Some(choice) = d.show(ctx) {
                let path = path.clone();
                self.confirm_trim = None;
                match choice {
                    Confirm::Yes => self.start_trim(ctx, path, false),
                    Confirm::Extra2 => self.start_trim(ctx, path, true),
                    Confirm::Extra | Confirm::No => {}
                }
            }
        }
        if let Some((d, _, _)) = &self.confirm_delete {
            if let Some(choice) = d.show(ctx) {
                let (_, names, map) = self.confirm_delete.take().unwrap();
                if choice != Confirm::No {
                    self.do_delete(choice, names, map);
                }
            }
        }
        if let Some(d) = &self.confirm_cloud_delete {
            if let Some(choice) = d.show(ctx) {
                self.confirm_cloud_delete = None;
                if choice == Confirm::Yes {
                    self.cloud.delete_all();
                }
            }
        }
        if let Some(d) = &mut self.share {
            if !d.show(ctx) {
                self.share = None;
            }
        }
        if let Some(d) = &self.confirm_cache {
            if let Some(choice) = d.show(ctx) {
                self.confirm_cache = None;
                if choice == Confirm::Yes {
                    self.clear_thumbnail_cache();
                }
            }
        }
        if let Some(d) = &self.error_dialog {
            if d.show(ctx).is_some() {
                self.error_dialog = None;
            }
        }
        if let Some((d, ctl)) = &mut self.compress {
            match d.show(ctx) {
                Some(CompressAction::Start(mb)) => match scanner::find_ffmpeg() {
                    Some(ff) => {
                        d.encoding = true;
                        d.progress = Some(0.0);
                        media::start_compress(d.targets.clone(), mb, ff, ctl.clone(), self.media_tx.clone(), ctx.clone());
                    }
                    None => {
                        d.status = "ffmpeg not found".into();
                        d.status_color = RED;
                    }
                },
                Some(CompressAction::Close) => {
                    ctl.cancel();
                    self.compress = None;
                }
                None => {}
            }
        }
    }
}

/// Python `_dark_dialog(kind="error")` for failed deletions.
fn error_dialog(head: &str, errors: Vec<String>) -> ConfirmDialog {
    ConfirmDialog {
        title: "Some deletions failed".into(),
        head: head.into(),
        more: errors.len().saturating_sub(8),
        details: errors.into_iter().take(8).collect(),
        error: true,
        ..Default::default()
    }
}

/// settings `window` = {"w", "h", "max"}; only used when w and h are set (Python `_saved_win`).
fn saved_window(st: &serde_json::Map<String, Value>) -> Option<(i64, i64, bool)> {
    let w = st.get("window")?.as_object()?;
    let num = |k: &str| w.get(k).and_then(|v| v.as_f64()).map(|f| f as i64).filter(|n| *n != 0);
    Some((num("w")?, num("h")?, w.get("max").and_then(|v| v.as_bool()).unwrap_or(false)))
}

/// Python `f"..."[:60]`.
fn cut60(s: &str) -> String {
    s.chars().take(60).collect()
}

impl eframe::App for FeatherApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // players that aren't on screen still have to render when mpv asks (else libmpv waits
        // 200 ms per frame): the preloading one, and the open one while its clip loads
        if let Some(gl) = frame.gl() {
            let (w, h) = self.video_px;
            if let Some(sb) = &self.standby {
                sb.render_offscreen(gl, w, h);
            }
            if let Ok(p) = &self.player {
                if !p.loaded {
                    p.render_offscreen(gl, w, h);
                }
            }
        }
        // TEMP: FP_DEBUG_FRAMES=1 logs frames per second and what asked for them
        if std::env::var_os("FP_DEBUG_FRAMES").is_some() {
            let id = egui::Id::new("dbg_frames");
            let (mut n, mut t0, mut evs): (u32, Option<Instant>, String) = ctx.data(|d| d.get_temp(id)).unwrap_or((0, None, String::new()));
            n += 1;
            let kinds: Vec<String> = ctx.input(|i| i.raw.events.iter().map(|e| format!("{e:?}").chars().take(40).collect()).collect());
            let causes: Vec<String> = ctx.repaint_causes().iter().map(|c| c.to_string().trim().to_owned()).collect();
            evs.push_str(&format!("[{}|{}]", if kinds.is_empty() { "-".to_owned() } else { kinds.join(",") }, causes.join(" ")));
            let start = *t0.get_or_insert_with(Instant::now);
            if start.elapsed() >= Duration::from_secs(1) {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("target/frames_debug.txt") {
                    let causes: Vec<String> = ctx.repaint_causes().iter().map(|c| c.to_string()).collect();
                    let mpv_cb = crate::mpv::UPDATE_CALLS.swap(0, std::sync::atomic::Ordering::Relaxed);
                    let _ = writeln!(f, "{n} frames/s hidden={} mpv_update_cb={mpv_cb} causes={causes:?} events={evs}", self.win.hidden);
                }
                n = 0;
                t0 = Some(Instant::now());
                evs.clear();
            }
            ctx.data_mut(|d| d.insert_temp(id, (n, t0, evs)));
        }
        self.window_tick(ctx);
        self.handle_messages(ctx);
        if self.win.hidden {
            if self.win.rehide_frames > 0 {
                // eframe shows the window after frame 1 (and winit re-applies its own styles
                // then), so `--minimized` hides it again for a few frames
                self.win.rehide_frames -= 1;
                window::hide_to_tray(self.win.hwnd);
                ctx.request_repaint();
            } else if !window::is_iconic(self.win.hwnd) {
                // restored from outside (a second launch's ShowWindow nudge without the
                // restore port): leave the tray properly
                self.show_window(ctx);
                return;
            }
            // in the tray: no UI, but cloud sync keeps running (Python `_cloud_tick`)
            if let Ok(p) = &mut self.player {
                p.poll();
            }
            self.cloud.tick(&self.prefs.checked_folders, &self.prefs.keyword_groups);
            ctx.request_repaint_after(Duration::from_secs(5));
            return;
        }
        // title bar first so it spans the whole width; gone in fullscreen
        let (fullscreen, maximized) = ctx.input(|i| (i.viewport().fullscreen.unwrap_or(false), i.viewport().maximized.unwrap_or(false)));
        if !fullscreen {
            match self.win.titlebar.show(ctx, maximized) {
                Some(TitleAction::Minimize) => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                Some(TitleAction::ToggleMaximize) => ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized)),
                Some(TitleAction::Close) => self.on_close(ctx),
                None => {}
            }
            if !maximized {
                titlebar::resize_edges(ctx);
            }
        }
        let p = match &mut self.player {
            Ok(p) => p,
            Err(e) => {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.heading("Could not start mpv");
                    ui.add_space(8.0);
                    ui.add(egui::TextEdit::multiline(&mut e.as_str()).desired_width(f32::INFINITY).desired_rows(12));
                    if ui.button("Copy error").clicked() {
                        ctx.copy_text(e.clone());
                    }
                });
                return;
            }
        };
        p.poll();
        if p.tracks_new {
            // Python _on_track_list_changed: the clip's tracks are known, play the chosen ones
            p.tracks_new = false;
            let path = p.path.clone();
            if let Some(path) = &path {
                if self.track_cache.insert(path.clone(), p.tracks.clone()).is_none() {
                    self.tracks_dirty.get_or_insert_with(Instant::now);
                }
            }
            let items = self.tracks.on_tracks(path.as_deref(), &p.tracks);
            apply_if_changed(p, items);
            p.tracks_handled = true;
        }
        // a clip opens paused and starts once its tracks are set up
        p.autoplay_tick();
        self.pb.tick(p);
        // cloud sync every 5 s, also while the settings page is open
        self.cloud.tick(&self.prefs.checked_folders, &self.prefs.keyword_groups);
        ctx.request_repaint_after(Duration::from_secs(5));
        self.folder_watch_tick(ctx);

        if self.settings_open {
            let star_folder = self.prefs.star_copy_folder.clone();
            let v = SettingsValues {
                star_copy: self.prefs.star_copy_enabled,
                star_folder: &star_folder,
                loop_clip: self.pb.loop_clip,
                start_always: self.start_always,
                trim_gpu: self.trim_gpu,
                recycle_bin: self.recycle_bin,
                remember_size: self.win.remember_size,
                close_to_tray: self.win.close_to_tray,
                start_with_windows: self.win.startup_on,
                audio: &self.tracks,
                auto_refresh: self.auto_refresh,
                see_more_step: self.lib.see_more_step,
                cache_info: &self.cache_info,
            };
            let acts = self.settings.show(ctx, &v, &self.cloud, &mut self.phone, &self.lib);
            for a in acts {
                self.settings_action(ctx, a);
            }
            self.dialogs(ctx);
            return;
        }
        let p = self.player.as_mut().unwrap();
        if let Some(step) = self.view.input(ctx, p, &mut self.pb) {
            self.nav_clip(step);
        }

        // Python `_enter_fullscreen`: everything except the video leaves the layout
        let mut grid_acts = Vec::new();
        let mut header_acts = Vec::new();
        let mut view_acts = Vec::new();
        self.grid.hovered = None; // set again by the clip list when it is shown
        if !fullscreen {
            // right: clip list (declared first so the header only spans the left side)
            let badges = Badges { audio: &self.tracks.badges };
            let mut panel = egui::SidePanel::right(CLIPS_PANEL).resizable(true).default_width(300.0).width_range(220.0..=440.0);
            if let Some(w) = self.fit.sidebar_w.take() {
                panel = panel.exact_width(w); // 16:9 fit: this frame only, then resizable again
            }
            panel
                .frame(egui::Frame::new().fill(theme::sidebar_bg()).stroke(egui::Stroke::new(1.0_f32, theme::c("#242444"))))
                .show_separator_line(false)
                .show(ctx, |ui| grid_acts = self.grid.show(ui, &mut self.lib, &self.prefs, &badges));

            let trim_mode = self.pb.trim_mode && self.lib.current.is_some();
            egui::TopBottomPanel::top("header")
                .frame(egui::Frame::new().fill(theme::bg()).inner_margin(egui::Margin::symmetric(16, 10)))
                .show_separator_line(false)
                .show(ctx, |ui| header_acts = self.header.show(ui, &self.lib, &self.prefs, trim_mode));

            let clip = match &self.lib.current {
                Some(path) => ClipState {
                    open: true,
                    starred: self.prefs.starred.contains(path),
                    liked: self.prefs.liked.contains(path),
                    trimmed: self.prefs.trim_enabled.contains(path),
                },
                None => ClipState::default(),
            };
            let p = self.player.as_mut().unwrap();
            view_acts = self.view.controls(ctx, p, &mut self.pb, &clip, &self.tracks);
        }
        let p = self.player.as_mut().unwrap();
        let video = self.view.video(ctx, p, &mut self.pb);
        if video.width() > 16.0 && video.height() > 16.0 {
            let ppp = ctx.pixels_per_point();
            self.video_px = ((video.width() * ppp).round() as i32, (video.height() * ppp).round() as i32);
        }
        if !fullscreen {
            self.fit_16_9(ctx, video, maximized);
        }

        for a in view_acts {
            self.view_action(a);
        }
        for a in header_acts {
            self.header_action(ctx, a);
        }
        for a in grid_acts {
            match a {
                GridAction::Play(path) => self.play(path),
                GridAction::ToggleLike(path) => {
                    if !self.prefs.liked.remove(&path) {
                        self.prefs.liked.insert(path);
                    }
                    self.prefs.save();
                }
                GridAction::ListChanged => self.load_thumbs(None),
                GridAction::SelectGame(key) => {
                    self.prefs.game_filter = key;
                    self.prefs.game_multi.clear();
                    self.lib.visible_count = library::VISIBLE_START;
                    self.load_thumbs(None);
                }
                GridAction::ToggleGameMulti(key) => {
                    // Python `_toggle_game_multi`
                    if key == "All" {
                        self.prefs.game_multi.clear();
                    } else {
                        let gf = self.prefs.game_filter.clone();
                        if self.prefs.game_multi.is_empty() && gf != "All" && !gf.is_empty() && self.lib.cats.norm_map.contains_key(&gf) {
                            self.prefs.game_multi.insert(gf); // keep a single left-click choice
                        }
                        if !self.prefs.game_multi.remove(&key) {
                            self.prefs.game_multi.insert(key);
                        }
                    }
                    self.prefs.game_filter = "All".into();
                    self.prefs.save();
                    self.lib.visible_count = library::VISIBLE_START;
                    self.load_thumbs(None);
                }
                GridAction::RenameCategory(key) => self.rename = RenameDialog::new(&key, &self.lib),
            }
        }
        self.preload_tick(ctx);
        self.dialogs(ctx);
    }

    fn on_exit(&mut self, gl: Option<&glow::Context>) {
        if let Some((_, ctl)) = &self.compress {
            ctl.cancel();
        }
        if let Some(d) = &mut self.share {
            d.close();
        }
        if let Ok(p) = &self.player {
            // Python saves prefs in on_close too
            self.prefs.volume = p.volume.round() as i64;
            self.prefs.save();
            p.shutdown(gl);
        }
        if let Some(sb) = &self.standby {
            sb.shutdown(gl);
        }
    }
}
