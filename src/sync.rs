//! Copying clips elsewhere, ported from FeatherPlayer:
//! - cloud sync (`_cloud_*`): new clips are copied into a cloud folder (Google Drive etc.)
//!   every 5 s; "Sync" also copies old ones; `cloud_copied.json` records what was copied;
//!   With "Share clip info" on it also writes `clips.json` (game, liked, starred, trim,
//!   length, date) and `thumbs\<clip>.jpg` into that folder for the ClipSync phone app;
//! - copying a newly starred clip to the starred-clips folder (`_copy_starred_clip`);
//! - Recycle Bin / drive checks used when deleting (`_can_recycle`, `_recycle_file`).

use crate::library::{category_key_for_video, NONE_KEY};
use crate::media::MediaMsg;
use crate::prefs::{self, data_dir, KeywordGroup};
use crate::scanner::{self, basename, date_from_filename, local_timestamp, py_mtime, video_duration, VIDEO_EXTS};
use eframe::egui;
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn now_ts() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn hidden(cmd: &mut std::process::Command, flags: u32) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(flags);
    }
    let _ = flags;
    cmd
}

/// Python `os.path.normcase(os.path.normpath(p))` for comparing folders.
fn norm(p: &str) -> String {
    let mut s = p.replace('/', "\\").to_lowercase();
    while s.contains("\\\\") {
        s = s.replace("\\\\", "\\");
    }
    if s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    s
}

fn ext_ok(name: &str) -> bool {
    let t = name.trim_start_matches('.');
    t.rfind('.').is_some_and(|i| VIDEO_EXTS.contains(&t[i + 1..].to_lowercase().as_str()))
}

fn read_json_map(path: &Path) -> Map<String, Value> {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default()
}

fn write_json_atomic(path: &Path, m: &Map<String, Value>) {
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    if std::fs::write(&tmp, serde_json::to_string(m).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// {clip path: (size, mtime)} for the watched folders (one subfolder level), skipping the
/// destination folder itself (Python `_cloud_scan`).
pub fn scan_sig(paths: &[String], dest: &str) -> Vec<(String, u64, f64)> {
    let dest_n = norm(dest);
    let mut out = Vec::new();
    fn walk(base: &str, depth: u8, dest_n: &str, out: &mut Vec<(String, u64, f64)>) {
        let Ok(rd) = std::fs::read_dir(base) else { return };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let path = scanner::join(base, &name);
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_file() {
                if ext_ok(&name) {
                    if let Ok(md) = std::fs::symlink_metadata(&path) {
                        out.push((path, md.len(), md.modified().map(py_mtime).unwrap_or(0.0)));
                    }
                }
            } else if depth == 0 && ft.is_dir() && norm(&path) != dest_n {
                walk(&path, 1, dest_n, out);
            }
        }
    }
    for base in paths {
        if norm(base) != dest_n {
            walk(base, 0, &dest_n, &mut out);
        }
    }
    out
}

/// The game (category) key a clip belongs to for the cloud game choice.
pub(crate) fn game_key(path: &str, groups: &[(String, KeywordGroup)]) -> String {
    category_key_for_video(path, groups).unwrap_or_else(|| NONE_KEY.to_owned())
}

// =============================================================================================
// Cloud sync
// =============================================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DotKind {
    Ok,
    Bad,
    Busy,
    Off,
}

/// Python `_cloud_kind`.
pub fn cloud_kind(text: &str) -> DotKind {
    let t = text.to_lowercase();
    if t == "off" {
        DotKind::Off
    } else if t.starts_with("up to date") || t.starts_with("all ") {
        DotKind::Ok
    } else if ["transferring", "waiting", "syncing", "checking", "already"].iter().any(|p| t.starts_with(p)) || t.contains("still recording") {
        DotKind::Busy
    } else {
        DotKind::Bad
    }
}


/// Shared state between the UI and the cloud worker threads.
struct CloudShared {
    enabled: AtomicBool,
    busy: AtomicBool,
    status: Mutex<String>,
    /// path -> [size, mtime, target] (+ true = removed from the cloud on purpose)
    copied: Mutex<Map<String, Value>>,
    /// size of each clip at the previous tick (a clip still growing is being recorded)
    prev: Mutex<HashMap<String, u64>>,
    ctx: egui::Context,
}

impl CloudShared {
    /// Python `_cloud_set_status`: while the switch is off only forced messages show.
    fn set_status(&self, text: impl Into<String>, force: bool) {
        let t = if !self.enabled.load(Ordering::SeqCst) && !force { "Off".to_owned() } else { text.into() };
        *self.status.lock().unwrap() = t;
        self.ctx.request_repaint();
    }
    fn save_copied(&self) {
        let _ = std::fs::create_dir_all(data_dir());
        write_json_atomic(&copied_path(), &self.copied.lock().unwrap());
    }
}

