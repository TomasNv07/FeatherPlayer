//! Playback rules on top of the player: trim mode (auto-pause at the trim end, skip past it by
//! hand), looping, seeking with the playback bar, play/pause. Ported from FeatherPlayer
//! `_start_progress_loop`, `_seek_start` / `_on_seek_drag` / `_seek_end`, `_toggle_play`,
//! `_apply_trim`, `_update_trim_mode_for_current`.

use crate::player::Player;
use crate::ui::trim_slider::{BarEvent, TrimBar};
use std::time::{Duration, Instant};

pub struct Playback {
    pub bar: TrimBar,
    /// Trim handles shown and enforced for the open clip.
    pub trim_mode: bool,
    /// Auto-paused on the trim end line.
    at_trim_end: bool,
    /// The user put the playhead on/after the trim end: let it play on.
    trim_past_ok: bool,
    trim_seek_t: Instant,
    pub loop_clip: bool,
    loop_seek_t: Instant,
    loop_prev_playing: bool,
    seeking: bool,
    seek_was_playing: bool,
    seek_last_val: i64,
    /// Position shown in the time label (may be held on the trim line).
    pub shown_pos: f64,
}

impl Playback {
    pub fn new(loop_clip: bool) -> Playback {
        let long_ago = Instant::now() - Duration::from_secs(10);
        Playback {
            bar: TrimBar::default(),
            trim_mode: false,
            at_trim_end: false,
            trim_past_ok: false,
            trim_seek_t: long_ago,
            loop_clip,
            loop_seek_t: long_ago,
            loop_prev_playing: false,
            seeking: false,
            seek_was_playing: false,
            seek_last_val: -1,
            shown_pos: 0.0,
        }
    }

    /// A new clip was opened (Python `_play_index` resets).
    pub fn reset_for_clip(&mut self) {
        self.at_trim_end = false;
        self.trim_past_ok = false;
        self.bar.value = 0.0;
        self.shown_pos = 0.0;
    }

    /// Show/hide the trim handles for the open clip (Python `_update_trim_mode_for_current`).
    /// `trim` = saved [start, end] on the 0..10000 scale.
    pub fn set_trim_mode(&mut self, enabled: bool, trim: Option<(f64, f64)>) {
        self.trim_mode = enabled;
        self.bar.trim_visible = enabled;
        match (enabled, trim) {
            (true, Some((s, e))) => self.bar.set_trim(s, e),
            _ => self.bar.set_trim(0.0, 10000.0),
        }
    }

    /// Python `_toggle_play`.
    pub fn toggle_play(&mut self, p: &mut Player) {
        if !p.loaded {
            return;
        }
        if p.playing() {
            self.at_trim_end = false;
            p.pause();
        } else if p.eof && !self.trim_mode {
            p.toggle_play(); // at the very end: start over
        } else {
            if self.at_trim_end && self.trim_mode && p.dur > 0.0 {
                p.seek(self.bar.trim_start / 10000.0 * p.dur);
            }
            self.at_trim_end = false;
            p.play();
        }
    }

