//! Right sidebar: filter/sort pill and the virtualized clip list (a single "tower" of cards).
//! Ported from FeatherPlayer `_vg_layout`, `_vg_draw_card`, `_vg_refresh` and the mouse handlers.

use super::icons;
use super::widgets::{self, Ghost, Row};
use crate::library::{Filter, Library, Sort, LOAD_ALL_ASK};
use crate::prefs::Prefs;
use crate::theme::{self, c};
use eframe::egui::{self, Align2, Color32, Pos2, Rect, Sense, Stroke, TextureHandle, Vec2};
use std::collections::{HashMap, HashSet};

const CARD_BORDER: f32 = 3.0;
const PAD: f32 = 4.0;

pub enum GridAction {
    Play(String),
    ToggleLike(String),
    /// Sort, filter or "load more" changed which clips are listed: reload thumbnails.
    ListChanged,
}

#[derive(PartialEq, Clone, Copy)]
enum Drop {
    Filter,
    Sort,
}

#[derive(Default)]
pub struct ClipGrid {
    pub thumbs: HashMap<String, TextureHandle>,
    rb_drag: bool,
    rb_toggled: HashSet<String>,
    open: Option<DropState>,
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
        self.header(ui, lib, &mut acts);
        self.grid(ui, lib, prefs, badges, &mut acts);
        acts
    }

    fn header(&mut self, ui: &mut egui::Ui, lib: &mut Library, acts: &mut Vec<GridAction>) {
        let mut filter_rect = Rect::NOTHING;
        let mut sort_rect = Rect::NOTHING;
        egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
            widgets::pill_frame().show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                let col_w = ((ui.available_width() - 1.0) / 2.0).floor();
                ui.horizontal(|ui| {
                    let r = Ghost::new(lib.filter.label()).font(theme::font_tiny()).fg(theme::text_primary()).arrow().width(col_w).show(ui);
                    filter_rect = r.rect;
                    if r.clicked() {
                        self.toggle(Drop::Filter, r.rect);
                    } else if r.hovered() && self.open.is_some_and(|o| o.which == Drop::Sort) {
                        self.open = Some(DropState { which: Drop::Filter, anchor: r.rect });
                    }
                    widgets::pill_divider(ui);
                    let r = Ghost::new(lib.sort.label()).font(theme::font_tiny()).fg(theme::text_primary()).arrow().width(ui.available_width()).show(ui);
                    sort_rect = r.rect;
                    if r.clicked() {
                        self.toggle(Drop::Sort, r.rect);
                    } else if r.hovered() && self.open.is_some_and(|o| o.which == Drop::Filter) {
                        self.open = Some(DropState { which: Drop::Sort, anchor: r.rect });
                    }
                });
            });
        });

        let Some(open) = self.open else { return };
        let ctx = ui.ctx().clone();
        let pos = Pos2::new(open.anchor.left(), open.anchor.bottom() + 3.0);
        let width = open.anchor.width().max(130.0);
        let mut chosen = false;
        let popup = match open.which {
            Drop::Filter => widgets::popup(&ctx, "filter_popup", pos, width, |ui| {
                for f in Filter::ALL {
                    if Row::new(f.label()).font(theme::font_tiny()).height(28.0).active(f == lib.filter).show(ui).clicked() {
                        lib.set_filter(f);
                        chosen = true;
                    }
                }
            }),
            Drop::Sort => widgets::popup(&ctx, "sort_popup", pos, width, |ui| {
                for s in Sort::ALL {
                    if Row::new(s.label()).font(theme::font_tiny()).height(28.0).active(s == lib.sort).show(ui).clicked() {
                        lib.apply_sort(s, true);
                        chosen = true;
                    }
                }
            }),
        };
        if chosen {
            self.open = None;
            acts.push(GridAction::ListChanged);
        } else if ctx.input(|i| i.pointer.any_pressed()) {
            if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                if !popup.contains(p) && !filter_rect.contains(p) && !sort_rect.contains(p) {
                    self.open = None;
                }
            }
        }
    }

    fn toggle(&mut self, which: Drop, anchor: Rect) {
        self.open = match self.open {
            Some(o) if o.which == which => None,
            _ => Some(DropState { which, anchor }),
        };
    }

    fn grid(&mut self, ui: &mut egui::Ui, lib: &mut Library, prefs: &Prefs, badges: &Badges, acts: &mut Vec<GridAction>) {
        let display_all = lib.filtered(prefs);
        let total = display_all.len();
        if total == 0 {
            if lib.loaded_once || !prefs.checked_folders.is_empty() {
                let r = ui.available_rect_before_wrap();
                ui.painter().text(Pos2::new(r.left() + 10.0, r.top() + 30.0), Align2::LEFT_CENTER, if lib.loaded_once { lib.empty_msg() } else { "" }, theme::font_ui(), theme::text_muted());
            }
            return;
        }
        let display: Vec<String> = display_all.into_iter().take(lib.visible_count).cloned().collect();
        let n = display.len();
        let ctx = ui.ctx().clone();
        let (pressed1, pressed2, down2, ptr) = ctx.input(|i| {
            (i.pointer.primary_pressed(), i.pointer.button_pressed(egui::PointerButton::Secondary), i.pointer.button_down(egui::PointerButton::Secondary), i.pointer.interact_pos())
        });
        if !down2 {
            self.rb_drag = false;
            self.rb_toggled.clear();
        }
        let mut load_more = false;
        let mut load_all = false;

        egui::ScrollArea::vertical().auto_shrink([false, false]).show_viewport(ui, |ui, vp| {
            let w = ui.available_width();
            let cw = (w - 16.0).max(60.0);
            let ch = ((cw - CARD_BORDER * 2.0) * 9.0 / 16.0).floor();
            let rh = ch + 22.0 + PAD * 2.0;
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
                let heart = self.draw_card(ui, card, ch, path, hovered, lib, prefs, badges);
                if hovered {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    if pressed1 {
                        if ptr.is_some_and(|p| heart.contains(p)) {
                            acts.push(GridAction::ToggleLike(path.clone()));
                        } else {
                            acts.push(GridAction::Play(path.clone()));
                        }
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

    /// Draws one card; returns the heart's hit rect.
    #[allow(clippy::too_many_arguments)]
    fn draw_card(&self, ui: &egui::Ui, card: Rect, ch: f32, path: &str, hover: bool, lib: &Library, prefs: &Prefs, badges: &Badges) -> Rect {
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
        let liked = prefs.liked.contains(path);
        let heart = icons::paint_shadow(
            ui,
            Pos2::new(thumb.right() - 6.0 - 16.0, thumb.top() + 6.0),
            "heart",
            16.0,
            if liked { c("#e8454a") } else { c("#dddddd") },
            liked,
            2.1,
        );
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
        heart
    }
}
