//! Playback bar with trim handles (Python `TrimSlider`) and the slim volume slider
//! (Python `SlimSlider`). Values run 0..10000 for the playback bar, 0..100 for volume.

use crate::theme::{self, c};
use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Sense, Vec2};

const HANDLE_R: f32 = 6.0; // pad = HANDLE_R + 2
const LINE_W: f32 = 5.0; // playhead: a thick rounded pill
const TRIM_HIT: f32 = 12.0; // px hit radius for grabbing a trim handle
const SNAP_PX: f32 = 15.0; // a dragged trim line snaps to the playhead within this
const PH_SNAP_PX: f32 = 10.0; // the dragged playhead snaps to a trim line within this
const KEPT_TINT: f32 = 0.22; // kept section blended this far toward the track colour

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrimEnd {
    Start,
    End,
}

/// What happened on the bar this frame (Python's on_press / on_change / on_release callbacks).
pub enum BarEvent {
    /// Playhead pressed (Python `_seek_start`), then `Drag` with the value.
    Press,
    Drag(f64),
    Release(f64),
    /// A trim handle was grabbed / moved / let go.
    TrimPress,
    TrimMove(f64),
    TrimRelease,
}

/// State of the playback bar (Python `TrimSlider` fields).
pub struct TrimBar {
    pub value: f64,
    pub trim_start: f64,
    pub trim_end: f64,
    /// Handles shown only while trim mode is on for the open clip.
    pub trim_visible: bool,
    dragging: bool,
    trim_drag: Option<TrimEnd>,
    snap_playhead: bool,
}

impl Default for TrimBar {
    fn default() -> Self {
        TrimBar { value: 0.0, trim_start: 0.0, trim_end: 10000.0, trim_visible: false, dragging: false, trim_drag: None, snap_playhead: false }
    }
}

struct Geo {
    rect: Rect,
    pad: f32,
}

impl Geo {
    fn val_to_x(&self, v: f64) -> f32 {
        let w = self.rect.width() - 2.0 * self.pad;
        self.rect.left() + self.pad + (v / 10000.0) as f32 * w
    }
    fn raw_x_to_val(&self, x: f32) -> f64 {
        let w = (self.rect.width() - 2.0 * self.pad).max(1.0);
        (((x - self.rect.left() - self.pad) / w).clamp(0.0, 1.0) * 10000.0) as f64
    }
}

/// Paint the part of a rounded pill `track` between x0 and x1 (keeps the round ends).
fn pill_part(p: &egui::Painter, track: Rect, x0: f32, x1: f32, col: Color32) {
    if x1 <= x0 {
        return;
    }
    let clip = Rect::from_min_max(Pos2::new(x0, track.top()), Pos2::new(x1, track.bottom()));
    p.with_clip_rect(clip.intersect(p.clip_rect())).rect_filled(track, track.height() / 2.0, col);
}

impl TrimBar {
    pub fn set_trim(&mut self, start: f64, end: f64) {
        self.trim_start = start.clamp(0.0, 10000.0);
        self.trim_end = end.clamp(0.0, 10000.0);
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging || self.trim_drag.is_some()
    }

    fn x_to_val(&self, g: &Geo, x: f32) -> f64 {
        let val = g.raw_x_to_val(x);
        if self.snap_playhead && self.trim_visible {
            let vx = g.val_to_x(val);
            let (ds, de) = ((vx - g.val_to_x(self.trim_start)).abs(), (vx - g.val_to_x(self.trim_end)).abs());
            if ds.min(de) <= PH_SNAP_PX {
                return if ds <= de { self.trim_start } else { self.trim_end };
            }
        }
        val
    }

    fn nearest_trim(&self, g: &Geo, x: f32) -> Option<TrimEnd> {
        let ds = (x - g.val_to_x(self.trim_start)).abs();
        let de = (x - g.val_to_x(self.trim_end)).abs();
        if ds <= TRIM_HIT && ds <= de {
            Some(TrimEnd::Start)
        } else if de <= TRIM_HIT {
            Some(TrimEnd::End)
        } else {
            None
        }
    }

    /// A press at `p` would grab a trim handle.
    fn trim_visible_hit(&self, g: &Geo, p: Option<egui::Pos2>) -> bool {
        self.trim_visible && p.is_some_and(|p| self.nearest_trim(g, p.x).is_some())
    }

