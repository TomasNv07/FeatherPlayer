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
    /// Start point (seconds) given to mpv with the file, when known: held as the position for
    /// a moment after the file has loaded (the bar, and the audio-track re-seek, use it).
    open_start: Option<f64>,
    /// Python `_mpv_hold_for_mix`: a clip opens paused and starts playing once its audio tracks
    /// are set up (`tracks_handled`), so the mix doesn't make it jump back after it started.
    /// Some(deadline) = start playing then at the latest (safety net).
    autoplay: Option<Instant>,
    /// The app has dealt with this clip's tracks (`tracks_new`).
    pub tracks_handled: bool,
    /// Track items last applied for this clip (None = mpv's default track).
    pub applied: Option<Vec<(i64, f64)>>,
    /// mpv has shown the first frame of this file (first PLAYBACK_RESTART).
    restarted: bool,
    /// Tracks were read but are reported (`tracks_new`) only once the first frame is there,
    /// so the track re-seek lands on the real start position. Some(since file loaded).
    tracks_pending: Option<Instant>,
    /// `hr-seek` was switched off for a fast keyframe start; switch it back on after it.
    keyframe_start: bool,
    pub dbg_t: Option<Instant>,
    /// Latest values of the observed mpv properties.
    obs: Observed,
    /// FILE_LOADED came; waiting for the new file's track list (property event).
    tracks_wait: Option<Instant>,
    /// This file's tracks have been read (a later track-list change re-reads them).
    tracks_read: bool,
}

#[derive(Default)]
struct Observed {
    pos: Option<f64>,
    dur: Option<f64>,
    /// `track-list` as JSON text.
    tracks: String,
}

/// The clips have a keyframe only every few seconds (e.g. 4.2 s at 60 fps). An exact seek to
/// the middle decodes everything from the keyframe before it (hundreds of frames on the CPU),
/// which is what made clips open late. The "middle" start doesn't need to be exact, so it opens
/// on the keyframe instead: one frame to decode.
fn keyframe_ok(start: Start) -> bool {
    matches!(start, Start::Middle)
}

/// How long a just-opened clip waits for its audio tracks before playing anyway.
const AUTOPLAY_MAX_WAIT: Duration = Duration::from_millis(1500);

