//! Left side: video area, now-playing row and the player controls (Python `_build_ui`
//! controls): playback bar with trim handles, speed menu, trim and fullscreen buttons,
//! star/heart, frame step + play/pause, volume. Plus keyboard, drag&drop and the audio-track
//! overlay (from the proof of concept; Python's audio menu is ported later).

use super::header::no_video;
use super::icons;
use super::trim_slider::slim_slider;
use super::widgets::{self, play_button};
use crate::playback::Playback;
use crate::player::Player;
use crate::tracks::AudioTracks;
use crate::theme::{self, c};
use eframe::egui::{self, Align2, Color32, Pos2, Rect, Sense, Vec2};
use std::time::{Duration, Instant};

/// Python `_SPEEDS`, `_SPEED_CELL_W`, `_SPEED_PAD`.
pub const SPEEDS: [f64; 7] = [0.1, 0.25, 0.5, 1.0, 1.25, 1.5, 2.0];
const SPEED_CELL_W: f32 = 38.0;
const SPEED_PAD: f32 = 6.0;

pub enum ViewAction {
    ToggleStar,
    /// Right click on the star: pick the starred-clips folder.
    ChooseStarFolder,
    ToggleLike,
    ToggleTrim,
    SetSpeed(f64),
    /// A trim handle was let go: save [start, end] (0..10000).
    TrimSaved(f64, f64),
    /// Audio tracks popup: tick / untick track n (1-based).
    AudioToggle(usize),
    /// Audio tracks popup: track n volume (value, slider released).
    AudioVol(usize, f64, bool),
    /// "Use defaults" / right click on the audio button.
    AudioReset,
}

/// What the controls need to know about the open clip.
#[derive(Default)]
pub struct ClipState {
    pub open: bool,
    pub starred: bool,
    pub liked: bool,
    pub trimmed: bool,
}

#[derive(Default)]
pub struct PlayerView {
    audio_menu: bool,
    speed_menu: Option<Rect>,
    pub now_playing: String,
    pub now_playing_mb: String,
    flash: Option<(String, Color32, Instant)>,
    muted: bool,
    pre_mute_vol: Option<f64>,
    /// Python click watcher state (`st["last"]`, `st["click_t"]`).
    last_click: Option<Instant>,
    click_t: Option<Instant>,
}

fn toggle_fullscreen(ctx: &egui::Context) {
    let cur = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!cur));
}

fn is_fullscreen(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().fullscreen.unwrap_or(false))
}

