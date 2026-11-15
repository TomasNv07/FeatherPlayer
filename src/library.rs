//! The clip list: what the last scan found, sort/filter, how many are shown, selection.
//! Ported from FeatherPlayer `_finish_load_folders`, `_on_sort_change`, `_render_grid`.

use crate::prefs::{KeywordGroup, Prefs};
use crate::scanner::{self, ScanResult, ScannedClip};
use chrono::{Local, NaiveDate, NaiveDateTime};
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Newest,
    Oldest,
    Largest,
    Smallest,
}

impl Sort {
    pub const ALL: [Sort; 4] = [Sort::Newest, Sort::Oldest, Sort::Largest, Sort::Smallest];
    pub fn label(self) -> &'static str {
        match self {
            Sort::Newest => "Newest",
            Sort::Oldest => "Oldest",
            Sort::Largest => "Largest",
            Sort::Smallest => "Smallest",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Starred,
    Liked,
    Trimmed,
}

impl Filter {
    pub const ALL: [Filter; 4] = [Filter::All, Filter::Starred, Filter::Liked, Filter::Trimmed];
    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "All clips",
            Filter::Starred => "Starred",
            Filter::Liked => "Liked",
            Filter::Trimmed => "Trimmed",
        }
    }
    fn empty_msg(self) -> &'static str {
        match self {
            Filter::All => "No videos found",
            Filter::Starred => "No starred clips",
            Filter::Liked => "No liked clips",
            Filter::Trimmed => "No trimmed clips",
        }
    }
}

pub struct ClipInfo {
    pub sort_mtime: f64,
    pub size: u64,
    pub clip_dt: Option<NaiveDateTime>,
    /// "dd-mm-YYYY  HH:MM"
    pub date_str: String,
    /// "12.3 MB"
    pub mb_str: String,
    /// "3 days ago" (worked out when the scan finished, like Python)
    pub ago_str: String,
}

/// Python's relative-time label.
pub fn ago(clip_dt: &NaiveDateTime, now: &NaiveDateTime) -> String {
    let delta = *now - *clip_dt;
    let secs = delta.num_milliseconds() as f64 / 1000.0;
    let days = (secs / 86400.0).floor() as i64; // Python timedelta.days rounds down
    if secs < 60.0 {
        "Just now".into()
    } else if secs < 3600.0 {
        let m = (secs / 60.0) as i64;
        format!("{m} minute{} ago", if m != 1 { "s" } else { "" })
    } else if days <= 0 {
        let h = (secs / 3600.0) as i64;
        format!("{h} hour{} ago", if h != 1 { "s" } else { "" })
    } else if days == 1 {
        "1 day ago".into()
    } else {
        format!("{days} days ago")
    }
}

pub fn fmt_size_total(total: u64) -> String {
    let gb = total as f64 / 1024f64.powi(3);
    if gb >= 1.0 { format!("{gb:.2} GB") } else { format!("{:.1} MB", total as f64 / 1024f64.powi(2)) }
}

// ---------------------------------------------------------------------------------------------
// Categories (Python `_category_key_for_video`, `_recompute_categories`)
// ---------------------------------------------------------------------------------------------

/// Filter key of clips without a game name ("Moments"); shared with the cloud-sync game list.
pub const NONE_KEY: &str = "__none__";

/// Stable category key of a clip: the first keyword group whose keyword starts the normalised
/// filename, else the normalised auto-detected game name, else None ("Moments").
pub fn category_key_for_video(path: &str, groups: &[(String, KeywordGroup)]) -> Option<String> {
    let name = scanner::basename(path);
    let fname_norm = scanner::normalize_game(scanner::stem(name));
    for (key, g) in groups {
        if g.keywords.iter().any(|kw| !kw.is_empty() && fname_norm.starts_with(kw.as_str())) {
            return Some(key.clone());
        }
    }
    let label = scanner::game_name_from_filename(name);
    if label.is_empty() || label == "Moments" {
        return None;
    }
    Some(scanner::normalize_game(&label))
}

/// Categories of the clips that are loaded right now. Building this NEVER deletes anything from
/// the keyword groups or renames: a category with no matching clip just isn't listed (the
/// Python app once lost keyword groups that way).
#[derive(Default)]
pub struct Categories {
    /// key -> auto-detected / keyword-group label
    pub auto_map: HashMap<String, String>,
    /// key -> label shown (user rename wins for auto categories), incl. NONE_KEY -> "Moments"
    pub norm_map: HashMap<String, String>,
    /// path -> filter key (category key or NONE_KEY)
    pub key_of: HashMap<String, String>,
    pub counts: HashMap<String, usize>,
}