fn copied_path() -> PathBuf {
    data_dir().join("cloud_copied.json")
}

fn rec_size(rec: &Value) -> Option<u64> {
    rec.get(0)?.as_u64().or_else(|| rec.get(0)?.as_f64().map(|f| f as u64))
}

pub struct Cloud {
    sh: Arc<CloudShared>,
    pub folder: String,
    pub since: f64,
    /// "new" (only clips recorded since the switch) or "all"
    pub mode: String,
    pub games: BTreeSet<String>,
    /// Also write `clips.json` + `thumbs\` for ClipSync (settings `cloud_share_info`, Rust only).
    pub share_info: bool,
    last_tick: Instant,
}

impl Cloud {
    pub fn new(ctx: egui::Context) -> Cloud {
        let st = prefs::read_settings();
        let enabled = st.get("cloud_enabled").and_then(|v| v.as_bool()).unwrap_or(false);
        let folder = st.get("cloud_folder").and_then(|v| v.as_str()).unwrap_or("").to_owned();
        let since = st.get("cloud_since").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let mode = match st.get("cloud_mode").and_then(|v| v.as_str()) {
            Some("all") => "all",
            _ => "new",
        }
        .to_owned();
        let games = st.get("cloud_games").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect()).unwrap_or_default();
        let sh = Arc::new(CloudShared {
            enabled: AtomicBool::new(enabled),
            busy: AtomicBool::new(false),
            status: Mutex::new(if enabled { "Up to date" } else { "Off" }.into()),
            copied: Mutex::new(read_json_map(&copied_path())),
            prev: Mutex::new(HashMap::new()),
            ctx,
        });
        let share_info = st.get("cloud_share_info").and_then(|v| v.as_bool()).unwrap_or(true);
        Cloud { sh, folder, since, mode, games, share_info, last_tick: Instant::now() }
    }

    pub fn enabled(&self) -> bool {
        self.sh.enabled.load(Ordering::SeqCst)
    }

    pub fn status(&self) -> String {
        self.sh.status.lock().unwrap().clone()
    }

    pub fn set_status(&self, text: &str, force: bool) {
        self.sh.set_status(text, force);
    }

    /// What the network share (`lan.rs`) serves: the same clips as cloud sync, same names.
    pub fn lan_input(&self, folders: &[String], groups: &[(String, KeywordGroup)], meta: ClipMeta) -> crate::lan::LanInput {
        let dest_n = norm(&self.folder);
        let copied = self
            .sh
            .copied
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(src, rec)| {
                let a = rec.as_array()?;
                if a.get(3).and_then(|v| v.as_bool()).unwrap_or(false) {
                    return None; // removed from the cloud on purpose
                }
                let t = a.get(2)?.as_str()?;
                (!self.folder.is_empty() && norm(&t[..t.len() - basename(t).len()]) == dest_n).then(|| (src.clone(), t.to_owned()))
            })
            .collect();
        crate::lan::LanInput {
            folders: folders.to_vec(),
            dest: self.folder.clone(),
            since: if self.mode == "all" { 0.0 } else { self.since },
            games: self.games.clone(),
            groups: groups.to_vec(),
            meta,
            copied,
        }
    }

    pub fn set_share_info(&mut self, on: bool) {
        self.share_info = on;
        prefs::save_setting("cloud_share_info", json!(on));
        self.last_tick = Instant::now() - Duration::from_secs(10); // write it on the next frame
    }

    /// Python `_on_cloud_toggle` (the folder check is done by the caller).
    pub fn set_enabled(&mut self, on: bool) {
        self.sh.enabled.store(on, Ordering::SeqCst);
        if on {
            self.since = now_ts(); // only clips from now on
            self.sh.prev.lock().unwrap().clear();
            prefs::save_setting("cloud_since", json!(self.since));
            self.sh.set_status("Up to date", false);
        } else {
            self.sh.set_status("Off", false);
        }
        prefs::save_setting("cloud_enabled", json!(on));
    }

    /// Python `_choose_cloud_folder` after the dialog. A new destination starts fresh.
    pub fn set_folder(&mut self, path: String) {
        if path != self.folder {
            self.folder = path.clone();
            self.sh.copied.lock().unwrap().clear();
            self.sh.save_copied();
            self.since = now_ts();
            self.sh.prev.lock().unwrap().clear();
            prefs::save_setting("cloud_folder", json!(path));
            prefs::save_setting("cloud_since", json!(self.since));
        }
    }

    /// Python `_set_cloud_mode`.
    pub fn set_mode(&mut self, mode: &str) {
        if mode == self.mode {
            return;
        }
        self.mode = mode.to_owned();
        prefs::save_setting("cloud_mode", json!(mode));
        if mode == "new" {
            self.since = now_ts();
            self.sh.prev.lock().unwrap().clear();
            prefs::save_setting("cloud_since", json!(self.since));
        }
        if self.enabled() {
            self.sh.set_status("Up to date", false);
        }
    }

    /// Python `_cloud_games_pick`: "All" clears the selection, others toggle.
    pub fn pick_game(&mut self, key: &str) {
        if key == "All" {
            self.games.clear();
        } else if !self.games.remove(key) {
            self.games.insert(key.to_owned());
        }
        prefs::save_setting("cloud_games", Value::Array(self.games.iter().map(|g| json!(g)).collect()));
    }

    /// Python `_cloud_copy_of`: the clip's copy in the current cloud folder (checked), or None.
    pub fn copy_of(&self, path: &str) -> Option<String> {
        if self.folder.is_empty() {
            return None;
        }
        let copied = self.sh.copied.lock().unwrap();
        let rec = copied.get(path)?.as_array()?;
        if rec.len() < 3 {
            return None;
        }
        let t = rec[2].as_str()?;
        let dir = match t.rfind(['\\', '/']) {
            Some(i) => &t[..i],
            None => "",
        };
        if norm(dir) != norm(&self.folder) {
            return None;
        }
        let size = rec_size(&Value::Array(rec.clone()))?;
        (std::fs::metadata(t).map(|m| m.is_file() && m.len() == size).unwrap_or(false)).then(|| t.to_owned())
    }

    /// After deleting a clip together with its cloud copy: forget the record.
    pub fn forget(&self, path: &str) {
        self.sh.copied.lock().unwrap().shift_remove(path);
        self.sh.save_copied();
    }

    /// After deleting only the cloud copy: keep the record, marked as removed on purpose,
    /// so sync doesn't upload it again.
    pub fn mark_removed(&self, path: &str) {
        let mut c = self.sh.copied.lock().unwrap();
        if let Some(Value::Array(rec)) = c.get_mut(path) {
            if rec.len() >= 3 {
                rec.truncate(3);
                rec.push(json!(true));
            }
        }
        drop(c);
        self.sh.save_copied();
    }

    /// Python `_cloud_tick` (every 5 s): copy finished new clips in the background.
    /// `meta` is only called when a worker actually starts (it copies prefs on the UI thread).
    pub fn tick(&mut self, folders: &[String], groups: &[(String, KeywordGroup)], meta: impl FnOnce() -> ClipMeta) {
        if self.last_tick.elapsed() < Duration::from_secs(5) {
            return;
        }
        self.last_tick = Instant::now();
        if !self.enabled() || self.sh.busy.load(Ordering::SeqCst) {
            return;
        }
        if self.folder.is_empty() {
            self.sh.set_status("Choose a folder first", false);
            return;
        }
        self.sh.busy.store(true, Ordering::SeqCst);
        let paths: Vec<String> = folders.iter().filter(|p| Path::new(p).is_dir()).cloned().collect();
        let (sh, dest, games, groups) = (self.sh.clone(), self.folder.clone(), self.games.clone(), groups.to_vec());
        let since = if self.mode == "all" { 0.0 } else { self.since };
        let meta = self.share_info.then(meta);
        std::thread::spawn(move || {
            cloud_worker(&sh, &paths, &dest, since, &games, &groups);
            if let Some(m) = meta {
                write_clip_info(&sh, &dest, &m);
            }
            sh.busy.store(false, Ordering::SeqCst);
        });
    }

    /// Python `_cloud_sync_now` (the Sync button). Returns false if a folder must be chosen.
    pub fn sync_now(&mut self, folders: &[String], groups: &[(String, KeywordGroup)], meta: impl FnOnce() -> ClipMeta) -> bool {
        if !self.enabled() {
            self.sh.set_status("Cloud sync is off. Turn it on first", true);
            return true;
        }
        if self.folder.is_empty() {
            return false;
        }
        if self.sh.busy.swap(true, Ordering::SeqCst) {
            self.sh.set_status("Already transferring, try again in a moment", true);
            return true;
        }
        self.sh.set_status("Checking folder access\u{2026}", true);
        let paths: Vec<String> = folders.iter().filter(|p| Path::new(p).is_dir()).cloned().collect();
        let (sh, dest, games, groups) = (self.sh.clone(), self.folder.clone(), self.games.clone(), groups.to_vec());
        let (mode_new, since) = (self.mode == "new", self.since);
        let meta = self.share_info.then(meta);
        std::thread::spawn(move || {
            sync_worker(&sh, &paths, &dest, mode_new, since, &games, &groups);
            if let Some(m) = meta {
                write_clip_info(&sh, &dest, &m);
            }
            sh.busy.store(false, Ordering::SeqCst);
        });
        true
    }

    /// Number of clips directly in the cloud folder (for "Delete all"), or an error status.
    pub fn count_cloud_clips(&self) -> Result<usize, String> {
        if self.folder.is_empty() || !Path::new(&self.folder).is_dir() {
            return Err("Choose a cloud folder first".into());
        }
        if self.sh.busy.load(Ordering::SeqCst) {
            return Err("Already transferring, try again in a moment".into());
        }
        let rd = std::fs::read_dir(&self.folder).map_err(|e| format!("No access: {e}"))?;
        Ok(rd.flatten().filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false) && ext_ok(&e.file_name().to_string_lossy())).count())
    }

    /// Python `_cloud_delete_worker`: delete every clip in the cloud folder; originals untouched.
    pub fn delete_all(&mut self) {
        if self.sh.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        self.sh.set_status("Deleting cloud clips\u{2026}", true);
        let (sh, folder) = (self.sh.clone(), self.folder.clone());
        std::thread::spawn(move || {
            let (mut done, mut failed) = (0, 0);
            match std::fs::read_dir(&folder) {
                Ok(rd) => {
                    let files: Vec<PathBuf> = rd.flatten().filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false) && ext_ok(&e.file_name().to_string_lossy())).map(|e| e.path()).collect();
                    for f in files {
                        if std::fs::remove_file(&f).is_ok() {
                            done += 1;
                        } else {
                            failed += 1;
                        }
                    }
                    {
                        let mut c = sh.copied.lock().unwrap();
                        for (_, rec) in c.iter_mut() {
                            if let Value::Array(r) = rec {
                                if r.len() >= 3 && r[2].as_str().is_some_and(|t| !Path::new(t).exists()) {
                                    r.truncate(3);
                                    r.push(json!(true));
                                }
                            }
                        }
                    }
                    sh.save_copied();
                    if failed > 0 {
                        sh.set_status(format!("Deleted {done}, {failed} failed"), true);
                    } else {
                        sh.set_status(format!("Deleted {done} clip{} from the cloud folder", if done == 1 { "" } else { "s" }), true);
                    }
                }
                Err(e) => sh.set_status(format!("Delete error: {e}"), true),
            }
            sh.busy.store(false, Ordering::SeqCst);
        });
    }
}

