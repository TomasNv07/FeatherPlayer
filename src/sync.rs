//! Copying clips elsewhere, ported from FeatherPlayer:
//! - cloud sync (`_cloud_*`): new clips are copied into a cloud folder (Google Drive etc.)
//!   every 5 s; "Sync" also copies old ones; `cloud_copied.json` records what was copied;
//! - phone sync (`_phone_*`): clips + `clips.json` are sent over the LAN to the ClipSync app
//!   (HTTP on port 8765, X-Token pairing code); `phone_synced.json` records what was sent;
//! - copying a newly starred clip to the starred-clips folder (`_copy_starred_clip`);
//! - Recycle Bin / drive checks used when deleting (`_can_recycle`, `_recycle_file`).

use crate::library::{category_key_for_video, NONE_KEY};
use crate::media::MediaMsg;
use crate::prefs::{self, data_dir, KeywordGroup};
use crate::scanner::{self, basename, date_from_filename, local_timestamp, py_mtime, video_duration, VIDEO_EXTS};
use eframe::egui;
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const PHONE_PORT: u16 = 8765;

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

/// The game (category) key a clip belongs to for the cloud/phone game choice.
fn game_key(path: &str, groups: &[(String, KeywordGroup)]) -> String {
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

/// Python `_phone_kind_of`.
pub fn phone_kind(text: &str) -> DotKind {
    let t = text.to_lowercase();
    if t == "off" {
        DotKind::Off
    } else if (t.starts_with("ready") || t.starts_with("done")) && !t.contains("failed") {
        DotKind::Ok
    } else if ["looking", "cutting", "sending", "already"].iter().any(|p| t.starts_with(p)) {
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
        Cloud { sh, folder, since, mode, games, last_tick: Instant::now() }
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
    pub fn tick(&mut self, folders: &[String], groups: &[(String, KeywordGroup)]) {
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
        std::thread::spawn(move || {
            cloud_worker(&sh, &paths, &dest, since, &games, &groups);
            sh.busy.store(false, Ordering::SeqCst);
        });
    }

    /// Python `_cloud_sync_now` (the Sync button). Returns false if a folder must be chosen.
    pub fn sync_now(&mut self, folders: &[String], groups: &[(String, KeywordGroup)]) -> bool {
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
        std::thread::spawn(move || {
            sync_worker(&sh, &paths, &dest, mode_new, since, &games, &groups);
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
// Phone sync (ClipSync)
// =============================================================================================

/// Python urllib.parse.quote(path): keeps letters, digits, "_.-~" and "/".
fn quote(path: &str) -> String {
    let mut out = String::new();
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"_.-~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// One HTTP request to the phone (Python `_phone_request`). Streams `body` in 1 MB chunks.
fn phone_request(ip: &str, token: &str, method: &str, path: &str, body: Option<&mut dyn Read>, size: u64, mut progress: impl FnMut(u64, u64)) -> std::io::Result<(u16, Vec<u8>)> {
    let addr: SocketAddr = format!("{ip}:{PHONE_PORT}").parse().map_err(|_| std::io::Error::other("bad address"))?;
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(60))?;
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    stream.set_write_timeout(Some(Duration::from_secs(60)))?;
    let _ = stream.set_nodelay(true);
    // Windows' default send buffer is small and caps Wi-Fi throughput
    let _ = socket2::SockRef::from(&stream).set_send_buffer_size(2 * 1024 * 1024);
    let mut s = stream;
    let head = format!("{method} {} HTTP/1.1\r\nHost: {ip}:{PHONE_PORT}\r\nAccept-Encoding: identity\r\nX-Token: {token}\r\nContent-Length: {size}\r\n\r\n", quote(path));
    s.write_all(head.as_bytes())?;
    if let Some(b) = body {
        let mut buf = vec![0u8; 1024 * 1024];
        let mut sent = 0u64;
        loop {
            let k = b.read(&mut buf)?;
            if k == 0 {
                break;
            }
            s.write_all(&buf[..k])?;
            sent += k as u64;
            progress(sent, size);
        }
    }
    s.flush()?;
    // response: status line, headers, then Content-Length bytes (or until closed)
    let mut data = Vec::new();
    let mut tmp = [0u8; 8192];
    let header_end;
    loop {
        let k = s.read(&mut tmp)?;
        if k == 0 {
            return Err(std::io::Error::other("connection closed"));
        }
        data.extend_from_slice(&tmp[..k]);
        if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            header_end = i + 4;
            break;
        }
    }
    let head = String::from_utf8_lossy(&data[..header_end]).into_owned();
    let status: u16 = head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let len = head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case("content-length").then(|| v.trim().parse::<usize>().ok()).flatten()
    });
    let mut body_bytes = data[header_end..].to_vec();
    match len {
        Some(n) => {
            while body_bytes.len() < n {
                let k = s.read(&mut tmp)?;
                if k == 0 {
                    break;
                }
                body_bytes.extend_from_slice(&tmp[..k]);
            }
            body_bytes.truncate(n);
        }
        None => {
            let _ = s.read_to_end(&mut body_bytes);
        }
    }
    Ok((status, body_bytes))
}

/// True if a ClipSync receiver answers on this address (Python `_phone_ping`).
fn phone_ping(ip: &str, timeout: Duration) -> bool {
    let Ok(addr) = format!("{ip}:{PHONE_PORT}").parse::<SocketAddr>() else { return false };
    let Ok(mut s) = TcpStream::connect_timeout(&addr, timeout) else { return false };
    let _ = s.set_read_timeout(Some(timeout));
    let _ = s.set_write_timeout(Some(timeout));
    if s.write_all(format!("GET /ping HTTP/1.1\r\nHost: {ip}:{PHONE_PORT}\r\nConnection: close\r\n\r\n").as_bytes()).is_err() {
        return false;
    }
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    serde_json::from_str::<Value>(body.trim()).ok().and_then(|v| v.get("app").and_then(|a| a.as_str()).map(|a| a == "ClipSync")).unwrap_or(false)
}

/// This PC's LAN address (Python `_local_ipv4s`, the adapter that would reach the LAN).
fn local_ipv4s() -> Vec<String> {
    let mut ips = Vec::new();
    if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
        if s.connect("10.255.255.255:1").is_ok() {
            if let Ok(a) = s.local_addr() {
                ips.push(a.ip().to_string());
            }
        }
    }
    ips.retain(|i| i.starts_with("192.168.") || i.starts_with("10.") || i.starts_with("172."));
    ips
}

/// Phone address, or "": the typed-in address, then the last one that worked, then a scan of
/// the local /24 network (Python `_find_phone`).
fn find_phone(saved: &str, manual: &str) -> String {
    if !manual.is_empty() {
        return if phone_ping(manual, Duration::from_millis(1500)) { manual.to_owned() } else { String::new() };
    }
    if !saved.is_empty() && phone_ping(saved, Duration::from_millis(1000)) {
        return saved.to_owned();
    }
    let mine: HashSet<String> = local_ipv4s().into_iter().collect();
    let subnets: BTreeSet<String> = mine.iter().filter_map(|i| i.rsplit_once('.').map(|(n, _)| n.to_owned())).collect();
    let cands: Arc<Vec<String>> = Arc::new(subnets.iter().flat_map(|n| (1..255).map(move |h| format!("{n}.{h}"))).filter(|ip| !mine.contains(ip)).collect());
    let next = Arc::new(AtomicUsize::new(0));
    let found: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let mut handles = Vec::new();
    for _ in 0..64 {
        let (cands, next, found) = (cands.clone(), next.clone(), found.clone());
        handles.push(std::thread::spawn(move || loop {
            if found.lock().unwrap().is_some() {
                return;
            }
            let i = next.fetch_add(1, Ordering::SeqCst);
            let Some(ip) = cands.get(i) else { return };
            let Ok(addr) = format!("{ip}:{PHONE_PORT}").parse::<SocketAddr>() else { continue };
            if TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok() && phone_ping(ip, Duration::from_millis(1500)) {
                found.lock().unwrap().get_or_insert_with(|| ip.clone());
                return;
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    let r = found.lock().unwrap().clone();
    r.unwrap_or_default()
}

/// Everything the phone worker needs, copied on the UI thread (Python `_phone_snapshot`).
pub struct PhoneSnap {
    pub kinds: BTreeSet<String>,
    pub starred: BTreeSet<String>,
    pub liked: BTreeSet<String>,
    pub trim_enabled: BTreeSet<String>,
    /// path -> [start, end] (0..10000)
    pub trims: HashMap<String, (f64, f64)>,
    pub folders: Vec<String>,
    pub groups: Vec<(String, KeywordGroup)>,
    /// category key -> label shown
    pub labels: HashMap<String, String>,
}

pub struct Phone {
    pub enabled: bool,
    pub kinds: BTreeSet<String>,
    pub code: String,
    pub manual_ip: String,
    status: Arc<Mutex<String>>,
    busy: Arc<AtomicBool>,
}

impl Phone {
    pub fn new() -> Phone {
        let st = prefs::read_settings();
        let kinds: BTreeSet<String> = st
            .get("phone_kinds")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).filter(|k| ["starred", "liked", "trimmed", "all"].contains(k)).map(str::to_owned).collect())
            .unwrap_or_else(|| ["starred".to_owned()].into());
        Phone {
            enabled: st.get("phone_enabled").and_then(|v| v.as_bool()).unwrap_or(false),
            kinds: if kinds.is_empty() { ["starred".to_owned()].into() } else { kinds },
            code: st.get("phone_token").and_then(|v| v.as_str()).unwrap_or("").to_owned(),
            manual_ip: st.get("phone_manual_ip").and_then(|v| v.as_str()).unwrap_or("").to_owned(),
            status: Arc::new(Mutex::new("Ready. Open the ClipSync app on your phone first".into())),
            busy: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn status(&self) -> String {
        self.status.lock().unwrap().clone()
    }

    /// Python `_phone_toggle_kind`: "All clips" alone, otherwise any mix, never none.
    pub fn toggle_kind(&mut self, key: &str) {
        if key == "all" {
            self.kinds = ["all".to_owned()].into();
        } else {
            self.kinds.remove("all");
            if !self.kinds.remove(key) {
                self.kinds.insert(key.to_owned());
            }
            if self.kinds.is_empty() {
                self.kinds.insert(key.to_owned());
            }
        }
        prefs::save_setting("phone_kinds", Value::Array(self.kinds.iter().map(|k| json!(k)).collect()));
    }

    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        prefs::save_setting("phone_enabled", json!(on));
    }

    /// Python `_phone_sync_now`.
    pub fn sync_now(&mut self, snap: PhoneSnap, ffmpeg: Option<PathBuf>, ctx: egui::Context) {
        if !self.enabled {
            return;
        }
        let set = |s: &Arc<Mutex<String>>, t: &str| *s.lock().unwrap() = t.to_owned();
        if self.busy.load(Ordering::SeqCst) {
            set(&self.status, "Already syncing, try again in a moment");
            return;
        }
        let code = self.code.trim().to_owned();
        let manual = self.manual_ip.trim().to_owned();
        if code.is_empty() {
            set(&self.status, "Type the pairing code shown in the ClipSync app");
            return;
        }
        prefs::save_setting("phone_token", json!(code));
        prefs::save_setting("phone_manual_ip", json!(manual));
        self.busy.store(true, Ordering::SeqCst);
        set(&self.status, "Looking for your phone\u{2026}");
        let (status, busy) = (self.status.clone(), self.busy.clone());
        std::thread::spawn(move || {
            let st = |t: String| {
                *status.lock().unwrap() = t;
                ctx.request_repaint();
            };
            phone_worker(&snap, &code, &manual, ffmpeg.as_deref(), &st);
            busy.store(false, Ordering::SeqCst);
            ctx.request_repaint();
        });
    }
}

fn synced_path() -> PathBuf {
    data_dir().join("phone_synced.json")
}

struct Item {
    path: String,
    name: String,
    remote: String,
    size: u64,
    game: String,
    trim: Option<(f64, f64)>,
    liked: bool,
    starred: bool,
    created: f64,
}

/// Python `_phone_build_items`.
fn build_items(snap: &PhoneSnap) -> Vec<Item> {
    let mut found: BTreeSet<String> = BTreeSet::new();
    if snap.kinds.contains("all") {
        let folders: Vec<String> = snap.folders.iter().filter(|p| Path::new(p).is_dir()).cloned().collect();
        found.extend(scan_sig(&folders, "\0none").into_iter().map(|(p, _, _)| p));
    } else {
        if snap.kinds.contains("starred") {
            found.extend(snap.starred.iter().cloned());
        }
        if snap.kinds.contains("liked") {
            found.extend(snap.liked.iter().cloned());
        }
        if snap.kinds.contains("trimmed") {
            found.extend(snap.trim_enabled.iter().filter(|p| snap.trims.contains_key(*p)).cloned());
        }
    }
    let mut items = Vec::new();
    for p in found.into_iter().filter(|p| Path::new(p).is_file()) {
        let Ok(md) = std::fs::metadata(&p) else { continue };
        let trim = if snap.trim_enabled.contains(&p) { snap.trims.get(&p).copied() } else { None };
        let key = game_key(&p, &snap.groups);
        let game = if key == NONE_KEY { "Moments".to_owned() } else { snap.labels.get(&key).cloned().unwrap_or(key) };
        let name = basename(&p).to_owned();
        let created = match date_from_filename(&name) {
            Some(d) => local_timestamp(&d),
            None => md.modified().map(py_mtime).unwrap_or(0.0),
        };
        items.push(Item { liked: snap.liked.contains(&p), starred: snap.starred.contains(&p), remote: String::new(), name, size: md.len(), game, trim, created, path: p });
    }
    // two clips with the same file name: the first keeps it, the others get a short path hash
    let mut seen = HashSet::new();
    for it in &mut items {
        let key = it.name.to_lowercase();
        if seen.insert(key) {
            it.remote = it.name.clone();
        } else {
            use md5::{Digest, Md5};
            let h: String = Md5::digest(norm(&it.path).as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
            let stem = scanner::stem(&it.name);
            it.remote = format!("{stem}_{}{}", &h[..6], &it.name[stem.len()..]);
            seen.insert(it.remote.to_lowercase());
        }
    }
    items
}

/// Python `_phone_cut`: lossless trimmed copy in a temp file (low priority), or None.
fn phone_cut(src: &str, trim: (f64, f64), ff: &Path) -> Option<PathBuf> {
    let dur = video_duration(src, ff);
    if dur <= 0.0 {
        return None;
    }
    let (t0, t1) = (trim.0 / 10000.0 * dur, trim.1 / 10000.0 * dur);
    if t1 <= t0 + 0.05 {
        return None;
    }
    let ext = &basename(src)[scanner::stem(basename(src)).len()..];
    let tmp = std::env::temp_dir().join(format!("fp_phone_{}_{}{}", std::process::id(), now_ts() as u64, if ext.is_empty() { ".mp4" } else { ext }));
    let ok = hidden(&mut std::process::Command::new(ff), 0x0800_0000 | 0x0000_4000)
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

fn phone_worker(snap: &PhoneSnap, code: &str, manual: &str, ff: Option<&Path>, st: &dyn Fn(String)) {
    let mut synced = read_json_map(&synced_path());
    let mut dirty = false;
    let result: Result<(), String> = (|| {
        let saved = prefs::read_settings().get("phone_ip").and_then(|v| v.as_str()).unwrap_or("").to_owned();
        let ip = find_phone(&saved, manual);
        if ip.is_empty() {
            st("Phone not found. Open ClipSync on the phone (same network)".into());
            return Ok(());
        }
        prefs::save_setting("phone_ip", json!(ip));
        let (status, data) = phone_request(&ip, code, "GET", "/list", None, 0, |_, _| {}).map_err(|e| e.to_string())?;
        if status == 401 {
            st("Wrong pairing code".into());
            return Ok(());
        }
        if status != 200 {
            st(format!("Phone answered with error {status}"));
            return Ok(());
        }
        let list: Value = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
        let on_phone: HashMap<String, u64> = list
            .get("files")
            .and_then(|f| f.as_array())
            .map(|a| a.iter().filter_map(|f| Some((f.get("name")?.as_str()?.to_owned(), f.get("size")?.as_u64()?))).collect())
            .unwrap_or_default();

        st("Checking your clips\u{2026}".into());
        let items = build_items(snap);
        if items.is_empty() {
            st("No clips match your selection".into());
            return Ok(());
        }
        // what really needs sending, before sending (so the counter is "3 of 91")
        let (mut todo, mut meta, mut skipped) = (Vec::new(), Vec::new(), 0);
        for it in &items {
            let sig = json!([it.size, it.trim.map(|(a, b)| json!([a, b])).unwrap_or(Value::Null)]);
            let entry = json!({"file": it.remote, "game": it.game, "tags": [], "liked": it.liked, "starred": it.starred,
                               "trimmed": it.trim.is_some(), "created": (it.created * 1000.0) as i64});
            let ps = on_phone.get(&it.remote);
            if ps.is_some() && ((it.trim.is_none() && ps == Some(&it.size)) || synced.get(&it.path) == Some(&sig)) {
                if synced.get(&it.path) != Some(&sig) {
                    synced.insert(it.path.clone(), sig);
                    dirty = true;
                }
                skipped += 1;
                meta.push(entry);
            } else {
                todo.push((it, sig, entry));
            }
        }
        if dirty {
            write_json_atomic(&synced_path(), &synced);
            dirty = false;
        }
        let total = todo.len();
        let (mut sent, mut failed) = (0, 0);
        let mut last_save = Instant::now();
        for (n, (it, sig, entry)) in todo.into_iter().enumerate() {
            let n = n + 1;
            let mut send_path = PathBuf::from(&it.path);
            let mut tmp = None;
            if let (Some(trim), Some(ff)) = (it.trim, ff) {
                st(format!("Cutting trimmed clip {n} of {total}\u{2026}"));
                tmp = phone_cut(&it.path, trim, ff);
                if let Some(t) = &tmp {
                    send_path = t.clone();
                }
            }
            let mut ok = false;
            if let Ok(size) = std::fs::metadata(&send_path).map(|m| m.len()) {
                for _attempt in 0..2 {
                    // one quiet retry for a dropped connection
                    let t_start = Instant::now();
                    let mut last_ui = (Instant::now() - Duration::from_secs(1), -1i64);
                    let Ok(mut f) = std::fs::File::open(&send_path) else { break };
                    let prog = |done: u64, size_: u64| {
                        let pct = if size_ > 0 { (done * 100 / size_) as i64 } else { 100 };
                        if last_ui.0.elapsed() > Duration::from_millis(500) && pct != last_ui.1 {
                            last_ui = (Instant::now(), pct);
                            let mbs = done as f64 / 1048576.0 / t_start.elapsed().as_secs_f64().max(0.001);
                            st(format!("Sending {n} of {total}: {} ({pct}%, {mbs:.1} MB/s)", it.remote));
                        }
                    };
                    if let Ok((code_, _)) = phone_request(&ip, code, "PUT", &format!("/clip/{}", it.remote), Some(&mut f), size, prog) {
                        if code_ == 200 {
                            ok = true;
                            break;
                        }
                    }
                }
            }
            if ok {
                sent += 1;
                synced.insert(it.path.clone(), sig);
                dirty = true;
                meta.push(entry);
            } else {
                failed += 1;
            }
            if let Some(t) = tmp {
                let _ = std::fs::remove_file(t);
            }
            if dirty && last_save.elapsed() > Duration::from_secs(5) {
                write_json_atomic(&synced_path(), &synced);
                dirty = false;
                last_save = Instant::now();
            }
        }
        let payload = serde_json::to_vec(&json!({"clips": meta})).unwrap_or_default();
        let _ = phone_request(&ip, code, "PUT", "/clips.json", Some(&mut payload.as_slice()), payload.len() as u64, |_, _| {});
        let mut msg = format!("Done: {sent} sent, {skipped} already on phone");
        if failed > 0 {
            msg += &format!(", {failed} failed. Press Sync to retry");
        }
        st(msg);
        Ok(())
    })();
    if let Err(e) = result {
        st(format!("Sync failed: {e}"));
    }
    if dirty {
        write_json_atomic(&synced_path(), &synced);
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
                if let Some(t) = phone_cut_plain(&src, trim, ff, dur) {
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

/// The starred-copy cut (same as the phone cut but normal priority, like Python).
fn phone_cut_plain(src: &str, trim: (f64, f64), ff: &Path, dur: f64) -> Option<PathBuf> {
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
    fn kinds_and_quote() {
        assert!(cloud_kind("Up to date") == DotKind::Ok && cloud_kind("All 3 clips transferred") == DotKind::Ok);
        assert!(cloud_kind("2 of 3 clips transferred, 1 still recording") == DotKind::Busy);
        assert!(cloud_kind("Failed: x (y). Will retry") == DotKind::Bad && cloud_kind("Off") == DotKind::Off);
        assert!(phone_kind("Done: 3 sent, 1 already on phone") == DotKind::Ok);
        assert!(phone_kind("Done: 3 sent, 0 already on phone, 1 failed. Press Sync to retry") == DotKind::Bad);
        assert_eq!(quote("/clip/My Clip ä.mkv"), "/clip/My%20Clip%20%C3%A4.mkv");
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

    /// A tiny fake ClipSync receiver on localhost: /ping, /list (401 on a wrong token), PUT.
    fn fake_phone(port: u16) -> std::thread::JoinHandle<Vec<(String, String, usize)>> {
        let l = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        std::thread::spawn(move || {
            let mut got = Vec::new();
            for _ in 0..4 {
                let (mut s, _) = l.accept().unwrap();
                let mut buf = Vec::new();
                let mut tmp = [0u8; 65536];
                let (head, mut body) = loop {
                    let k = s.read(&mut tmp).unwrap();
                    buf.extend_from_slice(&tmp[..k]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break (String::from_utf8_lossy(&buf[..i]).into_owned(), buf[i + 4..].to_vec());
                    }
                };
                let len: usize = head.lines().find_map(|l| l.strip_prefix("Content-Length: ")).and_then(|v| v.parse().ok()).unwrap_or(0);
                while body.len() < len {
                    let k = s.read(&mut tmp).unwrap();
                    body.extend_from_slice(&tmp[..k]);
                }
                let line = head.lines().next().unwrap().to_owned();
                let token = head.lines().find_map(|l| l.strip_prefix("X-Token: ")).unwrap_or("").to_owned();
                let (code, resp) = if line.starts_with("GET /ping") {
                    (200, r#"{"app": "ClipSync"}"#.to_owned())
                } else if token != "1234" {
                    (401, "{}".to_owned())
                } else if line.starts_with("GET /list") {
                    (200, r#"{"files": [{"name": "a.mkv", "size": 5}]}"#.to_owned())
                } else {
                    (200, "ok".to_owned())
                };
                got.push((line, token, body.len()));
                let _ = s.write_all(format!("HTTP/1.1 {code} X\r\nContent-Length: {}\r\n\r\n{resp}", resp.len()).as_bytes());
            }
            got
        })
    }

    #[test]
    fn phone_http_against_fake_receiver() {
        // the real port is fixed (8765); the test server uses it on 127.0.0.1 only
        let srv = fake_phone(PHONE_PORT);
        assert!(phone_ping("127.0.0.1", Duration::from_secs(2)));
        let (st, _) = phone_request("127.0.0.1", "0000", "GET", "/list", None, 0, |_, _| {}).unwrap();
        assert_eq!(st, 401);
        let (st, data) = phone_request("127.0.0.1", "1234", "GET", "/list", None, 0, |_, _| {}).unwrap();
        assert_eq!(st, 200);
        assert!(String::from_utf8_lossy(&data).contains("a.mkv"));
        let payload = vec![7u8; 3_000_000];
        let mut sent = 0;
        let (st, _) = phone_request("127.0.0.1", "1234", "PUT", "/clip/My Clip.mkv", Some(&mut payload.as_slice()), payload.len() as u64, |d, _| sent = d).unwrap();
        assert_eq!((st, sent), (200, 3_000_000));
        let got = srv.join().unwrap();
        assert_eq!(got[3].0, "PUT /clip/My%20Clip.mkv HTTP/1.1");
        assert_eq!(got[3].2, 3_000_000);
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
        std::fs::write(&f, b"test file from FeatherPlayer-rs tests, safe to delete").unwrap();
        let p = f.to_string_lossy().into_owned();
        assert!(can_recycle(&p), "temp dir should be on a fixed drive");
        assert!(recycle_file(&p));
        assert!(!f.exists());
    }
}