impl Categories {
    pub fn compute(videos: &[String], prefs: &Prefs) -> Categories {
        let mut c = Categories::default();
        let mut has_moments = false;
        for v in videos {
            let key = category_key_for_video(v, &prefs.keyword_groups);
            let fk = key.clone().unwrap_or_else(|| NONE_KEY.to_owned());
            *c.counts.entry(fk.clone()).or_default() += 1;
            c.key_of.insert(v.clone(), fk);
            let Some(key) = key else {
                has_moments = true;
                continue;
            };
            let label = match prefs.group(&key) {
                Some(g) => g.label.clone(),
                None => {
                    let label = scanner::game_name_from_filename(scanner::basename(v));
                    match c.auto_map.get(&key) {
                        Some(e) if e.chars().count() >= label.chars().count() => e.clone(),
                        _ => label,
                    }
                }
            };
            c.auto_map.insert(key, label);
        }
        for (k, auto) in &c.auto_map {
            c.norm_map.insert(k.clone(), prefs.rename(k).map(str::to_owned).unwrap_or_else(|| auto.clone()));
        }
        if has_moments {
            c.norm_map.insert(NONE_KEY.into(), "Moments".into());
        }
        c
    }

    pub fn label<'a>(&'a self, key: &'a str) -> &'a str {
        self.norm_map.get(key).map(String::as_str).unwrap_or(key)
    }

    /// Category keys sorted by their display label (case-insensitive), like the Python popup.
    pub fn sorted_keys(&self) -> Vec<String> {
        let mut v: Vec<String> = self.norm_map.keys().cloned().collect();
        v.sort_by_key(|k| self.label(k).to_lowercase());
        v
    }

    /// Text on the game button (Python `_update_game_btn_text`).
    pub fn button_text(&self, prefs: &Prefs) -> String {
        if prefs.game_multi.is_empty() {
            return self.label(&prefs.game_filter).to_owned();
        }
        let mut names: Vec<&str> = prefs.game_multi.iter().map(|k| self.label(k)).collect();
        names.sort();
        let joined = names.join(", ");
        if names.len() == 1 {
            names[0].to_owned()
        } else if names.len() == 2 && joined.chars().count() <= 18 {
            joined
        } else {
            format!("{} games", names.len())
        }
    }

    /// After a rescan / category change: drop a saved game filter whose category is gone
    /// (older prefs stored the label instead of the key, so match that too).
    pub fn validate(&self, prefs: &mut Prefs) {
        if prefs.game_filter != "All" && !self.norm_map.contains_key(&prefs.game_filter) {
            let want = prefs.game_filter.to_lowercase();
            prefs.game_filter = self.norm_map.iter().find(|(_, v)| v.to_lowercase() == want).map(|(k, _)| k.clone()).unwrap_or_else(|| "All".into());
        }
        prefs.game_multi.retain(|k| self.norm_map.contains_key(k));
        if !prefs.game_multi.is_empty() {
            prefs.game_filter = "All".into();
        }
    }
}

pub const VISIBLE_START: usize = 50;
pub const LOAD_ALL_ASK: usize = 1500;

pub struct Library {
    /// Clip paths in display order (sorted in place, like Python's `self.videos`).
    pub videos: Vec<String>,
    pub info: HashMap<String, ClipInfo>,
    pub sort: Sort,
    pub filter: Filter,
    pub visible_count: usize,
    pub see_more_step: usize,
    pub total_size: Option<u64>,
    pub loaded_once: bool,
    /// The open clip (Python `current_idx`, kept as a path).
    pub current: Option<String>,
    /// Right-click selection.
    pub selected: BTreeSet<String>,
    pub cats: Categories,
    /// Calendar filter (both ends inclusive). `date_pending`: start picked, waiting for the end.
    pub date_from: Option<NaiveDate>,
    pub date_to: Option<NaiveDate>,
    pub date_pending: bool,
}

