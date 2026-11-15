//! The clip list: what the last scan found, sort/filter, how many are shown, selection.
//! Ported from FeatherPlayer `_finish_load_folders`, `_on_sort_change`, `_render_grid`.

use crate::prefs::Prefs;
use crate::scanner::{ScanResult, ScannedClip};
use chrono::{Local, NaiveDateTime};
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
        }
    }

    /// Take a finished scan. Returns the paths that are new since the previous list.
    pub fn apply_scan(&mut self, r: ScanResult) -> Vec<String> {
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
        // a manual (non-auto) load clears the open clip and the selection
        self.current = None;
        self.selected.clear();
        self.total_size = Some(r.total_size);
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

    /// All clips passing the filter (game and date filters come with the categories step).
    pub fn filtered<'a>(&'a self, prefs: &'a Prefs) -> Vec<&'a String> {
        self.videos
            .iter()
            .filter(|v| match self.filter {
                Filter::All => true,
                Filter::Starred => prefs.starred.contains(*v),
                Filter::Liked => prefs.liked.contains(*v),
                Filter::Trimmed => prefs.trim_enabled.contains(*v),
            })
            .collect()
    }

    pub fn empty_msg(&self) -> &'static str {
        self.filter.empty_msg()
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
}
