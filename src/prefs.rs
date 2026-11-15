//! Data folder, `settings.json` and the clip prefs file, compatible with the Python app.
//!
//! Files in the data folder (`%USERPROFILE%\FeatherPlayer`):
//! - `settings.json`: app settings, pretty-printed (indent 2), written via `.tmp` + rename.
//!   Always read-modify-write one key at a time (`save_setting`), like Python `_save_setting`.
//! - `.clipplayer_prefs.json`: clip data (starred, liked, trims, folders, categories...).
//!   Before every save the current file is copied to `backups\prefs_YYYYmmdd_HHMMSS.json`
//!   (newest 30 kept). A save that would empty all protected keys while the file on disk
//!   has data is refused. If the main file is missing/empty/broken, the newest backup is loaded.
//!
//! Unlike Python, keys this port does not know yet are kept on save (Python only writes its
//! own fixed key list), so nothing written by the Python app is lost.

use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const PREFS_FILE: &str = ".clipplayer_prefs.json";
pub const SETTINGS_FILE: &str = "settings.json";
const BACKUP_KEEP: usize = 30;
/// Keys whose data must not be lost to an accidental wipe (Python `_PROTECTED_PREF_KEYS`).
const PROTECTED_KEYS: [&str; 6] = ["starred", "liked", "category_renames", "keyword_groups", "trims", "trim_enabled"];

// ---------------------------------------------------------------------------------------------
// Data folder
// ---------------------------------------------------------------------------------------------

/// `%USERPROFILE%\FeatherPlayer`. On first run an old `FeatherLauncher2` folder is renamed to it;
/// if that rename fails the old folder keeps being used (Python `_resolve_data_dir`).
pub fn data_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_default();
        resolve_data_dir(&home)
    })
}

fn resolve_data_dir(home: &Path) -> PathBuf {
    let new = home.join("FeatherPlayer");
    let old = home.join("FeatherLauncher2");
    if !new.exists() && old.is_dir() && fs::rename(&old, &new).is_err() {
        return old;
    }
    new
}