    /// Handle the playback bar's events. Returns Some((start, end)) when a trim handle was let
    /// go and the trim should be saved (Python `_apply_trim`).
    pub fn bar_events(&mut self, evs: Vec<BarEvent>, p: &mut Player) -> Option<(f64, f64)> {
        let mut saved = None;
        for e in evs {
            match e {
                BarEvent::Press => {
                    // pause while the playhead is dragged; resume on release
                    self.seeking = true;
                    self.seek_last_val = -1;
                    self.at_trim_end = false;
                    self.seek_was_playing = p.playing();
                    if self.seek_was_playing {
                        p.pause();
                    }
                }
                BarEvent::Drag(v) => {
                    if self.seeking && p.dur > 0.0 {
                        let iv = v as i64;
                        if (iv - self.seek_last_val).abs() >= 2 {
                            self.seek_last_val = iv;
                            let secs = iv as f64 / 10000.0 * p.dur;
                            p.seek(secs);
                            self.shown_pos = secs;
                        }
                    }
                }
                BarEvent::Release(v) => {
                    self.seeking = false;
                    self.at_trim_end = false;
                    let mut snapped = false;
                    if p.dur > 0.0 {
                        let secs = v / 10000.0 * p.dur;
                        // put by hand on (or past) the trim end: allowed to play from there
                        let t_end = self.bar.trim_end / 10000.0 * p.dur;
                        self.trim_past_ok = self.trim_mode && secs >= t_end - 0.05;
                        self.trim_seek_t = Instant::now();
                        // the press/drag already seeked here: a second exact seek to the same
                        // spot only made mpv decode again (felt like lag while paused)
                        if v as i64 != self.seek_last_val {
                            p.seek(secs);
                        }
                        self.shown_pos = secs;
                        // dropped exactly on a trim line: stay paused there
                        if self.trim_mode && ((v - self.bar.trim_start).abs() < 0.5 || (v - self.bar.trim_end).abs() < 0.5) {
                            snapped = true;
                            p.pause();
                        }
                    }
                    let was = std::mem::take(&mut self.seek_was_playing);
                    if was && !snapped {
                        p.play();
                    }
                }
                BarEvent::TrimPress => {
                    // pause and freeze the bar so the seeks show a still frame
                    self.seeking = true;
                    if p.playing() {
                        p.pause();
                    }
                }
                BarEvent::TrimMove(v) => {
                    // show the frame under the dragged trim line
                    if p.dur > 0.0 {
                        let secs = v / 10000.0 * p.dur;
                        p.seek(secs);
                        self.shown_pos = secs;
                    }
                }
                BarEvent::TrimRelease => {
                    self.seeking = false;
                    self.at_trim_end = false;
                    self.trim_past_ok = false;
                    let (s, e) = (self.bar.trim_start, self.bar.trim_end);
                    // start playback over from the new start line
                    if p.dur > 0.0 {
                        p.seek(s / 10000.0 * p.dur);
                    }
                    saved = Some((s, e));
                }
            }
        }
        saved
    }

    /// Once per frame (Python's 15 ms progress tick): trim-end pause, looping, bar position.
    pub fn tick(&mut self, p: &mut Player) {
        let playing = p.playing();
        let dur = p.dur;
        let mut pos = p.pos;
        if !self.seeking && dur > 0.0 {
            let t_end = self.bar.trim_end / 10000.0 * dur;
            if self.trim_mode && !self.trim_past_ok && pos > t_end && (self.at_trim_end || playing) {
                pos = t_end; // never draw past the trim line
            }
            if !self.bar.is_dragging() {
                self.bar.value = (pos / dur * 10000.0).clamp(0.0, 10000.0);
            }
            // back before the trim end (e.g. skipped back): enforcement resumes
            if self.trim_past_ok && pos < t_end - 0.15 && self.trim_seek_t.elapsed() > Duration::from_millis(700) {
                self.trim_past_ok = false;
            }
            // auto-pause when playback reaches the trim end (or loop back to the start)
            if playing && self.trim_mode && !self.at_trim_end && !self.trim_past_ok && self.loop_seek_t.elapsed() > Duration::from_millis(300) && pos >= t_end - 0.03 {
                if self.loop_clip {
                    let t_start = self.bar.trim_start / 10000.0 * dur;
                    self.loop_seek_t = Instant::now();
                    p.seek(t_start);
                    pos = t_start;
                } else {
                    // just stop on the frame that is showing: a seek to t_end here was a
                    // keyframe seek and made the picture snap back. The bar is drawn on the
                    // trim line anyway (pos is clamped above).
                    self.at_trim_end = true;
                    p.pause();
                    pos = pos.min(t_end);
                }
            }
            // loop an untrimmed clip at its end
            if self.loop_clip && !self.trim_mode && pos >= dur - 0.15 && (playing || self.loop_prev_playing) && self.loop_seek_t.elapsed() > Duration::from_millis(500) {
                self.loop_seek_t = Instant::now();
                p.seek(0.0);
                pos = 0.0;
                p.play();
            }
            self.shown_pos = pos;
        } else if !self.seeking {
            self.shown_pos = pos;
        }
        self.loop_prev_playing = playing;
    }
}
