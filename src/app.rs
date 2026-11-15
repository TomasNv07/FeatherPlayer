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
use crate::ui::dialogs::{CategorizeDialog, CompressAction, CompressDialog, Confirm, ConfirmDialog, RenameDialog};
use crate::ui::header::{Header, HeaderAction};
use crate::ui::player_view::{ClipState, PlayerView, ViewAction, SPEEDS};
use crate::ui::settings::{SettingsAction, SettingsPage, SettingsValues};
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
    pb: Playback,
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
        let clip_speed = st
            .get("clip_speeds")
            .and_then(|v| v.as_object())
            .map(|m| m.iter().filter_map(|(k, v)| v.as_f64().filter(|s| SPEEDS.contains(s) && *s != 1.0).map(|s| (k.clone(), s))).collect())
            .unwrap_or_default();
        let mut player = Player::start(cc, lib, prefs.volume as f64);
        let mut view = PlayerView::default();
        if let (Ok(p), Some(f)) = (&mut player, file) {
            p.open(&f);
            view.now_playing = scanner::basename(&f).to_owned();
        }
        let (media_tx, media_rx) = std::sync::mpsc::channel();
        let mut app = FeatherApp {
            player,
            pb: Playback::new(st.get_bool("loop_clip", false)),
            lib: Library::new(step),
            scanner: Scanner::new(cc.egui_ctx.clone()),
            view,
            grid: ClipGrid::default(),
            header: Header::default(),
            audio_badges,
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
        };
        app.load_folders(false);
        app
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
                    let auto = self.auto_scans.remove(&r.scan_id);
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
                        p.open_clip(&path, Start::Beginning);
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
        let mut st = prefs::read_settings();
        let mut changed = false;
        for key in ["clip_audio_tracks", "clip_audio_volumes", "clip_speeds"] {
            if let Some(Value::Object(m)) = st.get_mut(key) {
                if let Some(v) = m.get(src).cloned() {
                    m.insert(dst.to_owned(), v);
                    changed = true;
                    if key != "clip_speeds" {
                        self.audio_badges.insert(dst.to_owned());
                    }
                }
            }
        }
        if let Some(s) = self.clip_speed.get(src).copied() {
            self.clip_speed.insert(dst.to_owned(), s);
        }
        if changed {
            prefs::write_settings(&st);
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
        self.pb.reset_for_clip();
        // speed belongs to the clip: its own saved speed, otherwise 1x
        p.set_speed(self.clip_speed.get(&path).copied().unwrap_or(1.0));
        p.open_clip(&path, start);
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
                    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", p.replace('/', "\\"))).spawn();
                }
            }
            HeaderAction::Compress => self.open_compress(),
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
                p.stop();
                std::thread::sleep(Duration::from_millis(400));
            }
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
            SettingsAction::Close => self.settings_open = false,
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

/// Python `f"..."[:60]`.
fn cut60(s: &str) -> String {
    s.chars().take(60).collect()
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
        self.pb.tick(p);
        // cloud sync every 5 s, also while the settings page is open
        self.cloud.tick(&self.prefs.checked_folders, &self.prefs.keyword_groups);
        ctx.request_repaint_after(Duration::from_secs(5));

        if self.settings_open {
            let star_folder = self.prefs.star_copy_folder.clone();
            let v = SettingsValues {
                star_copy: self.prefs.star_copy_enabled,
                star_folder: &star_folder,
                loop_clip: self.pb.loop_clip,
                start_always: self.start_always,
                trim_gpu: self.trim_gpu,
                recycle_bin: self.recycle_bin,
            };
            let acts = self.settings.show(ctx, &v, &self.cloud, &mut self.phone, &self.lib);
            for a in acts {
                self.settings_action(ctx, a);
            }
            self.dialogs(ctx);
            return;
        }
        let p = self.player.as_mut().unwrap();
        self.view.input(ctx, p, &mut self.pb);

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
        let view_acts = self.view.controls(ctx, p, &mut self.pb, &clip);
        let video = self.view.video(ctx, p, &mut self.pb);
        self.view.audio_overlay(ctx, p, video);

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
        self.dialogs(ctx);
    }

    fn on_exit(&mut self, gl: Option<&glow::Context>) {
        if let Some((_, ctl)) = &self.compress {
            ctl.cancel();
        }
        if let Ok(p) = &self.player {
            // Python saves prefs in on_close too
            self.prefs.volume = p.volume.round() as i64;
            self.prefs.save();
            p.shutdown(gl);
        }
    }
}