/// Python `_fmt`: m:ss.
fn fmt(secs: f64) -> String {
    let s = secs.max(0.0) as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Round icon button with a hover circle (trim, fullscreen, star, heart, volume icon).
fn round_btn(ui: &mut egui::Ui, size: Vec2, hover_r: f32) -> (egui::Response, bool) {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let hov = resp.hovered();
    if hov {
        ui.painter().circle_filled(rect.center(), hover_r, c("#242444"));
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    (resp, hov)
}

/// Frame-step button (Python `_skip_btn`).
fn skip_btn(ui: &mut egui::Ui, icon: &'static str, size: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    if resp.is_pointer_button_down_on() {
        ui.painter().circle_filled(rect.center(), size / 2.0 - 1.0, c("#161628"));
    } else if resp.hovered() {
        ui.painter().circle_filled(rect.center(), size / 2.0 - 1.0, c("#2A2A4A"));
    }
    icons::paint(ui, rect.center(), icon, 18.0, c("#ffffff"), false, 1.9);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

impl PlayerView {
    /// Small status message left of the clip size, gone after 3.5 s (Python `_flash_star_status`).
    pub fn flash(&mut self, text: impl Into<String>, color: Color32) {
        self.flash = Some((text.into(), color, Instant::now() + Duration::from_millis(3500)));
    }

    /// Keyboard (Python's main-window bindings) and dropped files. Call before the panels.
    /// Returns -1 / +1 for Left / Right (newer / older clip, Python `_nav_clip`).
    pub fn input(&mut self, ctx: &egui::Context, p: &mut Player, pb: &mut Playback) -> Option<i32> {
        if let Some(f) = ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone())) {
            p.open(&f.to_string_lossy());
            self.now_playing = crate::scanner::basename(&f.to_string_lossy()).to_owned();
            self.now_playing_mb.clear();
        }
        if ctx.wants_keyboard_input() {
            return None; // typing in a text field
        }
        let key = |k| ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, k));
        let mut nav = None;
        if key(egui::Key::Space) && p.loaded {
            pb.toggle_play(p);
        }
        if key(egui::Key::ArrowLeft) {
            nav = Some(-1);
        }
        if key(egui::Key::ArrowRight) {
            nav = Some(1);
        }
        if key(egui::Key::M) {
            self.toggle_mute(p);
        }
        if key(egui::Key::F) {
            toggle_fullscreen(ctx);
        }
        // Python `_fullscreen_escape`
        if is_fullscreen(ctx) && key(egui::Key::Escape) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
        nav
    }

    /// Python `_set_volume`: moving the slider to 0 counts as muted, away from 0 unmutes.
    fn set_volume(&mut self, p: &mut Player, v: f64) {
        p.set_volume(v.round());
        if p.volume > 0.0 && self.muted {
            self.muted = false;
        }
        if p.volume == 0.0 && !self.muted {
            self.muted = true;
        }
    }

    /// Python `_toggle_mute`.
    fn toggle_mute(&mut self, p: &mut Player) {
        if self.muted {
            let restore = self.pre_mute_vol.filter(|v| *v > 0.0).unwrap_or(50.0);
            self.muted = false;
            p.set_volume(restore);
        } else {
            self.pre_mute_vol = Some(p.volume);
            self.muted = true;
            p.set_volume(0.0);
        }
    }

    /// Close the audio tracks popup (Python _close_audio_menu, also on a new clip).
    pub fn close_audio_menu(&mut self) {
        self.audio_menu = false;
    }

    pub fn controls(&mut self, ctx: &egui::Context, p: &mut Player, pb: &mut Playback, clip: &ClipState, at: &AudioTracks) -> Vec<ViewAction> {
        let mut acts = Vec::new();
        if !at.enabled {
            self.audio_menu = false;
        }
        let mut audio_btn = Rect::NOTHING;
        if p.volume == 0.0 {
            self.muted = true;
        }
        let mut speed_btn = Rect::NOTHING;
        egui::TopBottomPanel::bottom("controls").frame(egui::Frame::new().fill(theme::bg())).show_separator_line(false).show(ctx, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            // ---- now playing: title left, (flash status) + size right ----
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                ui.add_space(18.0);
                ui.label(egui::RichText::new(&self.now_playing).font(theme::font_bold()).color(theme::text_primary()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(18.0);
                    ui.label(egui::RichText::new(&self.now_playing_mb).font(theme::font_tiny()).color(theme::text_muted()));
                    if let Some((t, col, until)) = &self.flash {
                        if Instant::now() < *until {
                            ui.add_space(6.0);
                            ui.label(egui::RichText::new(t).font(theme::font_tiny()).color(*col));
                            ctx.request_repaint_after(*until - Instant::now());
                        }
                    }
                });
            });
            ui.add_space(2.0);

            // ---- progress row: time | bar | dur, speed, trim, fullscreen ----
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                ui.add_space(10.0 + 8.0);
                let tl = ui.allocate_exact_size(Vec2::new(30.0, 34.0), Sense::hover()).0;
                ui.painter().text(Pos2::new(tl.left(), tl.center().y), Align2::LEFT_CENTER, fmt(pb.shown_pos), theme::font_tiny(), theme::text_muted());
                let audio_w = if at.enabled { 4.0 + 34.0 } else { 0.0 };
                let right_w = 30.0 + audio_w + (4.0 + 42.0) + (4.0 + 34.0) + (4.0 + 34.0) + 10.0;
                let bar_w = (ui.available_width() - right_w - 16.0).max(60.0);
                ui.add_space(8.0);
                let evs = pb.bar.show(ui, bar_w, p.dur);
                if let Some((s, e)) = pb.bar_events(evs, p) {
                    acts.push(ViewAction::TrimSaved(s, e));
                }
                ui.add_space(8.0);
                let dl = ui.allocate_exact_size(Vec2::new(30.0, 34.0), Sense::hover()).0;
                ui.painter().text(Pos2::new(dl.right(), dl.center().y), Align2::RIGHT_CENTER, fmt(p.dur), theme::font_tiny(), theme::text_muted());

                // audio tracks (Python udio_btn): click = this clip's tracks, right click =
                // back to the default tracks; hidden when turned off in settings
                if at.enabled {
                    ui.add_space(4.0);
                    let (r, resp) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::click());
                    let hov = resp.hovered();
                    if hov && at.ctl {
                        ui.painter().circle_filled(r.center(), 16.0, c("#242444"));
                    }
                    if hov {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    let col = if !at.ctl {
                        theme::text_dim()
                    } else if at.changed() {
                        theme::accent()
                    } else if hov {
                        theme::text_primary()
                    } else {
                        theme::text_muted()
                    };
                    ui.painter().text(r.center() - Vec2::new(0.0, 1.0), Align2::CENTER_CENTER, "\u{266b}", theme::font_pt(12.0), col);
                    audio_btn = r;
                    if resp.clicked() {
                        self.audio_menu = !self.audio_menu;
                    }
                    if resp.secondary_clicked() {
                        acts.push(ViewAction::AudioReset);
                    }
                }

                // speed: click = menu of speeds, right click = back to 1x
                ui.add_space(4.0);
                let (r, resp) = ui.allocate_exact_size(Vec2::new(42.0, 34.0), Sense::click());
                let hov = resp.hovered();
                if hov {
                    ui.painter().circle_filled(r.center(), 16.0, c("#242444"));
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                let col = if p.speed != 1.0 { theme::accent() } else if hov { theme::text_primary() } else { theme::text_muted() };
                ui.painter().text(r.center() - Vec2::new(1.0, 1.0), Align2::CENTER_CENTER, format!("{}x", fmt_speed(p.speed)), theme::font_pt(10.0), col);
                speed_btn = r;
                if resp.clicked() {
                    self.speed_menu = if self.speed_menu.is_some() { None } else { Some(r) };
                }
                if resp.secondary_clicked() {
                    acts.push(ViewAction::SetSpeed(1.0));
                    self.speed_menu = None;
                }

                // trim: scissors, or x while trimmed
                ui.add_space(4.0);
                let (resp, hov) = round_btn(ui, Vec2::splat(34.0), 16.0);
                if clip.trimmed {
                    icons::paint(ui, resp.rect.center(), "x", 24.0, theme::accent(), false, 1.4);
                } else {
                    icons::paint(ui, resp.rect.center(), "scissors", 17.0, if hov { c("#bbbbbb") } else { theme::text_dim() }, false, 1.7);
                }
                if resp.clicked() && clip.open {
                    acts.push(ViewAction::ToggleTrim);
                }

                // fullscreen
                ui.add_space(4.0);
                let (resp, hov) = round_btn(ui, Vec2::splat(34.0), 16.0);
                let fs = is_fullscreen(ctx);
                icons::paint(ui, resp.rect.center(), if fs { "fullscreen-exit" } else { "fullscreen" }, 18.0, if hov { theme::text_primary() } else { theme::text_dim() }, false, 1.8);
                if resp.on_hover_text("Fullscreen (F)").clicked() {
                    toggle_fullscreen(ctx);
                }
            });

            // ---- control row: star, heart | transport (centred) | volume ----
            let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 64.0), Sense::hover());
            let inner = row.shrink2(Vec2::new(18.0, 8.0));
            let mut left = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)));
            left.spacing_mut().item_spacing = Vec2::ZERO;
            let (resp, hov) = round_btn(&mut left, Vec2::splat(34.0), 16.0);
            let col = if clip.starred { c("#e8c84a") } else if hov { c("#bbbbbb") } else { theme::text_dim() };
            icons::paint(&left, resp.rect.center(), "star", 20.0, col, clip.starred, 1.9);
            if resp.clicked() && clip.open {
                acts.push(ViewAction::ToggleStar);
            }
            if resp.secondary_clicked() {
                acts.push(ViewAction::ChooseStarFolder);
            }
            left.add_space(4.0);
            let (resp, hov) = round_btn(&mut left, Vec2::splat(34.0), 16.0);
            let col = if clip.liked { c("#e05555") } else if hov { c("#bbbbbb") } else { theme::text_dim() };
            icons::paint(&left, resp.rect.center() + Vec2::new(0.0, 1.0), "heart", 20.0, col, clip.liked, 1.9);
            if resp.clicked() && clip.open {
                acts.push(ViewAction::ToggleLike);
            }

            // volume (right)
            let mut right = ui.new_child(egui::UiBuilder::new().max_rect(inner.shrink2(Vec2::new(0.0, 0.0))).layout(egui::Layout::right_to_left(egui::Align::Center)));
            right.spacing_mut().item_spacing = Vec2::ZERO;
            right.add_space(7.0);
            let pct = right.allocate_exact_size(Vec2::new(32.0, 20.0), Sense::hover()).0;
            right.painter().text(Pos2::new(pct.right(), pct.center().y), Align2::RIGHT_CENTER, format!("{}%", p.volume.round() as i64), theme::font_tiny(), theme::text_muted());
            right.add_space(4.0);
            // the slider gets shorter instead of running into the centred buttons
            let room = (inner.right() - 7.0 - 36.0 - 6.0 - 30.0) - (inner.center().x + 70.0);
            let sw = room.clamp(40.0, 120.0);
            let mut sl_ui = right.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(Pos2::new(pct.left() - 4.0 - sw, inner.top()), Pos2::new(pct.left() - 4.0, inner.bottom()))).layout(egui::Layout::left_to_right(egui::Align::Center)));
            if let Some(v) = slim_slider(&mut sl_ui, sw, p.volume, 100.0) {
                self.set_volume(p, v);
            }
            right.add_space(sw + 6.0);
            let (resp, _) = round_btn(&mut right, Vec2::splat(30.0), 14.0);
            icons::paint(&right, resp.rect.center(), if self.muted { "mute" } else { "volume" }, 18.0, c("#ffffff"), false, 1.9);
            if resp.clicked() {
                self.toggle_mute(p);
            }

            // transport, dead centre: |< ( > ) >|
            let tw = 30.0 + 6.0 + 44.0 + 6.0 + 30.0;
            let t = Rect::from_center_size(row.center(), Vec2::new(tw, 44.0));
            let mut tui = ui.new_child(egui::UiBuilder::new().max_rect(t).layout(egui::Layout::left_to_right(egui::Align::Center)));
            tui.spacing_mut().item_spacing = Vec2::ZERO;
            if skip_btn(&mut tui, "skip-prev", 30.0).clicked() {
                p.frame_step(true);
            }
            tui.add_space(6.0);
            if play_button(&mut tui, p.playing()).clicked() {
                pb.toggle_play(p);
            }
            tui.add_space(6.0);
            if skip_btn(&mut tui, "skip-next", 30.0).clicked() {
                p.frame_step(false);
            }

            if !p.msg.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(18.0);
                    ui.colored_label(theme::ERROR, &p.msg);
                });
                ui.add_space(4.0);
            }
        });

        // speed menu: all speeds in one row, centred above the button
        if let Some(anchor) = self.speed_menu {
            let pw = SPEEDS.len() as f32 * SPEED_CELL_W + 2.0 * SPEED_PAD;
            let ph = 30.0 + 2.0 * SPEED_PAD;
            let screen = ctx.content_rect();
            let x = (anchor.center().x - pw / 2.0).clamp(0.0, (screen.width() - pw).max(0.0));
            let y = (anchor.top() - ph - 4.0).max(0.0);
            let mut chosen = None;
            let area = egui::Area::new(egui::Id::new("speed_menu")).fixed_pos(Pos2::new(x, y)).order(egui::Order::Foreground).show(ctx, |ui| {
                egui::Frame::new().fill(theme::card_bg()).stroke(egui::Stroke::new(1.0_f32, c("#52527e"))).corner_radius(10).inner_margin(egui::Margin::same(SPEED_PAD as i8)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                        for s in SPEEDS {
                            let (r, resp) = ui.allocate_exact_size(Vec2::new(SPEED_CELL_W, 30.0), Sense::click());
                            let active = (s - p.speed).abs() < 1e-9;
                            let hl = Rect::from_center_size(r.center(), Vec2::new(SPEED_CELL_W - 2.0, 26.0));
                            if resp.hovered() {
                                ui.painter().rect_filled(hl, 7.0, c("#2c2c50"));
                            } else if active {
                                ui.painter().rect_filled(hl, 7.0, c("#2A2A4A"));
                            }
                            let fg = if active { theme::accent() } else if resp.hovered() { Color32::WHITE } else { theme::text_muted() };
                            ui.painter().text(r.center(), Align2::CENTER_CENTER, format!("{}x", fmt_speed(s)), theme::font_ui(), fg);
                            if resp.clicked() {
                                chosen = Some(s);
                            }
                        }
                    });
                });
            });
            if let Some(s) = chosen {
                acts.push(ViewAction::SetSpeed(s));
                self.speed_menu = None;
            } else if ctx.input(|i| i.pointer.any_pressed()) {
                if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                    if !area.response.rect.contains(pos) && !speed_btn.contains(pos) {
                        self.speed_menu = None;
                    }
                }
            }
        }

        // audio tracks menu, centred above the button; stays open while ticking
        if self.audio_menu && audio_btn.is_positive() {
            let pw = if at.ctl { 340.0 } else { 240.0 };
            let area = egui::Area::new(egui::Id::new("audio_menu"))
                .fixed_pos(Pos2::new(audio_btn.center().x, audio_btn.top() - 4.0))
                .pivot(Align2::CENTER_BOTTOM)
                .constrain(true)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::new().fill(theme::card_bg()).stroke(egui::Stroke::new(1.0_f32, c("#52527e"))).corner_radius(10).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
                        ui.set_width(pw - 12.0);
                        ui.spacing_mut().item_spacing = Vec2::new(0.0, 2.0);
                        if !at.ctl {
                            // nothing to pick: say why and list what was found
                            egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
                                ui.set_max_width(210.0);
                                ui.add(egui::Label::new(egui::RichText::new(at.status_text()).font(theme::font_tiny()).color(theme::text_muted())).wrap());
                            });
                            for n in 1..=at.tracks.len() {
                                let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::hover());
                                ui.painter().text(Pos2::new(r.left() + 12.0, r.center().y), Align2::LEFT_CENTER, at.label(n), theme::font_ui(), theme::text_muted());
                            }
                        } else {
                            for n in 1..=at.tracks.len() {
                                let sel = at.sel.as_ref().is_some_and(|s| s.contains(&n));
                                let r = widgets::track_row(ui, &at.label(n), sel, at.eff_vol(n), 30.0);
                                if r.toggled {
                                    acts.push(ViewAction::AudioToggle(n));
                                }
                                if let Some((v, fin)) = r.volume {
                                    acts.push(ViewAction::AudioVol(n, v, fin));
                                }
                            }
                            ui.add_space(3.0);
                            if widgets::Row::new("Use defaults").height(30.0).show(ui).clicked() {
                                acts.push(ViewAction::AudioReset);
                            }
                        }
                    });
                });
            if ctx.input(|i| i.pointer.any_pressed()) {
                if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                    if !area.response.rect.contains(pos) && !audio_btn.contains(pos) {
                        self.audio_menu = false;
                    }
                }
            }
        }
        acts
    }

    /// Video area; returns its rectangle for overlays.
    pub fn video(&mut self, ctx: &egui::Context, p: &mut Player, pb: &mut Playback) -> Rect {
        let mut video_rect = Rect::NOTHING;
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::player_bg())).show(ctx, |ui| {
            let rect = ui.available_rect_before_wrap();
            video_rect = rect;
            let resp = ui.allocate_rect(rect, Sense::click());
            if p.loaded {
                ui.painter().add(p.video_callback(rect));
                // Python's click watcher: a press on the video toggles play (a second one
                // within 0.2 s is ignored); in fullscreen a double click (< 0.45 s) leaves
                // fullscreen and undoes the first click's play/pause
                if resp.hovered() && ui.input(|i| i.pointer.primary_pressed()) {
                    let now = Instant::now();
                    let since = |t: Option<Instant>| t.map_or(f64::INFINITY, |t| (now - t).as_secs_f64());
                    if is_fullscreen(ctx) && since(self.click_t) < 0.45 {
                        self.click_t = None;
                        self.last_click = Some(now);
                        pb.toggle_play(p);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                    } else if since(self.last_click) > 0.2 {
                        self.last_click = Some(now);
                        self.click_t = Some(now);
                        pb.toggle_play(p);
                    }
                }
            } else {
                no_video(ui, rect, if p.path.is_some() { "Loading\u{2026}" } else { "Select a clip to play" });
            }
        });
        if p.loaded && !p.paused {
            // Python checks the trim end every 15 ms
            ctx.request_repaint_after(Duration::from_millis(15));
        }
        video_rect
    }
}

/// Python `f"{spd:g}"`.
pub fn fmt_speed(s: f64) -> String {
    let t = format!("{s}");
    t.strip_suffix(".0").map(str::to_owned).unwrap_or(t)
}
