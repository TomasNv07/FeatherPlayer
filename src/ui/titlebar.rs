//! Themed in-app title bar (Python `_build_titlebar` + `TitleButton`): logo left, centred
//! "Feather Player", min / max / close right, a 2 px accent line under it. Dragging the bar
//! moves the window with Windows' own move loop (Aero snap), a double click maximises.
//! Also the resize strips along the window edges (the native frame is gone).

use crate::theme;
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};

pub const BAR_H: f32 = 37.0;
const LOGO_SIZE: f32 = 22.0;
/// Resize strip thickness / corner size (Python `_RS_T`, `_RS_C`).
const RS_T: f32 = 5.0;
const RS_C: f32 = 10.0;

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum TitleAction {
    Minimize,
    ToggleMaximize,
    Close,
}

#[derive(Default)]
pub struct TitleBar {
    logo: Option<Option<egui::TextureHandle>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Min,
    Max,
    Restore,
    Close,
}

/// Python `TitleButton` (37x29): soft rounded square on hover/press, close turns red.
fn title_button(ui: &mut egui::Ui, kind: Kind) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(37.0, 29.0), Sense::click());
    let bg = theme::bg();
    let pressed = resp.is_pointer_button_down_on();
    let (pill, fg) = if !resp.hovered() && !pressed {
        (None, theme::text_muted())
    } else if kind == Kind::Close {
        (Some(theme::mix(bg, c_raw("#e8454a"), if pressed { 0.38 } else { 0.24 })), c_raw("#ff6b6b"))
    } else {
        (Some(theme::mix(bg, Color32::WHITE, if pressed { 0.15 } else { 0.09 })), Color32::WHITE)
    };
    let p = ui.painter();
    if let Some(pill) = pill {
        p.rect_filled(Rect::from_min_max(rect.min + Vec2::new(3.0, 1.0), rect.max - Vec2::new(3.0, 1.0)), 7.0, pill);
    }
    // glyph centred on the middle pixel, 1 px strokes (Python draws on pixel centres)
    let cx = (rect.left() + 18.0).floor() + 0.5;
    let cy = (rect.top() + 14.0).floor() + 0.5;
    let s = Stroke::new(1.0_f32, fg);
    let r = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x1, y1));
    match kind {
        Kind::Min => {
            p.rect_filled(r(cx - 5.5, cy - 0.5, cx + 5.5, cy + 0.5), 0.0, fg);
        }
        Kind::Max => {
            p.rect_stroke(r(cx - 5.0, cy - 5.0, cx + 5.0, cy + 5.0), 1.5, s, egui::StrokeKind::Middle);
        }
        Kind::Restore => {
            p.rect_stroke(r(cx - 5.0, cy - 2.0, cx + 3.0, cy + 5.0), 1.5, s, egui::StrokeKind::Middle);
            p.rect_filled(r(cx - 2.5, cy - 5.5, cx + 5.5, cy - 4.5), 0.0, fg); // back window: top edge
            p.rect_filled(r(cx + 4.5, cy - 5.5, cx + 5.5, cy + 2.5), 0.0, fg); // back window: right edge
        }
        Kind::Close => {
            let (x, y, k) = (cx, cy, 5.0);
            let s = Stroke::new(1.25_f32, fg);
            p.line_segment([Pos2::new(x - k, y - k), Pos2::new(x + k, y + k)], s);
            p.line_segment([Pos2::new(x - k, y + k), Pos2::new(x + k, y - k)], s);
        }
    }
    resp.clicked()
}

/// `c()` without the theme mapping (Python uses these literals as-is).
fn c_raw(hex: &str) -> Color32 {
    Color32::from_hex(hex).unwrap_or(Color32::WHITE)
}

/// Python `_set_titlebar_logo`: pics/feather.png (or feather2.png), trimmed to its alpha
/// bbox, squared, outline thickened (9x9 max filter), shrunk with premultiplied alpha.
/// No file = the spot stays blank.
fn load_logo(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let path = super::super::tray::pic_path("feather.png").or_else(|| super::super::tray::pic_path("feather2.png"))?;
    let src = image::open(path).ok()?.to_rgba8();
    let (w, h) = src.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (x, y, p) in src.enumerate_pixels() {
        if p[3] > 0 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
        }
    }
    if x1 <= x0 {
        return None;
    }
    let crop = image::imageops::crop_imm(&src, x0, y0, x1 - x0, y1 - y0).to_image();
    let side = crop.width().max(crop.height());
    let mut sq = image::RgbaImage::new(side, side);
    image::imageops::overlay(&mut sq, &crop, ((side - crop.width()) / 2) as i64, ((side - crop.height()) / 2) as i64);
    // thicken: separable 9x9 max filter on alpha
    let n = side as usize;
    let a: Vec<u8> = sq.pixels().map(|p| p[3]).collect();
    let mut tmp = vec![0u8; n * n];
    for y in 0..n {
        for x in 0..n {
            tmp[y * n + x] = (x.saturating_sub(4)..(x + 5).min(n)).map(|k| a[y * n + k]).max().unwrap_or(0);
        }
    }
    for y in 0..n {
        for x in 0..n {
            sq.get_pixel_mut(x as u32, y as u32)[3] = (y.saturating_sub(4)..(y + 5).min(n)).map(|k| tmp[k * n + x]).max().unwrap_or(0);
        }
    }
    // premultiply, shrink, un-premultiply (no dark fringes)
    for p in sq.pixels_mut() {
        let al = p[3] as u32;
        for i in 0..3 {
            p[i] = (p[i] as u32 * al / 255) as u8;
        }
    }
    let px = (LOGO_SIZE * ctx.pixels_per_point()).round().max(1.0) as u32;
    let big = image::imageops::resize(&sq, px * 4, px * 4, image::imageops::FilterType::Lanczos3);
    let mut out = image::imageops::resize(&big, px, px, image::imageops::FilterType::Triangle);
    for p in out.pixels_mut() {
        let al = p[3] as u32;
        if al > 0 {
            for i in 0..3 {
                p[i] = (p[i] as u32 * 255 / al).min(255) as u8;
            }
        }
    }
    let img = egui::ColorImage::from_rgba_unmultiplied([px as usize, px as usize], out.as_raw());
    Some(ctx.load_texture("titlebar_logo", img, egui::TextureOptions::LINEAR))
}

