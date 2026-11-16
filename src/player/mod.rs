//! mpv-backed player: loading files, playback control, state polling, audio tracks, video GL.

pub mod audio;
pub mod video_gl;

use crate::mpv;
use audio::Track;
use eframe::egui;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use video_gl::VideoGl;

pub struct Player {
    pub core: Arc<mpv::Core>,
    pub vgl: Arc<Mutex<VideoGl>>,
    pub path: Option<String>,
    pub loaded: bool,
    pub dur: f64,
    pub pos: f64,
    pub paused: bool,
    pub eof: bool,
    pub volume: f64,
    pub speed: f64,
    /// After a seek, show this position until the deadline so the poll does not snap the thumb back.
    hold: Option<(Instant, f64)>,
    /// This clip's audio tracks (mpv track-list order).
    pub tracks: Vec<Track>,
    /// Set when the tracks of a newly opened clip became known; the app clears it.
    pub tracks_new: bool,
    lavfi_cur: String,
    /// `aid` as last set (Python `_aid_cur`).
    aid_cur: String,
    /// Pending position check after applying tracks: (when, position, applied at).
    audio_check: Option<(Instant, f64, Instant)>,
    pub msg: String,
    /// Where to land once the duration of a just-opened clip is known.
    pending_start: Option<(Start, Instant)>,
}

/// Where a clip opens (Python `_play_index` / `_seek_after_load`).
#[derive(Clone, Copy)]
pub enum Start {
    /// The middle of clips longer than 15 s, else the beginning.
    Middle,
    Beginning,
    /// Trim start, as a fraction of the duration.
    Trim(f64),
}

impl Player {
    /// Load libmpv, init it and create the OpenGL render context.
    pub fn start(cc: &eframe::CreationContext<'_>, lib: Option<PathBuf>, volume: f64) -> Result<Player, String> {
        let gpa = cc.get_proc_address.ok_or("eframe gave no get_proc_address (is the glow renderer active?)")?;
        let mut cands = mpv::default_candidates();
        if let Some(l) = lib {
            cands.insert(0, l);
        }
        let core = mpv::Core::load(&cands)?;
        core.init(&[
            ("vo", "libmpv"),
            ("idle", "yes"),
            ("keep-open", "yes"),
            ("terminal", "no"),
            ("hwdec", "no"),
            ("input-default-bindings", "no"),
            ("osc", "no"),
        ])?;
        core.create_render_context(gpa, cc.egui_ctx.clone())?;
        let p = Player {
            core: Arc::new(core),
            vgl: Arc::new(Mutex::new(VideoGl::default())),
            path: None,
            loaded: false,
            dur: 0.0,
            pos: 0.0,
            paused: false,
            eof: false,
            volume: volume.clamp(0.0, 100.0),
            speed: 1.0,
            hold: None,
            tracks: Vec::new(),
            tracks_new: false,
            lavfi_cur: String::new(),
            aid_cur: "auto".into(),
            audio_check: None,
            msg: String::new(),
            pending_start: None,
        };
        let _ = p.core.set("volume", &format!("{}", p.volume));
        Ok(p)
    }

    pub fn open(&mut self, path: &str) {
        // Python `_mpv_load_now`: no audio graph, and forget a track picked for the last clip
        let _ = self.core.set("lavfi-complex", "");
        self.lavfi_cur.clear();
        if self.aid_cur != "auto" {
            let _ = self.core.set("aid", "auto");
            self.aid_cur = "auto".into();
        }
        self.audio_check = None;
        match self.core.cmd(&["loadfile", path]) {
            Ok(()) => {
                self.path = Some(path.to_owned());
                self.loaded = false;
                self.tracks.clear();
                self.tracks_new = false;
                self.dur = 0.0;
                self.pos = 0.0;
                self.eof = false;
                self.hold = None;
                self.msg.clear();
                self.pending_start = None;
            }
            Err(e) => self.msg = e,
        }
    }

    /// Open a clip from the list and seek to its start point once the duration is known.
    pub fn open_clip(&mut self, path: &str, start: Start) {
        self.open(path);
        let _ = self.core.set("pause", "no");
        self.pending_start = Some((start, Instant::now()));
    }

    pub fn playing(&self) -> bool {
        self.loaded && !self.paused && !self.eof
    }

