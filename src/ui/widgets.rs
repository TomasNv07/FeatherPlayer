//! Custom-drawn widgets, ported from FeatherPlayer (Pill, GhostButton, RoundedPopup, PopupRow)
//! plus the player's seek bar and play button.

use super::icons;
use crate::theme::{self, c, CTRL_H, RADIUS};
use eframe::egui::{self, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, Vec2};

// ---------------------------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------------------------

pub fn text_width(ui: &egui::Ui, text: &str, font: &FontId) -> f32 {
    ui.painter().layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE).size().x
}

/// Python `_ellipsize`: cut `text` with "…" so it fits `maxw`. `keep_start` keeps the start
/// (keep="end" in Python), otherwise the tail is kept (better for file paths).
pub fn ellipsize(ui: &egui::Ui, text: &str, font: &FontId, maxw: f32, keep_start: bool) -> String {
    if maxw <= 0.0 || text_width(ui, text, font) <= maxw {
        return text.to_owned();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while chars.len() > 1 {
        if keep_start {
            chars.pop();
        } else {
            chars.remove(0);
        }
        let s: String = chars.iter().collect();
        let cand = if keep_start { format!("{s}\u{2026}") } else { format!("\u{2026}{s}") };
        if text_width(ui, &cand, font) <= maxw {
            return cand;
        }
    }
    "\u{2026}".into()
}

// ---------------------------------------------------------------------------------------------
// Pill: rounded group of controls (CARD_BG fill, 1px BORDER outline, radius 8, pad 2)
// ---------------------------------------------------------------------------------------------

pub fn pill_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::card_bg())
        .stroke(Stroke::new(1.0_f32, theme::border()))
        .corner_radius(RADIUS as u8)
        .inner_margin(egui::Margin::same(2))
}

/// Thin vertical divider inside a pill (7px from top and bottom).
pub fn pill_divider(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, CTRL_H - 4.0), Sense::hover());
    ui.painter().rect_filled(Rect::from_min_max(Pos2::new(rect.left(), rect.top() + 7.0), Pos2::new(rect.right(), rect.bottom() - 7.0)), 0.0, theme::divider());
}

// ---------------------------------------------------------------------------------------------
// GhostButton: flat, transparent until hovered/pressed
// ---------------------------------------------------------------------------------------------

pub struct Ghost<'a> {
    pub text: &'a str,
    pub icon: Option<&'static str>,
    pub icon_size: f32,
    pub font: FontId,
    pub fg: Color32,
    pub padx: f32,
    pub arrow: bool,
    pub accent_icon: bool,
    /// Fixed width (0 = from content).
    pub width: f32,
}

impl<'a> Ghost<'a> {
    pub fn new(text: &'a str) -> Self {
        Ghost { text, icon: None, icon_size: 18.0, font: theme::font_ui(), fg: theme::text_muted(), padx: 12.0, arrow: false, accent_icon: false, width: 0.0 }
    }
    pub fn icon(mut self, name: &'static str, size: f32) -> Self {
        self.icon = Some(name);
        self.icon_size = size;
        self
    }
    pub fn font(mut self, f: FontId) -> Self {
        self.font = f;
        self
    }
    pub fn fg(mut self, c: Color32) -> Self {
        self.fg = c;
        self
    }
    pub fn padx(mut self, p: f32) -> Self {
        self.padx = p;
        self
    }
    pub fn arrow(mut self) -> Self {
        self.arrow = true;
        self
    }
    pub fn accent_icon(mut self) -> Self {
        self.accent_icon = true;
        self
    }
    pub fn width(mut self, w: f32) -> Self {
        self.width = w;
        self
    }

    pub fn show(self, ui: &mut egui::Ui) -> egui::Response {
        let h = CTRL_H - 4.0;
        let tw = if self.text.is_empty() { 0.0 } else { text_width(ui, self.text, &self.font) };
        let base = self.icon.map_or(0.0, |_| self.icon_size) + tw + if self.icon.is_some() && !self.text.is_empty() { 8.0 } else { 0.0 };
        let w = if self.width > 0.0 { self.width } else { base + self.padx * 2.0 + if self.arrow { 14.0 } else { 0.0 } };
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
        let state = if resp.is_pointer_button_down_on() { 2 } else if resp.hovered() { 1 } else { 0 };
        let hover_bg = theme::card_hover();
        let press_bg = c("#12121e");
        let p = ui.painter();
        if state != 0 {
            p.rect_filled(rect, (RADIUS - 2.0).max(4.0), if state == 2 { press_bg } else { hover_bg });
        }
        let col = if state == 0 { self.fg } else { Color32::WHITE };
        let icol = if self.accent_icon { theme::accent() } else { col };
        let cy = rect.center().y;
        if let (Some(icon), false) = (self.icon, self.text.is_empty()) {
            let x0 = rect.left() + (w - (self.icon_size + 8.0 + tw)) / 2.0;
            icons::paint(ui, Pos2::new(x0 + self.icon_size / 2.0, cy), icon, self.icon_size, icol, false, 1.9);
            ui.painter().text(Pos2::new(x0 + self.icon_size + 8.0, cy), egui::Align2::LEFT_CENTER, self.text, self.font, col);
        } else if let Some(icon) = self.icon {
            icons::paint(ui, rect.center(), icon, self.icon_size, icol, false, 1.9);
        } else if self.arrow {
            let (chip_w, chip_m) = (18.0, 3.0);
            let (cx0, cx1) = (rect.right() - chip_m - chip_w, rect.right() - chip_m);
            let inset = ((h - 20.0) / 2.0).max(4.0);
            if state != 0 {
                p.rect_filled(Rect::from_min_max(Pos2::new(cx0, rect.top() + inset), Pos2::new(cx1, rect.bottom() - inset)), 6.0, if state == 2 { press_bg } else { hover_bg });
            }
            let label = ellipsize(ui, self.text, &self.font, cx0 - rect.left() - 4.0 - 10.0, true);
            ui.painter().text(Pos2::new(rect.left() + 10.0, cy), egui::Align2::LEFT_CENTER, label, self.font, col);
            icons::paint(ui, Pos2::new((cx0 + cx1) / 2.0, cy), "chevron-down", 12.0, col, false, 2.3);
        } else {
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, self.text, self.font, col);
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        resp
    }
}

