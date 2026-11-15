//! Audio track listing, picking and mixing (FeatherPlayer `_at_build_graph` rules).

use super::Player;
use std::time::{Duration, Instant};

pub struct Track {
    pub id: i64,
    pub label: String,
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
    pub(super) fn read_tracks(&mut self) {
        self.tracks.clear();
        let count: usize = self.core.get("track-list/count").and_then(|s| s.parse().ok()).unwrap_or(0);
        let mut n = 0;
        for i in 0..count {
            if self.core.get(&format!("track-list/{i}/type")).as_deref() != Some("audio") {
                continue;
            }
            let id: i64 = self.core.get(&format!("track-list/{i}/id")).and_then(|s| s.parse().ok()).unwrap_or(0);
            let title = self.core.get(&format!("track-list/{i}/title")).unwrap_or_default();
            let lang = self.core.get(&format!("track-list/{i}/lang")).unwrap_or_default();
            let codec = self.core.get(&format!("track-list/{i}/codec")).unwrap_or_default();
            n += 1;
            let detail = [title, lang, codec].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" \u{b7} ");
            self.tracks.push(Track { id, label: format!("Track {n}  {detail}") });
        }
        self.sel.clear();
        if !self.tracks.is_empty() {
            self.sel.insert(0);
            self.audio_status = format!("aid {} (mpv default)", self.tracks[0].id);
        }
    }

    /// Make mpv play exactly the ticked tracks (keeps the playback position, like _at_apply_keep_pos).
    pub fn apply_audio(&mut self) {
        let items: Vec<(i64, f64)> = self.sel.iter().filter_map(|&n| self.tracks.get(n)).map(|t| (t.id, 1.0)).collect();
        if items.is_empty() {
            return;
        }
        let pos = self.core.getf("time-pos");
        let (aid, graph) = build_graph(&items);
        if graph != self.lavfi_cur {
            if let Err(e) = self.core.set("lavfi-complex", &graph) {
                self.msg = e;
            }
            self.lavfi_cur = graph.clone();
        }
        if let Some(a) = aid {
            let _ = self.core.set("aid", &a.to_string());
            self.audio_status = format!("aid {a}");
        } else {
            self.audio_status = graph;
        }
        if let Some(p) = pos {
            let _ = self.core.cmd(&["seek", &format!("{p}"), "absolute+exact"]);
            self.hold = Some((Instant::now() + Duration::from_millis(400), p));
        }
    }
}