    fn apply_pending_start(&mut self) {
        let Some((start, since)) = self.pending_start else { return };
        if since.elapsed() > Duration::from_secs(3) {
            self.pending_start = None; // Python gives up after 60 x 50 ms
            return;
        }
        let dur = self.core.getf("duration").unwrap_or(0.0);
        if dur <= 0.0 {
            return;
        }
        self.pending_start = None;
        let t = match start {
            Start::Trim(f) if f > 0.0 => f * dur,
            Start::Middle if dur > 15.0 => dur * 0.5,
            _ => return,
        };
        let _ = self.core.cmd(&["seek", &format!("{t}"), "absolute"]);
        self.pos = t;
        self.hold = Some((Instant::now() + Duration::from_millis(400), t));
    }

    pub fn toggle_play(&mut self) {
        if self.eof {
            let _ = self.core.cmd(&["seek", "0", "absolute+exact"]);
            let _ = self.core.set("pause", "no");
            self.eof = false;
        } else {
            let _ = self.core.set("pause", if self.paused { "no" } else { "yes" });
        }
    }

    pub fn seek_to(&mut self, secs: f64) {
        let t = secs.clamp(0.0, self.dur.max(0.0));
        let _ = self.core.cmd(&["seek", &format!("{t}"), "absolute+exact"]);
        self.pos = t;
        self.hold = Some((Instant::now() + Duration::from_millis(400), t));
        self.eof = false;
    }

    pub fn pause(&mut self) {
        let _ = self.core.set("pause", "yes");
        self.paused = true;
    }

    pub fn play(&mut self) {
        let _ = self.core.set("pause", "no");
        self.paused = false;
    }

    /// Python `_mpv_seek`: `seek <t> absolute` (exact with mpv's default hr-seek).
    pub fn seek(&mut self, secs: f64) {
        let t = secs.max(0.0);
        let _ = self.core.cmd(&["seek", &format!("{t}"), "absolute"]);
        self.pos = t;
        self.hold = Some((Instant::now() + Duration::from_millis(400), t));
    }

    /// One frame back / forward, pausing first (Python `_frame_back` / `_frame_forward`).
    pub fn frame_step(&mut self, back: bool) {
        if !self.loaded {
            return;
        }
        if self.playing() {
            self.pause();
        }
        let _ = self.core.cmd(&[if back { "frame-back-step" } else { "frame-step" }]);
        self.hold = None;
    }

    /// Unload the file (mpv lets go of it, e.g. before it is replaced on disk).
    pub fn stop(&mut self) {
        let _ = self.core.cmd(&["stop"]);
        self.loaded = false;
    }

    pub fn set_volume(&mut self, v: f64) {
        self.volume = v.clamp(0.0, 100.0);
        let _ = self.core.set("volume", &format!("{}", self.volume));
    }

    /// Python `_apply_speed`: below 0.25x mpv's pitch-correcting stretcher mutes the audio,
    /// so pitch correction is switched off there (0.1x then plays, at a lower pitch).
    pub fn set_speed(&mut self, s: f64) {
        self.speed = s;
        let _ = self.core.set("audio-pitch-correction", if s >= 0.25 { "yes" } else { "no" });
        let _ = self.core.set("speed", &format!("{s}"));
    }

    /// Drain mpv events and refresh position/duration/pause state. Call once per frame.
    pub fn poll(&mut self) {
        loop {
            match self.core.poll_event() {
                mpv::EV_NONE => break,
                mpv::EV_FILE_LOADED => {
                    self.loaded = true;
                    self.read_tracks();
                }
                _ => {}
            }
        }
        if self.loaded {
            self.apply_pending_start();
            self.audio_check_tick();
            self.dur = self.core.getf("duration").unwrap_or(self.dur);
            if let Some(p) = self.core.getf("time-pos") {
                self.pos = p;
            }
            self.paused = self.core.flag("pause");
            self.eof = self.core.flag("eof-reached");
            if let Some((until, t)) = self.hold {
                if Instant::now() < until {
                    self.pos = t;
                } else {
                    self.hold = None;
                }
            }
        }
    }

    pub fn video_callback(&self, rect: egui::Rect) -> egui::PaintCallback {
        video_gl::video_callback(self.core.clone(), self.vgl.clone(), rect)
    }

    /// GL resources first, then the render context, then mpv (GL thread, context current).
    pub fn shutdown(&self, gl: Option<&eframe::glow::Context>) {
        if let (Some(gl), Ok(mut v)) = (gl, self.vgl.lock()) {
            v.destroy(gl);
        }
        self.core.shutdown();
    }
}
