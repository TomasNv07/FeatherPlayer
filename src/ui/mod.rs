//! UI: custom widgets and views.

pub mod clip_grid;
pub mod dialogs;
pub mod header;
pub mod icons;
pub mod player_view;
pub mod widgets;

/// `m:ss`, or `h:mm:ss` from one hour up.
pub fn fmt_time(s: f64) -> String {
    let s = s.max(0.0) as i64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}
