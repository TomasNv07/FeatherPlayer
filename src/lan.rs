//! Direct PC -> phone sync for the ClipSync Android app on the same network (owner's request
//! 2026-11-21, replaces the old push-style phone sync). FeatherPlayer only serves; ClipSync finds
//! the PC (its last address, else a scan of its /24 on `PORT`) and pulls what is new.
//!
//! Shared clips = the clips cloud sync would copy (its folders, games filter and New/All mode)
//! plus everything cloud sync already copied that is still in the cloud folder, under the SAME
//! file names as in the cloud folder. So the phone never copies a clip twice (once from the PC,
//! once from Google Drive), and `clips.json` / thumbnails match the Drive ones.
//!
//! HTTP/1.1, one request per connection (`Connection: close`). Every path except `/ping` needs the
//! header `X-Token: <pairing code>` (settings `lan_code`, shown in Settings).
//!   GET /ping            {"app": "FeatherPlayer", "host": "<PC name>"}
//!   GET /clips.json      {"clips": [clips.json entries + "size"]}
//!   GET /clip/<file>     the clip's bytes
//!   GET /thumb/<file>    the clip's thumbnail (FeatherPlayer's cached JPEG)

use crate::prefs::{self, KeywordGroup};
use crate::scanner::{self, basename, local_from_ts};
use crate::sync::{clip_entry, game_key, scan_sig, ClipMeta};
use eframe::egui;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const PORT: u16 = 8766;

/// What the server shares, copied on the UI thread every few seconds.
pub struct LanInput {
    pub folders: Vec<String>,
    /// the cloud folder ("" = none)
    pub dest: String,
    /// only clips modified after this (cloud sync "New clips"), 0 = all
    pub since: f64,
    pub games: BTreeSet<String>,
    pub groups: Vec<(String, KeywordGroup)>,
    pub meta: ClipMeta,
    /// (source path, path in the cloud folder) of every clip cloud sync copied
    pub copied: Vec<(String, String)>,
}

struct Shared {
    enabled: AtomicBool,
    code: Mutex<String>,
    input: Mutex<Option<Arc<LanInput>>>,
    /// lowercase file name -> (path to serve, source path) from the last listing
    files: Mutex<HashMap<String, (String, String)>>,
    status: Mutex<String>,
    ctx: egui::Context,
}

pub struct Lan {
    sh: Arc<Shared>,
    pub enabled: bool,
    pub code: String,
    started: bool,
    last_input: Instant,
}

fn now_ts() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn new_code() -> String {
    let n = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos() as u64 ^ d.as_secs()).unwrap_or(0);
    format!("{:06}", 100_000 + n % 900_000)
}

impl Lan {
    pub fn new(ctx: egui::Context) -> Lan {
        let st = prefs::read_settings();
        let enabled = st.get("lan_enabled").and_then(|v| v.as_bool()).unwrap_or(false);
        let code = match st.get("lan_code").and_then(|v| v.as_str()) {
            Some(c) if c.len() == 6 => c.to_owned(),
            _ => {
                let c = new_code();
                prefs::save_setting("lan_code", json!(c));
                c
            }
        };
        let sh = Arc::new(Shared {
            enabled: AtomicBool::new(enabled),
            code: Mutex::new(code.clone()),
            input: Mutex::new(None),
            files: Mutex::new(HashMap::new()),
            status: Mutex::new(if enabled { "Waiting for the phone".into() } else { "Off".into() }),
            ctx,
        });
        let mut lan = Lan { sh, enabled, code, started: false, last_input: Instant::now() - Duration::from_secs(60) };
        if enabled {
            lan.start();
        }
        lan
    }

    fn start(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        let sh = self.sh.clone();
        std::thread::spawn(move || match TcpListener::bind(("0.0.0.0", PORT)) {
            Ok(l) => {
                for conn in l.incoming().flatten() {
                    if !sh.enabled.load(Ordering::SeqCst) {
                        continue; // dropped = closed
                    }
                    let sh = sh.clone();
                    std::thread::spawn(move || {
                        let _ = handle(&sh, conn);
                    });
                }
            }
            Err(e) => set_status(&sh, format!("Can't open port {PORT}: {e}")),
        });
    }

    pub fn status(&self) -> String {
        self.sh.status.lock().unwrap().clone()
    }

    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        self.sh.enabled.store(on, Ordering::SeqCst);
        prefs::save_setting("lan_enabled", json!(on));
        set_status(&self.sh, if on { "Waiting for the phone".into() } else { "Off".into() });
        if on {
            self.last_input = Instant::now() - Duration::from_secs(60);
            self.start();
        }
    }

    /// A new pairing code (the phone has to type it again).
    pub fn new_code(&mut self) {
        self.code = new_code();
        *self.sh.code.lock().unwrap() = self.code.clone();
        prefs::save_setting("lan_code", json!(self.code));
    }

    /// Refreshes what is shared (every 5 s while on). `input` runs on the UI thread.
    pub fn tick(&mut self, input: impl FnOnce() -> LanInput) {
        if !self.enabled || self.last_input.elapsed() < Duration::from_secs(5) {
            return;
        }
        self.last_input = Instant::now();
        *self.sh.input.lock().unwrap() = Some(Arc::new(input()));
    }

    /// This PC's address on the network, for the Settings page.
    pub fn address(&self) -> String {
        if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
            if s.connect("10.255.255.255:1").is_ok() {
                if let Ok(a) = s.local_addr() {
                    return format!("{}:{PORT}", a.ip());
                }
            }
        }
        format!("port {PORT}")
    }
}

