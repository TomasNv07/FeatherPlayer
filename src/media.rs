//! ffmpeg jobs on background threads: "Apply trim" (cut a clip down to its trim, in place or
//! as a copy) and "Compress" (to a target size, NVENC first with a libx264 fallback).
//! Ported from FeatherPlayer `_apply_trim_cut` and `_open_compress_dialog` (`_compress_one`).

use crate::scanner::{self, cache_dir, cache_key, video_duration};
use regex::Regex;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Python `self._nvenc_ok`: once NVENC fails, later jobs go straight to the CPU encoder.
static NVENC_OK: AtomicBool = AtomicBool::new(true);

pub enum MediaMsg {
    /// Ask the UI thread to make mpv let go of this file (before it is replaced).
    ReleaseFile(String),
    /// Apply trim finished: (path, ok, error message).
    TrimCut(String, bool, String),
    /// Save as copy finished: (new path or None, error message).
    TrimCopy(Option<String>, String),
    /// Compress progress: (overall fraction, status text).
    Progress(f32, String),
    /// One clip of a compress batch done: (source, ok, new path).
    CompressedOne(String, bool, Option<String>),
    /// The compress batch is over: (done, failed names, cancelled).
    CompressDone(usize, Vec<String>, bool),
}

fn hidden(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

fn ffprobe_for(ff: &Path) -> PathBuf {
    let p = ff.with_file_name("ffprobe.exe");
    if p.is_file() { p } else { PathBuf::from("ffprobe") }
}

fn probe(ff: &Path, args: &[&str]) -> Option<String> {
    let out = hidden(Command::new(ffprobe_for(ff)).args(args)).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Video bitrate in kb/s (stream, else container, else size/duration), 0 if unknown
/// (Python `get_video_bitrate_kbps`).
pub fn video_bitrate_kbps(path: &str, ff: &Path) -> u64 {
    for (entries, sel) in [("stream=bit_rate", Some("v:0")), ("format=bit_rate", None)] {
        let mut args = vec!["-v", "error"];
        if let Some(s) = sel {
            args.extend(["-select_streams", s]);
        }
        args.extend(["-show_entries", entries, "-of", "default=noprint_wrappers=1:nokey=1", path]);
        if let Some(out) = probe(ff, &args) {
            for line in out.lines() {
                if let Ok(v) = line.trim().parse::<u64>() {
                    if v > 0 {
                        return v / 1000;
                    }
                }
            }
        }
    }
    let dur = video_duration(path, ff);
    match std::fs::metadata(path) {
        Ok(m) if dur > 0.0 => (m.len() as f64 * 8.0 / dur / 1000.0) as u64,
        _ => 0,
    }
}

/// Bitrate (bits/s, 0 = unknown) of every audio stream, or None if ffprobe failed.
pub fn audio_bitrates(path: &str, ff: &Path) -> Option<Vec<u64>> {
    let out = probe(ff, &["-v", "error", "-select_streams", "a", "-show_entries", "stream=bit_rate", "-of", "json", path])?;
    let v: serde_json::Value = serde_json::from_str(if out.trim().is_empty() { "{}" } else { &out }).ok()?;
    let streams = v.get("streams").and_then(|s| s.as_array()).cloned().unwrap_or_default();
    Some(
        streams
            .iter()
            .map(|s| match s.get("bit_rate") {
                Some(serde_json::Value::String(t)) => t.parse().unwrap_or(0),
                Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0),
                _ => 0,
            })
            .collect(),
    )
}

/// (width, height, fps) of the first video stream, zeros if unknown.
pub fn video_info(path: &str, ff: &Path) -> (u32, u32, f64) {
    let Some(out) = probe(ff, &["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height,r_frame_rate", "-of", "csv=p=0:s=,", path]) else {
        return (0, 0, 0.0);
    };
    let parts: Vec<&str> = out.trim().split(',').collect();
    if parts.len() != 3 {
        return (0, 0, 0.0);
    }
    let fps = match parts[2].split_once('/') {
        Some((n, d)) => n.parse::<f64>().unwrap_or(0.0) / d.parse::<f64>().unwrap_or(1.0),
        None => parts[2].parse().unwrap_or(0.0),
    };
    (parts[0].parse().unwrap_or(0), parts[1].parse().unwrap_or(0), if fps > 0.0 { fps } else { 30.0 })
}

/// Python `_copy_file_times`: dst gets src's modified and creation time.
pub fn copy_file_times(src: &str, dst: &str) {
    let Ok(md) = std::fs::metadata(src) else { return };
    let Ok(f) = std::fs::File::options().write(true).open(dst) else { return };
    let mut t = std::fs::FileTimes::new();
    if let Ok(a) = md.accessed() {
        t = t.set_accessed(a);
    }
    if let Ok(m) = md.modified() {
        t = t.set_modified(m);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTimesExt;
        if let Ok(c) = md.created() {
            t = t.set_created(c);
        }
    }
    let _ = f.set_times(t);
}

/// Python `shutil.copystat`: access + modified time (creation time is left alone).
fn copy_stat(src: &str, dst: &str) {
    let (Ok(md), Ok(f)) = (std::fs::metadata(src), std::fs::File::options().write(true).open(dst)) else { return };
    let mut t = std::fs::FileTimes::new();
    if let Ok(a) = md.accessed() {
        t = t.set_accessed(a);
    }
    if let Ok(m) = md.modified() {
        t = t.set_modified(m);
    }
    let _ = f.set_times(t);
}

fn split_ext(path: &str) -> (&str, &str) {
    let name = scanner::basename(path);
    let stem = scanner::stem(name);
    let cut = path.len() - (name.len() - stem.len());
    (&path[..cut], &path[cut..])
}

fn file_ok(p: &str) -> bool {
    std::fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false)
}

