//! Audio tracks editor (Python `_at_*`): which of a clip's audio tracks play and how loud.
//! Defaults (settings `default_audio_tracks` / `default_audio_volumes`), per-clip choices
//! (`clip_audio_tracks` / `clip_audio_volumes`), SteelSeries Game+Mic clips, the
//! `audio_tracks_btn` and `steelseries_first_track_only` settings. Track numbers are 1-based.

use crate::player::audio::Track;
use crate::prefs;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// (mpv track id, volume) pairs to play.
pub type Items = Vec<(i64, f64)>;

pub struct AudioTracks {
    /// Audio tracks button + default rows shown (settings `audio_tracks_btn`).
    pub enabled: bool,
    pub steel_first_only: bool,
    /// Default tracks (Python `_at_defaults`, sorted, at least one).
    pub defaults: Vec<usize>,
    /// Default volume per track number; 1.0 is left out.
    pub def_vol: BTreeMap<usize, f64>,
    clip_sel: HashMap<String, Vec<usize>>,
    clip_vol: HashMap<String, BTreeMap<usize, f64>>,
    /// Clips with saved tracks or volumes (music badge on the card).
    pub badges: HashSet<String>,
    // ---- the open clip ----
    path: Option<String>,
    pub tracks: Vec<Track>,
    /// Selected tracks, None = no control.
    pub sel: Option<BTreeSet<usize>>,
    /// True when the clip can be controlled (2+ tracks).
    pub ctl: bool,
    /// This clip's volumes that differ from the defaults.
    vol: BTreeMap<usize, f64>,
    /// SteelSeries clip: its default selection.
    steel: Option<BTreeSet<usize>>,
}

/// Python `_at_clean`: track numbers 1..6, no duplicates, sorted; [1] if empty.
fn clean(v: Option<&Value>) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    for x in v.and_then(|v| v.as_array()).into_iter().flatten() {
        let n = x.as_i64().or_else(|| x.as_f64().map(|f| f as i64)).or_else(|| x.as_str().and_then(|s| s.parse().ok()));
        let Some(n) = n else { break }; // Python stops at the first non-number
        if (1..=6).contains(&n) && !out.contains(&(n as usize)) {
            out.push(n as usize);
        }
    }
    out.sort();
    if out.is_empty() {
        vec![1]
    } else {
        out
    }
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

impl AudioTracks {
    pub fn load(st: &Map<String, Value>) -> AudioTracks {
        let get_bool = |k: &str, d: bool| st.get(k).and_then(|v| v.as_bool()).unwrap_or(d);
        let mut def_vol = BTreeMap::new();
        if let Some(m) = st.get("default_audio_volumes").and_then(|v| v.as_object()) {
            for (k, v) in m {
                if let (Ok(n), Some(f)) = (k.parse::<usize>(), num(v)) {
                    if (1..=6).contains(&n) && (0.0..=2.0).contains(&f) && f != 1.0 {
                        def_vol.insert(n, (f * 10.0).round() / 10.0);
                    }
                }
            }
        }
        let mut clip_vol = HashMap::new();
        if let Some(m) = st.get("clip_audio_volumes").and_then(|v| v.as_object()) {
            for (p, d) in m {
                let mm: BTreeMap<usize, f64> = d
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter_map(|(k, v)| Some((k.parse::<usize>().ok()?, num(v)?)))
                    .filter(|(k, v)| (1..=6).contains(k) && (0.0..=2.0).contains(v))
                    .collect();
                if !mm.is_empty() {
                    clip_vol.insert(p.clone(), mm);
                }
            }
        }
        let clip_sel = st.get("clip_audio_tracks").and_then(|v| v.as_object()).map(|m| m.iter().map(|(p, v)| (p.clone(), clean(Some(v)))).collect()).unwrap_or_default();
        let mut a = AudioTracks {
            enabled: get_bool("audio_tracks_btn", true),
            steel_first_only: get_bool("steelseries_first_track_only", false),
            defaults: clean(st.get("default_audio_tracks")),
            def_vol,
            clip_sel,
            clip_vol,
            badges: HashSet::new(),
            path: None,
            tracks: Vec::new(),
            sel: None,
            ctl: false,
            vol: BTreeMap::new(),
            steel: None,
        };
        a.rebuild_badges();
        a
    }

