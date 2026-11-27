//! Update check (owner's design, 2026-11-28; Rust only). Once at start, in the background,
//! GitHub is asked for the latest release. If its `vX.Y.Z` tag is newer than this build,
//! Settings shows an update block (accent border) and the header's gear gets an accent "!".
//! "Update now" downloads the release's `FeatherPlayer_Setup_X.Y.Z.exe` into %TEMP% and runs
//! it silently; the app quits and the installer starts the new version (`[Run]` with
//! `Check: WizardSilent` in setup\FeatherPlayer.iss).
//!
//! HTTPS goes through Windows' own curl.exe (Windows 10 1803+), so no TLS library is needed.
//! Only works while the repo (or a public downloads repo, see CLAUDE.md) is public.

use eframe::egui;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// This build's version: set by build_installer.bat (FP_VERSION), else the newest git tag
/// (see build.rs).
pub const VERSION: &str = env!("FP_VERSION");
/// (the repo was renamed from FeatherPlayer-rs on 2026-11-28; GitHub redirects the old name)
const LATEST_API: &str = "https://api.github.com/repos/TomasNv07/FeatherPlayer/releases/latest";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// The installer finished without having to close this copy: the app should quit.
pub static QUIT_AND_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[derive(Clone, Default, PartialEq, Debug)]
pub enum Phase {
    /// Nothing newer (or the check failed / hasn't finished): nothing is shown.
    #[default]
    None,
    Available,
    /// Downloading: (bytes so far, total bytes; 0 = unknown).
    Downloading(u64, u64),
    /// Downloaded: the app starts the installer (`Updater::install`).
    Ready(PathBuf),
    /// The installer runs; it asks the app to close (WM_ENDSESSION, see
    /// `window::watch_session_end`) just before it replaces the files, then starts the new
    /// version. The app stays open until then (owner 2026-11-28).
    Installing,
    Error(String),
}

#[derive(Clone, Default, Debug)]
pub struct Info {
    pub latest: String,
    url: String,
    /// Installer size from GitHub (for the progress bar).
    size: u64,
    pub phase: Phase,
}

impl Info {
    /// An update is known (show the block and the "!").
    pub fn available(&self) -> bool {
        self.phase != Phase::None
    }
}

pub struct Updater {
    st: Arc<Mutex<Info>>,
    /// Set by "Cancel" while downloading.
    cancel: Arc<std::sync::atomic::AtomicBool>,
    /// The running installer (to notice when it ends without closing the app = failed).
    installer: Mutex<Option<(std::process::Child, std::time::Instant)>>,
}

fn curl() -> Command {
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let mut c = Command::new(root.join(r"System32\curl.exe"));
    c.creation_flags(CREATE_NO_WINDOW).stdin(Stdio::null()).stderr(Stdio::null());
    // GitHub's API wants a User-Agent; a generic one (no user details)
    c.args(["-sSfL", "--max-time", "20", "-A", "FeatherPlayer-updater"]);
    c
}

/// "1.0.10" -> [1, 0, 10] (a leading "v" is ignored).
fn parse(v: &str) -> Vec<u32> {
    v.trim().trim_start_matches(['v', 'V']).split('.').map(|p| p.trim().parse().unwrap_or(0)).collect()
}

fn is_newer(latest: &str, current: &str) -> bool {
    let (a, b) = (parse(latest), parse(current));
    let n = a.len().max(b.len());
    let at = |v: &Vec<u32>, i: usize| v.get(i).copied().unwrap_or(0);
    (0..n).map(|i| at(&a, i).cmp(&at(&b, i))).find(|o| o.is_ne()) == Some(std::cmp::Ordering::Greater)
}

/// (tag, installer URL, installer size) of the latest release, from GitHub's JSON.
fn latest_release(json: &str) -> Option<(String, String, u64)> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let tag = v.get("tag_name")?.as_str()?.to_owned();
    let (url, size) = v.get("assets")?.as_array()?.iter().find_map(|a| {
        let name = a.get("name")?.as_str()?;
        if !(name.starts_with("FeatherPlayer_Setup_") && name.to_lowercase().ends_with(".exe")) {
            return None;
        }
        Some((a.get("browser_download_url")?.as_str()?.to_owned(), a.get("size").and_then(|s| s.as_u64()).unwrap_or(0)))
    })?;
    Some((tag, url, size))
}

impl Updater {
    /// Starts the one check (background thread).
    pub fn start(ctx: egui::Context) -> Updater {
        let st = Arc::new(Mutex::new(Info::default()));
        let s = st.clone();
        std::thread::spawn(move || {
            let Ok(out) = curl().arg(LATEST_API).stdout(Stdio::piped()).output() else { return };
            if !out.status.success() {
                return; // offline / rate limited: just no update shown
            }
            let Some((tag, url, size)) = latest_release(&String::from_utf8_lossy(&out.stdout)) else { return };
            if is_newer(&tag, VERSION) {
                *s.lock().unwrap() = Info { latest: tag.trim_start_matches(['v', 'V']).to_owned(), url, size, phase: Phase::Available };
                ctx.request_repaint();
            }
        });
        Updater { st, cancel: Arc::default(), installer: Mutex::new(None) }
    }

    pub fn info(&self) -> Info {
        self.st.lock().unwrap().clone()
    }

