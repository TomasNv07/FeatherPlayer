//! Custom-drawn widgets.

use crate::theme::{ACCENT, BUTTON_BG, BUTTON_HOVER, LINE};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};

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
    let (active, inactive) = if enabled { (ACCENT, LINE) } else { (LINE, LINE) };
    let track = Rect::from_min_max(Pos2::new(rect.left() + pad, cy - 2.0), Pos2::new(rect.right() - pad, cy + 2.0));
    p.rect_filled(track, 2.0, inactive);
    p.rect_filled(Rect::from_min_max(track.min, Pos2::new(x.max(track.min.x), track.max.y)), 2.0, active);
    let r = if down || resp.hovered() { 8.0 } else { 7.0 };
    if enabled {
        p.circle_filled(Pos2::new(x, cy), r, ACCENT);
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    commit
}

/// Round play/pause button.
pub fn play_button(ui: &mut egui::Ui, playing: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::click());
    let p = ui.painter_at(rect);
    p.circle_filled(rect.center(), 17.0, if resp.hovered() { BUTTON_HOVER } else { BUTTON_BG });
    let c = rect.center();
    if playing {
        for dx in [-4.5_f32, 4.5] {
            p.rect_filled(Rect::from_center_size(Pos2::new(c.x + dx, c.y), Vec2::new(4.0, 13.0)), 1.5, Color32::WHITE);
        }
    } else {
        let pts = vec![Pos2::new(c.x - 4.0, c.y - 8.0), Pos2::new(c.x - 4.0, c.y + 8.0), Pos2::new(c.x + 8.0, c.y)];
        p.add(egui::Shape::convex_polygon(pts, Color32::WHITE, Stroke::NONE));
    }
    resp
}