/// Where a clip opens (Python `_play_index` / `_seek_after_load`).
#[derive(Clone, Copy, PartialEq)]
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
        // debugging: FP_MPV_OPTS="key=value,key=value" extra mpv options (e.g. log-file=...)
        let extra: Vec<(String, String)> = std::env::var("FP_MPV_OPTS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|kv| kv.split_once('=').map(|(k, v)| (format!("?{}", k.trim()), v.trim().to_owned())))
            .collect();
        let mut opts: Vec<(&str, &str)> = vec![
            ("vo", "libmpv"),
            ("idle", "yes"),
            ("keep-open", "yes"),
            ("terminal", "no"),
            ("hwdec", "no"),
            ("input-default-bindings", "no"),
            ("osc", "no"),
            ("?ytdl", "no"),
            ("?load-scripts", "no"),
            ("?load-stats-overlay", "no"),
            ("?load-console", "no"),
            ("?load-auto-profiles", "no"),
            ("?load-select", "no"),
            ("?load-commands", "no"),
            ("?load-positioning", "no"),
        ];
        opts.extend(extra.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        core.init(&opts)?;
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
            open_start: None,
            autoplay: None,
            tracks_handled: false,
            applied: None,
            restarted: false,
            tracks_pending: None,
            keyframe_start: false,
            dbg_t: None,
            obs: Observed::default(),
            tracks_wait: None,
            tracks_read: false,
        };
        for (name, format) in [("time-pos", mpv::FORMAT_DBL), ("duration", mpv::FORMAT_DBL), ("pause", mpv::FORMAT_FLAG), ("eof-reached", mpv::FORMAT_FLAG), ("track-list", mpv::FORMAT_STRING)] {
            p.core.observe(name, format);
        }
        let _ = p.core.set("volume", &format!("{}", p.volume));
        Ok(p)
    }

    pub fn open(&mut self, path: &str) {
        self.open_at(path, None, false, None);
    }

    /// `start`: mpv `start` value for this file ("50%", "12.5%"), so mpv opens straight at that
    /// spot instead of decoding from 0 and seeking (exactly) after the file has loaded.
    /// `keyframe`: land on the keyframe at/before that spot (fast, see `keyframe_ok`).
    fn open_at(&mut self, path: &str, start: Option<String>, keyframe: bool, preset: Option<&[(i64, f64)]>) {
        // Everything here is sent async (in order): a blocking stop/loadfile/graph change waits
        // for the video output, which waits for us to render -> 200 ms stalls (see cmd_async).
        // `start` is a global option: always set it, so it never carries over to the next file
        let _ = self.core.set_async("start", start.as_deref().unwrap_or("none"));
        let _ = self.core.set_async("hr-seek", if keyframe { "no" } else { "default" });
        // stop the old clip first (Python `_mpv_load_now`): changing the audio graph while it
        // still plays made mpv rebuild + seek the OLD clip before loading
        if self.path.is_some() {
            let _ = self.core.cmd_async(&["stop"]);
        }
        // the audio graph for the new clip: its tracks set up before it opens when the track
        // layout is known (no rebuild + re-seek after it started), else none + mpv's first track
        let (aid, graph) = match preset {
            Some(items) if !items.is_empty() => audio::build_graph(items),
            _ => (None, String::new()),
        };
        let _ = self.core.set_async("lavfi-complex", &graph);
        self.lavfi_cur = graph;
        let aid = aid.map(|a| a.to_string()).unwrap_or_else(|| "auto".into());
        if self.aid_cur != aid {
            let _ = self.core.set_async("aid", &aid);
            self.aid_cur = aid;
        }
        self.audio_check = None;
        let r = self.core.cmd_async(&["loadfile", path]);
        self.dbg("  loadfile sent");
        match r {
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
                self.open_start = None;
                self.autoplay = None;
                self.tracks_handled = false;
                self.applied = preset.filter(|i| !i.is_empty()).map(|i| i.to_vec());
                self.restarted = false;
                self.tracks_pending = None;
                self.keyframe_start = keyframe;
                // `obs` is kept: mpv only reports a property when it changes, so a clip with the
                // same track layout / length as the last one sends no new value
                self.tracks_wait = None;
                self.tracks_read = false;
            }
            Err(e) => self.msg = e,
        }
    }

    /// Open a clip from the list at its start point, paused. `known_dur` = the clip's length
    /// from the scan (prefs `durations`): with it (or for a trim start) mpv opens straight at
    /// the spot; without it the old way is used (seek once mpv knows the duration).
    /// `autoplay`: start playing once the tracks are handled (false = preload, stays paused).
    /// `preset`: the clip's track items when its track layout is already known (set up before
    /// the file opens).
    pub fn open_clip(&mut self, path: &str, start: Start, known_dur: Option<f64>, autoplay: bool, preset: Option<&[(i64, f64)]>) {
        self.dbg_t = Some(Instant::now());
        self.dbg(&format!("open_clip autoplay={autoplay} preset={preset:?}"));
        // paused before it opens: playing starts once the audio tracks are set up
        let _ = self.core.set_async("pause", "yes");
        self.paused = true;
        let at = match start {
            Start::Trim(f) if f > 0.0 => Some(Some(format!("{:.4}%", f * 100.0))),
            Start::Trim(_) | Start::Beginning => Some(None),
            Start::Middle => known_dur.map(|d| (d > 15.0).then(|| "50%".to_owned())),
        };
        // a keyframe start lands somewhere before the middle: no position to hold there
        let secs = known_dur.and_then(|d| match start {
            Start::Trim(f) if f > 0.0 => Some(f * d),
            _ => None,
        });
        let keyframe = keyframe_ok(start) && matches!(at, Some(Some(_)));
        match at {
            Some(s) => {
                self.open_at(path, s, keyframe, preset);
                self.pending_start = None;
                self.open_start = secs;
                if let Some(t) = secs {
                    self.pos = t;
                }
            }
            None => {
                self.open_at(path, None, false, preset);
                self.pending_start = Some((start, Instant::now()));
            }
        }
        self.autoplay = autoplay.then(|| Instant::now() + AUTOPLAY_MAX_WAIT);
    }

    /// Play once the tracks are handled (or the wait ran out). Call every frame.
    pub fn autoplay_tick(&mut self) {
        let Some(deadline) = self.autoplay else { return };
        if (self.loaded && self.tracks_handled) || Instant::now() >= deadline {
            self.dbg(if self.tracks_handled { "autoplay: play" } else { "autoplay: DEADLINE" });
            self.autoplay = None;
            self.play();
        }
    }

    /// A preloaded clip becomes the playing one: play now if it is ready, else as soon as it is.
    pub fn start_when_ready(&mut self) {
        self.autoplay = Some(Instant::now() + AUTOPLAY_MAX_WAIT);
        self.autoplay_tick();
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
        let dur = self.obs.dur.unwrap_or(0.0);
        if dur <= 0.0 {
            return;
        }
        self.pending_start = None;
        let t = match start {
            Start::Trim(f) if f > 0.0 => f * dur,
            Start::Middle if dur > 15.0 => dur * 0.5,
            _ => return,
        };
        if keyframe_ok(start) {
            let _ = self.core.cmd(&["seek", &format!("{t}"), "absolute+keyframes"]);
        } else {
            let _ = self.core.cmd(&["seek", &format!("{t}"), "absolute"]);
            self.pos = t;
            self.hold = Some((Instant::now() + Duration::from_millis(400), t));
        }
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

    pub fn pause(&mut self) {
        self.autoplay = None; // paused by hand while waiting: stay paused
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
        let _ = self.core.cmd_async(&["stop"]);
        self.loaded = false;
    }

    /// Close the clip for good (hidden to the tray, clip deleted): like `stop`, and forget the
    /// file, so the video area says "Select a clip to play" instead of "Loading…".
    pub fn close(&mut self) {
        self.stop();
        self.path = None;
        self.autoplay = None;
        self.pending_start = None;
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

    /// debugging: with FP_DEBUG_LOAD set, clip-load timings go to `target/load_debug.txt`.
    pub fn dbg(&self, what: &str) {
        if let (Ok(_), Some(t)) = (std::env::var("FP_DEBUG_LOAD"), self.dbg_t) {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("target/load_debug.txt") {
                let _ = writeln!(f, "{:7.1} ms  {what}", t.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }

    /// First frame of the file is ready (or an actual seek finished).
    fn on_restart(&mut self) {
        self.dbg("PLAYBACK_RESTART");
        if !self.loaded {
            return; // a restart event of the previous file
        }
        self.restarted = true;
        if self.keyframe_start {
            // the fast start is done: seeks are exact again
            self.keyframe_start = false;
            let _ = self.core.set_async("hr-seek", "default");
        }
        if self.tracks_pending.take().is_some() {
            self.tracks_new = true;
        }
    }

    /// Drain mpv events and refresh position/duration/pause state. Call once per frame.
    /// No blocking mpv reads here: the values come as PROPERTY_CHANGE events (`observe`), since
    /// a blocking read while mpv's core is busy starting a clip froze the UI thread, which is
    /// also the render thread the video output was waiting for.
    pub fn poll(&mut self) {
        loop {
            let ev = self.core.next_event();
            match ev.id {
                mpv::EV_NONE => break,
                mpv::EV_PROPERTY_CHANGE => {
                    if let Some((name, v)) = ev.prop {
                        match (name.as_str(), v) {
                            ("time-pos", mpv::PropVal::Double(x)) => self.obs.pos = Some(x),
                            ("time-pos", _) => self.obs.pos = None,
                            ("duration", mpv::PropVal::Double(x)) => self.obs.dur = Some(x),
                            ("duration", _) => self.obs.dur = None,
                            // applied when they change only: play()/pause() set `paused` right
                            // away, and the event confirming it comes a moment later
                            ("pause", mpv::PropVal::Flag(b)) => self.paused = b,
                            ("eof-reached", mpv::PropVal::Flag(b)) => self.eof = b,
                            ("eof-reached", _) => self.eof = false,
                            ("track-list", v) => {
                                self.obs.tracks = if let mpv::PropVal::Str(s) = v { s } else { String::new() };
                                // read already (from the previous clip's list, which can arrive
                                // first): read again, the app re-applies the tracks if needed
                                if self.loaded && self.tracks_read && self.tracks_wait.is_none() {
                                    let list = self.obs.tracks.clone();
                                    if serde_json::from_str::<serde_json::Value>(&list).ok().and_then(|v| v.as_array().map(|a| !a.is_empty())).unwrap_or(false) {
                                        self.read_tracks(&list);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                mpv::EV_FILE_LOADED => {
                    self.loaded = true;
                    self.dbg("FILE_LOADED");
                    if let Some(t) = self.open_start.take() {
                        self.pos = t;
                        self.hold = Some((Instant::now() + Duration::from_millis(400), t));
                    }
                    // the track list may still be on its way as a property event
                    self.tracks_wait = Some(Instant::now());
                }
                mpv::EV_PLAYBACK_RESTART => self.on_restart(),
                ev => self.dbg(&format!("event {ev}")),
            }
        }
        if let Some(since) = self.tracks_wait {
            let list = self.obs.tracks.clone();
            let has_any = serde_json::from_str::<serde_json::Value>(&list).ok().and_then(|v| v.as_array().map(|a| !a.is_empty())).unwrap_or(false);
            if has_any || since.elapsed() > Duration::from_millis(500) {
                self.tracks_wait = None;
                self.read_tracks(&list);
                self.tracks_read = true;
                if !self.restarted {
                    // report the tracks once the first frame is there (see tracks_pending)
                    self.tracks_new = false;
                    self.tracks_pending = Some(Instant::now());
                }
            }
        }
        // safety net: no PLAYBACK_RESTART came (e.g. a file without video)
        if self.tracks_pending.is_some_and(|t| t.elapsed() > Duration::from_millis(800)) {
            self.on_restart();
        }
        if self.loaded {
            self.apply_pending_start();
            self.audio_check_tick();
            if let Some(d) = self.obs.dur {
                self.dur = d;
            }
            if let Some(p) = self.obs.pos {
                self.pos = p;
            }
            if let Some((until, t)) = self.hold {
                if Instant::now() < until {
                    self.pos = t;
                } else {
                    self.hold = None;
                }
            }
        }
    }

    /// mpv's latest reported position (no blocking read).
    pub(crate) fn obs_pos(&self) -> Option<f64> {
        self.obs.pos
    }

    /// Off-screen render when mpv wants a frame drawn (see `video_gl::render_offscreen`).
    pub fn render_offscreen(&self, gl: &eframe::glow::Context, w: i32, h: i32) {
        if self.path.is_some() {
            video_gl::render_offscreen(&self.core, &self.vgl, gl, w, h);
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