// ---------------------------------------------------------------------------------------------
// RoundedPopup + PopupRow
// ---------------------------------------------------------------------------------------------

pub fn popup_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::card_bg())
        .stroke(Stroke::new(1.0_f32, c("#33335a")))
        .corner_radius((RADIUS + 2.0) as u8)
        .inner_margin(egui::Margin::same(5))
}

/// Show a dropdown panel at `pos` (top-left) with a fixed `width`. Returns the panel rect.
pub fn popup(ctx: &egui::Context, id: &str, pos: Pos2, width: f32, add: impl FnOnce(&mut egui::Ui)) -> Rect {
    egui::Area::new(egui::Id::new(id))
        .fixed_pos(pos)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            popup_frame().show(ui, |ui| {
                ui.set_width(width - 10.0);
                ui.spacing_mut().item_spacing = Vec2::new(0.0, 2.0);
                add(ui);
            });
        })
        .response
        .rect
}

pub struct Row<'a> {
    pub text: &'a str,
    pub font: FontId,
    pub height: f32,
    pub active: bool,
    pub check: Option<bool>,
    pub subtitle: &'a str,
    pub icon: Option<&'static str>,
    /// Dim text at the right (e.g. a clip count).
    pub right_text: String,
}

impl<'a> Row<'a> {
    pub fn new(text: &'a str) -> Self {
        Row { text, font: theme::font_ui(), height: 30.0, active: false, check: None, subtitle: "", icon: None, right_text: String::new() }
    }
    pub fn font(mut self, f: FontId) -> Self {
        self.font = f;
        self
    }
    pub fn height(mut self, h: f32) -> Self {
        self.height = h;
        self
    }
    pub fn active(mut self, a: bool) -> Self {
        self.active = a;
        self
    }
    pub fn check(mut self, c: bool) -> Self {
        self.check = Some(c);
        self
    }
    pub fn subtitle(mut self, s: &'a str) -> Self {
        self.subtitle = s;
        self
    }
    pub fn right_text(mut self, t: String) -> Self {
        self.right_text = t;
        self
    }
    pub fn icon(mut self, i: &'static str) -> Self {
        self.icon = Some(i);
        self
    }

    pub fn show(self, ui: &mut egui::Ui) -> egui::Response {
        let w = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, self.height), Sense::click());
        let hover = resp.hovered();
        let p = ui.painter();
        let hl = rect.shrink(1.0);
        if hover {
            p.rect_filled(hl, 7.0, c("#2c2c50"));
        } else if self.active {
            p.rect_filled(hl, 7.0, c("#2A2A4A"));
        }
        let checked = self.check == Some(true);
        let fg = if hover || checked || (self.check.is_none() && self.active) { Color32::WHITE } else { theme::text_muted() };
        let cy = rect.center().y;
        let mut x = rect.left() + 12.0;
        if let Some(on) = self.check {
            let b = Rect::from_min_max(Pos2::new(x, cy - 8.0), Pos2::new(x + 16.0, cy + 8.0));
            if on {
                p.rect_filled(b, 5.0, theme::accent());
                icons::paint(ui, b.center(), "check", 12.0, theme::accent_fg(), false, 2.6);
            } else {
                ui.painter().rect(b, 5.0, c("#242444"), Stroke::new(1.0_f32, c("#52527e")), egui::StrokeKind::Inside);
            }
            x += 26.0;
        } else if let Some(icon) = self.icon {
            icons::paint(ui, Pos2::new(x + 9.0, cy), icon, 18.0, theme::accent(), false, 1.9);
            x += 28.0;
        }
        let mut right_w = 12.0;
        if !self.right_text.is_empty() {
            ui.painter().text(Pos2::new(rect.right() - 12.0, cy), egui::Align2::RIGHT_CENTER, &self.right_text, theme::font_tiny(), theme::text_dim());
            right_w = 12.0 + self.right_text.chars().count() as f32 * 6.0 + 8.0;
        } else if self.active && self.check.is_none() && self.icon.is_none() {
            icons::paint(ui, Pos2::new(rect.right() - 16.0, cy), "check", 12.0, theme::accent(), false, 2.4);
            right_w = 32.0;
        }
        let room = (rect.right() - x - right_w).max(20.0);
        let p = ui.painter();
        if !self.subtitle.is_empty() {
            p.text(Pos2::new(x, cy - 7.0), egui::Align2::LEFT_CENTER, ellipsize(ui, self.text, &self.font, room, true), self.font.clone(), fg);
            p.text(Pos2::new(x, cy + 8.0), egui::Align2::LEFT_CENTER, ellipsize(ui, self.subtitle, &theme::font_tiny(), room, false), theme::font_tiny(), theme::text_dim());
        } else {
            p.text(Pos2::new(x, cy), egui::Align2::LEFT_CENTER, ellipsize(ui, self.text, &self.font, room, true), self.font, fg);
        }
        if hover {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        resp
    }
}