/// Python `_cloud_copy_one`: same name + same size = already there; same name + different
/// size = "name (1).ext". Copies via ".part" so cloud apps ignore it until it is complete.
fn copy_one(src: &str, dest_dir: &str, size: u64) -> Result<String, String> {
    let name = basename(src);
    let stem = scanner::stem(name);
    let ext = &name[stem.len()..];
    let mut target = scanner::join(dest_dir, name);
    let mut k = 0;
    while Path::new(&target).exists() {
        if std::fs::metadata(&target).map(|m| m.len() == size).unwrap_or(false) {
            return Ok(target);
        }
        k += 1;
        target = scanner::join(dest_dir, &format!("{stem} ({k}){ext}"));
    }
    let part = format!("{target}.part");
    let res = std::fs::copy(src, &part).map_err(|e| e.to_string()).and_then(|_| std::fs::rename(&part, &target).map_err(|e| e.to_string()));
    if Path::new(&part).exists() {
        let _ = std::fs::remove_file(&part);
    }
    res.map(|_| target)
}

fn cloud_worker(sh: &CloudShared, paths: &[String], dest: &str, since: f64, games: &BTreeSet<String>, groups: &[(String, KeywordGroup)]) {
    if !Path::new(dest).is_dir() {
        sh.set_status("Folder not available, will retry", false);
        return;
    }
    let mut sig = scan_sig(paths, dest);
    if !games.is_empty() {
        sig.retain(|(p, _, _)| games.contains(&game_key(p, groups)));
    }
    let prev: HashMap<String, u64> = std::mem::replace(&mut *sh.prev.lock().unwrap(), sig.iter().map(|(p, s, _)| (p.clone(), *s)).collect());
    let now = now_ts();
    let (mut todo, mut waiting) = (Vec::new(), 0);
    for (p, size, mt) in sig {
        if mt < since - 2.0 {
            continue; // older than the switch: ignored
        }
        if sh.copied.lock().unwrap().get(&p).and_then(rec_size) == Some(size) {
            continue; // already copied
        }
        if size == 0 || prev.get(&p) != Some(&size) || now - mt < 5.0 {
            waiting += 1; // still being recorded
            continue;
        }
        todo.push((p, size, mt));
    }
    if todo.is_empty() {
        sh.set_status(if waiting > 0 { "Waiting for the recording to finish\u{2026}" } else { "Up to date" }, false);
        return;
    }
    let mut failed = None;
    let n = todo.len();
    for (i, (p, size, mt)) in todo.into_iter().enumerate() {
        if !sh.enabled.load(Ordering::SeqCst) {
            return;
        }
        sh.set_status(format!("Transferring clip {} of {n}\u{2026}", i + 1), false);
        match copy_one(&p, dest, size) {
            Ok(target) => {
                sh.copied.lock().unwrap().insert(p.clone(), json!([size, mt, target]));
                sh.save_copied();
            }
            Err(e) => failed = Some((basename(&p).to_owned(), e)),
        }
    }
    match failed {
        Some((name, e)) => sh.set_status(format!("Failed: {name} ({e}). Will retry"), false),
        None => sh.set_status("Up to date", false),
    }
}