impl Library {
    pub fn new(see_more_step: usize) -> Library {
        Library {
            videos: Vec::new(),
            info: HashMap::new(),
            sort: Sort::Newest,
            filter: Filter::All,
            visible_count: VISIBLE_START,
            see_more_step,
            total_size: None,
            loaded_once: false,
            current: None,
            selected: BTreeSet::new(),
            cats: Categories::default(),
            date_from: None,
            date_to: None,
            date_pending: false,
        }
    }

    pub fn recompute_categories(&mut self, prefs: &Prefs) {
        self.cats = Categories::compute(&self.videos, prefs);
    }

    /// Calendar day of a clip (filename date, else file mtime).
    pub fn clip_date(&self, path: &str) -> Option<NaiveDate> {
        self.info.get(path)?.clip_dt.map(|d| d.date())
    }

    /// {day: number of clips} for every loaded clip.
    pub fn clip_dates(&self) -> HashMap<NaiveDate, usize> {
        let mut m = HashMap::new();
        for v in &self.videos {
            if let Some(d) = self.clip_date(v) {
                *m.entry(d).or_default() += 1;
            }
        }
        m
    }

    /// `a = None` clears the date filter, otherwise filter to a..b (b defaults to a).
    pub fn set_date_range(&mut self, a: Option<NaiveDate>, b: Option<NaiveDate>, pending: bool) {
        self.date_from = a;
        self.date_to = a.map(|a| b.unwrap_or(a));
        self.date_pending = a.is_some() && pending;
        self.visible_count = VISIBLE_START;
    }

    /// Take a finished scan. Returns the paths that are new since the previous list.
    /// `auto` = silent background refresh (Python `auto=True`): keeps the open clip and selection.
    pub fn apply_scan(&mut self, r: ScanResult, prefs: &mut Prefs, auto: bool) -> Vec<String> {
        let prev: HashSet<String> = self.videos.iter().cloned().collect();
        let now = Local::now().naive_local();
        self.info.clear();
        self.videos = r.clips.iter().map(|c| c.path.clone()).collect();
        for ScannedClip { path, sort_mtime, size, clip_dt } in r.clips {
            let date_str = clip_dt.map(|d| d.format("%d-%m-%Y  %H:%M").to_string()).unwrap_or_default();
            let mb_str = format!("{:.1} MB", size as f64 / (1024.0 * 1024.0));
            let ago_str = clip_dt.as_ref().map(|d| ago(d, &now)).unwrap_or_default();
            self.info.insert(path, ClipInfo { sort_mtime, size, clip_dt, date_str, mb_str, ago_str });
        }
        self.loaded_once = true;
        if auto {
            let cur: HashSet<&String> = self.videos.iter().collect();
            self.selected.retain(|s| cur.contains(s));
            if self.current.as_ref().is_some_and(|c| !cur.contains(c)) {
                self.current = None;
            }
        } else {
            // a manual load clears the open clip and the selection
            self.current = None;
            self.selected.clear();
        }
        self.total_size = Some(r.total_size);
        self.recompute_categories(prefs);
        self.cats.validate(prefs);
        self.apply_sort(self.sort, false);
        self.videos.iter().filter(|p| !prev.contains(*p)).cloned().collect()
    }

