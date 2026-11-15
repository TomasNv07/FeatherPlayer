//! App shell: owns the player and the views, hands frames to eframe.

use crate::player::Player;
use crate::prefs::Prefs;
use crate::theme;
use crate::ui::player_view::PlayerView;
use eframe::{egui, glow};
use std::path::PathBuf;

pub struct FeatherApp {
    player: Result<Player, String>,
    prefs: Prefs,
    view: PlayerView,
}

impl FeatherApp {
    pub fn new(cc: &eframe::CreationContext<'_>, lib: Option<PathBuf>, file: Option<String>) -> Self {
        theme::apply(&cc.egui_ctx);
        let prefs = Prefs::load();
        let mut player = Player::start(cc, lib, prefs.volume as f64);
        if let (Ok(p), Some(f)) = (&mut player, file) {
            p.open(&f);
        }
        FeatherApp { player, prefs, view: PlayerView::default() }
    }
}

impl eframe::App for FeatherApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        match &mut self.player {
            Ok(p) => self.view.show(ctx, p),
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
            // Python saves prefs in on_close too
            self.prefs.volume = p.volume.round() as i64;
            self.prefs.save();
            p.shutdown(gl);
        }
    }
}