impl TitleBar {
    /// The bar as the first top panel. `maximized` picks the max/restore glyph.
    pub fn show(&mut self, ctx: &egui::Context, maximized: bool) -> Option<TitleAction> {
        let mut act = None;
        let logo = self.logo.get_or_insert_with(|| load_logo(ctx)).clone();
        egui::TopBottomPanel::top("titlebar").exact_height(BAR_H + 2.0).frame(egui::Frame::new().fill(theme::bg())).show_separator_line(false).show(ctx, |ui| {
            let full = ui.max_rect();
            let bar = Rect::from_min_size(full.min, Vec2::new(full.width(), BAR_H));
            // background drag / double click (buttons drawn after take priority)
            let drag = ui.interact(bar, ui.id().with("tb_drag"), Sense::click_and_drag());
            if drag.double_clicked() {
                act = Some(TitleAction::ToggleMaximize);
            } else if drag.drag_started_by(egui::PointerButton::Primary) {
                // Windows' own move loop; it also un-maximises when dragged
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            let p = ui.painter();
            if let Some(t) = &logo {
                let r = Rect::from_center_size(Pos2::new(bar.left() + 12.0 + LOGO_SIZE / 2.0, bar.center().y), Vec2::splat(LOGO_SIZE));
                p.image(t.id(), r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            }
            p.text(bar.center(), egui::Align2::CENTER_CENTER, "Feather Player", theme::font_title(), theme::text_primary());
            // accent line under the bar
            p.rect_filled(Rect::from_min_max(Pos2::new(full.left(), bar.bottom()), Pos2::new(full.right(), bar.bottom() + 2.0)), 0.0, theme::accent());

            let btns = Rect::from_min_max(Pos2::new(bar.right() - 200.0, bar.top()), Pos2::new(bar.right() - 6.0, bar.bottom()));
            ui.scope_builder(egui::UiBuilder::new().max_rect(btns).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(2.0, 0.0);
                if title_button(ui, Kind::Close) {
                    act = Some(TitleAction::Close);
                }
                if title_button(ui, if maximized { Kind::Restore } else { Kind::Max }) {
                    act = Some(TitleAction::ToggleMaximize);
                }
                if title_button(ui, Kind::Min) {
                    act = Some(TitleAction::Minimize);
                }
            });
        });
        act
    }
}

/// Edge strips that start Windows' sizing loop (only while not maximised / fullscreen).
pub fn resize_edges(ctx: &egui::Context) {
    use egui::viewport::ResizeDirection as D;
    use egui::CursorIcon as C;
    let s = ctx.content_rect();
    let (l, t, r, b) = (s.left(), s.top(), s.right(), s.bottom());
    let strips = [
        (Rect::from_min_max(Pos2::new(l, t), Pos2::new(l + RS_C, t + RS_T)), D::NorthWest, C::ResizeNwSe),
        (Rect::from_min_max(Pos2::new(r - RS_C, t), Pos2::new(r, t + RS_T)), D::NorthEast, C::ResizeNeSw),
        (Rect::from_min_max(Pos2::new(l, b - RS_T), Pos2::new(l + RS_C, b)), D::SouthWest, C::ResizeNeSw),
        (Rect::from_min_max(Pos2::new(r - RS_C, b - RS_T), Pos2::new(r, b)), D::SouthEast, C::ResizeNwSe),
        (Rect::from_min_max(Pos2::new(l, t + RS_T), Pos2::new(l + RS_T, b - RS_T)), D::West, C::ResizeHorizontal),
        (Rect::from_min_max(Pos2::new(r - RS_T, t + RS_T), Pos2::new(r, b - RS_T)), D::East, C::ResizeHorizontal),
        (Rect::from_min_max(Pos2::new(l + RS_C, t), Pos2::new(r - RS_C, t + RS_T)), D::North, C::ResizeVertical),
        (Rect::from_min_max(Pos2::new(l + RS_C, b - RS_T), Pos2::new(r - RS_C, b)), D::South, C::ResizeVertical),
    ];
    for (i, (rect, dir, cursor)) in strips.into_iter().enumerate() {
        egui::Area::new(egui::Id::new(("resize_edge", i))).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
            let (_, resp) = ui.allocate_exact_size(rect.size(), Sense::drag());
            if resp.hovered() || resp.dragged() {
                ui.ctx().set_cursor_icon(cursor);
            }
            if resp.drag_started_by(egui::PointerButton::Primary) {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
            }
        });
    }
}