fn set_status(sh: &Shared, t: String) {
    *sh.status.lock().unwrap() = t;
    sh.ctx.request_repaint();
}

/// %XX decoding of the request path (ClipSync sends names with Uri.encode).
fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The shared clips: (clips.json entries, lowercase name -> (path to serve, source path)).
fn listing(inp: &LanInput) -> (Vec<Value>, HashMap<String, (String, String)>) {
    let mut files: HashMap<String, (String, String)> = HashMap::new();
    let mut srcs: HashSet<String> = HashSet::new();
    let mut rows: Vec<(String, String, u64)> = Vec::new(); // (source, name, size)
    // 1. what cloud sync copied and is still in the cloud folder (same name as on Drive)
    for (src, target) in &inp.copied {
        let Ok(md) = std::fs::metadata(target) else { continue };
        if !md.is_file() {
            continue;
        }
        let name = basename(target).to_owned();
        if files.contains_key(&name.to_lowercase()) {
            continue;
        }
        // the local original is faster to read than the Drive copy, and the same bytes
        let serve = if std::fs::metadata(src).map(|m| m.len() == md.len()).unwrap_or(false) { src.clone() } else { target.clone() };
        files.insert(name.to_lowercase(), (serve, src.clone()));
        srcs.insert(src.clone());
        rows.push((src.clone(), name, md.len()));
    }
    // 2. what cloud sync would copy but hasn't yet (or there is no cloud folder)
    let dest = if inp.dest.is_empty() { "\0none" } else { inp.dest.as_str() };
    let now = now_ts();
    for (p, size, mt) in scan_sig(&inp.folders, dest) {
        if srcs.contains(&p) || size == 0 || now - mt < 5.0 || mt < inp.since - 2.0 {
            continue; // already listed / still recording / older than "New clips"
        }
        if !inp.games.is_empty() && !inp.games.contains(&game_key(&p, &inp.groups)) {
            continue;
        }
        let mut name = basename(&p).to_owned();
        if files.contains_key(&name.to_lowercase()) {
            // two clips with the same file name: like cloud sync's copy_one, "name (1).ext"
            let stem = scanner::stem(&name).to_owned();
            let ext = name[stem.len()..].to_owned();
            let mut k = 1;
            while files.contains_key(&format!("{stem} ({k}){ext}").to_lowercase()) {
                k += 1;
            }
            name = format!("{stem} ({k}){ext}");
        }
        files.insert(name.to_lowercase(), (p.clone(), p.clone()));
        rows.push((p, name, size));
    }
    rows.sort_by(|a, b| a.1.cmp(&b.1));
    let clips = rows
        .iter()
        .map(|(src, name, size)| {
            let mut e = clip_entry(src, name, &inp.meta);
            e["size"] = json!(size);
            e
        })
        .collect();
    (clips, files)
}

fn reply(s: &mut TcpStream, code: u16, ctype: &str, body: &[u8]) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Error",
    };
    s.write_all(format!("HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes())?;
    s.write_all(body)?;
    s.flush()
}

fn send_file(s: &mut TcpStream, path: &str) -> std::io::Result<()> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let _ = socket2::SockRef::from(&*s).set_send_buffer_size(2 * 1024 * 1024);
    s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n").as_bytes())?;
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let k = f.read(&mut buf)?;
        if k == 0 {
            break;
        }
        s.write_all(&buf[..k])?;
    }
    s.flush()
}

