//! Clip folder scanning (background thread), ffprobe durations and ffmpeg thumbnails
//! (worker threads). Ported from FeatherPlayer `_load_folders`, `_load_thumbnails`, `get_thumb`.
//!
//! Compatibility with the Python app:
//! - Clip paths are built like Python `os.scandir` (`base + "\\" + name`), because the path
//!   string is the key for starred/liked/durations/trims in the prefs file.
//! - Thumbnails go to `<data>\.clipplayer_cache\<md5>.jpg` with the same cache key
//!   (`md5("{path}|{size}|{mtime}|tw140")`, mtime as Python's float repr), so existing
//!   thumbnails are reused and both apps share the cache.

use crate::prefs;
use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};
use eframe::egui::ColorImage;
use md5::{Digest, Md5};
use regex::Regex;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const VIDEO_EXTS: [&str; 8] = ["mp4", "mkv", "avi", "mov", "wmv", "m4v", "flv", "webm"];
pub const THUMB_W: u32 = 140;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// ---------------------------------------------------------------------------------------------
// Filename helpers
// ---------------------------------------------------------------------------------------------

pub fn basename(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// `os.scandir` entry path: `base + "\\" + name`, unless base already ends in `\`, `/` or `:`.
pub fn join(base: &str, name: &str) -> String {
    if base.is_empty() || base.ends_with(['\\', '/', ':']) {
        format!("{base}{name}")
    } else {
        format!("{base}\\{name}")
    }
}

fn ext_ok(name: &str) -> bool {
    // os.path.splitext: a leading dot is not an extension (".mp4" has none)
    let trimmed = name.trim_start_matches('.');
    match trimmed.rfind('.') {
        Some(i) => VIDEO_EXTS.contains(&trimmed[i + 1..].to_lowercase().as_str()),
        None => false,
    }
}

fn re(pat: &str) -> Regex {
    Regex::new(pat).unwrap()
}

/// Date/time encoded in a clip's filename (Python `date_from_filename`), as local time.
pub fn date_from_filename(name: &str) -> Option<NaiveDateTime> {
    static RES: OnceLock<[Regex; 3]> = OnceLock::new();
    let [r1, r2, r3] = RES.get_or_init(|| {
        [
            re(r"(?i)clip-from-([A-Za-z]{3})-(\d{1,2})-(\d{4})"),
            re(r"(\d{4})[-_](\d{2})[-_](\d{2})[\s_-]+(\d{2})[-_](\d{2})[-_](\d{2})"),
            re(r"(\d{4})\.(\d{2})\.(\d{2})\s*-\s*(\d{2})\.(\d{2})\.(\d{2})(?:\.\d+)?"),
        ]
    });
    let name = basename(name);
    let num = |c: &regex::Captures, i: usize| c.get(i).and_then(|m| m.as_str().parse::<u32>().ok());
    if let Some(c) = r1.captures(name) {
        const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
        let mon = MONTHS.iter().position(|m| *m == c[1].to_lowercase()).map(|i| i as u32 + 1);
        if let (Some(mon), Some(d), Some(y)) = (mon, num(&c, 2), num(&c, 3)) {
            if let Some(dt) = NaiveDate::from_ymd_opt(y as i32, mon, d).and_then(|d| d.and_hms_opt(0, 0, 0)) {
                return Some(dt);
            }
        }
    }
    for r in [r2, r3] {
        if let Some(c) = r.captures(name) {
            let v: Option<Vec<u32>> = (1..=6).map(|i| num(&c, i)).collect();
            if let Some(v) = v {
                if let Some(dt) = NaiveDate::from_ymd_opt(v[0] as i32, v[1], v[2]).and_then(|d| d.and_hms_opt(v[3], v[4], v[5])) {
                    return Some(dt);
                }
            }
        }
    }
    None
}

/// Python `datetime.timestamp()` of a naive local time.
pub fn local_timestamp(dt: &NaiveDateTime) -> f64 {
    match Local.from_local_datetime(dt) {
        chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => t.timestamp() as f64,
        chrono::LocalResult::None => dt.and_utc().timestamp() as f64,
    }
}

/// Python `os.stat().st_mtime` (seconds + nanoseconds * 1e-9, same float maths).
pub fn py_mtime(t: SystemTime) -> f64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as f64 + d.subsec_nanos() as f64 * 1e-9,
        Err(e) => {
            let d = e.duration();
            let (s, n) = (d.as_secs() as i64, d.subsec_nanos());
            if n == 0 { -(s as f64) } else { (-s - 1) as f64 + (1_000_000_000 - n) as f64 * 1e-9 }
        }
    }
}

