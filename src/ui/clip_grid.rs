//! Right sidebar: filter/sort pill and the virtualized clip list (a single "tower" of cards).
//! Ported from FeatherPlayer `_vg_layout`, `_vg_draw_card`, `_vg_refresh` and the mouse handlers.

use super::icons;
use super::widgets::{self, Ghost, Row};
use crate::library::{Filter, Library, Sort, LOAD_ALL_ASK, NONE_KEY};
use chrono::{Datelike, Local, NaiveDate};
use crate::prefs::Prefs;
use crate::theme::{self, c};
use eframe::egui::{self, Align2, Color32, Pos2, Rect, Sense, Stroke, TextureHandle, Vec2};
use std::collections::{HashMap, HashSet};

const CARD_BORDER: f32 = 3.0;
/// Calendar day cell (Tk label width=3 in FONT_TINY, pady 3).
const CAL_CELL: (f32, f32) = (30.0, 24.0);

fn calendar_size() -> (f32, f32) {
    (7.0 * (CAL_CELL.0 + 2.0) + 18.0, 0.0)
}

/// Weeks of a month, Monday first; days outside the month are None.
fn month_weeks(y: i32, m: u32) -> Vec<[Option<NaiveDate>; 7]> {
    let Some(first) = NaiveDate::from_ymd_opt(y, m, 1) else { return Vec::new() };
    let mut weeks = Vec::new();
    let mut week = [None; 7];
    let mut d = first;
    let mut col = first.weekday().num_days_from_monday() as usize;
    while d.month() == m {
        week[col] = Some(d);
        col += 1;
        if col == 7 {
            weeks.push(week);
            week = [None; 7];
            col = 0;
        }
        match d.succ_opt() {
            Some(n) => d = n,
            None => break,
        }
    }
    if col > 0 {
        weeks.push(week);
    }
    weeks
}
const PAD: f32 = 4.0;

pub enum GridAction {
    Play(String),
    /// Sort, filter, date or "load more" changed which clips are listed: reload thumbnails.
    ListChanged,
    /// Left click on a category: show only it ("All" = everything).
    SelectGame(String),
    /// Right click on a category: add/remove it from the multi-selection (popup stays open).
    ToggleGameMulti(String),
    /// Middle click on a category: rename its label.
    RenameCategory(String),
}

#[derive(PartialEq, Clone, Copy)]
enum Drop {
    Game,
    Filter,
    Sort,
    Date,
}

#[derive(Default)]
pub struct ClipGrid {
    pub thumbs: HashMap<String, TextureHandle>,
    rb_drag: bool,
    rb_toggled: HashSet<String>,
    open: Option<DropState>,
    /// Month shown in the calendar, and whether its year list is open.
    cal_ym: (i32, u32),
    cal_year_open: bool,
    /// Clip card under the mouse this frame (preloaded by the app).
    pub hovered: Option<String>,
}

#[derive(Clone, Copy)]
struct DropState {
    which: Drop,
    anchor: Rect,
}

/// Per-clip badges shown on the thumbnail.
pub struct Badges<'a> {
    /// Clips with their own audio tracks / volumes (settings `clip_audio_tracks` / `_volumes`).
    pub audio: &'a HashSet<String>,
}

impl ClipGrid {
    pub fn show(&mut self, ui: &mut egui::Ui, lib: &mut Library, prefs: &Prefs, badges: &Badges) -> Vec<GridAction> {
        let mut acts = Vec::new();
        self.hovered = None;
        self.header(ui, lib, prefs, &mut acts);
        self.grid(ui, lib, prefs, badges, &mut acts);
        acts
    }

