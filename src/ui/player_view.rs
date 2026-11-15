//! Left side: video area, now-playing row and player controls, audio-track overlay,
//! keyboard and drag&drop.
//!
//! The controls are still the proof-of-concept set (play, seek, speed, volume, audio tracks
//! overlay) in the theme colours. The full FeatherPlayer controls (frame step, star/heart,
//! trim slider, speed/audio menus) are ported in a later step.

use super::fmt_time;
use super::header::no_video;
use super::widgets::{play_button, seek_bar};
use crate::player::Player;
use crate::theme;
use eframe::egui::{self, Align2, Color32, Pos2, Rect, Sense};
use std::time::Duration;

/// UI-only state of the player screen.
#[derive(Default)]
pub struct PlayerView {
    /// Seek-bar fraction while the pointer is held down on it.
    scrub: Option<f32>,
    audio_menu: bool,
    pub now_playing: String,
    pub now_playing_mb: String,
}

fn toggle_fullscreen(ctx: &egui::Context) {
    let cur = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!cur));
}

impl PlayerView {
    /// Keyboard and dropped files. Call before the panels.
    pub fn input(&mut self, ctx: &egui::Context, p: &mut Player) {
        if let Some(f) = ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone())) {
            p.open(&f.to_string_lossy());
            self.now_playing = crate::scanner::basename(&f.to_string_lossy()).to_owned();
            self.now_playing_mb.clear();
        }
        if !p.loaded || ctx.wants_keyboard_input() {
            return;
        }
        let key = |k| ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, k));
        if key(egui::Key::Space) {
            p.toggle_play();
        }
        if key(egui::Key::ArrowLeft) {
            p.seek_to(p.pos - 5.0);
        }
        if key(egui::Key::ArrowRight) {
            p.seek_to(p.pos + 5.0);
        }
        if key(egui::Key::ArrowUp) {
            p.set_volume(p.volume + 5.0);
        }
        if key(egui::Key::ArrowDown) {
            p.set_volume(p.volume - 5.0);
        }
        if key(egui::Key::F) {
            toggle_fullscreen(ctx);
        }
    }

    pub fn controls(&mut self, ctx: &egui::Context, p: &mut Player) {
        egui::TopBottomPanel::bottom("controls").frame(egui::Frame::new().fill(theme::bg())).show_separator_line(false).show(ctx, |ui| {
            ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
            // now playing
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(18.0);
                ui.label(egui::RichText::new(&self.now_playing).font(theme::font_bold()).color(theme::text_primary()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(18.0);
                    ui.label(egui::RichText::new(&self.now_playing_mb).font(theme::font_tiny()).color(theme::text_muted()));
                });
            });
            ui.add_space(2.0);
            // progress
            let shown_pos = self.scrub.map(|f| f as f64 * p.dur).unwrap_or(p.pos);
            ui.horizontal(|ui| {
                ui.add_space(18.0);
                ui.add_sized([34.0, 30.0], egui::Label::new(egui::RichText::new(fmt_time(shown_pos)).font(theme::font_tiny()).color(theme::text_muted())));
                let w = (ui.available_width() - 34.0 - 28.0).max(60.0);
                let frac = if p.dur > 0.0 { (p.pos / p.dur) as f32 } else { 0.0 };
                if let Some(f) = seek_bar(ui, w, frac.clamp(0.0, 1.0), p.dur > 0.0, &mut self.scrub) {
                    p.seek_to(f as f64 * p.dur);
                }
                ui.add_sized([34.0, 30.0], egui::Label::new(egui::RichText::new(fmt_time(p.dur)).font(theme::font_tiny()).color(theme::text_muted())));
            });
            // transport row: speeds left, play centred, volume right
            let (row, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 64.0), Sense::hover());
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(row.shrink2(egui::vec2(18.0, 8.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
            child.spacing_mut().item_spacing = egui::vec2(4.0, 0.0);
            for s in [0.5_f64, 1.0, 1.5, 2.0] {
                let on = (p.speed - s).abs() < 0.01;
                let txt = egui::RichText::new(format!("{s}x")).font(theme::font_pt(10.0)).color(if on { theme::accent() } else { theme::text_muted() });
                if child.add(egui::Button::new(txt).frame(false)).clicked() {
                    p.set_speed(s);
                }
            }
            child.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(format!("{}%", p.volume.round() as i64)).font(theme::font_tiny()).color(theme::text_muted()));
                ui.spacing_mut().slider_width = 120.0;
                let mut vol = p.volume;
                if ui.add(egui::Slider::new(&mut vol, 0.0..=100.0).show_value(false)).changed() {
                    p.set_volume(vol);
                }
            });
            let mut play = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_center_size(row.center(), egui::vec2(44.0, 44.0))));
            if play_button(&mut play, p.playing()).clicked() && p.loaded {
                p.toggle_play();
            }
            if !p.msg.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(18.0);
                    ui.colored_label(theme::ERROR, &p.msg);
                });
                ui.add_space(4.0);
            }
        });
    }

    /// Video area; returns its rectangle for overlays.
    pub fn video(&mut self, ctx: &egui::Context, p: &mut Player) -> Rect {
        let mut video_rect = Rect::NOTHING;
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::player_bg())).show(ctx, |ui| {
            let rect = ui.available_rect_before_wrap();
            video_rect = rect;
            let resp = ui.allocate_rect(rect, Sense::click());
            if p.loaded {
                ui.painter().add(p.video_callback(rect));
                if resp.double_clicked() {
                    toggle_fullscreen(ctx);
                } else if resp.clicked() {
                    p.toggle_play();
                }
            } else {
                no_video(ui, rect, if p.path.is_some() { "Loading\u{2026}" } else { "Select a clip to play" });
            }
        });
        if p.loaded && !p.paused {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
        video_rect
    }

    /// Audio track picker drawn ON TOP of the video.
    pub fn audio_overlay(&mut self, ctx: &egui::Context, p: &mut Player, video_rect: Rect) {
        if !(p.loaded && !p.tracks.is_empty() && video_rect.is_positive()) {
            return;
        }
        let anchor = Pos2::new(video_rect.right() - 14.0, video_rect.top() + 14.0);
        let btn = egui::Area::new(egui::Id::new("audio_btn"))
            .fixed_pos(anchor)
            .pivot(Align2::RIGHT_TOP)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let label = if p.sel.len() > 1 { format!("\u{266b}  mixing {}", p.sel.len()) } else { "\u{266b}  Audio tracks".to_string() };
                ui.add(egui::Button::new(egui::RichText::new(label).font(theme::font_ui()).color(Color32::WHITE)).fill(Color32::from_black_alpha(170)))
            });
        if btn.inner.clicked() {
            self.audio_menu = !self.audio_menu;
        }
        if !self.audio_menu {
            return;
        }
        let mut changed = false;
        let menu = egui::Area::new(egui::Id::new("audio_menu"))
            .fixed_pos(anchor + egui::vec2(0.0, 34.0))
            .pivot(Align2::RIGHT_TOP)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                super::widgets::popup_frame().show(ui, |ui| {
                    ui.set_width(300.0);
                    ui.label(egui::RichText::new("Tick several tracks to mix them").font(theme::font_tiny()).color(theme::text_dim()));
                    ui.add_space(4.0);
                    for i in 0..p.tracks.len() {
                        let on = p.sel.contains(&i);
                        if super::widgets::Row::new(&p.tracks[i].label).check(on).show(ui).clicked() {
                            if !on {
                                p.sel.insert(i);
                            } else if p.sel.len() > 1 {
                                p.sel.remove(&i);
                            }
                            changed = true;
                        }
                    }
                });
            });
        if changed {
            p.apply_audio();
        }
        // click outside closes the menu
        if ctx.input(|i| i.pointer.any_pressed()) {
            if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                if !menu.response.rect.contains(pos) && !btn.response.rect.contains(pos) {
                    self.audio_menu = false;
                }
            }
        }
    }
}