fn sync_worker(sh: &CloudShared, paths: &[String], dest: &str, mode_new: bool, since: f64, games: &BTreeSet<String>, groups: &[(String, KeywordGroup)]) {
    let st = |t: String| sh.set_status(t, true);
    if !Path::new(dest).is_dir() {
        st("No access: folder not available".into());
        return;
    }
    let test = scanner::join(dest, ".featherplayer_write_test");
    if let Err(e) = std::fs::write(&test, b"ok").and_then(|_| std::fs::remove_file(&test)) {
        st(format!("No access: can't write to the folder ({e})"));
        return;
    }
    let mut sig = scan_sig(paths, dest);
    if !games.is_empty() {
        sig.retain(|(p, _, _)| games.contains(&game_key(p, groups)));
    }
    if mode_new {
        sig.retain(|(_, _, mt)| *mt >= since - 2.0);
    }
    let now = now_ts();
    let total = sig.len();
    let (mut todo, mut busy_files) = (Vec::new(), 0);
    for (p, size, mt) in sig {
        let rec = sh.copied.lock().unwrap().get(&p).cloned();
        if let Some(r) = &rec {
            let a = r.as_array().cloned().unwrap_or_default();
            if rec_size(r) == Some(size) && a.len() > 3 && a[3].as_bool().unwrap_or(false) {
                continue; // removed from the cloud on purpose
            }
            if rec_size(r) == Some(size) && a.len() > 2 && a[2].as_str().is_some_and(|t| std::fs::metadata(t).map(|m| m.is_file() && m.len() == size).unwrap_or(false)) {
                continue; // verified: it is in the folder
            }
        }
        if size == 0 || now - mt < 5.0 {
            busy_files += 1; // still being recorded
            continue;
        }
        todo.push((p, size, mt));
    }
    let report = |busy: usize| {
        if busy > 0 {
            format!("{} of {total} clips transferred, {busy} still recording", total - busy)
        } else {
            format!("All {total} clips transferred")
        }
    };
    if todo.is_empty() {
        st(if total == 0 { "Up to date".into() } else { report(busy_files) });
        return;
    }
    let n = todo.len();
    let mut failed = 0;
    for (i, (p, size, mt)) in todo.into_iter().enumerate() {
        st(format!("Syncing clip {} of {n}\u{2026}", i + 1));
        match copy_one(&p, dest, size) {
            Ok(target) => {
                sh.copied.lock().unwrap().insert(p, json!([size, mt, target]));
                sh.save_copied();
            }
            Err(_) => failed += 1,
        }
    }
    if failed > 0 {
        st(format!("Synced {}, {failed} failed. Press Sync to retry", n - failed));
    } else {
        st(report(busy_files));
    }
}