    fn snap_to_playhead(&self, g: &Geo, raw: f64) -> f64 {
        if (g.val_to_x(raw) - g.val_to_x(self.value)).abs() <= SNAP_PX { self.value } else { raw }
    }

    /// `dur` = clip length in seconds (sets the minimum trim gap).
    pub fn show(&mut self, ui: &mut egui::Ui, width: f32, dur: f64) -> Vec<BarEvent> {
        let h = 28.0;
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, h), Sense::click_and_drag());
        let g = Geo { rect, pad: HANDLE_R + 2.0 };
        let min_gap = if dur > 0.0 { (10000.0 / dur).floor().max(1.0) } else { 100.0 };
        let mut ev = Vec::new();
        let ptr = resp.interact_pointer_pos().or(resp.hover_pos());
        // where the primary button really went down / up this frame: when a frame is slow the
        // pointer has often moved on already, and the playhead must land on the clicked spot
        let button_pos = |pressed: bool| {
            ui.input(|i| {
                i.events.iter().rev().find_map(|e| match e {
                    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: p, .. } if *p == pressed => Some(*pos),
                    _ => None,
                })
            })
        };

        // ---- input ----
        // press and release inside one frame (a quick click on a slow frame): a whole seek
        if resp.clicked() && !self.is_dragging() && !self.trim_visible_hit(&g, button_pos(true).or(ptr)) {
            if let Some(p) = button_pos(false).or(ptr) {
                self.value = self.x_to_val(&g, p.x);
                ev.push(BarEvent::Press);
                ev.push(BarEvent::Drag(self.value));
                ev.push(BarEvent::Release(self.value));
            }
        } else if resp.is_pointer_button_down_on() && !self.is_dragging() {
            // press
            if let Some(p) = button_pos(true).or(ptr) {
                let which = if self.trim_visible { self.nearest_trim(&g, p.x) } else { None };
                if let Some(w) = which {
                    self.trim_drag = Some(w);
                    ev.push(BarEvent::TrimPress);
                } else {
                    self.snap_playhead = true;
                    self.dragging = true;
                    self.value = self.x_to_val(&g, p.x);
                    ev.push(BarEvent::Press);
                    ev.push(BarEvent::Drag(self.value));
                }
            }
        } else if resp.is_pointer_button_down_on() {
            if let Some(p) = ptr {
                match self.trim_drag {
                    Some(TrimEnd::Start) => {
                        let v = self.snap_to_playhead(&g, self.x_to_val(&g, p.x));
                        self.trim_start = v.min(self.trim_end - min_gap).max(0.0);
                        ev.push(BarEvent::TrimMove(self.trim_start));
                    }
                    Some(TrimEnd::End) => {
                        let v = self.snap_to_playhead(&g, self.x_to_val(&g, p.x));
                        self.trim_end = v.min(10000.0).max(self.trim_start + min_gap);
                        ev.push(BarEvent::TrimMove(self.trim_end));
                    }
                    None => {
                        let v = self.x_to_val(&g, p.x);
                        if v != self.value {
                            self.value = v;
                            ev.push(BarEvent::Drag(v));
                        }
                    }
                }
            }
        } else if self.is_dragging() {
            // release
            let was_trim = self.trim_drag.take().is_some();
            if self.dragging {
                self.dragging = false;
                // the release spot, not where the pointer is now
                if let Some(p) = button_pos(false) {
                    self.value = self.x_to_val(&g, p.x);
                }
                ev.push(BarEvent::Release(self.value));
            }
            self.snap_playhead = false;
            if was_trim {
                ev.push(BarEvent::TrimRelease);
            }
        }
        if self.trim_visible && (self.trim_drag.is_some() || resp.hover_pos().is_some_and(|p| self.nearest_trim(&g, p.x).is_some())) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }

        self.paint(ui, &g);
        ev
    }

    fn paint(&self, ui: &egui::Ui, g: &Geo) {
        let p = ui.painter();
        let r = g.rect;
        let (pad, th) = (g.pad, 8.0);
        let cy = r.top() + (r.height() / 2.0).floor();
        let track = Rect::from_min_max(Pos2::new(r.left() + pad, cy - th), Pos2::new(r.right() - pad, cy + th));
        let fill = c("#ffffff");
        p.rect_filled(track, th, theme::bar_track());
        let mut hx = g.val_to_x(self.value);
        if !self.trim_visible {
            pill_part(p, track, track.left(), hx, fill);
        } else {
            let sx = g.val_to_x(self.trim_start);
            let ex = g.val_to_x(self.trim_end);
            // excluded zones: dimmed, following the rounded ends
            let dim = theme::mix(theme::bar_track(), c("#000000"), 0.6);
            if sx > track.left() + 1.0 {
                pill_part(p, track, track.left(), sx, dim);
            }
            if ex < track.right() - 1.0 {
                pill_part(p, track, ex, track.right(), dim);
            }
            // playhead sitting on a trim line is drawn a little inside it
            let (gap, tol) = (6.0, 3.0);
            if ex - sx >= 2.0 * gap + 2.0 {
                if sx - tol <= hx && hx <= sx + gap {
                    hx = sx + gap;
                } else if ex - gap <= hx && hx <= ex + tol {
                    hx = ex - gap;
                }
            }
            // kept part: square ends, from the start line to the playhead
            let kept = theme::mix(fill, theme::bar_track(), KEPT_TINT);
            let fx1 = hx.min(ex).max(sx);
            if fx1 > sx {
                p.rect_filled(Rect::from_min_max(Pos2::new(sx, cy - th), Pos2::new(fx1, cy + th)), 0.0, kept);
            }
            // trim handles: thin white brackets [ ] with flags pointing into the kept part
            let (y0, y1, fl) = (r.top() + 2.0, r.bottom() - 2.0, 5.0);
            let white = c("#ffffff");
            for (bx, d) in [(sx, 1.0_f32), (ex, -1.0)] {
                p.rect_filled(Rect::from_min_max(Pos2::new(bx - 1.0, y0), Pos2::new(bx + 2.0, y1)), 0.0, white);
                let (a, b) = (bx - 1.0, bx + d * fl);
                let (fx0, fx1) = (a.min(b), a.max(b) + 1.0);
                p.rect_filled(Rect::from_min_max(Pos2::new(fx0, y0), Pos2::new(fx1, y0 + 2.0)), 0.0, white);
                p.rect_filled(Rect::from_min_max(Pos2::new(fx0, y1 - 2.0), Pos2::new(fx1, y1)), 0.0, white);
            }
        }
        playhead(p, hx, r.top() + 3.0, r.bottom() - 4.0);
    }
}

