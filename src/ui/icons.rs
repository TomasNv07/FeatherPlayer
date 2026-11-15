//! Vector icons, ported from FeatherPlayer `_icon_image` (24x24 design grid), rasterized with
//! tiny-skia at the screen's pixel density and cached as egui textures.
//! `paint_shadow` adds the soft dark shadow of `ui_icon_shadow` (for icons over thumbnails).

use eframe::egui::{self, Color32, ColorImage, Pos2, Rect, TextureHandle, TextureOptions, Vec2};
use std::collections::HashMap;
use std::f32::consts::PI;
use std::sync::{Mutex, OnceLock};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    name: &'static str,
    px: u32,
    color: [u8; 4],
    filled: bool,
    stroke: u32,
    shadow: bool,
}

fn cache() -> &'static Mutex<HashMap<Key, TextureHandle>> {
    static C: OnceLock<Mutex<HashMap<Key, TextureHandle>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Draw icon `name` (`size` points square) centred on `center`.
pub fn paint(ui: &egui::Ui, center: Pos2, name: &'static str, size: f32, color: Color32, filled: bool, stroke: f32) {
    let tex = texture(ui.ctx(), name, size, color, filled, stroke, false);
    let r = Rect::from_center_size(center, Vec2::splat(size));
    ui.painter().image(tex.id(), r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
}

/// Shadowed icon. Like Python, the image is `SHADOW_PAD` larger on every side; `glyph_min` is the
/// top-left of the glyph itself.
pub const SHADOW_PAD: f32 = 4.0;
pub fn paint_shadow(ui: &egui::Ui, glyph_min: Pos2, name: &'static str, size: f32, color: Color32, filled: bool, stroke: f32) -> Rect {
    let tex = texture(ui.ctx(), name, size, color, filled, stroke, true);
    let r = Rect::from_min_size(glyph_min - Vec2::splat(SHADOW_PAD), Vec2::splat(size + 2.0 * SHADOW_PAD));
    ui.painter().image(tex.id(), r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
    r
}

fn texture(ctx: &egui::Context, name: &'static str, size: f32, color: Color32, filled: bool, stroke: f32, shadow: bool) -> TextureHandle {
    let ppp = ctx.pixels_per_point();
    let px = (size * ppp).round().max(1.0) as u32;
    let key = Key { name, px, color: color.to_array(), filled, stroke: stroke.to_bits(), shadow };
    let mut c = cache().lock().unwrap();
    if let Some(t) = c.get(&key) {
        return t.clone();
    }
    let img = if shadow {
        let pad = (SHADOW_PAD * ppp).round() as u32;
        with_shadow(&render(name, px, color, filled, stroke), pad, 1.8 * ppp, 1.8, ppp.round().max(1.0) as u32)
    } else {
        render(name, px, color, filled, stroke)
    };
    let img = ColorImage::from_rgba_premultiplied([img.width() as usize, img.height() as usize], img.data());
    let t = ctx.load_texture(format!("icon-{name}"), img, TextureOptions::LINEAR);
    c.insert(key, t.clone());
    t
}

// ---- shape helpers (Python `_rounded_poly`, `_arc_pts`) -------------------------------------

type P = (f32, f32);

/// Dense point list tracing `pts` with every corner rounded by radius r.
fn rounded_poly(pts: &[P], r: f32, closed: bool, steps: usize) -> Vec<P> {
    let n = pts.len();
    let mut out = Vec::new();
    if !closed {
        out.push(pts[0]);
    }
    let idxs: Vec<usize> = if closed { (0..n).collect() } else { (1..n - 1).collect() };
    for i in idxs {
        let (p0, p1, p2) = (pts[(i + n - 1) % n], pts[i], pts[(i + 1) % n]);
        let v1 = (p0.0 - p1.0, p0.1 - p1.1);
        let v2 = (p2.0 - p1.0, p2.1 - p1.1);
        let (l1, l2) = (v1.0.hypot(v1.1), v2.0.hypot(v2.1));
        if l1 == 0.0 || l2 == 0.0 {
            out.push(p1);
            continue;
        }
        let rr = r.min(l1 / 2.0).min(l2 / 2.0);
        let a = (p1.0 + v1.0 / l1 * rr, p1.1 + v1.1 / l1 * rr);
        let b = (p1.0 + v2.0 / l2 * rr, p1.1 + v2.1 / l2 * rr);
        for k in 0..=steps {
            let t = k as f32 / steps as f32;
            let u = 1.0 - t;
            out.push((u * u * a.0 + 2.0 * u * t * p1.0 + t * t * b.0, u * u * a.1 + 2.0 * u * t * p1.1 + t * t * b.1));
        }
    }
    if !closed {
        out.push(pts[n - 1]);
    } else {
        out.push(out[0]);
    }
    out
}

fn rpoly(pts: &[P], r: f32) -> Vec<P> {
    rounded_poly(pts, r, true, 6)
}

fn arc_pts(cx: f32, cy: f32, r: f32, a0: f32, a1: f32, n: usize) -> Vec<P> {
    (0..=n)
        .map(|i| {
            let a = (a0 + (a1 - a0) * i as f32 / n as f32).to_radians();
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

struct Canvas {
    pm: Pixmap,
    paint: Paint<'static>,
    k: f32,
    sw: f32,
}

impl Canvas {
    fn path(&self, pts: &[P]) -> Option<tiny_skia::Path> {
        let mut pb = PathBuilder::new();
        pb.move_to(pts[0].0 * self.k, pts[0].1 * self.k);
        for p in &pts[1..] {
            pb.line_to(p.0 * self.k, p.1 * self.k);
        }
        pb.finish()
    }
    fn dot(&mut self, x: f32, y: f32, rad: f32) {
        if let Some(c) = PathBuilder::from_circle(x, y, rad.max(0.01)) {
            self.pm.fill_path(&c, &self.paint, FillRule::Winding, Transform::identity(), None);
        }
    }
    fn stroke_pts(&mut self, pts: &[P], w: f32) {
        if pts.len() >= 2 {
            if let Some(p) = self.path(pts) {
                let st = Stroke { width: w, line_cap: tiny_skia::LineCap::Round, line_join: tiny_skia::LineJoin::Round, ..Default::default() };
                self.pm.stroke_path(&p, &self.paint, &st, Transform::identity(), None);
            }
        }
    }
    /// Polyline with round caps.
    fn line_w(&mut self, pts: &[P], w: f32) {
        self.stroke_pts(pts, w);
        let k = self.k;
        for (x, y) in [pts[0], pts[pts.len() - 1]] {
            self.dot(x * k, y * k, w / 2.0);
        }
    }
    fn line(&mut self, pts: &[P]) {
        self.line_w(pts, self.sw);
    }
    fn fill(&mut self, pts: &[P]) {
        if let Some(p) = self.path(pts) {
            self.pm.fill_path(&p, &self.paint, FillRule::Winding, Transform::identity(), None);
        }
    }
    fn shape(&mut self, pts: &[P], fill: bool) {
        if fill {
            self.fill(pts);
        }
        self.stroke_pts(pts, self.sw);
        let k = self.k;
        self.dot(pts[0].0 * k, pts[0].1 * k, self.sw / 2.0);
    }
    fn ring(&mut self, cx: f32, cy: f32, r: f32) {
        if let Some(c) = PathBuilder::from_circle(cx * self.k, cy * self.k, r * self.k) {
            let st = Stroke { width: self.sw, ..Default::default() };
            self.pm.stroke_path(&c, &self.paint, &st, Transform::identity(), None);
        }
    }
    fn rrect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) {
        self.fill(&rpoly(&[(x0, y0), (x1, y0), (x1, y1), (x0, y1)], r));
    }
    fn poly_with_edge(&mut self, pts: &[P], w: f32) {
        self.fill(pts);
        self.stroke_pts(pts, w);
    }
}

fn render(name: &str, px: u32, color: Color32, filled: bool, stroke: f32) -> Pixmap {
    let pm = Pixmap::new(px, px).unwrap();
    let mut paint = Paint::default();
    let [r, g, b, a] = color.to_srgba_unmultiplied();
    paint.set_color_rgba8(r, g, b, a);
    paint.anti_alias = true;
    let k = px as f32 / 24.0;
    let mut c = Canvas { pm, paint, k, sw: (stroke * k).max(1.0) };
    match name {
        "scissors" => {
            c.ring(6.0, 6.0, 2.7);
            c.ring(6.0, 18.0, 2.7);
            c.line(&[(20.0, 4.0), (8.4, 15.6)]);
            c.line(&[(14.5, 14.5), (20.0, 20.0)]);
            c.line(&[(8.4, 8.4), (12.0, 12.0)]);
        }
        "plus" => {
            c.line(&[(12.0, 5.0), (12.0, 19.0)]);
            c.line(&[(5.0, 12.0), (19.0, 12.0)]);
        }
        "fullscreen" => {
            c.line(&[(4.0, 9.0), (4.0, 4.0), (9.0, 4.0)]);
            c.line(&[(15.0, 4.0), (20.0, 4.0), (20.0, 9.0)]);
            c.line(&[(20.0, 15.0), (20.0, 20.0), (15.0, 20.0)]);
            c.line(&[(9.0, 20.0), (4.0, 20.0), (4.0, 15.0)]);
        }
        "fullscreen-exit" => {
            c.line(&[(9.0, 4.0), (9.0, 9.0), (4.0, 9.0)]);
            c.line(&[(15.0, 4.0), (15.0, 9.0), (20.0, 9.0)]);
            c.line(&[(20.0, 15.0), (15.0, 15.0), (15.0, 20.0)]);
            c.line(&[(4.0, 15.0), (9.0, 15.0), (9.0, 20.0)]);
        }
        "x" => {
            c.line(&[(6.5, 6.5), (17.5, 17.5)]);
            c.line(&[(17.5, 6.5), (6.5, 17.5)]);
        }
        "check" => c.line(&[(5.0, 12.5), (10.0, 17.5), (19.0, 7.0)]),
        "chevron-down" => c.line(&[(6.0, 9.0), (12.0, 15.0), (18.0, 9.0)]),
        "chevron-left" => c.line(&[(15.0, 5.5), (8.5, 12.0), (15.0, 18.5)]),
        "calendar" => {
            c.shape(&rounded_poly(&[(3.8, 5.6), (20.2, 5.6), (20.2, 20.2), (3.8, 20.2)], 2.4, true, 6), false);
            c.line(&[(3.8, 10.4), (20.2, 10.4)]);
            c.line(&[(8.2, 3.2), (8.2, 7.4)]);
            c.line(&[(15.8, 3.2), (15.8, 7.4)]);
        }
        "arrow-left" => {
            c.line(&[(19.0, 12.0), (5.0, 12.0)]);
            c.line(&[(11.0, 6.0), (5.0, 12.0), (11.0, 18.0)]);
        }
        "folder" | "folder-plus" => {
            c.shape(&rounded_poly(&[(3.0, 6.2), (9.4, 6.2), (11.8, 9.0), (21.0, 9.0), (21.0, 18.8), (3.0, 18.8)], 2.0, true, 6), false);
            if name == "folder-plus" {
                c.line(&[(12.0, 11.8), (12.0, 16.2)]);
                c.line(&[(9.8, 14.0), (14.2, 14.0)]);
            }
        }
        "share" => {
            c.ring(18.0, 5.5, 2.8);
            c.ring(6.0, 12.0, 2.8);
            c.ring(18.0, 18.5, 2.8);
            c.line(&[(8.5, 10.7), (15.5, 6.8)]);
            c.line(&[(8.5, 13.3), (15.5, 17.2)]);
        }
        "compress" => {
            c.line(&[(4.5, 12.0), (19.5, 12.0)]);
            c.line(&[(12.0, 3.5), (12.0, 9.5)]);
            c.line(&[(8.5, 6.5), (12.0, 10.0), (15.5, 6.5)]);
            c.line(&[(12.0, 20.5), (12.0, 14.5)]);
            c.line(&[(8.5, 17.5), (12.0, 14.0), (15.5, 17.5)]);
        }
        "refresh" => {
            let (a0, a1) = (-35.0_f32, 245.0_f32);
            c.line(&arc_pts(12.0, 12.0, 7.8, a0, a1, 64));
            let r1 = a1.to_radians();
            let (ex, ey) = (12.0 + 7.8 * r1.cos(), 12.0 + 7.8 * r1.sin());
            let (tx, ty) = (-r1.sin(), r1.cos());
            for sgn in [1.0_f32, -1.0] {
                let ang = ty.atan2(tx) + PI + sgn * 38.0_f32.to_radians();
                c.line(&[(ex, ey), (ex + 4.4 * ang.cos(), ey + 4.4 * ang.sin())]);
            }
        }
        "gear" => {
            let mut pts = Vec::new();
            for i in 0..8 {
                let a = i as f32 * 45.0;
                for (da, rad) in [(-15.0, 7.3), (-10.0, 9.6), (10.0, 9.6), (15.0, 7.3)] {
                    let t = (a + da as f32).to_radians();
                    pts.push((12.0 + rad * t.cos(), 12.0 + rad * t.sin()));
                }
            }
            c.shape(&rounded_poly(&pts, 0.9, true, 4), false);
            c.ring(12.0, 12.0, 3.1);
        }
        "play" => {
            let p = rpoly(&[(7.6, 4.8), (19.4, 12.0), (7.6, 19.2)], 2.2);
            c.poly_with_edge(&p, 1.1 * k);
        }
        "pause" => {
            c.rrect(6.3, 4.6, 10.4, 19.4, 1.3);
            c.rrect(13.6, 4.6, 17.7, 19.4, 1.3);
        }
        "skip-prev" => {
            c.rrect(5.0, 5.0, 7.8, 19.0, 0.9);
            c.poly_with_edge(&rpoly(&[(19.6, 5.2), (9.4, 12.0), (19.6, 18.8)], 1.6), 0.9 * k);
        }
        "skip-next" => {
            c.rrect(16.2, 5.0, 19.0, 19.0, 0.9);
            c.poly_with_edge(&rpoly(&[(4.4, 5.2), (14.6, 12.0), (4.4, 18.8)], 1.6), 0.9 * k);
        }
        "volume" | "mute" => {
            let p = rpoly(&[(3.8, 9.6), (7.8, 9.6), (12.6, 5.4), (12.6, 18.6), (7.8, 14.4), (3.8, 14.4)], 1.0);
            c.poly_with_edge(&p, 0.8 * k);
            if name == "volume" {
                c.line(&arc_pts(12.6, 12.0, 4.6, -48.0, 48.0, 24));
                c.line(&arc_pts(12.6, 12.0, 8.4, -48.0, 48.0, 32));
            } else {
                c.line(&[(16.4, 9.4), (21.2, 14.6)]);
                c.line(&[(21.2, 9.4), (16.4, 14.6)]);
            }
        }
        "trash" => {
            c.line(&[(4.5, 7.0), (19.5, 7.0)]);
            c.line(&[(9.5, 7.0), (9.5, 4.6), (14.5, 4.6), (14.5, 7.0)]);
            c.shape(&rounded_poly(&[(6.4, 7.6), (7.4, 19.6), (16.6, 19.6), (17.6, 7.6)], 1.2, true, 6), false);
            c.line(&[(10.2, 11.0), (10.2, 16.0)]);
            c.line(&[(13.8, 11.0), (13.8, 16.0)]);
        }
        "alert" => {
            c.shape(&rounded_poly(&[(12.0, 3.8), (21.2, 19.8), (2.8, 19.8)], 1.8, true, 6), false);
            c.line(&[(12.0, 9.8), (12.0, 14.2)]);
            c.line(&[(12.0, 17.0), (12.0, 17.0)]);
        }
        "star" => {
            let pts: Vec<P> = (0..10)
                .map(|i| {
                    let rad = if i % 2 == 0 { 10.2 } else { 4.5 };
                    let t = (-90.0 + i as f32 * 36.0).to_radians();
                    (12.0 + rad * t.cos(), 12.8 + rad * t.sin())
                })
                .collect();
            c.shape(&rounded_poly(&pts, 1.1, true, 4), filled);
        }
        "heart" => {
            let raw: Vec<P> = (0..160)
                .map(|i| {
                    let t = 2.0 * PI * i as f32 / 160.0;
                    (16.0 * t.sin().powi(3), -(13.0 * t.cos() - 5.0 * (2.0 * t).cos() - 2.0 * (3.0 * t).cos() - (4.0 * t).cos()))
                })
                .collect();
            let (mut x0, mut x1, mut y0, mut y1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
            for &(x, y) in &raw {
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
            let sc = (17.6 / (x1 - x0)).min(16.4 / (y1 - y0));
            let (cx, cy) = ((x1 + x0) / 2.0, (y1 + y0) / 2.0);
            let mut pts: Vec<P> = raw.iter().map(|&(x, y)| (12.0 + (x - cx) * sc, 12.4 + (y - cy) * sc)).collect();
            pts.push(pts[0]);
            c.shape(&pts, filled);
        }
        "music" => {
            let hr = 2.9;
            for (hx, hy) in [(7.0, 18.2), (17.0, 16.2)] {
                c.dot(hx * k, hy * k, hr * k);
            }
            let w = c.sw.max(1.9 * k);
            c.line_w(&[(9.5, 17.4), (9.5, 6.4)], w);
            c.line_w(&[(19.5, 15.4), (19.5, 4.4)], w);
            c.fill(&[(9.5, 6.0), (19.5, 4.0), (19.5, 7.6), (9.5, 9.6)]);
        }
        _ => {}
    }
    c.pm
}

/// Python `ui_icon_shadow`: glyph on a canvas `pad` larger per side, over its own alpha moved
/// 1px down, gaussian-blurred and boosted by `gain`, in black.
fn with_shadow(glyph: &Pixmap, pad: u32, sigma: f32, gain: f32, down: u32) -> Pixmap {
    let n = glyph.width() + 2 * pad;
    let mut alpha = image::GrayImage::new(n, n);
    for y in 0..glyph.height() {
        for x in 0..glyph.width() {
            let a = glyph.pixel(x, y).map(|p| p.alpha()).unwrap_or(0);
            let (tx, ty) = (x + pad, y + pad + down);
            if ty < n {
                alpha.put_pixel(tx, ty, image::Luma([a]));
            }
        }
    }
    let blurred = image::imageops::blur(&alpha, sigma);
    let mut out = Pixmap::new(n, n).unwrap();
    let data = out.data_mut();
    for y in 0..n {
        for x in 0..n {
            let sa = (blurred.get_pixel(x, y)[0] as f32 * gain).min(255.0) / 255.0;
            let g = if x >= pad && y >= pad && x - pad < glyph.width() && y - pad < glyph.height() {
                glyph.pixel(x - pad, y - pad).map(|p| [p.red(), p.green(), p.blue(), p.alpha()]).unwrap_or([0; 4])
            } else {
                [0; 4]
            };
            let ga = g[3] as f32 / 255.0;
            // premultiplied "glyph over black shadow"
            let a = ga + sa * (1.0 - ga);
            let i = ((y * n + x) * 4) as usize;
            data[i] = g[0];
            data[i + 1] = g[1];
            data[i + 2] = g[2];
            data[i + 3] = (a * 255.0).round() as u8;
        }
    }
    out
}