// =============================================================================================
// Clip info for ClipSync (clips.json + thumbs\ in the cloud folder)
// =============================================================================================

/// Clip state copied from prefs on the UI thread for `write_clip_info`.
pub struct ClipMeta {
    pub starred: BTreeSet<String>,
    pub liked: BTreeSet<String>,
    /// path -> [start, end] (0..10000) for clips whose trim is switched on
    pub trims: HashMap<String, (f64, f64)>,
    /// path -> seconds (prefs `durations`)
    pub durations: HashMap<String, f64>,
    pub groups: Vec<(String, KeywordGroup)>,
    /// category key -> label shown
    pub labels: HashMap<String, String>,
}

pub const THUMBS_DIR: &str = "thumbs";

/// One clips.json entry (ClipSync's format, see its README).
pub(crate) fn clip_entry(src: &str, name: &str, m: &ClipMeta) -> Value {
    let key = game_key(src, &m.groups);
    let game = if key == NONE_KEY { "Moments".to_owned() } else { m.labels.get(&key).cloned().unwrap_or(key) };
    let created = match date_from_filename(basename(src)) {
        Some(d) => local_timestamp(&d),
        None => std::fs::metadata(src).and_then(|md| md.modified()).map(py_mtime).unwrap_or(0.0),
    };
    let trim = m.trims.get(src);
    let mut e = json!({"file": name, "game": game, "tags": [], "liked": m.liked.contains(src),
                       "starred": m.starred.contains(src), "trimmed": trim.is_some(),
                       "created": (created * 1000.0) as i64});
    if let Some((a, b)) = trim {
        // fractions of the clip length (prefs keep 0..10000), so the phone needs no exact duration
        e["trim"] = json!([a / 10000.0, b / 10000.0]);
    }
    if let Some(d) = m.durations.get(src) {
        e["duration"] = json!(d);
    }
    e
}