/// Python `repr(float)`: shortest round-trip digits, with ".0" on whole numbers.
pub fn py_float_repr(f: f64) -> String {
    let s = format!("{f}");
    if s.contains(['.', 'e', 'i', 'N']) { s } else { format!("{s}.0") }
}

pub fn local_from_ts(ts: f64) -> Option<NaiveDateTime> {
    Local.timestamp_opt(ts.floor() as i64, ((ts - ts.floor()) * 1e9) as u32).single().map(|t| t.naive_local())
}

const GAME_ALIASES: [(&str, &str); 2] = [("cs2", "Counter-Strike 2"), ("tarkov", "Escape from Tarkov")];
const UUID_ALIASES: [(&str, &str); 2] =
    [("c2258301-017b-4b6b-89e8-9c5ffea5767c", "ERA Season 7"), ("8e9a5856-640d-43de-a1f0-65d04b073e5a", "ERA Season 6")];

/// Lowercase and strip everything that is not a-z / 0-9.
pub fn normalize_game(label: &str) -> String {
    label.to_lowercase().chars().filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit()).collect()
}

fn game_alias(label: &str) -> String {
    let n = normalize_game(label);
    GAME_ALIASES.iter().find(|(k, _)| *k == n).map(|(_, v)| v.to_string()).unwrap_or_else(|| label.to_owned())
}

/// Python `os.path.splitext(name)[0]` (a leading dot is not an extension).
pub fn stem(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 && !name[..i].chars().all(|c| c == '.') => &name[..i],
        _ => name,
    }
}

/// Python `game_name_from_filename`.
pub fn game_name_from_filename(name: &str) -> String {
    static RES: OnceLock<[Regex; 4]> = OnceLock::new();
    let [dvr, clipfrom, date_, dvr_date] = RES.get_or_init(|| {
        [re(r"(?i)\.DVR$"), re(r"(?i)^(.+?)-clip-from-[A-Za-z]+-\d{1,2}-\d{4}"), re(r"^(.+?)_(\d{4}-\d{2}-\d{2})"), re(r"^(.+?)\s+\d{4}\.\d{2}\.\d{2}")]
    });
    let name = basename(name);
    let stem = dvr.replace(stem(name), "").into_owned();
    if let Some(c) = clipfrom.captures(&stem) {
        let label = c[1].replace(['-', '_'], " ").trim().to_owned();
        return if label.is_empty() { "Moments".into() } else { game_alias(&label) };
    }
    let prefix = if let Some(i) = stem.find("__") {
        stem[..i].to_owned()
    } else if let Some(c) = date_.captures(&stem) {
        c[1].to_owned()
    } else if let Some(c) = dvr_date.captures(&stem) {
        c[1].to_owned()
    } else {
        return "Moments".into();
    };
    if let Some((_, v)) = UUID_ALIASES.iter().find(|(k, _)| *k == prefix.trim().to_lowercase()) {
        return v.to_string();
    }
    let label = prefix.replace(['-', '_'], " ").trim().to_owned();
    if label.is_empty() { "Moments".into() } else { game_alias(&label) }
}

// ---------------------------------------------------------------------------------------------
// Folder scan
// ---------------------------------------------------------------------------------------------

/// One clip found by a scan. `sort_mtime` is the filename date if there is one, else st_mtime
/// (Python `mtime_from_filename`), and is what Newest/Oldest sort by.
#[derive(Clone)]
pub struct ScannedClip {
    pub path: String,
    pub sort_mtime: f64,
    pub size: u64,
    /// Filename date, else file mtime, local time (Python `clip_dt`).
    pub clip_dt: Option<NaiveDateTime>,
}