fn trim_format(ext: &str) -> &'static str {
    match ext {
        ".mp4" | ".m4v" => "mp4",
        ".mov" => "mov",
        ".mkv" => "matroska",
        ".webm" => "webm",
        ".avi" => "avi",
        ".wmv" => "asf",
        ".flv" => "flv",
        _ => "matroska",
    }
}

// ---------------------------------------------------------------------------------------------
// Apply trim
// ---------------------------------------------------------------------------------------------

pub struct TrimJob {
    pub path: String,
    /// [start, end] on the 0..10000 scale.
    pub range: (f64, f64),
    pub as_copy: bool,
    pub use_gpu: bool,
    pub ffmpeg: PathBuf,
}

/// Cut the clip down to the trim. Re-encodes the video (exact cut points) with NVENC or
/// libx264, never above the source bitrate, and copies all audio tracks.
pub fn start_trim(job: TrimJob, tx: Sender<MediaMsg>, ctx: eframe::egui::Context) {
    std::thread::spawn(move || {
        let path = job.path.clone();
        let tmp = format!("{path}.trimtmp"); // not a video extension: the folder scan ignores it
        let res = run_trim(&job, &tmp, &tx);
        if res.is_err() && Path::new(&tmp).exists() {
            let _ = std::fs::remove_file(&tmp);
        }
        let msg = match (res, job.as_copy) {
            (Ok(Some(dest)), true) => MediaMsg::TrimCopy(Some(dest), String::new()),
            (Err(e), true) => MediaMsg::TrimCopy(None, e),
            (Ok(_), false) => MediaMsg::TrimCut(path, true, String::new()),
            (Err(e), false) => MediaMsg::TrimCut(path, false, e),
            (Ok(None), true) => MediaMsg::TrimCopy(None, "no output".into()),
        };
        let _ = tx.send(msg);
        ctx.request_repaint();
    });
}