/// Writes `<dest>\clips.json` for every clip cloud sync copied there, and makes the missing
/// `<dest>\thumbs\<file>.jpg` (a copy of FeatherPlayer's own cached thumbnail). ClipSync (the phone app) reads both from the same Drive folder.
/// clips.json is only rewritten when its content changed, so Drive doesn't re-upload it every 5 s.
fn write_clip_info(sh: &CloudShared, dest: &str, m: &ClipMeta) {
    if !Path::new(dest).is_dir() {
        return;
    }
    let dest_n = norm(dest);
    // copied records -> (source path, file name in the cloud folder)
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut seen = HashSet::new();
    for (src, rec) in sh.copied.lock().unwrap().iter() {
        let Some(a) = rec.as_array() else { continue };
        if a.get(3).and_then(|v| v.as_bool()).unwrap_or(false) {
            continue; // removed from the cloud on purpose
        }
        let Some(target) = a.get(2).and_then(|v| v.as_str()) else { continue };
        let name = basename(target);
        let parent = &target[..target.len() - name.len()];
        if norm(parent) != dest_n || !Path::new(target).is_file() || !seen.insert(name.to_lowercase()) {
            continue;
        }
        pairs.push((src.clone(), name.to_owned()));
    }
    pairs.sort_by(|a, b| a.1.cmp(&b.1));
    let clips: Vec<Value> = pairs.iter().map(|(src, name)| clip_entry(src, name, m)).collect();

    // thumbnails: drop the ones whose clip is gone, make the missing ones
    let tdir = PathBuf::from(scanner::join(dest, THUMBS_DIR));
    let _ = std::fs::create_dir_all(&tdir);
    let want: HashSet<String> = pairs.iter().map(|(_, n)| format!("{n}.jpg").to_lowercase()).collect();
    if let Ok(rd) = std::fs::read_dir(&tdir) {
        for e in rd.flatten() {
            if !want.contains(&e.file_name().to_string_lossy().to_lowercase()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let missing: Vec<&(String, String)> = pairs.iter().filter(|(_, n)| !tdir.join(format!("{n}.jpg")).is_file()).collect();
    if !missing.is_empty() {
        // FeatherPlayer's own thumbnail cache (.clipplayer_cache) usually has them already, so this is
        // mostly a file copy; ffmpeg only runs for a clip it hasn't made a thumbnail for yet.
        let ff = scanner::find_ffmpeg();
        {
            let before = sh.status.lock().unwrap().clone();
            let n = missing.len();
            for (i, (src, name)) in missing.into_iter().enumerate() {
                if !sh.enabled.load(Ordering::SeqCst) {
                    return;
                }
                sh.set_status(format!("Copying thumbnails {} of {n}\u{2026}", i + 1), false);
                // copied in via ".part" so Drive never uploads half a file
                if let Some(cached) = scanner::get_thumb(src, ff.as_deref()) {
                    let part = tdir.join(format!("{name}.jpg.part"));
                    if std::fs::copy(&cached, &part).is_ok() && std::fs::rename(&part, tdir.join(format!("{name}.jpg"))).is_err() {
                        let _ = std::fs::remove_file(&part);
                    }
                }
            }
            sh.set_status(before, false);
        }
    }

    let text = serde_json::to_string_pretty(&json!({"clips": clips})).unwrap_or_default();
    let path = PathBuf::from(scanner::join(dest, "clips.json"));
    if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
        let tmp = PathBuf::from(format!("{}.tmp", path.display()));
        if std::fs::write(&tmp, &text).is_ok() && std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

// =============================================================================================
// Starred clips folder
// =============================================================================================

/// Python `_copy_starred_clip` worker: copy (a lossless trimmed cut if the clip is trimmed)
/// into the starred-clips folder, unless a file with that name is already there.
pub fn copy_starred(src: String, dest_folder: String, trim: Option<(f64, f64)>, ff: Option<PathBuf>, tx: Sender<MediaMsg>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let flash = |t: String, c: [u8; 3]| {
            let _ = tx.send(MediaMsg::Flash(t, c));
            ctx.request_repaint();
        };
        let (green, yellow, red) = ([0x7e, 0xc9, 0x7e], [0xc8, 0xc8, 0x64], [0xe0, 0x55, 0x55]);
        let fname = basename(&src).to_owned();
        let mut actual = PathBuf::from(&src);
        let mut tmp = None;
        if let (Some(trim), Some(ff)) = (trim, ff.as_deref()) {
            let dur = video_duration(&src, ff);
            if dur > 0.0 && trim.1 / 10000.0 * dur > trim.0 / 10000.0 * dur + 0.05 {
                flash("Cutting trimmed clip\u{2026}".into(), yellow);
                if let Some(t) = cut_plain(&src, trim, ff, dur) {
                    actual = t.clone();
                    tmp = Some(t);
                }
            }
        }
        let dest = scanner::join(&dest_folder, &fname);
        if Path::new(&dest).exists() {
            flash(format!("Already in folder: {fname}"), yellow);
        } else {
            match std::fs::copy(&actual, &dest) {
                Ok(_) => {
                    // (std::fs::copy keeps the modified time, like shutil.copy2)
                    let label = if tmp.is_some() { "Copied trimmed clip" } else { "Copied" };
                    flash(format!("{label}: {fname}"), green);
                }
                Err(_) => flash("Copy failed \u{2014} see log".into(), red),
            }
        }
        if let Some(t) = tmp {
            let _ = std::fs::remove_file(t);
        }
    });
}

/// The starred-copy cut: lossless `-c copy` of the trimmed part into a temp file.
fn cut_plain(src: &str, trim: (f64, f64), ff: &Path, dur: f64) -> Option<PathBuf> {
    let (t0, t1) = (trim.0 / 10000.0 * dur, trim.1 / 10000.0 * dur);
    let ext = &basename(src)[scanner::stem(basename(src)).len()..];
    let tmp = std::env::temp_dir().join(format!("fp_star_{}_{}{}", std::process::id(), now_ts() as u64, if ext.is_empty() { ".mp4" } else { ext }));
    let ok = hidden(&mut std::process::Command::new(ff), 0x0800_0000)
        .args(["-y", "-ss", &t0.to_string(), "-to", &t1.to_string(), "-i", src, "-map", "0:v?", "-map", "0:a?", "-map_metadata", "0", "-c", "copy"])
        .arg(&tmp)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok && std::fs::metadata(&tmp).map(|m| m.len() > 0).unwrap_or(false) {
        Some(tmp)
    } else {
        let _ = std::fs::remove_file(&tmp);
        None
    }
}

// =============================================================================================
// Recycle Bin
// =============================================================================================

/// Python `os.path.abspath` for the Windows shell: backslashes only.
fn win_path(p: &str) -> String {
    p.replace('/', "\\")
}

/// True if Windows has a Recycle Bin for this file's drive (fixed local drives only).
pub fn can_recycle(path: &str) -> bool {
    let p = win_path(path);
    if p.starts_with("\\\\") || p.len() < 2 || &p[1..2] != ":" {
        return false;
    }
    #[cfg(windows)]
    {
        let root: Vec<u16> = format!("{}\\", &p[..2]).encode_utf16().chain([0]).collect();
        unsafe { windows_sys::Win32::Storage::FileSystem::GetDriveTypeW(root.as_ptr()) == 3 } // DRIVE_FIXED
    }
    #[cfg(not(windows))]
    false
}

/// Move one file to the Recycle Bin (SHFileOperationW, silent). True on success.
pub fn recycle_file(path: &str) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::{SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE, SHFILEOPSTRUCTW};
        let full = win_path(path);
        let from: Vec<u16> = full.encode_utf16().chain([0, 0]).collect(); // double-NUL terminated list
        let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
        op.wFunc = FO_DELETE;
        op.pFrom = from.as_ptr();
        op.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI) as u16;
        let rc = unsafe { SHFileOperationW(&mut op) };
        rc == 0 && op.fAnyOperationsAborted == 0 && !Path::new(&full).exists()
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_norm() {
        assert!(cloud_kind("Up to date") == DotKind::Ok && cloud_kind("All 3 clips transferred") == DotKind::Ok);
        assert!(cloud_kind("2 of 3 clips transferred, 1 still recording") == DotKind::Busy);
        assert!(cloud_kind("Failed: x (y). Will retry") == DotKind::Bad && cloud_kind("Off") == DotKind::Off);
        assert_eq!(norm("G:/My Drive\\Clips\\"), "g:\\my drive\\clips");
    }

    #[test]
    fn cloud_copy_one_names() {
        let d = std::env::temp_dir().join(format!("fp_cloud_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("dest")).unwrap();
        let src = d.join("a.mkv");
        std::fs::write(&src, b"12345").unwrap();
        let dest = d.join("dest").to_string_lossy().into_owned();
        let t1 = copy_one(&src.to_string_lossy(), &dest, 5).unwrap();
        assert!(t1.ends_with("a.mkv"));
        assert_eq!(copy_one(&src.to_string_lossy(), &dest, 5).unwrap(), t1, "same size = already there");
        std::fs::write(&src, b"1234567").unwrap();
        assert!(copy_one(&src.to_string_lossy(), &dest, 7).unwrap().ends_with("a (1).mkv"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn real_files_are_never_recycled_here() {
        assert!(!can_recycle(r"\\server\share\x.mkv"));
    }

    #[test]
    fn clip_info_json_for_copied_clips() {
        let d = std::env::temp_dir().join(format!("fp_clipinfo_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("dest")).unwrap();
        let dest = d.join("dest").to_string_lossy().into_owned();
        let src = scanner::join(&d.to_string_lossy(), "Valorant 2026-11-20 12-00-00.mkv");
        std::fs::write(&src, b"x").unwrap();
        let target = copy_one(&src, &dest, 1).unwrap();
        let gone = scanner::join(&dest, "gone.mkv"); // record without a file: left out
        let sh = CloudShared {
            enabled: AtomicBool::new(true),
            busy: AtomicBool::new(false),
            status: Mutex::new(String::new()),
            copied: Mutex::new(Map::from_iter([(src.clone(), json!([1, 0.0, target])), ("other.mkv".into(), json!([1, 0.0, gone]))])),
            prev: Mutex::new(HashMap::new()),
            ctx: egui::Context::default(),
        };
        let m = ClipMeta {
            starred: [src.clone()].into(),
            liked: BTreeSet::new(),
            trims: [(src.clone(), (2500.0, 5000.0))].into(),
            durations: [(src.clone(), 40.0)].into(),
            groups: Vec::new(),
            labels: HashMap::new(),
        };
        write_clip_info(&sh, &dest, &m);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(d.join("dest").join("clips.json")).unwrap()).unwrap();
        let clips = v["clips"].as_array().unwrap();
        assert_eq!(clips.len(), 1);
        let c = &clips[0];
        assert_eq!(c["file"], "Valorant 2026-11-20 12-00-00.mkv");
        assert_eq!((c["starred"].as_bool(), c["liked"].as_bool(), c["trimmed"].as_bool()), (Some(true), Some(false), Some(true)));
        assert_eq!(c["trim"], json!([0.25, 0.5]));
        assert_eq!(c["duration"], json!(40.0));
        assert!(c["created"].as_i64().unwrap() > 1_700_000_000_000, "ms from the file name date");
        assert!(d.join("dest").join(THUMBS_DIR).is_dir());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Reads the real cloud_copied.json and checks it round-trips unchanged (read-only):
    /// `cargo test -- --ignored real_cloud_copied`
    #[test]
    #[ignore]
    fn real_cloud_copied_roundtrip() {
        let p = copied_path();
        let text = std::fs::read_to_string(&p).expect("no cloud_copied.json");
        let orig: Value = serde_json::from_str(&text).unwrap();
        let m = read_json_map(&p);
        assert_eq!(Value::Object(m.clone()), orig);
        let again: Value = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(again, orig);
        println!("{} cloud records", m.len());
    }

    /// Moves a throwaway temp file to the Recycle Bin: `cargo test -- --ignored real_recycle`
    #[test]
    #[ignore]
    fn real_recycle_temp_file() {
        let f = std::env::temp_dir().join("featherplayer_recycle_test.txt");
        std::fs::write(&f, b"test file from FeatherPlayer tests, safe to delete").unwrap();
        let p = f.to_string_lossy().into_owned();
        assert!(can_recycle(&p), "temp dir should be on a fixed drive");
        assert!(recycle_file(&p));
        assert!(!f.exists());
    }
}
