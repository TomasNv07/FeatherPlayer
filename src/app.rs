//! App shell: owns the player, prefs, clip library, scanner and the views.

use crate::library::Library;
use crate::player::{Player, Start};
use crate::prefs::{self, JsonGet, Prefs};
use crate::scanner::{self, Msg, Scanner};
use crate::theme;
use crate::ui::clip_grid::{Badges, ClipGrid, GridAction};
use crate::ui::header::{Header, HeaderAction};
use crate::ui::player_view::PlayerView;
use eframe::{egui, glow};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Python `_SEE_MORE` choices for "Load more".
const SEE_MORE_STEPS: [usize; 6] = [25, 50, 100, 200, 500, 1000];
/// New durations are written to prefs at most this often (Python saved after every single one).
const DUR_SAVE_DELAY: Duration = Duration::from_secs(2);

pub struct FeatherApp {
    player: Result<Player, String>,
    prefs: Prefs,
    lib: Library,
    scanner: Scanner,
    view: PlayerView,
    grid: ClipGrid,
    header: Header,
    /// Clips with their own audio tracks / volumes (music badge).
    audio_badges: HashSet<String>,
    start_always: bool,
    start_zero: HashSet<String>,
    dur_dirty: Option<Instant>,
}

/// Folder from a dialog, written like Tk's askdirectory ("Z:/All clips/Clips"), since folder
/// strings are part of every clip path key in prefs.
fn tk_dir(p: PathBuf) -> String {
    p.to_string_lossy().replace('\\', "/")
}

