//! Header above the video: "ALL CLIPS  • count  • size", the Folder pill (with the saved
//! folders dropdown) and the actions pill. Ported from FeatherPlayer `_build_ui` (hdr) and
//! `_open_folder_dropdown`. Actions pill like Python: delete, categorize, show in folder,
//! share, compress, refresh, settings.

use super::widgets::{self, Ghost, Row};
use crate::library::{fmt_size_total, Library};
use crate::prefs::Prefs;
use crate::scanner::basename;
use crate::theme;
use eframe::egui::{self, Align2, Pos2, Rect, Sense, Vec2};

pub enum HeaderAction {
    /// "Folder" button: pick a folder and show only it.
    PickFolder,
    /// "Add new folder" in the dropdown: pick a folder and add it to the checked ones.
    AddFolder,
    /// Checkbox of a saved folder in the dropdown.
    ToggleFolder(String),
    Refresh,
    Categorize,
    ShowInFolder,
    Share,
    Compress,
    ApplyTrim,
    Delete,
    Settings,
}

#[derive(Default)]
pub struct Header {
    dropdown: Option<Rect>,
}

/// Small dot before a header stat (Python `_make_stat_dot`).
fn stat(ui: &mut egui::Ui, text: &str) {
    if text.is_empty() {
        return;
    }
    ui.add_space(8.0);
    let (r, _) = ui.allocate_exact_size(Vec2::new(9.0, 16.0), Sense::hover());
    ui.painter().circle_filled(Pos2::new(r.left() + 1.5, r.center().y + 1.0), 1.5, theme::text_dim());
    ui.label(egui::RichText::new(text).font(theme::font_ui()).color(theme::text_dim()));
}