fn read_json_object(path: &Path) -> Option<Map<String, Value>> {
    let text = fs::read_to_string(path).ok()?;
    match serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// Write `text` to `path` through `path.tmp` + rename, so a crash never leaves a half file.
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

// ---------------------------------------------------------------------------------------------
// settings.json
// ---------------------------------------------------------------------------------------------

pub fn read_settings() -> Map<String, Value> {
    read_settings_in(data_dir())
}

pub fn write_settings(s: &Map<String, Value>) {
    write_settings_in(data_dir(), s)
}

/// Set one key and write the file back (other keys untouched).
pub fn save_setting(key: &str, value: Value) {
    let dir = data_dir();
    let mut s = read_settings_in(dir);
    s.insert(key.to_owned(), value);
    write_settings_in(dir, &s);
}

fn read_settings_in(dir: &Path) -> Map<String, Value> {
    read_json_object(&dir.join(SETTINGS_FILE)).unwrap_or_default()
}

fn write_settings_in(dir: &Path, s: &Map<String, Value>) {
    if let Ok(text) = serde_json::to_string_pretty(s) {
        let _ = write_atomic(&dir.join(SETTINGS_FILE), &text);
    }
}

/// Lenient typed reads from a JSON map (wrong type -> default, like Python's `bool(...)`/`.get`).
pub trait JsonGet {
    fn get_bool(&self, key: &str, default: bool) -> bool;
    fn get_i64(&self, key: &str, default: i64) -> i64;
    fn get_str(&self, key: &str, default: &str) -> String;
    fn get_str_list(&self, key: &str) -> Vec<String>;
}

impl JsonGet for Map<String, Value> {
    fn get_bool(&self, key: &str, default: bool) -> bool {
        match self.get(key) {
            None => default,
            // Python truthiness
            Some(Value::Bool(b)) => *b,
            Some(Value::Null) => false,
            Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
            Some(Value::String(s)) => !s.is_empty(),
            Some(Value::Array(a)) => !a.is_empty(),
            Some(Value::Object(o)) => !o.is_empty(),
        }
    }
    fn get_i64(&self, key: &str, default: i64) -> i64 {
        match self.get(key) {
            Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(default),
            Some(Value::String(s)) => s.trim().parse().unwrap_or(default),
            _ => default,
        }
    }
    fn get_str(&self, key: &str, default: &str) -> String {
        match self.get(key) {
            Some(Value::String(s)) => s.clone(),
            _ => default.to_owned(),
        }
    }
    fn get_str_list(&self, key: &str) -> Vec<String> {
        match self.get(key) {
            Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect(),
            _ => Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// .clipplayer_prefs.json
// ---------------------------------------------------------------------------------------------

/// Clip prefs. Typed fields are the ones ported so far; everything else stays in `raw`
/// untouched and is written back as it was. Not typed yet (later steps): `durations`
/// (path -> "m:ss"), `trims` (name -> [start, end] on a 0..10000 scale, floats),
/// `category_renames`, `keyword_groups`.
pub struct Prefs {
    dir: PathBuf,
    pub raw: Map<String, Value>,
    pub folder: String,
    pub saved_folders: Vec<String>,
    pub checked_folders: Vec<String>,
    pub active_folder: String,
    pub volume: i64,
    pub starred: BTreeSet<String>,
    pub liked: BTreeSet<String>,
    pub star_copy_folder: String,
    pub star_copy_enabled: bool,
    pub trim_enabled: BTreeSet<String>,
    pub game_filter: String,
    pub game_multi: BTreeSet<String>,
}

impl Prefs {
    pub fn load() -> Prefs {
        Prefs::load_in(data_dir())
    }

    pub fn load_in(dir: &Path) -> Prefs {
        let raw = load_data(dir);
        let folder = raw.get_str("folder", "");
        // old prefs stored the star copy folder under "drive_sync_folder"
        let star_copy_folder = match raw.get("star_copy_folder") {
            Some(Value::String(s)) => s.clone(),
            _ => raw.get_str("drive_sync_folder", ""),
        };
        let game_multi: BTreeSet<String> = raw.get_str_list("game_multi").into_iter().collect();
        let mut game_filter = raw.get_str("game_filter", "All");
        if !game_multi.is_empty() {
            game_filter = "All".into();
        }
        let mut p = Prefs {
            dir: dir.to_owned(),
            saved_folders: raw.get_str_list("saved_folders"),
            checked_folders: raw.get_str_list("checked_folders"),
            active_folder: raw.get_str("active_folder", &folder),
            folder,
            volume: raw.get_i64("volume", 55),
            starred: raw.get_str_list("starred").into_iter().collect(),
            liked: raw.get_str_list("liked").into_iter().collect(),
            star_copy_folder,
            star_copy_enabled: raw.get_bool("star_copy_enabled", false),
            trim_enabled: raw.get_str_list("trim_enabled").into_iter().collect(),
            game_filter,
            game_multi,
            raw,
        };
        p.fix_folders();
        p
    }

    /// Back-compat folder fixes done on load by the Python app.
    fn fix_folders(&mut self) {
        if !self.folder.is_empty() && !self.saved_folders.contains(&self.folder) {
            self.saved_folders.push(self.folder.clone());
        }
        let a = self.active_folder.clone();
        if !a.is_empty() && !self.checked_folders.contains(&a) {
            self.checked_folders = vec![a.clone()];
        }
        if !a.is_empty() && self.checked_folders.first().is_some_and(|f| *f != a) {
            self.checked_folders.retain(|f| *f != a);
            self.checked_folders.insert(0, a);
        }
    }

    /// The full JSON object that `save` would write.
    pub fn to_json(&self) -> Map<String, Value> {
        let mut d = self.raw.clone();
        let list = |v: &mut dyn Iterator<Item = &String>| Value::Array(v.map(|s| Value::String(s.clone())).collect());
        d.insert("folder".into(), self.folder.clone().into());
        d.insert("saved_folders".into(), list(&mut self.saved_folders.iter()));
        d.insert("checked_folders".into(), list(&mut self.checked_folders.iter()));
        d.insert("active_folder".into(), self.active_folder.clone().into());
        d.insert("volume".into(), self.volume.into());
        d.insert("starred".into(), list(&mut self.starred.iter()));
        d.insert("liked".into(), list(&mut self.liked.iter()));
        d.insert("star_copy_folder".into(), self.star_copy_folder.clone().into());
        d.insert("star_copy_enabled".into(), self.star_copy_enabled.into());
        d.insert("trim_enabled".into(), list(&mut self.trim_enabled.iter()));
        d.insert("game_filter".into(), self.game_filter.clone().into());
        d.insert("game_multi".into(), list(&mut self.game_multi.iter()));
        for k in ["durations", "trims", "category_renames", "keyword_groups"] {
            d.entry(k).or_insert_with(|| Value::Object(Map::new()));
        }
        d
    }

    /// Back up the file on disk, then write (unless it looks like an accidental wipe).
    /// Returns false if the save was refused or failed.
    pub fn save(&mut self) -> bool {
        let data = self.to_json();
        let path = self.dir.join(PREFS_FILE);
        backup_current(&self.dir);
        if looks_like_accidental_wipe(&path, &data) {
            return false;
        }
        let Ok(text) = serde_json::to_string(&data) else { return false };
        let ok = write_atomic(&path, &text).is_ok();
        if ok {
            self.raw = data;
        }
        ok
    }
}

fn backup_dir(dir: &Path) -> PathBuf {
    let d = dir.join("backups");
    let _ = fs::create_dir_all(&d);
    d
}

/// Backups sorted oldest to newest by modification time.
fn list_backups(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(backup_dir(dir)) else { return Vec::new() };
    let mut v: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("prefs_") && n.ends_with(".json")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    v.sort();
    v.into_iter().map(|(_, p)| p).collect()
}

fn load_data(dir: &Path) -> Map<String, Value> {
    if let Some(m) = read_json_object(&dir.join(PREFS_FILE)) {
        if !m.is_empty() {
            return m;
        }
    }
    // Main file missing / unreadable / empty -> newest backup, never silently {}
    // (that would let the next save wipe everything).
    list_backups(dir).last().and_then(|p| read_json_object(p)).unwrap_or_default()
}

fn looks_like_accidental_wipe(path: &Path, new: &Map<String, Value>) -> bool {
    let Some(old) = read_json_object(path) else { return false };
    let has = |m: &Map<String, Value>| PROTECTED_KEYS.iter().any(|k| m.get_bool(k, false));
    has(&old) && !has(new)
}

/// Copy the current prefs file into backups (keeps its mtime, like `shutil.copy2`), keep newest 30.
fn backup_current(dir: &Path) {
    let src = dir.join(PREFS_FILE);
    if !src.exists() {
        return;
    }
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let dst = backup_dir(dir).join(format!("prefs_{stamp}.json"));
    if fs::copy(&src, &dst).is_err() {
        return;
    }
    if let Ok(f) = fs::File::options().write(true).open(&dst) {
        if let Ok(m) = fs::metadata(&src).and_then(|m| m.modified()) {
            let _ = f.set_modified(m);
        }
    }
    let all = list_backups(dir);
    if all.len() > BACKUP_KEEP {
        for old in &all[..all.len() - BACKUP_KEEP] {
            let _ = fs::remove_file(old);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fp_prefs_test_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(dir: &Path, v: Value) {
        fs::write(dir.join(PREFS_FILE), v.to_string()).unwrap();
    }

    #[test]
    fn roundtrip_keeps_unknown_and_untyped_keys() {
        let d = tmp("roundtrip");
        write(&d, json!({
            "volume": 40, "starred": ["a.mkv"], "trims": {"x.mkv": [4628.82, 6870.45]},
            "keyword_groups": {"minecraft": {"label": "Minecraft", "keywords": ["javaw"]}},
            "something_new": 1
        }));
        let mut p = Prefs::load_in(&d);
        assert_eq!(p.volume, 40);
        p.liked.insert("b.mkv".into());
        assert!(p.save());
        let back = read_json_object(&d.join(PREFS_FILE)).unwrap();
        assert_eq!(back["something_new"], json!(1));
        assert_eq!(back["trims"], json!({"x.mkv": [4628.82, 6870.45]}));
        assert_eq!(back["keyword_groups"]["minecraft"]["keywords"], json!(["javaw"]));
        assert_eq!(back["liked"], json!(["b.mkv"]));
        assert_eq!(list_backups(&d).len(), 1);
    }

    #[test]
    fn refuses_accidental_wipe() {
        let d = tmp("wipe");
        write(&d, json!({"starred": ["a.mkv"], "volume": 55}));
        let mut p = Prefs::load_in(&d);
        p.starred.clear();
        assert!(!p.save());
        assert_eq!(read_json_object(&d.join(PREFS_FILE)).unwrap()["starred"], json!(["a.mkv"]));
    }

    #[test]
    fn falls_back_to_newest_backup() {
        let d = tmp("fallback");
        let b = backup_dir(&d);
        fs::write(b.join("prefs_20200101_000000.json"), json!({"volume": 11}).to_string()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(b.join("prefs_20200102_000000.json"), json!({"volume": 22}).to_string()).unwrap();
        fs::write(d.join(PREFS_FILE), "{broken").unwrap();
        assert_eq!(Prefs::load_in(&d).volume, 22);
    }

    #[test]
    fn folder_back_compat() {
        let d = tmp("folders");
        write(&d, json!({"folder": "Z:/a", "saved_folders": ["Z:/b"], "active_folder": "Z:/b", "checked_folders": ["Z:/c", "Z:/b"]}));
        let p = Prefs::load_in(&d);
        assert_eq!(p.saved_folders, ["Z:/b", "Z:/a"]);
        assert_eq!(p.checked_folders, ["Z:/b", "Z:/c"]);
    }

    #[test]
    fn settings_save_one_key() {
        let d = tmp("settings");
        fs::write(d.join(SETTINGS_FILE), json!({"theme": "Obsidian", "loop_clip": true}).to_string()).unwrap();
        let mut s = read_settings_in(&d);
        s.insert("theme".into(), json!("Midnight"));
        write_settings_in(&d, &s);
        let s = read_settings_in(&d);
        assert_eq!(s.get_str("theme", ""), "Midnight");
        assert!(s.get_bool("loop_clip", false));
    }

    /// Read-only check against the real data folder: `cargo test -- --ignored real_prefs`
    #[test]
    #[ignore]
    fn real_prefs_roundtrip_read_only() {
        let disk = read_json_object(&data_dir().join(PREFS_FILE)).expect("no prefs file");
        let out = Prefs::load().to_json();
        for (k, v) in &disk {
            match (v, &out[k]) {
                // sets may come back in a different order
                (Value::Array(a), Value::Array(b)) if k != "saved_folders" && k != "checked_folders" => {
                    let (mut a, mut b) = (a.clone(), b.clone());
                    a.sort_by_key(|x| x.to_string());
                    b.sort_by_key(|x| x.to_string());
                    assert_eq!(a, b, "{k}");
                }
                (a, b) => assert_eq!(a, b, "{k}"),
            }
        }
    }

    #[test]
    fn migrates_old_folder() {
        let home = tmp("migrate");
        fs::create_dir_all(home.join("FeatherLauncher2")).unwrap();
        assert_eq!(resolve_data_dir(&home), home.join("FeatherPlayer"));
        assert!(home.join("FeatherPlayer").is_dir() && !home.join("FeatherLauncher2").exists());
    }
}