impl FeatherApp {
    pub fn new(cc: &eframe::CreationContext<'_>, lib: Option<PathBuf>, file: Option<String>) -> Self {
        theme::apply(&cc.egui_ctx);
        let prefs = Prefs::load();
        let st = prefs::read_settings();
        let step = st.get_i64("see_more_step", 50);
        let step = SEE_MORE_STEPS.iter().copied().find(|s| *s as i64 == step).unwrap_or(50);
        let keys = |k: &str| st.get(k).and_then(|v| v.as_object()).map(|m| m.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();
        let audio_badges = keys("clip_audio_tracks").into_iter().chain(keys("clip_audio_volumes")).collect();
        let mut player = Player::start(cc, lib, prefs.volume as f64);
        let mut view = PlayerView::default();
        if let (Ok(p), Some(f)) = (&mut player, file) {
            p.open(&f);
            view.now_playing = scanner::basename(&f).to_owned();
        }
        let mut app = FeatherApp {
            player,
            lib: Library::new(step),
            scanner: Scanner::new(cc.egui_ctx.clone()),
            view,
            grid: ClipGrid::default(),
            header: Header::default(),
            audio_badges,
            start_always: st.get_bool("always_start_beginning", false),
            start_zero: st.get_str_list("start_from_beginning_clips").into_iter().collect(),
            dur_dirty: None,
            prefs,
        };
        app.load_folders();
        app
    }

    /// Python `_load_folders(self._checked_folders)`: active folder first, scan in the background.
    fn load_folders(&mut self) {
        let paths: Vec<String> = self.prefs.checked_folders.iter().filter(|p| std::path::Path::new(p).is_dir()).cloned().collect();
        if paths.is_empty() {
            return;
        }
        let active = if paths.contains(&self.prefs.active_folder) { self.prefs.active_folder.clone() } else { paths[0].clone() };
        self.prefs.folder = active.clone();
        let mut ordered = vec![active.clone()];
        ordered.extend(paths.into_iter().filter(|p| *p != active));
        self.scanner.scan(ordered);
    }

    /// Queue thumbnails/durations for the clips that are listed. `new` (clips that appeared since
    /// the last scan) go first; `None` = everything at full speed.
    fn load_thumbs(&mut self, new: Option<&HashSet<String>>) {
        let visible: Vec<String> = self.lib.filtered(&self.prefs).into_iter().take(self.lib.visible_count).cloned().collect();
        let todo: Vec<String> = visible.into_iter().filter(|v| !self.grid.thumbs.contains_key(v) || !self.prefs.has_duration(v)).collect();
        let (prio, rest) = match new {
            Some(n) => todo.into_iter().partition(|v| n.contains(v)),
            None => (todo, Vec::new()),
        };
        let prefs = &self.prefs;
        self.scanner.load_thumbs(prio, rest, |p| !prefs.has_duration(p));
    }

    fn handle_messages(&mut self, ctx: &egui::Context) {
        while let Ok(m) = self.scanner.rx.try_recv() {
            match m {
                Msg::Scan(r) => {
                    if r.scan_id != self.scanner.current_scan() {
                        continue; // superseded by a newer refresh
                    }
                    let new: HashSet<String> = self.lib.apply_scan(r).into_iter().collect();
                    let current: HashSet<&String> = self.lib.videos.iter().collect();
                    self.grid.thumbs.retain(|k, _| current.contains(k));
                    self.view.now_playing.clear();
                    self.view.now_playing_mb.clear();
                    self.load_thumbs(Some(&new));
                    self.prefs.save();
                }
                Msg::Thumb(t) => {
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
    }

    /// Python `_play_index`.
    fn play(&mut self, path: String) {
        let Ok(p) = &mut self.player else { return };
        if self.lib.current.as_deref() == Some(path.as_str()) && p.playing() {
            return;
        }
        let start = match self.prefs.trim(&path) {
            Some((s, _)) if self.prefs.trim_enabled.contains(&path) => Start::Trim(s),
            _ if self.start_always || self.start_zero.contains(&path) => Start::Beginning,
            _ => Start::Middle,
        };
        p.open_clip(&path, start);
        let name = scanner::basename(&path);
        let info = self.lib.info.get(&path);
        let ts = info.and_then(|i| i.clip_dt).map(|d| d.format("%d.%m.%Y %H:%M").to_string()).unwrap_or_default();
        let game = scanner::game_name_from_filename(name);
        self.view.now_playing = if ts.is_empty() { game } else { format!("{game}  {ts}") };
        self.view.now_playing_mb = info.map(|i| format!("{:.1} MB", i.size as f64 / (1024.0 * 1024.0))).unwrap_or_default();
        self.lib.current = Some(path);
    }

    fn header_action(&mut self, a: HeaderAction) {
        let pick = || rfd::FileDialog::new().set_title("Select your video folder").pick_folder().map(tk_dir);
        match a {
            HeaderAction::PickFolder => {
                if let Some(path) = pick() {
                    self.lib.visible_count = crate::library::VISIBLE_START;
                    if !self.prefs.saved_folders.contains(&path) {
                        self.prefs.saved_folders.push(path.clone());
                    }
                    self.prefs.checked_folders = vec![path.clone()];
                    self.prefs.active_folder = path;
                    self.prefs.save();
                    self.load_folders();
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
                    self.load_folders();
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
                self.load_folders();
            }
            HeaderAction::Refresh => self.load_folders(),
        }
    }
}

impl eframe::App for FeatherApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_messages(ctx);
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
        self.view.input(ctx, p);

        // right: clip list (declared first so the header only spans the left side)
        let badges = Badges { audio: &self.audio_badges };
        let mut grid_acts = Vec::new();
        egui::SidePanel::right("clips")
            .resizable(true)
            .default_width(300.0)
            .width_range(220.0..=440.0)
            .frame(egui::Frame::new().fill(theme::sidebar_bg()).stroke(egui::Stroke::new(1.0_f32, theme::c("#242444"))))
            .show_separator_line(false)
            .show(ctx, |ui| grid_acts = self.grid.show(ui, &mut self.lib, &self.prefs, &badges));

        let mut header_acts = Vec::new();
        egui::TopBottomPanel::top("header")
            .frame(egui::Frame::new().fill(theme::bg()).inner_margin(egui::Margin::symmetric(16, 10)))
            .show_separator_line(false)
            .show(ctx, |ui| header_acts = self.header.show(ui, &self.lib, &self.prefs));

        let p = self.player.as_mut().unwrap();
        self.view.controls(ctx, p);
        let video = self.view.video(ctx, p);
        self.view.audio_overlay(ctx, p, video);

        for a in header_acts {
            self.header_action(a);
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
            }
        }
    }

    fn on_exit(&mut self, gl: Option<&glow::Context>) {
        if let Ok(p) = &self.player {
            // Python saves prefs in on_close too
            self.prefs.volume = p.volume.round() as i64;
            self.prefs.save();
            p.shutdown(gl);
        }
    }
}