impl Header {
    /// `trim_mode`: show the "Apply trim" pill (trim mode is on for the open clip).
    /// `update_badge`: an update is available: accent "!" at the gear's top-right.
    pub fn show(&mut self, ui: &mut egui::Ui, lib: &Library, prefs: &Prefs, trim_mode: bool, update_badge: bool) -> Vec<HeaderAction> {
        let mut acts = Vec::new();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            ui.label(egui::RichText::new("ALL CLIPS").font(theme::font_title()).color(theme::text_primary()));
            let count = if lib.loaded_once { lib.videos.len().to_string() } else { String::new() };
            stat(ui, &count);
            let size = lib.total_size.filter(|_| !lib.videos.is_empty()).map(fmt_size_total).unwrap_or_default();
            stat(ui, &size);
            // fixed row height (pill = CTRL_H + 2px stroke): with an auto-growing row egui centres
            // the first pill in the short initial row and the next ones in the grown row, so the
            // pills ended up at different heights
            let row = Vec2::new(ui.available_width(), theme::CTRL_H + 2.0);
            ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Min), |ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                // the pills' inner `horizontal` rows start at this height; equal to the Ghost
                // height so egui doesn't re-centre (and shift) them
                ui.spacing_mut().interact_size.y = theme::CTRL_H - 4.0;
                // far right: Folder | v
                let mut arrow = None;
                // (this layout runs right to left, so the pill's items are added last-first)
                widgets::pill_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                        let r = Ghost::new("").icon("chevron-down", 14.0).padx(6.0).show(ui).on_hover_text("Saved folders");
                        if r.clicked() {
                            self.dropdown = if self.dropdown.is_some() { None } else { Some(r.rect) };
                        }
                        arrow = Some(r.rect);
                        widgets::pill_divider(ui);
                        if Ghost::new("Folder").icon("folder-plus", 18.0).font(theme::font_bold()).fg(theme::text_primary()).padx(10.0).accent_icon().show(ui).on_hover_text("Choose a clips folder").clicked() {
                            acts.push(HeaderAction::PickFolder);
                        }
                    });
                });
                if let (Some(a), Some(_)) = (arrow, self.dropdown) {
                    self.dropdown = Some(a);
                }
                ui.add_space(8.0);
                // actions (right to left): Delete | Categorize | Show in folder | Share | Compress | Refresh | Settings
                widgets::pill_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                        let tip = if update_badge { "Settings \u{2014} an update is available" } else { "Settings" };
                        let gear = Ghost::new("").icon("gear", 18.0).padx(8.0).show(ui).on_hover_text(tip);
                        if update_badge {
                            // small accent "!" at the gear's top-right (owner 2026-11-28)
                            let p = Pos2::new(gear.rect.right() - 6.0, gear.rect.top() + 6.0);
                            ui.painter().text(p, Align2::CENTER_CENTER, "!", egui::FontId::new(13.0, theme::bold_family()), theme::accent());
                        }
                        if gear.clicked() {
                            acts.push(HeaderAction::Settings);
                        }
                        widgets::pill_divider(ui);
                        if Ghost::new("").icon("refresh", 18.0).padx(8.0).show(ui).on_hover_text("Refresh").clicked() {
                            acts.push(HeaderAction::Refresh);
                        }
                        widgets::pill_divider(ui);
                        if Ghost::new("").icon("compress", 18.0).padx(8.0).show(ui).on_hover_text("Compress clips").clicked() {
                            acts.push(HeaderAction::Compress);
                        }
                        widgets::pill_divider(ui);
                        if Ghost::new("").icon("share", 18.0).padx(8.0).show(ui).on_hover_text("Share").clicked() {
                            acts.push(HeaderAction::Share);
                        }
                        widgets::pill_divider(ui);
                        if Ghost::new("").icon("folder", 18.0).padx(8.0).show(ui).on_hover_text("Show in folder").clicked() {
                            acts.push(HeaderAction::ShowInFolder);
                        }
                        widgets::pill_divider(ui);
                        if Ghost::new("").icon("plus", 18.0).padx(8.0).show(ui).on_hover_text("Categorize clips").clicked() {
                            acts.push(HeaderAction::Categorize);
                        }
                        widgets::pill_divider(ui);
                        // delete: number of selected clips beside the icon, red on hover
                        let n = lib.selected.len();
                        let count = if n >= 1 { n.to_string() } else { String::new() };
                        let card = theme::card_bg();
                        let red = egui::Color32::from_rgb(0xff, 0x44, 0x55);
                        let r = Ghost::new(&count)
                            .icon("trash", 18.0)
                            .padx(8.0)
                            .font(theme::font_bold())
                            .text_left()
                            .hover_colors(egui::Color32::from_rgb(0xff, 0x8a, 0x94), theme::mix(card, red, 0.22), theme::mix(card, red, 0.10))
                            .show(ui)
                            .on_hover_text("Delete selected clips");
                        if r.clicked() {
                            acts.push(HeaderAction::Delete);
                        }
                    });
                });
                if trim_mode {
                    ui.add_space(8.0);
                    widgets::pill_frame().show(ui, |ui| {
                        if Ghost::new("Apply trim").font(theme::font_bold()).fg(theme::text_primary()).padx(12.0).show(ui).on_hover_text("Cut the clip down to the trimmed part (permanent)").clicked() {
                            acts.push(HeaderAction::ApplyTrim);
                        }
                    });
                }
            });
        });
        if let Some(arrow) = self.dropdown {
            self.folder_dropdown(ui.ctx(), arrow, prefs, &mut acts);
        }
        acts
    }

    fn folder_dropdown(&mut self, ctx: &egui::Context, arrow: Rect, prefs: &Prefs, acts: &mut Vec<HeaderAction>) {
        let width = 300.0;
        let pos = Pos2::new(arrow.right() + 2.0 - width, arrow.bottom() + 4.0);
        let mut close = false;
        let rect = widgets::popup(ctx, "folder_dropdown", pos, width, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(egui::RichText::new("Saved folders").font(theme::font_tiny()).color(theme::text_dim()));
            });
            ui.add_space(3.0);
            if prefs.saved_folders.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.label(egui::RichText::new("No saved folders yet").font(theme::font_ui()).color(theme::text_muted()));
                });
                ui.add_space(8.0);
            }
            for fp in &prefs.saved_folders {
                let b = basename(fp);
                let display = if b.is_empty() { fp.as_str() } else { b };
                if Row::new(display).height(44.0).check(prefs.checked_folders.contains(fp)).subtitle(fp).show(ui).clicked() {
                    acts.push(HeaderAction::ToggleFolder(fp.clone()));
                }
            }
            ui.add_space(5.0);
            let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
            ui.painter().rect_filled(r.shrink2(Vec2::new(8.0, 0.0)), 0.0, theme::divider());
            ui.add_space(4.0);
            if Row::new("Add new folder").height(34.0).icon("folder-plus").show(ui).clicked() {
                acts.push(HeaderAction::AddFolder);
                close = true;
            }
        });
        if close {
            self.dropdown = None;
        } else if ctx.input(|i| i.pointer.any_pressed()) {
            if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                if !rect.contains(p) && !arrow.contains(p) {
                    self.dropdown = None;
                }
            }
        }
    }
}

/// "Select a clip to play" placeholder text.
pub fn no_video(ui: &egui::Ui, rect: Rect, text: &str) {
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, theme::font_pt(14.0), theme::text_dim());
}