    fn rebuild_badges(&mut self) {
        self.badges = self.clip_sel.keys().chain(self.clip_vol.keys()).cloned().collect();
    }

    /// Python `_at_clip_reset`: a new clip is being opened (or none).
    pub fn clip_reset(&mut self, path: Option<&str>) {
        self.path = path.map(str::to_owned);
        self.tracks.clear();
        self.sel = None;
        self.ctl = false;
        self.vol.clear();
        self.steel = None;
    }

    /// Python `_at_eff_vol`: this clip's own volume, else the default.
    pub fn eff_vol(&self, n: usize) -> f64 {
        if self.steel.is_some() {
            return self.vol.get(&n).copied().unwrap_or(1.0); // SteelSeries: settings defaults don't apply
        }
        self.vol.get(&n).or_else(|| self.def_vol.get(&n)).copied().unwrap_or(1.0)
    }

    /// Python `_at_default_sel`: default tracks this clip really has; track 1 if none match.
    fn default_sel(&self, count: usize) -> BTreeSet<usize> {
        if let Some(s) = &self.steel {
            return s.clone();
        }
        let sel: BTreeSet<usize> = self.defaults.iter().copied().filter(|n| *n <= count).collect();
        if sel.is_empty() {
            [1].into()
        } else {
            sel
        }
    }

    /// Python `_at_changed`: the button turns accent-coloured.
    pub fn changed(&self) -> bool {
        match (&self.sel, self.ctl) {
            (Some(sel), true) => *sel != self.default_sel(self.tracks.len()) || !self.vol.is_empty(),
            _ => false,
        }
    }

    /// Python `_at_label`.
    pub fn label(&self, n: usize) -> String {
        let Some(t) = self.tracks.get(n - 1) else { return format!("Track {n}") };
        let mut title = t.title.trim().to_owned();
        if t.handler == "Game" || t.handler == "Mic" {
            title = t.handler.clone();
        }
        if title.is_empty() {
            format!("Track {n}")
        } else {
            format!("{n}  {title}")
        }
    }

