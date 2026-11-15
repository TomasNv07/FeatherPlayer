//! Player screen: video area, bottom controls, audio-track overlay, keyboard and drag&drop.

use super::fmt_time;
use super::widgets::{play_button, seek_bar};
use crate::player::Player;
use crate::theme::{BAR_BG, ERROR, MUTED};
use eframe::egui::{self, Align2, Color32, Pos2, Rect, Sense};
use std::time::Duration;

/// UI-only state of the player screen.
#[derive(Default)]
pub struct PlayerView {
    /// Seek-bar fraction while the pointer is held down on it.
    scrub: Option<f32>,
    audio_menu: bool,
}

fn toggle_fullscreen(ctx: &egui::Context) {
    let cur = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!cur));
}

impl PlayerView {
    pub fn show(&mut self, ctx: &egui::Context, p: &mut Player) {
        p.poll();
        self.input(ctx, p);
        self.controls(ctx, p);
        let video_rect = self.video(ctx, p);
        self.audio_overlay(ctx, p, video_rect);

        if p.loaded && !p.paused {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }

    fn input(&mut self, ctx: &egui::Context, p: &mut Player) {
        if let Some(f) = ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone())) {
            p.open(&f.to_string_lossy());
        }
        if !p.loaded {
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

    fn controls(&mut self, ctx: &egui::Context, p: &mut Player) {
        egui::TopBottomPanel::bottom("controls")
            .frame(egui::Frame::new().fill(BAR_BG).inner_margin(egui::Margin::symmetric(14, 8)))
            .show(ctx, |ui| {
                let shown_pos = self.scrub.map(|f| f as f64 * p.dur).unwrap_or(p.pos);
                ui.horizontal(|ui| {
                    ui.add_sized([52.0, 18.0], egui::Label::new(egui::RichText::new(fmt_time(shown_pos)).color(MUTED).monospace()));
                    let w = (ui.available_width() - 60.0).max(60.0);
                    let frac = if p.dur > 0.0 { (p.pos / p.dur) as f32 } else { 0.0 };
                    if let Some(f) = seek_bar(ui, w, frac.clamp(0.0, 1.0), p.dur > 0.0, &mut self.scrub) {
                        p.seek_to(f as f64 * p.dur);
                    }
                    ui.label(egui::RichText::new(fmt_time(p.dur)).color(MUTED).monospace());
                });
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    if play_button(ui, p.loaded && !p.paused && !p.eof).clicked() && p.loaded {
                        p.toggle_play();
                    }
                    #[cfg(windows)]
                    if ui.button("Open\u{2026}").clicked() {
                        if let Some(f) = rfd::FileDialog::new()
                            .add_filter("Video", &["mp4", "mkv", "mov", "webm", "avi", "m4v"])
                            .pick_file()
                        {
                            p.open(&f.to_string_lossy());
                        }
                    }
                    ui.add_space(8.0);
                    for s in [0.5_f64, 1.0, 1.5, 2.0] {
                        if ui.selectable_label((p.speed - s).abs() < 0.01, format!("{s}x")).clicked() {
                            p.set_speed(s);
                        }
                    }
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("Vol").color(MUTED));
                    ui.spacing_mut().slider_width = 110.0;
                    let mut vol = p.volume;
                    if ui.add(egui::Slider::new(&mut vol, 0.0..=100.0).show_value(false)).changed() {
                        p.set_volume(vol);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let name = p.path.as_deref().map(|s| s.rsplit(['\\', '/']).next().unwrap_or(s)).unwrap_or("no file");
                        ui.label(egui::RichText::new(name).color(MUTED).small());
                    });
                });
                if !p.audio_status.is_empty() {
                    ui.label(egui::RichText::new(format!("audio: {}", p.audio_status)).color(MUTED).small().monospace());
                }
                if !p.msg.is_empty() {
                    ui.colored_label(ERROR, &p.msg);
                }
            });
    }

    /// Video area; returns its rectangle for overlays.
    fn video(&mut self, ctx: &egui::Context, p: &mut Player) -> Rect {
        let mut video_rect = Rect::NOTHING;
        egui::CentralPanel::default().frame(egui::Frame::new().fill(Color32::BLACK)).show(ctx, |ui| {
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
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    if p.path.is_some() { "Loading\u{2026}" } else { "Drop a video file here\n(or start with: featherplayer-rs <file>)" },
                    egui::FontId::proportional(18.0),
                    MUTED,
                );
            }
        });
        video_rect
    }

    /// Audio track picker drawn ON TOP of the video.
    fn audio_overlay(&mut self, ctx: &egui::Context, p: &mut Player, video_rect: Rect) {
        if !(p.loaded && !p.tracks.is_empty() && video_rect.is_positive()) {
            return;
        }
        let anchor = Pos2::new(video_rect.right() - 14.0, video_rect.top() + 14.0);
        let btn = egui::Area::new(egui::Id::new("audio_btn"))
            .fixed_pos(anchor)
            .pivot(Align2::RIGHT_TOP)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let label = if p.sel.len() > 1 { format!("Audio: mixing {}", p.sel.len()) } else { "Audio tracks".to_string() };
                ui.add(egui::Button::new(egui::RichText::new(label).color(Color32::WHITE)).fill(Color32::from_black_alpha(170)))
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
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(egui::RichText::new("Tick several tracks to mix them").color(MUTED).small());
                    ui.add_space(4.0);
                    for (i, t) in p.tracks.iter().enumerate() {
                        let mut on = p.sel.contains(&i);
                        if ui.checkbox(&mut on, &t.label).changed() {
                            if on {
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