    fn header(&mut self, ui: &mut egui::Ui, lib: &mut Library, prefs: &Prefs, acts: &mut Vec<GridAction>) {
        let mut rects: Vec<(Drop, Rect)> = Vec::new();
        let mut clicked: Option<(Drop, Rect)> = None;
        let mut hovered: Option<(Drop, Rect)> = None;
        let mut clear_date = false;
        let game_text = lib.cats.button_text(prefs);
        egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
            // calendar pill at the right, the stretching pill takes what is left
            let row_w = ui.available_width();
            ui.allocate_ui_with_layout(Vec2::new(row_w, theme::CTRL_H), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                widgets::pill_frame().show(ui, |ui| {
                    let mut g = Ghost::new("").icon("calendar", 16.0).fg(theme::text_primary()).padx(8.0);
                    if lib.date_from.is_some() {
                        g = g.accent_icon();
                    }
                    let r = g.show(ui);
                    rects.push((Drop::Date, r.rect));
                    if r.clicked() {
                        clicked = Some((Drop::Date, r.rect));
                    }
                    if r.secondary_clicked() {
                        clear_date = true;
                    }
                });
                ui.add_space(6.0);
                // frame: 2px margin + 1px stroke on each side
                let inner_w = (ui.available_width() - 6.0).floor().max(60.0);
                widgets::pill_frame().show(ui, |ui| {
                    ui.set_width(inner_w);
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    let col_w = ((inner_w - 2.0) / 3.0).floor();
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                        let items = [(Drop::Game, game_text.as_str()), (Drop::Filter, lib.filter.label()), (Drop::Sort, lib.sort.label())];
                        for (i, (d, text)) in items.into_iter().enumerate() {
                            if i > 0 {
                                widgets::pill_divider(ui);
                            }
                            let w = if i == 2 { inner_w - 2.0 * col_w - 2.0 } else { col_w };
                            let r = Ghost::new(text).font(theme::font_tiny()).fg(theme::text_primary()).arrow().width(w).show(ui);
                            rects.push((d, r.rect));
                            if r.clicked() {
                                clicked = Some((d, r.rect));
                            } else if r.hovered() {
                                hovered = Some((d, r.rect));
                            }
                        }
                    });
                });
            });
        });
        if clear_date {
            lib.set_date_range(None, None, false);
            self.open = None;
            acts.push(GridAction::ListChanged);
        }
        if let Some((d, r)) = clicked {
            self.toggle(d, r, lib);
        } else if let (Some((d, r)), Some(o)) = (hovered, self.open) {
            // hovering another dropdown button while one is open switches to it
            if o.which != d && o.which != Drop::Date && d != Drop::Date {
                self.open = Some(DropState { which: d, anchor: r });
            }
        }

        let Some(open) = self.open else { return };
        let ctx = ui.ctx().clone();
        let pos = Pos2::new(open.anchor.left(), open.anchor.bottom() + 3.0);
        let mut close = false;
        let popup = match open.which {
            Drop::Filter => widgets::popup(&ctx, "filter_popup", pos, open.anchor.width().max(130.0), |ui| {
                for f in Filter::ALL {
                    if Row::new(f.label()).font(theme::font_tiny()).height(28.0).active(f == lib.filter).show(ui).clicked() {
                        lib.set_filter(f);
                        acts.push(GridAction::ListChanged);
                        close = true;
                    }
                }
            }),
            Drop::Sort => widgets::popup(&ctx, "sort_popup", pos, open.anchor.width().max(130.0), |ui| {
                for s in Sort::ALL {
                    if Row::new(s.label()).font(theme::font_tiny()).height(28.0).active(s == lib.sort).show(ui).clicked() {
                        lib.apply_sort(s, true);
                        acts.push(GridAction::ListChanged);
                        close = true;
                    }
                }
            }),
            Drop::Game => widgets::popup(&ctx, "game_popup", pos, open.anchor.width().max(200.0), |ui| {
                egui::ScrollArea::vertical().max_height(350.0).show(ui, |ui| {
                    let multi = &prefs.game_multi;
                    let mut keys = vec!["All".to_owned()];
                    keys.extend(lib.cats.sorted_keys());
                    for key in keys {
                        let label = if key == "All" { "All" } else { lib.cats.label(&key) };
                        let count = if key == "All" { lib.videos.len() } else { lib.cats.counts.get(&key).copied().unwrap_or(0) };
                        let mut row = Row::new(label).font(theme::font_tiny()).height(28.0).right_text(count.to_string());
                        if multi.is_empty() {
                            row = row.active(key == prefs.game_filter);
                        } else {
                            row = row.check(multi.contains(&key));
                        }
                        let r = row.show(ui);
                        if r.clicked() {
                            acts.push(GridAction::SelectGame(key.clone()));
                            close = true;
                        } else if r.secondary_clicked() {
                            acts.push(GridAction::ToggleGameMulti(key.clone()));
                        } else if r.middle_clicked() && key != "All" && key != NONE_KEY {
                            acts.push(GridAction::RenameCategory(key.clone()));
                            close = true;
                        }
                    }
                });
            }),
            Drop::Date => {
                let (w, _) = calendar_size();
                let pos = Pos2::new((open.anchor.right() - w).max(0.0), open.anchor.bottom() + 3.0);
                let mut changed = false;
                let r = widgets::popup(&ctx, "date_popup", pos, w, |ui| {
                    let (c, ch) = self.calendar(ui, lib);
                    close = c;
                    changed = ch;
                });
                if changed {
                    acts.push(GridAction::ListChanged);
                }
                r
            }
        };
        if close {
            self.open = None;
        } else if ctx.input(|i| i.pointer.any_pressed()) {
            if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                if !popup.contains(p) && !rects.iter().any(|(_, r)| r.contains(p)) {
                    self.open = None;
                }
            }
        }
    }

    fn toggle(&mut self, which: Drop, anchor: Rect, lib: &Library) {
        self.open = match self.open {
            Some(o) if o.which == which => None,
            _ => Some(DropState { which, anchor }),
        };
        if which == Drop::Date && self.open.is_some() {
            let base = lib.date_from.unwrap_or_else(|| Local::now().date_naive());
            self.cal_ym = (base.year(), base.month());
            self.cal_year_open = false;
        }
    }

    fn shift_month(&mut self, delta: i32) {
        let (mut y, mut m) = (self.cal_ym.0, self.cal_ym.1 as i32 + delta);
        while m < 1 {
            m += 12;
            y -= 1;
        }
        while m > 12 {
            m -= 12;
            y += 1;
        }
        self.cal_ym = (y, m as u32);
        self.cal_year_open = false;
    }

    /// Python `_build_calendar`. Returns (close popup, filter changed).
    fn calendar(&mut self, ui: &mut egui::Ui, lib: &mut Library) -> (bool, bool) {
        let (close, mut changed) = (false, false);
        let (y, m) = self.cal_ym;
        let (a, b) = (lib.date_from, lib.date_to);
        let today = Local::now().date_naive();
        let have = lib.clip_dates();
        let dim = Color32::from_rgb(0x6b, 0x6b, 0x88);
        let range_bg = Color32::from_rgb(0x4a, 0x18, 0x30);
        let range_hov = Color32::from_rgb(0x5e, 0x20, 0x40);
        let (cw, chh) = (CAL_CELL.0, CAL_CELL.1);
        let width = ui.available_width();
        ui.spacing_mut().item_spacing = Vec2::ZERO;

        // label-like button
        let cell = |ui: &mut egui::Ui, size: Vec2, text: &str, fg: Color32, bg: Color32, hov: Color32, font: egui::FontId| -> egui::Response {
            let (r, resp) = ui.allocate_exact_size(size, Sense::click());
            let fill = if resp.hovered() { hov } else { bg };
            if fill != theme::card_bg() {
                ui.painter().rect_filled(r, 0.0, fill);
            }
            ui.painter().text(r.center(), Align2::CENTER_CENTER, text, font, fg);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            resp
        };

        // header: < Month Year >
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            if cell(ui, Vec2::new(30.0, 22.0), "\u{2039}", theme::text_muted(), theme::card_bg(), theme::card_hover(), theme::font_bold()).clicked() {
                self.shift_month(-1);
            }
            let month = NaiveDate::from_ymd_opt(y, m, 1).map(|d| d.format("%B").to_string()).unwrap_or_default();
            let yr = format!("{y} {}", if self.cal_year_open { "\u{25b4}" } else { "\u{25be}" });
            let mid_w = widgets::text_width(ui, &month, &theme::font_bold()) + 4.0 + widgets::text_width(ui, &yr, &theme::font_bold()) + 8.0;
            let side = ((width - 60.0 - mid_w) / 2.0).max(0.0);
            ui.add_space(side);
            ui.label(egui::RichText::new(&month).font(theme::font_bold()).color(theme::text_primary()));
            ui.add_space(4.0);
            let yw = widgets::text_width(ui, &yr, &theme::font_bold()) + 8.0;
            if cell(ui, Vec2::new(yw, 22.0), &yr, theme::text_primary(), theme::card_bg(), theme::card_hover(), theme::font_bold()).clicked() {
                self.cal_year_open = !self.cal_year_open;
            }
            ui.add_space((width - 30.0 - ui.min_rect().width()).max(0.0));
            if cell(ui, Vec2::new(30.0, 22.0), "\u{203a}", theme::text_muted(), theme::card_bg(), theme::card_hover(), theme::font_bold()).clicked() {
                self.shift_month(1);
            }
        });
        ui.add_space(4.0);

        if self.cal_year_open {
            // years with clips + this year + the shown year, newest first, 3 per row
            let mut years: Vec<i32> = have.keys().map(|d| d.year()).chain([today.year(), y]).collect();
            years.sort_unstable_by(|a, b| b.cmp(a));
            years.dedup();
            for row in years.chunks(3) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(2.0, 0.0);
                    ui.add_space(((width - 3.0 * 66.0 - 4.0) / 2.0).max(0.0));
                    for &yy in row {
                        let sel = yy == y;
                        let (fg, bg, font) = if sel { (theme::accent_fg(), theme::accent(), theme::font_bold()) } else { (theme::text_primary(), theme::card_bg(), theme::font_tiny()) };
                        if cell(ui, Vec2::new(66.0, 24.0), &yy.to_string(), fg, bg, if sel { bg } else { theme::card_hover() }, font).clicked() {
                            self.cal_ym = (yy, m);
                            self.cal_year_open = false;
                        }
                    }
                });
                ui.add_space(2.0);
            }
        } else {
            let pad_x = ((width - 7.0 * (cw + 2.0)) / 2.0).max(0.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(2.0, 0.0);
                ui.add_space(pad_x);
                for wd in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"] {
                    cell(ui, Vec2::new(cw, 20.0), wd, dim, theme::card_bg(), theme::card_bg(), theme::font_tiny());
                }
            });
            for week in month_weeks(y, m) {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(2.0, 0.0);
                    ui.add_space(pad_x);
                    for d in week {
                        let Some(d) = d else {
                            ui.allocate_exact_size(Vec2::new(cw, chh), Sense::hover());
                            continue;
                        };
                        let has = have.contains_key(&d);
                        let (fg, bg, hov) = match (a, b) {
                            (Some(a), Some(b)) if d == a || d == b => (theme::accent_fg(), theme::accent(), theme::accent()),
                            (Some(a), Some(b)) if a < d && d < b => (theme::text_primary(), range_bg, range_hov),
                            _ => (if d == today { theme::accent() } else if has { theme::text_primary() } else { dim }, theme::card_bg(), theme::card_hover()),
                        };
                        let font = if has { theme::font_bold() } else { theme::font_tiny() };
                        if cell(ui, Vec2::new(cw, chh), &d.day().to_string(), fg, bg, hov, font).clicked() {
                            // 1st click = start day, 2nd click = end day (any order); the popup
                            // stays open either way (owner's change 2026-11-27, Python closed it)
                            if lib.date_from.is_some() && lib.date_pending {
                                let s = lib.date_from.unwrap();
                                let (lo, hi) = if s <= d { (s, d) } else { (d, s) };
                                lib.set_date_range(Some(lo), Some(hi), false);
                            } else {
                                lib.set_date_range(Some(d), Some(d), true);
                            }
                            changed = true;
                        }
                    }
                });
            }
            let hint = if lib.date_pending {
                "Now pick the end date".to_owned()
            } else {
                match (lib.date_from, lib.date_to) {
                    (None, _) | (_, None) => "Pick a start date, then an end date".to_owned(),
                    (Some(a), Some(b)) if a == b => "Click another day to make a range".to_owned(),
                    (Some(a), Some(b)) => format!("{} days selected", (b - a).num_days() + 1),
                }
            };
            ui.add_space(3.0);
            ui.vertical_centered(|ui| ui.label(egui::RichText::new(hint).font(theme::font_tiny()).color(dim)));
        }

        // footer: shift a day back/forward, today, clear
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(2.0, 0.0);
            let bw = ((width - 6.0) / 4.0).floor();
            // shift the whole selected range (or today) by `delta` days
            let step = |lib: &mut Library, delta: i64| -> Option<NaiveDate> {
                let a = lib.date_from.unwrap_or(today);
                let b = lib.date_to.unwrap_or(a);
                let td = chrono::Duration::days(delta);
                let (a2, b2) = (a.checked_add_signed(td)?, b.checked_add_signed(td)?);
                lib.set_date_range(Some(a2), Some(b2), false);
                Some(a2)
            };
            let mut stepped = None;
            if cell(ui, Vec2::new(bw, 22.0), "\u{2039} Day", theme::text_muted(), theme::card_bg(), theme::card_hover(), theme::font_tiny()).clicked() {
                stepped = step(lib, -1);
            }
            if cell(ui, Vec2::new(bw, 22.0), "Today", theme::text_muted(), theme::card_bg(), theme::card_hover(), theme::font_tiny()).clicked() {
                lib.set_date_range(Some(today), Some(today), false);
                self.cal_ym = (today.year(), today.month());
                changed = true;
            }
            if cell(ui, Vec2::new(bw, 22.0), "Day \u{203a}", theme::text_muted(), theme::card_bg(), theme::card_hover(), theme::font_tiny()).clicked() {
                stepped = step(lib, 1);
            }
            if let Some(a2) = stepped {
                self.cal_ym = (a2.year(), a2.month());
                changed = true;
            }
            if cell(ui, Vec2::new(bw, 22.0), "Clear", theme::text_muted(), theme::card_bg(), theme::card_hover(), theme::font_tiny()).clicked() {
                lib.set_date_range(None, None, false);
                changed = true;
            }
        });
        ui.add_space(2.0);

        // mouse wheel over the popup = previous/next month
        if ui.ui_contains_pointer() {
            let dy = ui.ctx().input(|i| i.raw_scroll_delta.y);
            if dy != 0.0 {
                self.shift_month(if dy > 0.0 { -1 } else { 1 });
            }
        }
        (close, changed)
    }

    fn grid(&mut self, ui: &mut egui::Ui, lib: &mut Library, prefs: &Prefs, badges: &Badges, acts: &mut Vec<GridAction>) {
        let display_all = lib.filtered(prefs);
        let total = display_all.len();
        if total == 0 {
            if lib.loaded_once || !prefs.checked_folders.is_empty() {
                let r = ui.available_rect_before_wrap();
                let msg = if lib.loaded_once { lib.empty_msg() } else { String::new() };
                ui.painter().text(Pos2::new(r.left() + 10.0, r.top() + 30.0), Align2::LEFT_CENTER, msg, theme::font_ui(), theme::text_muted());
            }
            return;
        }
        let display: Vec<String> = display_all.into_iter().take(lib.visible_count).cloned().collect();
        let n = display.len();
        let ctx = ui.ctx().clone();
        let (pressed1, pressed2, down2) = ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.button_pressed(egui::PointerButton::Secondary), i.pointer.button_down(egui::PointerButton::Secondary)));
        if !down2 {
            self.rb_drag = false;
            self.rb_toggled.clear();
        }
        let mut load_more = false;
        let mut load_all = false;

        // wheel + scrollbar only: egui's drag-to-scroll moved the list while dragging on cards
        // one wheel notch scrolls one whole card (owner 2026-11-28; egui turns a notch into
        // `line_scroll_speed` points)
        let row_h = |w: f32| {
            let cw = (w - 16.0).max(60.0);
            let ch = ((cw - CARD_BORDER * 2.0) * 9.0 / 16.0).floor();
            (cw, ch, ch + 22.0 + PAD * 2.0)
        };
        let line = ctx.options(|o| o.input_options.line_scroll_speed).max(1.0);
        let wheel = row_h(ui.available_width()).2 / line;
        egui::ScrollArea::vertical().auto_shrink([false, false]).scroll_source(egui::scroll_area::ScrollSource::SCROLL_BAR | egui::scroll_area::ScrollSource::MOUSE_WHEEL).wheel_scroll_multiplier(Vec2::new(1.0, wheel)).show_viewport(ui, |ui, vp| {
            let w = ui.available_width();
            let (cw, ch, rh) = row_h(w);
            let extra = if total > n { 76.0 } else { 0.0 };
            let total_h = n as f32 * rh + PAD + extra;
            let (area, _) = ui.allocate_exact_size(Vec2::new(w, total_h), Sense::hover());
            let o = area.min;
            let first = ((vp.min.y / rh).floor().max(0.0)) as usize;
            let last = (((vp.max.y / rh).ceil()) as usize).min(n);
            for idx in first..last {
                let path = &display[idx];
                let x0 = o.x + ((w - cw) / 2.0).floor();
                let y0 = o.y + idx as f32 * rh + PAD;
                let card = Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x0 + cw, y0 + rh - PAD));
                let resp = ui.interact(card, ui.id().with(("card", path)), Sense::click());
                let hovered = resp.contains_pointer();
                self.draw_card(ui, card, ch, path, hovered, lib, prefs, badges);
                if hovered {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    self.hovered = Some(path.clone());
                    if pressed1 {
                        acts.push(GridAction::Play(path.clone()));
                    }
                    if pressed2 {
                        self.rb_drag = true;
                        self.rb_toggled.clear();
                    }
                    if (pressed2 || self.rb_drag) && self.rb_toggled.insert(path.clone()) {
                        if !lib.selected.remove(path) {
                            lib.selected.insert(path.clone());
                        }
                    }
                }
            }
            if total > n {
                let remaining = total - n;
                let btn_y = o.y + n as f32 * rh + PAD + 4.0;
                let mk = |ui: &mut egui::Ui, y: f32, text: String, id: &str| {
                    let r = Rect::from_min_max(Pos2::new(o.x + 8.0, y), Pos2::new(o.x + w - 8.0, y + 30.0));
                    let resp = ui.interact(r, ui.id().with(id), Sense::click());
                    let p = ui.painter();
                    p.rect(r, 0.0, if resp.hovered() { theme::card_hover() } else { theme::sidebar_bg() }, Stroke::new(1.0_f32, theme::text_dim()), egui::StrokeKind::Inside);
                    p.text(r.center(), Align2::CENTER_CENTER, text, theme::font_tiny(), theme::text_muted());
                    resp.clicked()
                };
                load_more = mk(ui, btn_y, format!("Load more  ({} clips)", lib.see_more_step.min(remaining)), "see_more");
                load_all = mk(ui, btn_y + 38.0, format!("Load all  ({remaining} clips)"), "load_all");
            }
        });
        if load_more {
            lib.show_more();
            acts.push(GridAction::ListChanged);
        }
        if load_all {
            let ok = total <= LOAD_ALL_ASK
                || rfd::MessageDialog::new()
                    .set_title("Load all clips")
                    .set_description(format!("Load all {total} clips at once?\n\nThe app may freeze for a while while the list and thumbnails load."))
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    == rfd::MessageDialogResult::Yes;
            if ok {
                lib.show_all(total);
                acts.push(GridAction::ListChanged);
            }
        }
    }

    /// Draws one card.
    #[allow(clippy::too_many_arguments)]
    fn draw_card(&self, ui: &egui::Ui, card: Rect, ch: f32, path: &str, hover: bool, lib: &Library, prefs: &Prefs, badges: &Badges) {
        let p = ui.painter();
        let b = CARD_BORDER;
        let is_active = lib.current.as_deref() == Some(path);
        let is_selected = lib.selected.contains(path);
        let bg = if hover { theme::card_hover() } else { theme::card_bg() };
        let bdr = if is_selected { c("#e0e0e0") } else if is_active { theme::accent() } else { c("#242444") };
        let r = 7.0;
        p.rect_filled(card, r, bdr);
        p.rect_filled(card.shrink(b), (r - b).max(2.0), bg);
        let thumb = Rect::from_min_size(card.min + Vec2::splat(b), Vec2::new(card.width() - 2.0 * b, ch));
        p.rect_filled(thumb, widgets::top_rounded((r - b).max(2.0)), c("#0F0F1A"));
        match self.thumbs.get(path) {
            Some(tex) => {
                // scale to the slot width, centre-crop (or letterbox) the height
                let [sw, sh] = tex.size();
                let new_h = sh as f32 * thumb.width() / sw as f32;
                if new_h >= thumb.height() {
                    let v = (new_h - thumb.height()) / 2.0 / new_h;
                    p.image(tex.id(), thumb, Rect::from_min_max(Pos2::new(0.0, v), Pos2::new(1.0, 1.0 - v)), Color32::WHITE);
                } else {
                    p.rect_filled(thumb, 0.0, Color32::BLACK);
                    let r = Rect::from_center_size(thumb.center(), Vec2::new(thumb.width(), new_h));
                    p.image(tex.id(), r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                }
                // badges: scissors when trimmed, music when the clip has its own audio settings
                let mut bx = thumb.left() + 5.0;
                if prefs.trim_enabled.contains(path) {
                    icons::paint_shadow(ui, Pos2::new(bx, thumb.top() + 5.0), "scissors", 17.0, c("#dddddd"), false, 1.7);
                    bx += 21.0;
                }
                if badges.audio.contains(path) {
                    icons::paint_shadow(ui, Pos2::new(bx, thumb.top() + 5.0), "music", 17.0, c("#dddddd"), false, 1.7);
                }
            }
            None => icons::paint(ui, thumb.center(), "play", 22.0, theme::text_dim(), false, 1.9),
        }
        // owner's change 2026-11-27: no clickable outline heart; a filled heart when liked and a
        // filled star next to it when starred, as badges only (right to left: heart, star)
        let mut bx = thumb.right() - 6.0 - 16.0;
        if prefs.liked.contains(path) {
            icons::paint_shadow(ui, Pos2::new(bx, thumb.top() + 6.0), "heart", 16.0, c("#e8454a"), true, 2.1);
            bx -= 21.0;
        }
        if prefs.starred.contains(path) {
            icons::paint_shadow(ui, Pos2::new(bx, thumb.top() + 6.0), "star", 16.0, c("#e8c84a"), true, 2.1);
        }
        let ly = thumb.bottom() + 11.0;
        let p = ui.painter();
        if let Some(info) = lib.info.get(path) {
            let mut date_x = card.left() + 6.0;
            if !info.ago_str.is_empty() {
                let r = p.text(Pos2::new(date_x, ly), Align2::LEFT_CENTER, &info.ago_str, theme::font_tiny(), theme::text_dim());
                date_x = r.right() + 8.0;
            }
            p.text(Pos2::new(date_x, ly), Align2::LEFT_CENTER, &info.date_str, theme::font_tiny(), theme::text_muted());
            p.text(Pos2::new(card.right() - 6.0, ly), Align2::RIGHT_CENTER, &info.mb_str, theme::font_tiny(), theme::text_dim());
        }
    }
}