    /// Python `_at_status_text`.
    pub fn status_text(&self) -> &'static str {
        match self.tracks.len() {
            0 => "No audio tracks found yet (no clip loaded, or it is still opening)",
            1 => "This clip has only one audio track, so there is nothing to choose",
            _ => "This clip's Game and Mic tracks are mixed automatically",
        }
    }

    /// The selection as (mpv id, volume) items (Python `_at_apply`).
    fn items(&self) -> Items {
        let Some(sel) = &self.sel else { return Vec::new() };
        sel.iter().filter(|n| **n >= 1 && **n <= self.tracks.len()).map(|&n| (self.tracks[n - 1].id, self.eff_vol(n))).collect()
    }

    /// Python `_on_track_list_changed` + `_at_on_tracks`: the open clip's tracks are known.
    /// Returns what to apply (None: mpv's default first track is already right).
    pub fn on_tracks(&mut self, path: Option<&str>, tracks: &[Track]) -> Option<Items> {
        if self.path.as_deref() != path {
            self.clip_reset(path);
        }
        let has = |h: &str| tracks.iter().any(|t| t.handler == h);
        let steel_clip = has("Game") && has("Mic");
        if steel_clip && self.steel_first_only {
            // SteelSeries clips start on their first track only (no mix, no black screen);
            // the Mic can still be ticked for one clip with the button
            self.steel = Some([1].into());
        } else if steel_clip {
            self.steel = Some(tracks.iter().enumerate().filter(|(_, t)| t.handler == "Game" || t.handler == "Mic").map(|(i, _)| i + 1).collect());
        } else {
            self.steel = None;
            if tracks.len() < 2 {
                // nothing to pick for this clip
                self.tracks = tracks.to_vec();
                self.ctl = false;
                self.sel = None;
                return None;
            }
        }
        self.tracks = tracks.to_vec();
        let n = tracks.len();
        let cur = self.path.clone().unwrap_or_default();
        let saved = if self.enabled { self.clip_sel.get(&cur) } else { None };
        let sel: BTreeSet<usize> = saved.into_iter().flatten().copied().filter(|k| *k <= n).collect();
        self.sel = Some(if sel.is_empty() { self.default_sel(n) } else { sel });
        let sv = if self.enabled { self.clip_vol.get(&cur).cloned() } else { None };
        self.vol = sv.unwrap_or_default().into_iter().filter(|(k, _)| *k <= n).collect();
        self.ctl = true;
        let items = self.items();
        let first = tracks[0].id;
        if items.is_empty() || (items.len() == 1 && items[0].0 == first && (items[0].1 - 1.0).abs() < 0.005) {
            return None; // already what is wanted
        }
        Some(items)
    }

    /// Python `_at_remember`: save this clip's tracks and volumes (or forget whichever
    /// matches the defaults again, so later changes to the defaults still apply).
    fn remember(&mut self) {
        let (Some(path), Some(sel)) = (self.path.clone(), self.sel.clone()) else { return };
        if sel != self.default_sel(self.tracks.len()) {
            self.clip_sel.insert(path.clone(), sel.into_iter().collect());
        } else {
            self.clip_sel.remove(&path);
        }
        if !self.vol.is_empty() {
            self.clip_vol.insert(path, self.vol.clone());
        } else {
            self.clip_vol.remove(&path);
        }
        self.save_clips();
    }

    fn save_clips(&mut self) {
        let mut sel: Vec<(&String, &Vec<usize>)> = self.clip_sel.iter().collect();
        sel.sort();
        let sel: Map<String, Value> = sel.into_iter().map(|(p, v)| (p.clone(), Value::from(v.clone()))).collect();
        let mut vol: Vec<(&String, &BTreeMap<usize, f64>)> = self.clip_vol.iter().collect();
        vol.sort_by(|a, b| a.0.cmp(b.0));
        let vol: Map<String, Value> = vol
            .into_iter()
            .map(|(p, m)| (p.clone(), Value::Object(m.iter().map(|(k, v)| (k.to_string(), Value::from(*v))).collect())))
            .collect();
        let mut st = prefs::read_settings();
        st.insert("clip_audio_tracks".into(), Value::Object(sel));
        st.insert("clip_audio_volumes".into(), Value::Object(vol));
        prefs::write_settings(&st);
        self.rebuild_badges();
    }

    /// Python `_at_toggle_track` (always keeps one track playing).
    pub fn toggle_track(&mut self, n: usize) -> Option<Items> {
        let sel = self.sel.as_mut()?;
        if sel.contains(&n) {
            if sel.len() == 1 {
                return None;
            }
            sel.remove(&n);
        } else {
            sel.insert(n);
        }
        let items = self.items();
        self.remember();
        Some(items)
    }

    /// Python `_at_set_vol`: `fin` = slider released (apply + save).
    pub fn set_vol(&mut self, n: usize, v: f64, fin: bool) -> Option<Items> {
        if (v - self.def_vol.get(&n).copied().unwrap_or(1.0)).abs() < 0.005 {
            self.vol.remove(&n); // same as the default: nothing to keep
        } else {
            self.vol.insert(n, v);
        }
        if !fin {
            return None;
        }
        let apply = self.sel.as_ref().is_some_and(|s| s.contains(&n));
        let items = self.items();
        self.remember();
        apply.then_some(items)
    }

    /// Python `_at_reset` ("Use defaults", right click on the button).
    pub fn reset(&mut self) -> Option<Items> {
        if !self.ctl {
            return None;
        }
        self.sel = Some(self.default_sel(self.tracks.len()));
        self.vol.clear();
        let items = self.items();
        self.remember();
        Some(items)
    }

    /// Python `_on_audio_btn_toggle`.
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        prefs::save_setting("audio_tracks_btn", Value::Bool(on));
    }

    /// Python `_on_steel_first_toggle`.
    pub fn set_steel_first(&mut self, on: bool) {
        self.steel_first_only = on;
        prefs::save_setting("steelseries_first_track_only", Value::Bool(on));
    }

    /// Python `_on_at_default_tick` (at least one default track).
    pub fn default_tick(&mut self, n: usize) {
        let mut cur: BTreeSet<usize> = self.defaults.iter().copied().collect();
        if cur.contains(&n) {
            if cur.len() == 1 {
                return;
            }
            cur.remove(&n);
        } else {
            cur.insert(n);
        }
        self.defaults = cur.into_iter().collect();
        prefs::save_setting("default_audio_tracks", Value::from(self.defaults.clone()));
    }

    /// Python `_on_at_default_vol`. Returns what to apply when the open clip follows the
    /// default for that track.
    pub fn default_vol(&mut self, n: usize, v: f64, fin: bool) -> Option<Items> {
        if (v - 1.0).abs() < 0.005 {
            self.def_vol.remove(&n);
        } else {
            self.def_vol.insert(n, v);
        }
        if !fin {
            return None;
        }
        let m: Map<String, Value> = self.def_vol.iter().map(|(k, x)| (k.to_string(), Value::from(*x))).collect();
        prefs::save_setting("default_audio_volumes", Value::Object(m));
        let follows = self.ctl && self.sel.as_ref().is_some_and(|s| !s.is_empty()) && !self.vol.contains_key(&n);
        follows.then(|| self.items())
    }

    /// Python `_copy_clip_settings` (audio part): a new file made from a clip keeps its
    /// tracks and volumes.
    pub fn copy_clip(&mut self, src: &str, dst: &str) {
        let mut changed = false;
        if let Some(v) = self.clip_sel.get(src).cloned() {
            self.clip_sel.insert(dst.to_owned(), v);
            changed = true;
        }
        if let Some(v) = self.clip_vol.get(src).cloned() {
            self.clip_vol.insert(dst.to_owned(), v);
            changed = true;
        }
        if changed {
            self.save_clips();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: i64, h: &str) -> Track {
        Track { id, title: String::new(), handler: h.into() }
    }

    fn fresh() -> AudioTracks {
        AudioTracks::load(&Map::new())
    }

    #[test]
    fn clean_rules() {
        assert_eq!(clean(Some(&serde_json::json!([3, 1, 3, 9]))), vec![1, 3]);
        assert_eq!(clean(Some(&serde_json::json!([]))), vec![1]);
        assert_eq!(clean(None), vec![1]);
    }

    #[test]
    fn single_track_no_control() {
        let mut a = fresh();
        assert_eq!(a.on_tracks(Some("c"), &[t(1, "")]), None);
        assert!(!a.ctl);
    }

    #[test]
    fn steelseries_mixes_game_and_mic() {
        let mut a = fresh();
        let items = a.on_tracks(Some("c"), &[t(1, "Game"), t(2, "Mic")]).unwrap();
        assert_eq!(items, vec![(1, 1.0), (2, 1.0)]);
        assert!(!a.changed());
        a.steel_first_only = true;
        assert_eq!(a.on_tracks(Some("d"), &[t(1, "Game"), t(2, "Mic")]), None);
    }

    #[test]
    fn defaults_apply_to_plain_multitrack() {
        let mut a = fresh();
        a.defaults = vec![1, 2];
        a.def_vol.insert(2, 0.5);
        let items = a.on_tracks(Some("c"), &[t(1, ""), t(2, ""), t(3, "")]).unwrap();
        assert_eq!(items, vec![(1, 1.0), (2, 0.5)]);
    }
}