fn handle(sh: &Shared, mut s: TcpStream) -> std::io::Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    s.set_write_timeout(Some(Duration::from_secs(60)))?;
    let _ = s.set_nodelay(true);
    let mut data = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let k = s.read(&mut tmp)?;
        if k == 0 {
            return Ok(());
        }
        data.extend_from_slice(&tmp[..k]);
        if data.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if data.len() > 16 * 1024 {
            return Ok(());
        }
    }
    let head = String::from_utf8_lossy(&data).into_owned();
    let mut first = head.lines().next().unwrap_or("").split_whitespace();
    let (method, raw_path) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
    let token = head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case("x-token").then(|| v.trim().to_owned())
    });
    let path = unquote(raw_path.split('?').next().unwrap_or(""));
    if method != "GET" {
        return reply(&mut s, 404, "application/json", b"{}");
    }
    if path == "/ping" {
        let host = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into());
        return reply(&mut s, 200, "application/json", json!({"app": "FeatherPlayer", "host": host}).to_string().as_bytes());
    }
    if token.as_deref() != Some(sh.code.lock().unwrap().as_str()) {
        return reply(&mut s, 401, "application/json", b"{}");
    }
    let lookup = |name: &str| -> Option<(String, String)> {
        let key = name.to_lowercase();
        if let Some(hit) = sh.files.lock().unwrap().get(&key).cloned() {
            return Some(hit);
        }
        let inp = sh.input.lock().unwrap().clone()?;
        let (_, files) = listing(&inp);
        let hit = files.get(&key).cloned();
        *sh.files.lock().unwrap() = files;
        hit
    };
    if path == "/clips.json" {
        let Some(inp) = sh.input.lock().unwrap().clone() else {
            return reply(&mut s, 200, "application/json", b"{\"clips\": []}");
        };
        let (clips, files) = listing(&inp);
        let n = clips.len();
        *sh.files.lock().unwrap() = files;
        let peer = s.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
        let at = local_from_ts(now_ts()).map(|t| t.format("%H:%M").to_string()).unwrap_or_default();
        set_status(sh, format!("Phone {peer} checked at {at} ({n} clips shared)"));
        let body = serde_json::to_string_pretty(&json!({"clips": clips})).unwrap_or_default();
        return reply(&mut s, 200, "application/json", body.as_bytes());
    }
    if let Some(name) = path.strip_prefix("/clip/") {
        return match lookup(name) {
            Some((serve, _)) => send_file(&mut s, &serve),
            None => reply(&mut s, 404, "application/json", b"{}"),
        };
    }
    if let Some(name) = path.strip_prefix("/thumb/") {
        let name = name.strip_suffix(".jpg").unwrap_or(name);
        if let Some((_, src)) = lookup(name) {
            if let Some(t) = scanner::get_thumb(&src, scanner::find_ffmpeg().as_deref()) {
                return send_file(&mut s, &t.to_string_lossy());
            }
        }
        return reply(&mut s, 404, "application/json", b"{}");
    }
    reply(&mut s, 404, "application/json", b"{}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unquote_paths() {
        assert_eq!(unquote("/clip/My%20Clip%20%C3%A4.mkv"), "/clip/My Clip ä.mkv");
        assert_eq!(unquote("/clip/a%2"), "/clip/a%2");
    }

    fn get(port: u16, path: &str, token: Option<&str>) -> (u16, Vec<u8>) {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let tok = token.map(|t| format!("X-Token: {t}\r\n")).unwrap_or_default();
        s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n{tok}\r\n").as_bytes()).unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        let i = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&buf[..i]).into_owned();
        (head.split_whitespace().nth(1).unwrap().parse().unwrap(), buf[i + 4..].to_vec())
    }

    #[test]
    fn serves_clips_to_the_phone() {
        let d = std::env::temp_dir().join(format!("fp_lan_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let clip = d.join("My Clip ä.mkv");
        std::fs::write(&clip, vec![7u8; 3_000_000]).unwrap();
        // older than 5 s, so it isn't "still recording"
        std::fs::File::options().write(true).open(&clip).unwrap().set_modified(SystemTime::now() - Duration::from_secs(60)).unwrap();
        let folder = d.to_string_lossy().replace('\\', "/");
        let sh = Arc::new(Shared {
            enabled: AtomicBool::new(true),
            code: Mutex::new("123456".into()),
            input: Mutex::new(Some(Arc::new(LanInput {
                folders: vec![folder],
                dest: String::new(),
                since: 0.0,
                games: BTreeSet::new(),
                groups: Vec::new(),
                meta: ClipMeta { starred: BTreeSet::new(), liked: BTreeSet::new(), trims: HashMap::new(), durations: HashMap::new(), groups: Vec::new(), labels: HashMap::new() },
                copied: Vec::new(),
            }))),
            files: Mutex::new(HashMap::new()),
            status: Mutex::new(String::new()),
            ctx: egui::Context::default(),
        });
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let sh2 = sh.clone();
        std::thread::spawn(move || {
            for c in l.incoming().flatten() {
                let sh = sh2.clone();
                std::thread::spawn(move || { let _ = handle(&sh, c); });
            }
        });
        let (st, body) = get(port, "/ping", None);
        assert_eq!(st, 200);
        assert!(String::from_utf8_lossy(&body).contains("FeatherPlayer"));
        assert_eq!(get(port, "/clips.json", None).0, 401);
        assert_eq!(get(port, "/clips.json", Some("000000")).0, 401);
        let (st, body) = get(port, "/clips.json", Some("123456"));
        assert_eq!(st, 200);
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["clips"][0]["file"], "My Clip ä.mkv");
        assert_eq!(v["clips"][0]["size"], 3_000_000);
        let (st, body) = get(port, "/clip/My%20Clip%20%C3%A4.mkv", Some("123456"));
        assert_eq!((st, body.len()), (200, 3_000_000));
        assert_eq!(get(port, "/clip/nope.mkv", Some("123456")).0, 404);
        assert!(sh.status.lock().unwrap().contains("1 clips shared"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