/// Round-capped vertical pill (Tk line with capstyle="round").
fn playhead(p: &egui::Painter, x: f32, y0: f32, y1: f32) {
    let hw = LINE_W / 2.0;
    p.rect_filled(Rect::from_min_max(Pos2::new(x - hw, y0 - hw), Pos2::new(x + hw, y1 + hw)), CornerRadius::same(hw.round() as u8), theme::accent());
}

/// Python `SlimSlider` (volume): 22px tall, 12px thick track, white fill, accent handle.
/// Returns the new value while it is pressed/dragged.
pub fn slim_slider(ui: &mut egui::Ui, width: f32, value: f64, max: f64) -> Option<f64> {
    let h = 22.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, h), Sense::click_and_drag());
    let pad = HANDLE_R + 2.0;
    let w = (rect.width() - 2.0 * pad).max(1.0);
    let mut out = None;
    if resp.is_pointer_button_down_on() || resp.drag_stopped() || resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let v = ((p.x - rect.left() - pad) / w).clamp(0.0, 1.0) as f64 * max;
            out = Some(v);
        }
    }
    let v = out.unwrap_or(value);
    let p = ui.painter();
    let th = 6.0;
    let cy = rect.top() + (h / 2.0).floor();
    let track = Rect::from_min_max(Pos2::new(rect.left() + pad, cy - th), Pos2::new(rect.right() - pad, cy + th));
    p.rect_filled(track, th, theme::bar_track());
    let hx = rect.left() + pad + (v / max) as f32 * w;
    pill_part(p, track, track.left(), hx, c("#ffffff"));
    playhead(p, hx, rect.top() + 3.0, rect.bottom() - 3.0);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    out
}