fn run_trim(job: &TrimJob, tmp: &str, tx: &Sender<MediaMsg>) -> Result<Option<String>, String> {
    let (path, ff) = (job.path.as_str(), job.ffmpeg.as_path());
    let dur = video_duration(path, ff);
    let t0 = job.range.0 / 10000.0 * dur;
    let t1 = job.range.1 / 10000.0 * dur;
    if dur <= 0.0 || t1 <= t0 + 0.05 {
        return Err("bad trim range".into());
    }
    let (stem, ext_raw) = split_ext(path);
    let ext = if ext_raw.is_empty() { ".mp4".to_owned() } else { ext_raw.to_lowercase() };
    let fmt = trim_format(&ext);
    let mut dest = path.to_owned();
    if job.as_copy {
        dest = format!("{stem} (trimmed){ext_raw}");
        let mut k = 1;
        while Path::new(&dest).exists() {
            k += 1;
            dest = format!("{stem} (trimmed {k}){ext_raw}");
        }
    }
    let mut args: Vec<String> = [ "-y", "-ss", &format!("{t0:.3}"), "-i", path, "-t", &format!("{:.3}", t1 - t0), "-map", "0:v:0", "-map", "0:a?", "-map_metadata", "0"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let cpu_v = ["-c:v", "libx264", "-preset", "veryfast", "-crf", "18"];
    let gpu_v = ["-c:v", "h264_nvenc", "-preset", "medium", "-rc", "vbr", "-cq", "19", "-b:v", "0"];
    let mut tail: Vec<String> = Vec::new();
    // never bigger than the source: cap the video bitrate at the original's
    let src_kbps = video_bitrate_kbps(path, ff);
    if src_kbps > 0 {
        let cap = src_kbps.max(500);
        tail.extend(["-maxrate".into(), format!("{cap}k"), "-bufsize".into(), format!("{}k", cap * 2)]);
    }
    tail.extend(["-c:a".into(), "copy".into()]);
    if fmt == "mp4" || fmt == "mov" {
        tail.extend(["-movflags".into(), "+faststart".into()]);
    }
    tail.extend(["-f".into(), fmt.into(), tmp.into()]);
    let run = |v: &[&str]| -> bool {
        let mut a = args.clone();
        a.extend(v.iter().map(|s| s.to_string()));
        a.extend(tail.iter().cloned());
        let ok = hidden(Command::new(ff).args(&a)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
        ok && file_ok(tmp)
    };
    let mut ok = false;
    // GPU encoder first (much faster); quietly fall back to the CPU encode
    if job.use_gpu && NVENC_OK.load(Ordering::SeqCst) {
        ok = run(&gpu_v);
        if ok {
            NVENC_OK.store(true, Ordering::SeqCst);
        } else {
            let _ = std::fs::remove_file(tmp);
        }
    }
    if !ok {
        ok = run(&cpu_v);
    }
    args.clear();
    if !ok {
        return Err("ffmpeg failed".into());
    }
    copy_file_times(path, tmp);
    if job.as_copy {
        std::fs::rename(tmp, &dest).map_err(|e| e.to_string())?;
        return Ok(Some(dest));
    }
    // the old thumbnail's name depends on size/date: work it out before replacing
    let old_thumb = cache_key(path).map(|k| cache_dir().join(k));
    // mpv holds the file open: let it go, then swap (retry while Windows still locks it)
    let _ = tx.send(MediaMsg::ReleaseFile(path.to_owned()));
    std::thread::sleep(Duration::from_millis(600));
    let mut last = None;
    for _ in 0..10 {
        match std::fs::rename(tmp, path) {
            Ok(()) => {
                last = None;
                break;
            }
            Err(e) => {
                last = Some(e.to_string());
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    }
    if let Some(e) = last {
        return Err(e);
    }
    if let Some(t) = old_thumb {
        let _ = std::fs::remove_file(t);
    }
    Ok(None)
}

// ---------------------------------------------------------------------------------------------
// Compress
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
pub struct CompressCtl {
    pub cancelled: Arc<AtomicBool>,
    pub proc: Arc<Mutex<Option<Child>>>,
}

impl CompressCtl {
    pub fn new() -> Self {
        CompressCtl { cancelled: Arc::new(AtomicBool::new(false)), proc: Arc::new(Mutex::new(None)) }
    }
    /// Python `_on_close`: stop the batch and kill the running ffmpeg.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(c) = self.proc.lock().unwrap().as_mut() {
            let _ = c.kill();
        }
    }
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

pub fn start_compress(targets: Vec<String>, nominal_mb: u32, ffmpeg: PathBuf, ctl: CompressCtl, tx: Sender<MediaMsg>, ctx: eframe::egui::Context) {
    std::thread::spawn(move || {
        let n = targets.len();
        let (mut done, mut failed) = (0, Vec::new());
        for (idx, src) in targets.iter().enumerate() {
            if ctl.is_cancelled() {
                break;
            }
            let base = idx as f32 / n as f32;
            let span = 1.0 / n as f32;
            let short = scanner::basename(src).to_owned();
            let label = |prep: bool| if n > 1 { format!("Encoding {}/{n} \u{2014} {short}", idx + 1) } else if prep { "Preparing\u{2026}".into() } else { "Encoding\u{2026}".into() };
            let _ = tx.send(MediaMsg::Progress(base, label(true)));
            ctx.request_repaint();
            let progress = |f: f32| {
                let _ = tx.send(MediaMsg::Progress(base + span * f, label(false)));
                ctx.request_repaint();
            };
            match compress_one(src, nominal_mb, &ffmpeg, &ctl, &progress) {
                Ok(dst) => {
                    done += 1;
                    let _ = tx.send(MediaMsg::CompressedOne(src.clone(), true, Some(dst)));
                }
                Err(_) => {
                    failed.push(short);
                    let _ = tx.send(MediaMsg::CompressedOne(src.clone(), false, None));
                }
            }
            ctx.request_repaint();
        }
        let _ = tx.send(MediaMsg::CompressDone(done, failed, ctl.is_cancelled()));
        ctx.request_repaint();
    });
}

fn compress_one(src: &str, nominal_mb: u32, ff: &Path, ctl: &CompressCtl, progress: &dyn Fn(f32)) -> Result<String, String> {
    let dur = video_duration(src, ff);
    if dur <= 0.0 {
        return Err("Could not read clip duration".into());
    }
    let (base, ext) = split_ext(src);
    let ext = if ext.is_empty() { ".mp4" } else { ext };
    let mut dst = format!("{base}_{nominal_mb}MB{ext}");
    let mut counter = 1;
    while Path::new(&dst).exists() {
        dst = format!("{base}_{nominal_mb}MB_{counter}{ext}");
        counter += 1;
    }
    // every audio track is copied unchanged; the video gets what is left of the budget
    let audio_bits: u64 = match audio_bitrates(src, ff) {
        None => 160_000,
        Some(r) => r.iter().map(|&b| if b > 0 { b } else { 160_000 }).sum(),
    };
    let target_mb = nominal_mb as f64 - 0.5; // flat cushion for container overhead
    let video_bits = ((target_mb * 8.0 * 1024.0 * 1024.0 / dur) as i64 - audio_bits as i64).max(200_000) as u64;
    let bufsize = video_bits * 2;
    // downscale only if the bit budget is too tight for the source resolution
    let (sw, sh, sfps) = video_info(src, ff);
    let mut scale = None;
    if sw > 0 && sh > 0 {
        let fps = if sfps > 0.0 { sfps } else { 30.0 };
        let floor = 0.07 * 0.4;
        let ref_bps = |w: f64, h: f64| w * h * fps * floor;
        if (video_bits as f64) < ref_bps(sw as f64, sh as f64) {
            let mut new_h = sh;
            for h in [1080u32, 720, 540, 480, 360, 240] {
                if h >= sh {
                    continue;
                }
                new_h = h;
                let new_w = sw as f64 * h as f64 / sh as f64;
                if video_bits as f64 >= ref_bps(new_w, h as f64) {
                    break;
                }
            }
            if new_h < sh {
                scale = Some(format!("scale=-2:{new_h}"));
            }
        }
    }
    let vf: Vec<String> = scale.map(|s| vec!["-vf".to_owned(), s]).unwrap_or_default();
    let vb = video_bits.to_string();
    let bs = bufsize.to_string();
    let build = |codec: &[&str], audio: &[&str]| -> Vec<String> {
        let mut a: Vec<String> = vec!["-y".into(), "-i".into(), src.into()];
        a.extend(codec.iter().map(|s| s.to_string()));
        a.extend(vf.iter().cloned());
        a.extend(["-b:v", &vb, "-maxrate", &vb, "-bufsize", &bs].iter().map(|s| s.to_string()));
        a.extend(["-map", "0:v:0?", "-map", "0:a?", "-c:a"].iter().map(|s| s.to_string()));
        a.extend(audio.iter().map(|s| s.to_string()));
        a.extend(["-movflags", "+faststart", &dst].iter().map(|s| s.to_string()));
        a
    };
    let encode = |args: Vec<String>| -> bool {
        let Ok(mut child) = hidden(Command::new(ff).args(&args)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn() else { return false };
        let mut err = child.stderr.take();
        *ctl.proc.lock().unwrap() = Some(child);
        // progress from ffmpeg's "time=hh:mm:ss.xx"
        let re = Regex::new(r"time=(\d+):(\d+):(\d+\.?\d*)").unwrap();
        let mut buf = String::new();
        let mut chunk = [0u8; 512];
        if let Some(e) = err.as_mut() {
            while let Ok(k) = e.read(&mut chunk) {
                if k == 0 {
                    break;
                }
                buf.push_str(&String::from_utf8_lossy(&chunk[..k]));
                if let Some(c) = re.captures_iter(&buf).last() {
                    let cur = c[1].parse::<f64>().unwrap_or(0.0) * 3600.0 + c[2].parse::<f64>().unwrap_or(0.0) * 60.0 + c[3].parse::<f64>().unwrap_or(0.0);
                    progress((cur / dur).clamp(0.0, 1.0) as f32);
                }
                if buf.len() > 4096 {
                    buf.drain(..buf.len() - 512);
                }
            }
        }
        let status = ctl.proc.lock().unwrap().take().and_then(|mut c| c.wait().ok());
        status.is_some_and(|s| s.success())
    };
    let rm = || {
        let _ = std::fs::remove_file(&dst);
    };
    let mut ok = false;
    if !ctl.is_cancelled() && NVENC_OK.load(Ordering::SeqCst) {
        ok = encode(build(&["-c:v", "h264_nvenc", "-preset", "fast", "-rc", "vbr"], &["copy"]));
        NVENC_OK.store(ok, Ordering::SeqCst);
    }
    if !ok && !ctl.is_cancelled() {
        rm();
        ok = encode(build(&["-c:v", "libx264", "-preset", "veryfast"], &["copy"]));
        if !ok && !ctl.is_cancelled() {
            // last resort: the tracks couldn't be copied, re-encode each at OBS's 160 kbps
            rm();
            ok = encode(build(&["-c:v", "libx264", "-preset", "veryfast"], &["aac", "-b:a", "160k"]));
        }
    }
    if !ok || ctl.is_cancelled() {
        rm();
        return Err(if ctl.is_cancelled() { "cancelled".into() } else { "ffmpeg failed".into() });
    }
    copy_stat(src, &dst);
    Ok(dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(split_ext(r"Z:/a\b.c\clip.mkv"), (r"Z:/a\b.c\clip", ".mkv"));
        assert_eq!(split_ext(r"Z:/a\noext"), (r"Z:/a\noext", ""));
        assert_eq!(trim_format(".mkv"), "matroska");
        assert_eq!(trim_format(".m4v"), "mp4");
    }

    /// Runs Apply trim (in place and as copy) and Compress on a TEMP COPY of the first real clip:
    /// `cargo test --release -- --ignored real_media --nocapture`
    #[test]
    #[ignore]
    fn real_media_jobs() {
        let ff = scanner::find_ffmpeg().expect("ffmpeg");
        let prefs = crate::prefs::Prefs::load();
        let clips = scanner::scan_folders(&prefs.checked_folders);
        let src = clips.iter().find(|c| c.size < 60_000_000).expect("a clip under 60 MB");
        let dir = std::env::temp_dir().join("fp_media_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ext = split_ext(&src.path).1.to_owned();
        let copy = dir.join(format!("test{ext}")).to_string_lossy().into_owned();
        std::fs::copy(&src.path, &copy).unwrap();
        let dur = video_duration(&copy, &ff);
        println!("source {} ({dur:.1}s, {} MB)", src.path, src.size / 1_000_000);
        let (tx, rx) = std::sync::mpsc::channel();

        // save as copy: 20%..60%
        let job = TrimJob { path: copy.clone(), range: (2000.0, 6000.0), as_copy: true, use_gpu: true, ffmpeg: ff.clone() };
        let dest = run_trim(&job, &format!("{copy}.trimtmp"), &tx).expect("trim copy").expect("dest");
        let d2 = video_duration(&dest, &ff);
        println!("trimmed copy {dest}: {d2:.2}s (want ~{:.2})", dur * 0.4);
        // audio is copied (not re-cut), so the file can run a bit longer than the trim
        assert!((d2 - dur * 0.4).abs() < 2.0);
        assert!(dest.ends_with(&format!("test (trimmed){ext}")));
        let m0 = std::fs::metadata(&copy).unwrap().modified().unwrap();
        assert_eq!(std::fs::metadata(&dest).unwrap().modified().unwrap(), m0, "file date copied");

        // in place: 0..50%
        let job = TrimJob { path: copy.clone(), range: (0.0, 5000.0), as_copy: false, use_gpu: true, ffmpeg: ff.clone() };
        run_trim(&job, &format!("{copy}.trimtmp"), &tx).expect("trim in place");
        assert!(matches!(rx.try_recv(), Ok(MediaMsg::ReleaseFile(_))));
        let d3 = video_duration(&copy, &ff);
        println!("trimmed in place: {d3:.2}s (want ~{:.2})", dur * 0.5);
        assert!((d3 - dur * 0.5).abs() < 2.0);
        assert!(!Path::new(&format!("{copy}.trimtmp")).exists());

        // compress the trimmed copy to 10 MB
        let ctl = CompressCtl::new();
        let out = compress_one(&dest, 10, &ff, &ctl, &|_| {}).expect("compress");
        let mb = std::fs::metadata(&out).unwrap().len() as f64 / 1024.0 / 1024.0;
        println!("compressed {out}: {mb:.2} MB, nvenc ok = {}", NVENC_OK.load(Ordering::SeqCst));
        assert!(out.ends_with(&format!("_10MB{ext}")));
        assert!(mb <= 10.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
