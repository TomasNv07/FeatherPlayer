//! FeatherPlayer-rs : proof of concept.
//!
//! Checks the three things that decide whether a Rust port of FeatherPlayer is practical:
//!   1. mpv draws INSIDE our own window (libmpv OpenGL render API -> egui paint callback)
//!   2. egui widgets (menus, buttons) can be drawn ON TOP of the video
//!   3. FeatherPlayer's multi-audio-track mixing (lavfi-complex / amix) works the same way
//! plus a seek bar that jumps exactly where you click.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod mpv;

use eframe::egui::{self, Align2, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use eframe::{egui_glow, glow};
use glow::HasContext;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ACCENT: Color32 = Color32::from_rgb(0x38, 0xd5, 0xff);
const BAR_BG: Color32 = Color32::from_rgb(0x12, 0x14, 0x1c);
const LINE: Color32 = Color32::from_rgb(0x3a, 0x3f, 0x52);
const MUTED: Color32 = Color32::from_rgb(0x9a, 0xa1, 0xb5);

// ---------------------------------------------------------------------------------------------
// GL side: mpv renders into an off-screen texture, which is blitted into the video rectangle.
// (mpv can only render at the origin of a framebuffer, so it cannot draw straight into a
//  sub-rectangle of the window.)
// ---------------------------------------------------------------------------------------------
#[derive(Default)]
struct VideoGl {
    fbo: Option<glow::Framebuffer>,
    tex: Option<glow::Texture>,
    w: i32,
    h: i32,
}

impl VideoGl {
    fn ensure(&mut self, gl: &glow::Context, w: i32, h: i32) {
        unsafe {
            if self.fbo.is_none() {
                self.fbo = gl.create_framebuffer().ok();
                self.tex = gl.create_texture().ok();
                self.w = 0;
                self.h = 0;
            }
            if (self.w, self.h) != (w, h) {
                gl.bind_texture(glow::TEXTURE_2D, self.tex);
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA8 as i32,
                    w,
                    h,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(None),
                );
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::NEAREST as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::NEAREST as i32);
                gl.bind_framebuffer(glow::FRAMEBUFFER, self.fbo);
                gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, self.tex, 0);
                gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                gl.bind_texture(glow::TEXTURE_2D, None);
                self.w = w;
                self.h = h;
            }
        }
    }

    fn destroy(&mut self, gl: &glow::Context) {
        unsafe {
            if let Some(f) = self.fbo.take() {
                gl.delete_framebuffer(f);
            }
            if let Some(t) = self.tex.take() {
                gl.delete_texture(t);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Player state
// ---------------------------------------------------------------------------------------------
struct Track {
    id: i64,
    label: String,
}

struct Player {
    core: Arc<mpv::Core>,
    vgl: Arc<Mutex<VideoGl>>,
    path: Option<String>,
    loaded: bool,
    dur: f64,
    pos: f64,
    paused: bool,
    eof: bool,
    volume: f64,
    speed: f64,
    scrub: Option<f32>,
    hold: Option<(Instant, f64)>,
    tracks: Vec<Track>,
    sel: BTreeSet<usize>,
    audio_menu: bool,
    audio_status: String,
    lavfi_cur: String,
    msg: String,
}

/// Same rules as FeatherPlayer._at_build_graph: one untouched track is picked directly (aid),
/// several are mixed with amix (which divides the level by N, so volume=N puts it back).
fn build_graph(items: &[(i64, f64)]) -> (Option<i64>, String) {
    if items.len() == 1 && (items[0].1 - 1.0).abs() < 0.005 {
        return (Some(items[0].0), String::new());
    }
    if items.len() == 1 {
        return (None, format!("[aid{}]volume={:.3}[ao]", items[0].0, items[0].1));
    }
    let mut parts = Vec::new();
    let mut labels = String::new();
    for (k, (id, v)) in items.iter().enumerate() {
        if (v - 1.0).abs() < 0.005 {
            labels.push_str(&format!("[aid{id}]"));
        } else {
            parts.push(format!("[aid{id}]volume={v:.3}[v{k}]"));
            labels.push_str(&format!("[v{k}]"));
        }
    }
    let n = items.len();
    parts.push(format!("{labels}amix=inputs={n}:duration=longest,volume={n}[ao]"));
    (None, parts.join(";"))
}

fn fmt_time(s: f64) -> String {
    let s = s.max(0.0) as i64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

impl Player {
    fn open(&mut self, path: &str) {
        let _ = self.core.set("lavfi-complex", "");
        self.lavfi_cur.clear();
        match self.core.cmd(&["loadfile", path]) {
            Ok(()) => {
                self.path = Some(path.to_owned());
                self.loaded = false;
                self.tracks.clear();
                self.sel.clear();
                self.dur = 0.0;
                self.pos = 0.0;
                self.eof = false;
                self.hold = None;
                self.audio_status.clear();
                self.msg.clear();
            }
            Err(e) => self.msg = e,
        }
    }

    fn read_tracks(&mut self) {
        self.tracks.clear();
        let count: usize = self.core.get("track-list/count").and_then(|s| s.parse().ok()).unwrap_or(0);
        let mut n = 0;
        for i in 0..count {
            if self.core.get(&format!("track-list/{i}/type")).as_deref() != Some("audio") {
                continue;
            }
            let id: i64 = self.core.get(&format!("track-list/{i}/id")).and_then(|s| s.parse().ok()).unwrap_or(0);
            let title = self.core.get(&format!("track-list/{i}/title")).unwrap_or_default();
            let lang = self.core.get(&format!("track-list/{i}/lang")).unwrap_or_default();
            let codec = self.core.get(&format!("track-list/{i}/codec")).unwrap_or_default();
            n += 1;
            let detail = [title, lang, codec].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" \u{b7} ");
            self.tracks.push(Track { id, label: format!("Track {n}  {detail}") });
        }
        self.sel.clear();
        if !self.tracks.is_empty() {
            self.sel.insert(0);
            self.audio_status = format!("aid {} (mpv default)", self.tracks[0].id);
        }
    }

    /// Make mpv play exactly the ticked tracks (keeps the playback position, like _at_apply_keep_pos).
    fn apply_audio(&mut self) {
        let items: Vec<(i64, f64)> = self.sel.iter().filter_map(|&n| self.tracks.get(n)).map(|t| (t.id, 1.0)).collect();
        if items.is_empty() {
            return;
        }
        let pos = self.core.getf("time-pos");
        let (aid, graph) = build_graph(&items);
        if graph != self.lavfi_cur {
            if let Err(e) = self.core.set("lavfi-complex", &graph) {
                self.msg = e;
            }
            self.lavfi_cur = graph.clone();
        }
        if let Some(a) = aid {
            let _ = self.core.set("aid", &a.to_string());
            self.audio_status = format!("aid {a}");
        } else {
            self.audio_status = graph;
        }
        if let Some(p) = pos {
            let _ = self.core.cmd(&["seek", &format!("{p}"), "absolute+exact"]);
            self.hold = Some((Instant::now() + Duration::from_millis(400), p));
        }
    }

    fn toggle_play(&mut self) {
        if self.eof {
            let _ = self.core.cmd(&["seek", "0", "absolute+exact"]);
            let _ = self.core.set("pause", "no");
            self.eof = false;
        } else {
            let _ = self.core.set("pause", if self.paused { "no" } else { "yes" });
        }
    }

    fn seek_to(&mut self, secs: f64) {
        let t = secs.clamp(0.0, self.dur.max(0.0));
        let _ = self.core.cmd(&["seek", &format!("{t}"), "absolute+exact"]);
        self.pos = t;
        self.hold = Some((Instant::now() + Duration::from_millis(400), t));
        self.eof = false;
    }

    fn poll(&mut self) {
        loop {
            match self.core.poll_event() {
                mpv::EV_NONE => break,
                mpv::EV_FILE_LOADED => {
                    self.loaded = true;
                    self.read_tracks();
                }
                _ => {}
            }
        }
        if self.loaded {
            self.dur = self.core.getf("duration").unwrap_or(self.dur);
            if let Some(p) = self.core.getf("time-pos") {
                self.pos = p;
            }
            self.paused = self.core.flag("pause");
            self.eof = self.core.flag("eof-reached");
            if let Some((until, t)) = self.hold {
                if Instant::now() < until {
                    self.pos = t;
                } else {
                    self.hold = None;
                }
            }
        }
    }

    fn video_callback(&self, rect: Rect) -> egui::PaintCallback {
        let core = self.core.clone();
        let vgl = self.vgl.clone();
        egui::PaintCallback {
            rect,
            callback: Arc::new(egui_glow::CallbackFn::new(move |info, painter| {
                let vp = info.viewport_in_pixels();
                let (w, h) = (vp.width_px, vp.height_px);
                if w <= 0 || h <= 0 {
                    return;
                }
                let gl: &glow::Context = painter.gl().as_ref();
                let mut st = vgl.lock().unwrap();
                st.ensure(gl, w, h);
                let Some(fbo) = st.fbo else { return };
                if core.render(fbo.0.get() as i32, w, h) {
                    unsafe {
                        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, st.fbo);
                        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
                        gl.blit_framebuffer(
                            0,
                            0,
                            w,
                            h,
                            vp.left_px,
                            vp.from_bottom_px,
                            vp.left_px + w,
                            vp.from_bottom_px + h,
                            glow::COLOR_BUFFER_BIT,
                            glow::NEAREST,
                        );
                        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                    }
                }
            })),
        }
    }

    fn ui(&mut self, ctx: &egui::Context) {
        self.poll();

        // ---- input: drag&drop, keyboard ----
        if let Some(p) = ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone())) {
            self.open(&p.to_string_lossy());
        }
        if self.loaded {
            let (space, left, right, up, down, f) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Space),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::F),
                )
            });
            if space {
                self.toggle_play();
            }
            if left {
                let t = self.pos - 5.0;
                self.seek_to(t);
            }
            if right {
                let t = self.pos + 5.0;
                self.seek_to(t);
            }
            if up || down {
                self.volume = (self.volume + if up { 5.0 } else { -5.0 }).clamp(0.0, 100.0);
                let _ = self.core.set("volume", &format!("{}", self.volume));
            }
            if f {
                let cur = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!cur));
            }
        }

        // ---- bottom controls ----
        egui::TopBottomPanel::bottom("controls")
            .frame(egui::Frame::new().fill(BAR_BG).inner_margin(egui::Margin::symmetric(14, 8)))
            .show(ctx, |ui| {
                let shown_pos = self.scrub.map(|f| f as f64 * self.dur).unwrap_or(self.pos);
                ui.horizontal(|ui| {
                    ui.add_sized([52.0, 18.0], egui::Label::new(egui::RichText::new(fmt_time(shown_pos)).color(MUTED).monospace()));
                    let w = (ui.available_width() - 60.0).max(60.0);
                    let frac = if self.dur > 0.0 { (self.pos / self.dur) as f32 } else { 0.0 };
                    if let Some(f) = seek_bar(ui, w, frac.clamp(0.0, 1.0), self.dur > 0.0, &mut self.scrub) {
                        let t = f as f64 * self.dur;
                        self.seek_to(t);
                    }
                    ui.label(egui::RichText::new(fmt_time(self.dur)).color(MUTED).monospace());
                });
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    if play_button(ui, self.loaded && !self.paused && !self.eof).clicked() && self.loaded {
                        self.toggle_play();
                    }
                    #[cfg(windows)]
                    if ui.button("Open\u{2026}").clicked() {
                        if let Some(p) = rfd::FileDialog::new()
                            .add_filter("Video", &["mp4", "mkv", "mov", "webm", "avi", "m4v"])
                            .pick_file()
                        {
                            self.open(&p.to_string_lossy());
                        }
                    }
                    ui.add_space(8.0);
                    for s in [0.5_f64, 1.0, 1.5, 2.0] {
                        if ui.selectable_label((self.speed - s).abs() < 0.01, format!("{s}x")).clicked() {
                            self.speed = s;
                            let _ = self.core.set("speed", &format!("{s}"));
                        }
                    }
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("Vol").color(MUTED));
                    ui.spacing_mut().slider_width = 110.0;
                    if ui.add(egui::Slider::new(&mut self.volume, 0.0..=100.0).show_value(false)).changed() {
                        let _ = self.core.set("volume", &format!("{}", self.volume));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let name = self.path.as_deref().map(|p| p.rsplit(['\\', '/']).next().unwrap_or(p)).unwrap_or("no file");
                        ui.label(egui::RichText::new(name).color(MUTED).small());
                    });
                });
                if !self.audio_status.is_empty() {
                    ui.label(egui::RichText::new(format!("audio: {}", self.audio_status)).color(MUTED).small().monospace());
                }
                if !self.msg.is_empty() {
                    ui.colored_label(Color32::from_rgb(255, 120, 120), &self.msg);
                }
            });

        // ---- video area ----
        let mut video_rect = Rect::NOTHING;
        egui::CentralPanel::default().frame(egui::Frame::new().fill(Color32::BLACK)).show(ctx, |ui| {
            let rect = ui.available_rect_before_wrap();
            video_rect = rect;
            let resp = ui.allocate_rect(rect, Sense::click());
            if self.loaded {
                ui.painter().add(self.video_callback(rect));
                if resp.double_clicked() {
                    let cur = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!cur));
                } else if resp.clicked() {
                    self.toggle_play();
                }
            } else {
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    if self.path.is_some() { "Loading\u{2026}" } else { "Drop a video file here\n(or start with: featherplayer-rs <file>)" },
                    egui::FontId::proportional(18.0),
                    MUTED,
                );
            }
        });

        // ---- overlay drawn ON TOP of the video: audio track picker ----
        if self.loaded && !self.tracks.is_empty() && video_rect.is_positive() {
            let btn = egui::Area::new(egui::Id::new("audio_btn"))
                .fixed_pos(Pos2::new(video_rect.right() - 14.0, video_rect.top() + 14.0))
                .pivot(Align2::RIGHT_TOP)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    let label = if self.sel.len() > 1 {
                        format!("Audio: mixing {}", self.sel.len())
                    } else {
                        "Audio tracks".to_string()
                    };
                    ui.add(egui::Button::new(egui::RichText::new(label).color(Color32::WHITE)).fill(Color32::from_black_alpha(170)))
                });
            if btn.inner.clicked() {
                self.audio_menu = !self.audio_menu;
            }
            if self.audio_menu {
                let mut changed = false;
                let menu = egui::Area::new(egui::Id::new("audio_menu"))
                    .fixed_pos(Pos2::new(video_rect.right() - 14.0, video_rect.top() + 14.0 + 34.0))
                    .pivot(Align2::RIGHT_TOP)
                    .order(egui::Order::Foreground)
                    .show(ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.label(egui::RichText::new("Tick several tracks to mix them").color(MUTED).small());
                            ui.add_space(4.0);
                            for (i, t) in self.tracks.iter().enumerate() {
                                let mut on = self.sel.contains(&i);
                                if ui.checkbox(&mut on, &t.label).changed() {
                                    if on {
                                        self.sel.insert(i);
                                    } else if self.sel.len() > 1 {
                                        self.sel.remove(&i);
                                    }
                                    changed = true;
                                }
                            }
                        });
                    });
                if changed {
                    self.apply_audio();
                }
                // click outside closes the menu
                let pressed = ctx.input(|i| i.pointer.any_pressed());
                if pressed {
                    if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                        if !menu.response.rect.contains(p) && !btn.response.rect.contains(p) {
                            self.audio_menu = false;
                        }
                    }
                }
            }
        }

        if self.loaded && !self.paused {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Widgets
// ---------------------------------------------------------------------------------------------

/// Seek bar that jumps to exactly where you press (also right next to the current position)
/// and follows the finger/mouse while dragging. Returns the final fraction on release.
fn seek_bar(ui: &mut egui::Ui, width: f32, value: f32, enabled: bool, scrub: &mut Option<f32>) -> Option<f32> {
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

fn play_button(ui: &mut egui::Ui, playing: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::click());
    let p = ui.painter_at(rect);
    let bg = if resp.hovered() { Color32::from_rgb(0x2a, 0x2f, 0x42) } else { Color32::from_rgb(0x20, 0x24, 0x33) };
    p.circle_filled(rect.center(), 17.0, bg);
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

// ---------------------------------------------------------------------------------------------
// App shell
// ---------------------------------------------------------------------------------------------
struct FeatherApp {
    player: Result<Player, String>,
}

impl FeatherApp {
    fn new(cc: &eframe::CreationContext<'_>, lib: Option<PathBuf>, file: Option<String>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        FeatherApp { player: Self::start(cc, lib, file) }
    }

    fn start(cc: &eframe::CreationContext<'_>, lib: Option<PathBuf>, file: Option<String>) -> Result<Player, String> {
        let gpa = cc.get_proc_address.ok_or("eframe gave no get_proc_address (is the glow renderer active?)")?;
        let mut cands = mpv::default_candidates();
        if let Some(l) = lib {
            cands.insert(0, l);
        }
        let core = mpv::Core::load(&cands)?;
        core.init(&[
            ("vo", "libmpv"),
            ("idle", "yes"),
            ("keep-open", "yes"),
            ("terminal", "no"),
            ("hwdec", "no"),
            ("input-default-bindings", "no"),
            ("osc", "no"),
        ])?;
        core.create_render_context(gpa, cc.egui_ctx.clone())?;
        let mut p = Player {
            core: Arc::new(core),
            vgl: Arc::new(Mutex::new(VideoGl::default())),
            path: None,
            loaded: false,
            dur: 0.0,
            pos: 0.0,
            paused: false,
            eof: false,
            volume: 100.0,
            speed: 1.0,
            scrub: None,
            hold: None,
            tracks: Vec::new(),
            sel: BTreeSet::new(),
            audio_menu: false,
            audio_status: String::new(),
            lavfi_cur: String::new(),
            msg: String::new(),
        };
        let _ = p.core.set("volume", "100");
        if let Some(f) = file {
            p.open(&f);
        }
        Ok(p)
    }
}

impl eframe::App for FeatherApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        match &mut self.player {
            Ok(p) => p.ui(ctx),
            Err(e) => {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.heading("Could not start mpv");
                    ui.add_space(8.0);
                    ui.add(egui::TextEdit::multiline(&mut e.as_str()).desired_width(f32::INFINITY).desired_rows(12));
                    if ui.button("Copy error").clicked() {
                        ctx.copy_text(e.clone());
                    }
                });
            }
        }
    }

    fn on_exit(&mut self, gl: Option<&glow::Context>) {
        if let Ok(p) = &self.player {
            if let (Some(gl), Ok(mut v)) = (gl, p.vgl.lock()) {
                v.destroy(gl);
            }
            p.core.shutdown(); // frees the render context first, then mpv
        }
    }
}

fn main() -> eframe::Result<()> {
    let mut file = None;
    let mut lib = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--lib" {
            lib = args.next().map(PathBuf::from);
        } else {
            file = Some(a);
        }
    }
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 720.0])
            .with_min_inner_size([640.0, 400.0])
            .with_title("FeatherPlayer (Rust) - mpv render test")
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("FeatherPlayer-rs", options, Box::new(move |cc| Ok(Box::new(FeatherApp::new(cc, lib, file)))))
}