pub struct ScanResult {
    pub scan_id: u64,
    pub clips: Vec<ScannedClip>,
    pub total_size: u64,
}

fn file_entry(path: String, name: &str, md: &std::fs::Metadata) -> ScannedClip {
    let fn_dt = date_from_filename(name);
    let mtime = md.modified().map(py_mtime).unwrap_or(0.0);
    let sort_mtime = fn_dt.as_ref().map(local_timestamp).unwrap_or(mtime);
    let clip_dt = fn_dt.or_else(|| local_from_ts(mtime));
    ScannedClip { path, sort_mtime, size: md.len(), clip_dt }
}

fn list_dir(base: &str) -> Vec<(String, String, std::fs::FileType)> {
    let Ok(rd) = std::fs::read_dir(base) else { return Vec::new() };
    rd.flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let ft = e.file_type().ok()?;
            Some((join(base, &name), name, ft))
        })
        .collect()
}

/// Clips directly in `base` and one level of subfolders (no symlinks followed).
fn scan_dir(base: &str, out: &mut Vec<ScannedClip>) {
    for (path, name, ft) in list_dir(base) {
        if ft.is_file() && ext_ok(&name) {
            if let Ok(md) = std::fs::symlink_metadata(&path) {
                out.push(file_entry(path, &name, &md));
            }
        } else if ft.is_dir() {
            for (sp, sn, sft) in list_dir(&path) {
                if sft.is_file() && ext_ok(&sn) {
                    if let Ok(md) = std::fs::symlink_metadata(&sp) {
                        out.push(file_entry(sp, &sn, &md));
                    }
                }
            }
        }
    }
}

/// Scan `paths` in order, newest first (stable, like Python's `sort(reverse=True)`).
pub fn scan_folders(paths: &[String]) -> Vec<ScannedClip> {
    let mut files = Vec::new();
    for p in paths {
        scan_dir(p, &mut files);
    }
    files.sort_by(|a, b| b.sort_mtime.partial_cmp(&a.sort_mtime).unwrap_or(std::cmp::Ordering::Equal));
    files
}

// ---------------------------------------------------------------------------------------------
// ffmpeg / ffprobe
// ---------------------------------------------------------------------------------------------

/// ffmpeg.exe: next to our exe (`ffmpeg\`), the data folder's `ffmpeg\` (where the Python app
/// keeps it when run from source), PATH, then common install folders (Python `find_ffmpeg`).
pub fn find_ffmpeg() -> Option<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(d) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        dirs.push(d.join("ffmpeg"));
    }
    dirs.push(prefs::data_dir().join("ffmpeg"));
    for d in dirs {
        let p = d.join("ffmpeg.exe");
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&path) {
            let p = d.join("ffmpeg.exe");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    [r"C:\ffmpeg\bin\ffmpeg.exe", r"C:\Program Files\ffmpeg\bin\ffmpeg.exe"].iter().map(PathBuf::from).find(|p| p.is_file())
}

fn ffprobe_for(ffmpeg: &Path) -> PathBuf {
    let p = ffmpeg.with_file_name("ffprobe.exe");
    if p.is_file() { p } else { PathBuf::from("ffprobe") }
}