    pub fn apply_sort(&mut self, sort: Sort, reset_visible: bool) {
        self.sort = sort;
        if self.videos.is_empty() {
            return;
        }
        if reset_visible {
            self.visible_count = VISIBLE_START;
        }
        let info = &self.info;
        let mtime = |p: &String| info.get(p).map(|i| i.sort_mtime).unwrap_or(0.0);
        let size = |p: &String| info.get(p).map(|i| i.size).unwrap_or(0);
        let cmp_f = |a: f64, b: f64| a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal);
        match sort {
            Sort::Newest => self.videos.sort_by(|a, b| cmp_f(mtime(b), mtime(a))),
            Sort::Oldest => self.videos.sort_by(|a, b| cmp_f(mtime(a), mtime(b))),
            Sort::Largest => self.videos.sort_by_key(|p| std::cmp::Reverse(size(p))),
            Sort::Smallest => self.videos.sort_by_key(size),
        }
    }

    pub fn set_filter(&mut self, f: Filter) {
        self.filter = f;
        self.visible_count = VISIBLE_START;
    }

    /// All clips passing the filter, then the game filter, then the date filter.
    pub fn filtered<'a>(&'a self, prefs: &'a Prefs) -> Vec<&'a String> {
        let key = |v: &str| self.cats.key_of.get(v).map(String::as_str).unwrap_or(NONE_KEY);
        self.videos
            .iter()
            .filter(|v| match self.filter {
                Filter::All => true,
                Filter::Starred => prefs.starred.contains(*v),
                Filter::Liked => prefs.liked.contains(*v),
                Filter::Trimmed => prefs.trim_enabled.contains(*v),
            })
            .filter(|v| {
                if !prefs.game_multi.is_empty() {
                    prefs.game_multi.contains(key(v))
                } else {
                    prefs.game_filter.is_empty() || prefs.game_filter == "All" || key(v) == prefs.game_filter
                }
            })
            .filter(|v| match (self.date_from, self.date_to) {
                (Some(a), Some(b)) => self.clip_date(v).is_some_and(|d| a <= d && d <= b),
                _ => true,
            })
            .collect()
    }

    pub fn empty_msg(&self) -> String {
        match (self.date_from, self.date_to) {
            (Some(a), Some(b)) if a == b => format!("No clips on {}", a.format("%d.%m.%Y")),
            (Some(a), Some(b)) => format!("No clips from {} to {}", a.format("%d.%m.%Y"), b.format("%d.%m.%Y")),
            _ => self.filter.empty_msg().to_owned(),
        }
    }

    pub fn show_more(&mut self) {
        self.visible_count += self.see_more_step;
    }

    pub fn show_all(&mut self, total: usize) {
        self.visible_count = self.visible_count.max(total);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(d: u32, h: u32, m: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, d).unwrap().and_hms_opt(h, m, s).unwrap()
    }

    #[test]
    fn ago_labels() {
        let now = at(16, 12, 0, 0);
        assert_eq!(ago(&at(16, 11, 59, 30), &now), "Just now");
        assert_eq!(ago(&at(16, 11, 59, 0), &now), "1 minute ago");
        assert_eq!(ago(&at(16, 11, 0, 0), &now), "1 hour ago");
        assert_eq!(ago(&at(15, 13, 0, 0), &now), "23 hours ago");
        assert_eq!(ago(&at(15, 12, 0, 0), &now), "1 day ago");
        assert_eq!(ago(&at(10, 12, 0, 0), &now), "6 days ago");
        assert_eq!(ago(&at(17, 12, 0, 0), &now), "Just now");
    }

    #[test]
    fn sizes() {
        assert_eq!(fmt_size_total(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(fmt_size_total(3 * 1024 * 1024 * 1024), "3.00 GB");
    }

    fn group(label: &str, kws: &[&str]) -> KeywordGroup {
        KeywordGroup { label: label.into(), keywords: kws.iter().map(|s| s.to_string()).collect(), extra: Default::default() }
    }

    #[test]
    fn category_keys() {
        let groups = vec![("minecraft".to_owned(), group("Minecraft", &["javaw"])), ("cs".to_owned(), group("CS", &["counterstrike"]))];
        assert_eq!(category_key_for_video("Z:/c\\javaw_2026-01-01_10-00-00.mp4", &groups).as_deref(), Some("minecraft"));
        assert_eq!(category_key_for_video("Z:/c\\Counter-Strike 2 2024.01.05 - 21.03.44.02.DVR.mp4", &groups).as_deref(), Some("cs"));
        assert_eq!(category_key_for_video("Z:/c\\RocketLeague_2026-05-31_22-22-04.mp4", &groups).as_deref(), Some("rocketleague"));
        assert_eq!(category_key_for_video("Z:/c\\JHGjhg.mp4", &groups), None);
    }

    /// Dumps {path: [filter key, label]} for the real clip folders, to compare with Python:
    /// `FP_CAT_DUMP=out.json cargo test -- --ignored cat_dump`
    #[test]
    #[ignore]
    fn cat_dump() {
        let prefs = crate::prefs::Prefs::load();
        let clips = crate::scanner::scan_folders(&prefs.checked_folders);
        let videos: Vec<String> = clips.into_iter().map(|c| c.path).collect();
        let cats = Categories::compute(&videos, &prefs);
        let out: serde_json::Map<String, serde_json::Value> =
            videos.iter().map(|v| { let k = &cats.key_of[v]; (v.clone(), serde_json::json!([k, cats.label(k)])) }).collect();
        std::fs::write(std::env::var("FP_CAT_DUMP").unwrap(), serde_json::to_string(&out).unwrap()).unwrap();
    }
}