    /// "Update now": download the installer into %TEMP% (background thread), with progress
    /// from the file's size against GitHub's asset size.
    pub fn download(&self, ctx: &egui::Context) {
        use std::sync::atomic::Ordering;
        let (url, ver, total) = {
            let mut i = self.st.lock().unwrap();
            if !matches!(i.phase, Phase::Available | Phase::Error(_)) {
                return;
            }
            i.phase = Phase::Downloading(0, i.size);
            (i.url.clone(), i.latest.clone(), i.size)
        };
        self.cancel.store(false, Ordering::SeqCst);
        let (s, cancel, ctx) = (self.st.clone(), self.cancel.clone(), ctx.clone());
        std::thread::spawn(move || {
            let dest = std::env::temp_dir().join(format!("FeatherPlayer_Setup_{ver}.exe"));
            let _ = std::fs::remove_file(&dest);
            let fail = |msg: &str| {
                let _ = std::fs::remove_file(&dest);
                s.lock().unwrap().phase = Phase::Error(msg.to_owned());
                ctx.request_repaint();
            };
            let Ok(mut child) = curl().args(["--max-time", "900", "-o"]).arg(&dest).arg(&url).stdout(Stdio::null()).spawn() else {
                return fail("Could not start the download.");
            };
            let ok = loop {
                if cancel.load(Ordering::SeqCst) {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&dest);
                    s.lock().unwrap().phase = Phase::Available;
                    ctx.request_repaint();
                    return;
                }
                if let Ok(Some(st)) = child.try_wait() {
                    break st.success();
                }
                let got = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
                s.lock().unwrap().phase = Phase::Downloading(got, total);
                ctx.request_repaint();
                std::thread::sleep(std::time::Duration::from_millis(100));
            };
            let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
            if ok && size > 1_000_000 && (total == 0 || size == total) {
                s.lock().unwrap().phase = Phase::Ready(dest);
                ctx.request_repaint();
            } else {
                fail("Download failed. Check the internet connection and try again.");
            }
        });
    }

    /// "Cancel" in the update window (only while downloading).
    pub fn cancel(&self) {
        self.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Start the downloaded installer silently. The app keeps running: the installer asks it
    /// to close (Restart Manager -> WM_QUERYENDSESSION / WM_ENDSESSION) right before it
    /// replaces the files, and starts the new version when done.
    pub fn install(&self, path: &Path) {
        match Command::new(path).args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS"]).spawn() {
            Ok(child) => {
                *self.installer.lock().unwrap() = Some((child, std::time::Instant::now()));
                self.st.lock().unwrap().phase = Phase::Installing;
            }
            Err(_) => self.st.lock().unwrap().phase = Phase::Error("Could not start the installer. Try again.".into()),
        }
    }

    /// While installing: the installer ended but this app is still running, so it did not get
    /// to replace the files.
    pub fn check_installer(&self) {
        let mut g = self.installer.lock().unwrap();
        let Some((child, _since)) = g.as_mut() else { return };
        if let Ok(Some(st)) = child.try_wait() {
            *g = None;
            if st.success() {
                // installed, but this copy runs from somewhere else (e.g. a dev build), so the
                // installer didn't have to close it: quit, and open the installed version
                // a moment later (it would hand over to us while we still run)
                QUIT_AND_OPEN.store(true, std::sync::atomic::Ordering::SeqCst);
                if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
                    let exe = PathBuf::from(dir).join(r"Programs\Feather Player\FeatherPlayer.exe");
                    if exe.exists() {
                        let _ = Command::new("cmd").creation_flags(CREATE_NO_WINDOW).args(["/c", "ping -n 3 127.0.0.1 >nul & start \"\""]).arg(&exe).spawn();
                    }
                }
            } else {
                self.st.lock().unwrap().phase = Phase::Error(format!("The update could not be installed (code {}). Try again.", st.code().unwrap_or(-1)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(is_newer("v1.0.10", "1.0.9"));
        assert!(is_newer("1.1.0", "1.0.99"));
        assert!(!is_newer("v1.0.9", "1.0.9"));
        assert!(!is_newer("1.0.8", "1.0.9"));
        assert!(is_newer("2.0", "1.9.9"));
    }

    #[test]
    fn picks_the_installer_asset() {
        let json = r#"{"tag_name":"v1.0.9","assets":[{"name":"notes.txt","browser_download_url":"x"},{"name":"FeatherPlayer_Setup_1.0.9.exe","browser_download_url":"https://example/FeatherPlayer_Setup_1.0.9.exe"}]}"#;
        assert_eq!(latest_release(json), Some(("v1.0.9".into(), "https://example/FeatherPlayer_Setup_1.0.9.exe".into(), 0)));
        assert_eq!(latest_release(r#"{"tag_name":"v1","assets":[]}"#), None);
    }

    /// Asks the real GitHub API (network): `cargo test --release -- --ignored real_update_check --nocapture`
    #[test]
    #[ignore]
    fn real_update_check() {
        let out = curl().arg(LATEST_API).stdout(Stdio::piped()).output().expect("curl");
        let r = latest_release(&String::from_utf8_lossy(&out.stdout));
        println!("this build {VERSION}, latest {r:?}");
        assert!(r.is_some());
    }
}