// ---------------------------------------------------------------------------------------------
// Player widgets (from the proof of concept; the full player controls are ported later)
// ---------------------------------------------------------------------------------------------

/// Seek bar that jumps to exactly where you press (also right next to the current position)
/// and follows the finger/mouse while dragging. Returns the final fraction on release.
pub fn seek_bar(ui: &mut egui::Ui, width: f32, value: f32, enabled: bool, scrub: &mut Option<f32>) -> Option<f32> {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::click_and_drag());
    let pad = 9.0;
    let span = (rect.width() - 2.0 * pad).max(1.0);
    let frac_at = |x: f32| ((x - rect.left() - pad) / span).clamp(0.0, 1.0);

    let down = enabled && resp.is_pointer_button_down_on();
    let released = enabled && (resp.clicked() || resp.drag_stopped());
    if down || released {
        if let Some(p) = resp.interact_pointer_pos() {
            *scrub = Some(frac_at(p.x));
        }
    }
    let mut commit = None;
    if !down && (released || scrub.is_some()) {
        commit = scrub.take();
    }

    let shown = scrub.unwrap_or(value);
    let cy = rect.center().y;
    let x = rect.left() + pad + span * shown;
    let p = ui.painter_at(rect);
    let fill = if enabled { Color32::WHITE } else { theme::bar_track() };
    let track = Rect::from_min_max(Pos2::new(rect.left() + pad, cy - 2.0), Pos2::new(rect.right() - pad, cy + 2.0));
    p.rect_filled(track, 2.0, theme::bar_track());
    p.rect_filled(Rect::from_min_max(track.min, Pos2::new(x.max(track.min.x), track.max.y)), 2.0, fill);
    if enabled {
        let r = if down || resp.hovered() { 8.0 } else { 7.0 };
        p.circle_filled(Pos2::new(x, cy), r, theme::accent());
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    commit
}

/// Round play/pause button (Python `_play_pause_btn`, 44px).
pub fn play_button(ui: &mut egui::Ui, playing: bool) -> egui::Response {
    let size = 44.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let col = if resp.is_pointer_button_down_on() {
        c("#333333")
    } else if resp.hovered() {
        c("#2A2A4A")
    } else {
        c("#242444")
    };
    let p = ui.painter();
    p.circle(rect.center(), size / 2.0 - 2.0, col, Stroke::new(1.0_f32, c("#45456e")));
    icons::paint(ui, rect.center(), if playing { "pause" } else { "play" }, 20.0, Color32::WHITE, false, 1.9);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Python `RoundedButton` (radius 9, 1px border a bit lighter than the fill).
pub fn rounded_button(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Color32, hover_bg: Color32, font: FontId, padx: f32, pady: f32) -> egui::Response {
    let tw = text_width(ui, text, &font);
    let h = (font.size * 1.33 + pady * 2.0).max(22.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(tw + padx * 2.0, h), Sense::click());
    let fill = if resp.hovered() { hover_bg } else { bg };
    let lighter = |c: Color32| Color32::from_rgb(c.r().saturating_add(28), c.g().saturating_add(28), c.b().saturating_add(28));
    ui.painter().rect(rect, 9.0, fill, Stroke::new(1.0_f32, lighter(fill)), egui::StrokeKind::Inside);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, font, fg);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Single-line text field in the dialog style (#242444 fill, flat).
pub fn entry(ui: &mut egui::Ui, text: &mut String, width: f32) -> egui::Response {
    egui::Frame::new()
        .fill(c("#242444"))
        .inner_margin(egui::Margin::symmetric(6, 6))
        .show(ui, |ui| {
            ui.add(egui::TextEdit::singleline(text).font(theme::font_ui()).text_color(theme::text_primary()).frame(false).desired_width(width - 12.0))
        })
        .inner
}

/// Top-rounded rectangle (thumbnail area of a clip card).
pub fn top_rounded(r: f32) -> CornerRadius {
    let r = r as u8;
    CornerRadius { nw: r, ne: r, sw: 0, se: 0 }
}
