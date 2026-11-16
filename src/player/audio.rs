//! Audio tracks on the mpv side: reading the track list and playing a chosen set of tracks at
//! chosen volumes (FeatherPlayer `_at_build_graph` / `_at_apply` / `_at_apply_keep_pos`).
//! Which tracks to play is decided in `crate::tracks`.

use super::Player;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Track {
    pub id: i64,
    pub title: String,
    /// Stream `handler_name` metadata ("Game" / "Mic" in SteelSeries clips).
    pub handler: String,
}

/// Same rules as FeatherPlayer._at_build_graph: one untouched track is picked directly (aid),
/// several are mixed with amix (which divides the level by N, so volume=N puts it back).
pub fn build_graph(items: &[(i64, f64)]) -> (Option<i64>, String) {
    if items.len() == 1 && (items[0].1 - 1.0).abs() < 0.005 {
        return (Some(items[0].0), String::new());
    }
    if items.len() == 1 {
        return (None, format!("[aid{}]volume={:.3}[ao]", items[0].0, items[0].1));
    }
    let mut parts = Vec::new();
    let mut labels = String::new();
    for (k, (id, v)) in items.iter().enumerate() {
        if (v - 1.0).abs() < 0.005 {
            labels.push_str(&format!("[aid{id}]"));
        } else {
            parts.push(format!("[aid{id}]volume={v:.3}[v{k}]"));
            labels.push_str(&format!("[v{k}]"));
        }
    }
    let n = items.len();
    parts.push(format!("{labels}amix=inputs={n}:duration=longest,volume={n}[ao]"));
    (None, parts.join(";"))
}

impl Player {
    /// Python `_read_track_list`: track-list as JSON text, audio entries only.
    /// `raw` = the observed `track-list` JSON.
    pub(super) fn read_tracks(&mut self, raw: &str) {
        self.tracks.clear();
        let list: serde_json::Value = serde_json::from_str(raw).unwrap_or_default();
        for t in list.as_array().into_iter().flatten() {
            if t.get("type").and_then(|v| v.as_str()) != Some("audio") {
                continue;
            }
            let s = |v: Option<&serde_json::Value>| v.and_then(|v| v.as_str()).unwrap_or("").to_owned();
            self.tracks.push(Track {
                id: t.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
                title: s(t.get("title")),
                handler: s(t.get("metadata").and_then(|m| m.get("handler_name"))),
            });
        }
        self.tracks_new = true;
    }

    /// Python `_mpv_set_lavfi`: only set lavfi-complex when it changes (each set rebuilds
    /// mpv's filter graph).
    pub(super) fn set_lavfi(&mut self, graph: &str) {
        if graph != self.lavfi_cur {
            // async: a blocking graph change waits for the video output (see Core::cmd_async)
            if let Err(e) = self.core.set_async("lavfi-complex", graph) {
                self.msg = e;
            }
            self.lavfi_cur = graph.to_owned();
        }
    }

    /// Python `_at_apply_keep_pos`: play exactly these (track id, volume) items without losing
    /// the place (adding a track makes mpv jump ahead): remember the position, apply, seek
    /// back, and check again after 350 ms.
    pub fn apply_tracks(&mut self, items: &[(i64, f64)]) {
        if items.is_empty() || !self.loaded {
            return;
        }
        self.applied = Some(items.to_vec());
        // a clip that was just told where to start: that spot, not mpv's position yet
        let held = self.hold.filter(|(until, _)| Instant::now() < *until).map(|(_, t)| t);
        let pos = held.or(self.obs_pos());
        let (aid, graph) = build_graph(items);
        self.set_lavfi(&graph);
        if let Some(a) = aid {
            let _ = self.core.set_async("aid", &a.to_string());
            self.aid_cur = a.to_string();
        }
        if let Some(p) = pos {
            let _ = self.core.cmd_async(&["seek", &format!("{p}"), "absolute+exact"]);
            let now = Instant::now();
            self.hold = Some((now + Duration::from_millis(400), p));
            self.audio_check = Some((now + Duration::from_millis(350), p, now));
        }
    }

    /// The 350 ms check of `_at_apply_keep_pos`: if mpv still ended up away from where it
    /// should be, put it back.
    pub(super) fn audio_check_tick(&mut self) {
        let Some((due, pos, t0)) = self.audio_check else { return };
        if Instant::now() < due {
            return;
        }
        self.audio_check = None;
        let Some(now) = self.obs_pos() else { return };
        let expect = if self.paused { pos } else { pos + t0.elapsed().as_secs_f64() };
        if (now - expect).abs() > 0.5 {
            let _ = self.core.cmd(&["seek", &format!("{expect}"), "absolute+exact"]);
        }
    }
}