fn hidden(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Run with a timeout (killed after it), returning stdout.
fn run_timeout(cmd: &mut Command, timeout: Duration) -> Option<Vec<u8>> {
    let mut child = hidden(cmd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut out, &mut buf);
        buf
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(15)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    reader.join().ok()
}

/// Seconds, or 0 (Python `get_video_duration`).
pub fn video_duration(path: &str, ffmpeg: &Path) -> f64 {
    let out = run_timeout(
        Command::new(ffprobe_for(ffmpeg)).args(["-v", "error", "-show_entries", "format=duration", "-of", "default=noprint_wrappers=1:nokey=1", path]),
        Duration::from_secs(8),
    );
    out.and_then(|o| String::from_utf8_lossy(&o).trim().parse().ok()).unwrap_or(0.0)
}

/// The clip's audio tracks as mpv will number them (1.. in stream order), with the stream
/// title and `handler_name` tag. Lets the app set up a clip's audio mix before mpv opens it.
pub fn audio_tracks(path: &str, ffmpeg: &Path) -> Option<Vec<crate::player::audio::Track>> {
    let out = run_timeout(
        Command::new(ffprobe_for(ffmpeg)).args(["-v", "error", "-select_streams", "a", "-show_entries", "stream=index:stream_tags", "-of", "json", path]),
        Duration::from_secs(8),
    )?;
    let v: serde_json::Value = serde_json::from_slice(&out).ok()?;
    let streams = v.get("streams")?.as_array()?;
    let tag = |s: &serde_json::Value, key: &str| -> String {
        s.get("tags").and_then(|t| t.as_object()).and_then(|t| t.iter().find(|(k, _)| k.eq_ignore_ascii_case(key))).and_then(|(_, v)| v.as_str()).unwrap_or("").to_owned()
    };
    Some(streams.iter().enumerate().map(|(i, s)| crate::player::audio::Track { id: i as i64 + 1, title: tag(s, "title"), handler: tag(s, "handler_name") }).collect())
}

/// `m:ss` or `h:mm:ss` from whole seconds, as stored in prefs `durations`.
pub fn fmt_dur(secs: f64) -> String {
    let s = secs as i64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

#[cfg(test)]
#[test]
fn parse_dur_formats() {
    assert_eq!(parse_dur("1:05"), Some(65.0));
    assert_eq!(parse_dur("1:02:03"), Some(3723.0));
    assert_eq!(parse_dur(&fmt_dur(754.0)), Some(754.0));
    assert_eq!(parse_dur("x"), None);
}

/// Seconds from a prefs `durations` string (`m:ss` / `h:mm:ss`).
pub fn parse_dur(s: &str) -> Option<f64> {
    let mut secs = 0.0;
    for part in s.trim().split(':') {
        secs = secs * 60.0 + part.trim().parse::<f64>().ok()?;
    }
    Some(secs)
}

pub fn cache_dir() -> PathBuf {
    prefs::data_dir().join(".clipplayer_cache")
}

/// Python `cache_key`: md5 of `"{path}|{st_size}|{st_mtime}|tw140"` + ".jpg".
pub fn cache_key(path: &str) -> Option<String> {
    let md = std::fs::metadata(path).ok()?;
    let mtime = py_float_repr(md.modified().map(py_mtime).ok()?);
    let uid = format!("{path}|{}|{mtime}|tw{THUMB_W}", md.len());
    let d = Md5::digest(uid.as_bytes());
    Some(format!("{}.jpg", d.iter().map(|b| format!("{b:02x}")).collect::<String>()))
}

/// Cached thumbnail path, made with ffmpeg if missing (Python `get_thumb`).
pub fn get_thumb(path: &str, ffmpeg: Option<&Path>) -> Option<PathBuf> {
    let dir = cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    let cached = dir.join(cache_key(path)?);
    let ok = |p: &Path| std::fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false);
    if ok(&cached) {
        return Some(cached);
    }
    let ffmpeg = ffmpeg?;
    let dur = video_duration(path, ffmpeg);
    let seek = if dur <= 15.0 { 0.0 } else { (dur / 2.0).max(1.0) };
    let tw = THUMB_W * 2;
    run_timeout(
        Command::new(ffmpeg).args(["-y", "-ss", &(seek as i64).to_string(), "-i", path, "-vframes", "1", "-update", "1", "-vf", &format!("scale={tw}:-2"), "-q:v", "2"]).arg(&cached),
        Duration::from_secs(10),
    );
    ok(&cached).then_some(cached)
}

// ---------------------------------------------------------------------------------------------
// Background work: one scan thread per refresh, a fixed pool of thumbnail workers
// ---------------------------------------------------------------------------------------------

pub struct ThumbResult {
    pub path: String,
    /// New duration string for prefs `durations` (only when it was asked for).
    pub dur: Option<String>,
    pub image: Option<ColorImage>,
    /// Audio track layout (only when it was asked for).
    pub tracks: Option<Vec<crate::player::audio::Track>>,
}

struct Job {
    gen: u64,
    path: String,
    need_dur: bool,
    need_tracks: bool,
    /// Phase 2 of a refresh (already-known clips): throttled so new clips win.
    slow: bool,
}

struct Queue {
    jobs: Mutex<VecDeque<Job>>,
    cv: Condvar,
    gen: AtomicU64,
}

pub enum Msg {
    Scan(ScanResult),
    Thumb(ThumbResult),
}

pub struct Scanner {
    tx: Sender<Msg>,
    pub rx: Receiver<Msg>,
    q: Arc<Queue>,
    scan_id: Arc<AtomicU64>,
    repaint: eframe::egui::Context,
}

const WORKERS: usize = 4;

impl Scanner {
    pub fn new(repaint: eframe::egui::Context) -> Scanner {
        let (tx, rx) = std::sync::mpsc::channel();
        let q = Arc::new(Queue { jobs: Mutex::new(VecDeque::new()), cv: Condvar::new(), gen: AtomicU64::new(0) });
        let ffmpeg = find_ffmpeg();
        if ffmpeg.is_none() {
            eprintln!("ffmpeg.exe not found: no new thumbnails or durations");
        }
        for _ in 0..WORKERS {
            let (q, tx, ff, ctx) = (q.clone(), tx.clone(), ffmpeg.clone(), repaint.clone());
            std::thread::spawn(move || worker(q, tx, ff, ctx));
        }
        Scanner { tx, rx, q, scan_id: Arc::new(AtomicU64::new(0)), repaint }
    }

    /// Start a background scan. Older scans still running are ignored when they finish.
    pub fn scan(&self, paths: Vec<String>) -> u64 {
        let id = self.scan_id.fetch_add(1, Ordering::SeqCst) + 1;
        let paths: Vec<String> = paths.into_iter().filter(|p| Path::new(p).is_dir()).collect();
        let (tx, cur, ctx) = (self.tx.clone(), self.scan_id.clone(), self.repaint.clone());
        std::thread::spawn(move || {
            let clips = scan_folders(&paths);
            if cur.load(Ordering::SeqCst) != id {
                return; // a newer refresh started while we were scanning
            }
            let total_size = clips.iter().map(|c| c.size).sum();
            let _ = tx.send(Msg::Scan(ScanResult { scan_id: id, clips, total_size }));
            ctx.request_repaint();
        });
        id
    }

    pub fn current_scan(&self) -> u64 {
        self.scan_id.load(Ordering::SeqCst)
    }

    /// Replace the thumbnail queue. `priority` (clips new since the last scan) go first; when
    /// there are any, the rest is throttled (Python `_load_thumbnails` two phases).
    /// `need_dur(path)` says whether the duration is still unknown, `need_tracks(path)` the
    /// audio track layout.
    pub fn load_thumbs(&self, priority: Vec<String>, rest: Vec<String>, need_dur: impl Fn(&str) -> bool, need_tracks: impl Fn(&str) -> bool) {
        let gen = self.q.gen.fetch_add(1, Ordering::SeqCst) + 1;
        let slow_rest = !priority.is_empty();
        let mut jobs = self.q.jobs.lock().unwrap();
        jobs.clear();
        for p in priority {
            jobs.push_back(Job { gen, need_dur: need_dur(&p), need_tracks: need_tracks(&p), path: p, slow: false });
        }
        for p in rest {
            jobs.push_back(Job { gen, need_dur: need_dur(&p), need_tracks: need_tracks(&p), path: p, slow: slow_rest });
        }
        self.q.cv.notify_all();
    }
}

fn worker(q: Arc<Queue>, tx: Sender<Msg>, ffmpeg: Option<PathBuf>, ctx: eframe::egui::Context) {
    loop {
        let job = {
            let mut jobs = q.jobs.lock().unwrap();
            loop {
                if let Some(j) = jobs.pop_front() {
                    break j;
                }
                jobs = q.cv.wait(jobs).unwrap();
            }
        };
        if job.gen != q.gen.load(Ordering::SeqCst) {
            continue;
        }
        let mut dur = None;
        if job.need_dur {
            if let Some(ff) = &ffmpeg {
                let secs = video_duration(&job.path, ff);
                if secs > 0.0 {
                    dur = Some(fmt_dur(secs));
                }
            }
        }
        let tracks = if job.need_tracks { ffmpeg.as_deref().and_then(|ff| audio_tracks(&job.path, ff)) } else { None };
        let image = get_thumb(&job.path, ffmpeg.as_deref()).and_then(|p| decode(&p));
        if dur.is_some() || image.is_some() || tracks.is_some() {
            let _ = tx.send(Msg::Thumb(ThumbResult { path: job.path, dur, image, tracks }));
            ctx.request_repaint();
        }
        if job.slow {
            std::thread::sleep(Duration::from_millis(30 * WORKERS as u64 / 2));
        }
    }
}

fn decode(p: &Path) -> Option<ColorImage> {
    let img = image::open(p).ok()?.to_rgba8();
    Some(ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_dates() {
        let d = |s: &str| date_from_filename(s).map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string());
        assert_eq!(d("RocketLeague_2026-05-31_22-22-04.mp4").as_deref(), Some("2026-05-31 22:22:04"));
        assert_eq!(d("Counter-strike 2 2024.01.05 - 21.03.44.02.DVR.mp4").as_deref(), Some("2024-01-05 21:03:44"));
        assert_eq!(d("valorant-clip-from-Feb-25-2022.mp4").as_deref(), Some("2022-02-25 00:00:00"));
        assert_eq!(d("random.mkv"), None);
        assert_eq!(d("x_2026-02-30_10-00-00.mp4"), None);
    }

    #[test]
    fn game_names() {
        assert_eq!(game_name_from_filename("RocketLeague_2026-05-31_22-22-04.mp4"), "RocketLeague");
        assert_eq!(game_name_from_filename("CS2_2026-05-31_22-22-04.mp4"), "Counter-Strike 2");
        assert_eq!(game_name_from_filename("Escape-the-Backrooms__abc.mkv"), "Escape the Backrooms");
        assert_eq!(game_name_from_filename("Counter-strike 2 2024.01.05 - 21.03.44.02.DVR.mp4"), "Counter strike 2");
        assert_eq!(game_name_from_filename("valorant-clip-from-Feb-25-2022.mp4"), "valorant");
        assert_eq!(game_name_from_filename("JHGjhg.mp4"), "Moments");
    }

    #[test]
    fn paths_and_repr() {
        assert_eq!(join("Z:/All clips/Clips", "a.mkv"), "Z:/All clips/Clips\\a.mkv");
        assert_eq!(join("Z:/", "a.mkv"), "Z:/a.mkv");
        assert_eq!(py_float_repr(1717171717.0), "1717171717.0");
        assert_eq!(py_float_repr(1717171717.1234567), "1717171717.1234567");
        assert!(ext_ok("a.MKV") && !ext_ok(".mkv") && !ext_ok("a.txt"));
        assert_eq!(fmt_dur(65.9), "1:05");
        assert_eq!(fmt_dur(3725.0), "1:02:05");
    }

    /// Checks the cache key against thumbnails the Python app already made:
    /// `cargo test -- --ignored real_thumb_cache`
    #[test]
    #[ignore]
    fn real_thumb_cache_keys_match() {
        let p = prefs::Prefs::load();
        let clips = scan_folders(&p.checked_folders);
        assert!(!clips.is_empty(), "no clips in {:?}", p.checked_folders);
        let hits = clips.iter().take(200).filter(|c| cache_dir().join(cache_key(&c.path).unwrap()).is_file()).count();
        println!("{hits} of {} first clips have a Python-made thumbnail", clips.len().min(200));
        assert!(hits > 0);
    }
}
