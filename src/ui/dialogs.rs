//! Category dialogs, ported from FeatherPlayer `_open_rename_category_dialog` and
//! `_open_categorize_dialog`. Both write to the clip prefs file right away (no separate save).
//!
//! Keyword groups are only ever deleted when the user removes a group's last keyword with ×
//! (never as a side effect of recomputing categories).

use super::widgets::{self, rounded_button};
use crate::library::{Library, NONE_KEY};
use crate::prefs::{KeywordGroup, Prefs};
use crate::scanner::{self, normalize_game};
use crate::theme::{self, c};
use eframe::egui::{self, Vec2};

fn dialog_frame() -> egui::Frame {
    egui::Frame::new().fill(theme::card_bg()).stroke(egui::Stroke::new(1.0_f32, c("#33335a"))).corner_radius(10).inner_margin(egui::Margin::ZERO)
}

fn small_note(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).font(theme::font_tiny()).color(theme::text_dim())).wrap());
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).font(theme::font_ui()).color(theme::text_primary()));
}

fn separator(ui: &mut egui::Ui) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(r, 0.0, c("#2A2A4A"));
}

fn save_btn(ui: &mut egui::Ui, text: &str) -> bool {
    rounded_button(ui, text, theme::accent_fg(), theme::accent(), c("#FF5C8D"), theme::font_bold(), 14.0, 6.0).clicked()
}

fn plain_btn(ui: &mut egui::Ui, text: &str, fg: egui::Color32, padx: f32) -> bool {
    rounded_button(ui, text, fg, c("#242444"), c("#2A2A4A"), theme::font_ui(), padx, 6.0).clicked()
}

const BODY_W: f32 = 280.0;

// ---------------------------------------------------------------------------------------------
// Rename category
// ---------------------------------------------------------------------------------------------

pub struct RenameDialog {
    key: String,
    text: String,
    auto_default: String,
    focus: bool,
}

impl RenameDialog {
    /// None for "All", "Moments" or a category that isn't listed.
    pub fn new(key: &str, lib: &Library) -> Option<RenameDialog> {
        if key == "All" || key == NONE_KEY || !lib.cats.norm_map.contains_key(key) {
            return None;
        }
        let current = lib.cats.label(key).to_owned();
        let auto_default = lib.cats.auto_map.get(key).cloned().unwrap_or_else(|| current.clone());
        Some(RenameDialog { key: key.to_owned(), text: current, auto_default, focus: true })
    }

    /// Returns false when the dialog is closed.
    pub fn show(&mut self, ctx: &egui::Context, prefs: &mut Prefs, lib: &mut Library) -> bool {
        let mut open = true;
        let mut save = false;
        let resp = egui::Modal::new(egui::Id::new("rename_category")).frame(dialog_frame()).show(ctx, |ui| {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(24, 20)).show(ui, |ui| {
                ui.set_width(BODY_W);
                heading(ui, "Category name");
                ui.add_space(8.0);
                let r = widgets::entry(ui, &mut self.text, BODY_W);
                if self.focus {
                    r.request_focus();
                    self.focus = false;
                }
                ui.add_space(8.0);
                small_note(ui, &format!("Only renames the label \u{2014} clip files aren't touched. Auto-detected name: \"{}\"", self.auto_default));
            });
            separator(ui);
            egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    if plain_btn(ui, "Reset to default", theme::text_muted(), 10.0) {
                        self.text = self.auto_default.clone();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if plain_btn(ui, "Cancel", theme::text_primary(), 14.0) {
                            open = false;
                        }
                        ui.add_space(6.0);
                        if save_btn(ui, "Save") {
                            save = true;
                        }
                    });
                });
            });
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                save = true;
            }
        });
        if resp.should_close() {
            open = false;
        }
        if save {
            let new = self.text.trim().to_owned();
            if !new.is_empty() {
                if let Some(g) = prefs.group_mut(&self.key) {
                    g.label = new;
                } else if new == self.auto_default {
                    prefs.set_rename(&self.key, None);
                } else {
                    prefs.set_rename(&self.key, Some(new));
                }
                lib.recompute_categories(prefs);
                prefs.save();
            }
            open = false;
        }
        open
    }
}

// ---------------------------------------------------------------------------------------------
// Categorize: keyword -> category, plus the list of existing keyword groups
// ---------------------------------------------------------------------------------------------

pub struct CategorizeDialog {
    kw: String,
    cat: String,
    err: String,
    focus: bool,
}

/// Python's checks after a category change: keep only listed categories in the game filter.
fn prune_game_filter(prefs: &mut Prefs, lib: &Library) {
    prefs.game_multi.retain(|k| lib.cats.norm_map.contains_key(k));
    if prefs.game_filter != "All" && !lib.cats.norm_map.contains_key(&prefs.game_filter) {
        prefs.game_filter = "All".into();
    }
}

impl CategorizeDialog {
    /// The keyword is prefilled from the clip that is playing.
    pub fn new(current: Option<&str>) -> CategorizeDialog {
        let mut kw = current.map(|p| scanner::game_name_from_filename(scanner::basename(p))).unwrap_or_default();
        if kw == "Moments" {
            kw.clear();
        }
        CategorizeDialog { cat: kw.clone(), kw, err: String::new(), focus: true }
    }

    /// Returns (still open, clip list changed).
    pub fn show(&mut self, ctx: &egui::Context, prefs: &mut Prefs, lib: &mut Library) -> (bool, bool) {
        let mut open = true;
        let mut save = false;
        let mut delete: Option<(String, String)> = None;
        let resp = egui::Modal::new(egui::Id::new("categorize")).frame(dialog_frame()).show(ctx, |ui| {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(24, 20)).show(ui, |ui| {
                ui.set_width(BODY_W);
                heading(ui, "Keyword");
                ui.add_space(6.0);
                small_note(ui, "Any clip whose filename starts with this joins the category below. Reusing a keyword that's already assigned moves it to the new category.");
                ui.add_space(8.0);
                let r = widgets::entry(ui, &mut self.kw, BODY_W);
                if self.focus {
                    r.request_focus();
                    self.focus = false;
                }
                ui.add_space(14.0);
                heading(ui, "Category");
                ui.add_space(6.0);
                small_note(ui, "Type an existing category to add to it, or a new name to create one.");
                ui.add_space(8.0);
                widgets::entry(ui, &mut self.cat, BODY_W);
                if !self.err.is_empty() {
                    ui.add_space(8.0);
                    ui.add(egui::Label::new(egui::RichText::new(&self.err).font(theme::font_tiny()).color(egui::Color32::from_rgb(0xe0, 0x68, 0x5f))).wrap());
                }
                ui.add_space(16.0);
                heading(ui, "Your categories");
                ui.add_space(2.0);
                small_note(ui, "Click a keyword to reuse it above (edit the category, then Save to move it). Click \u{d7} to remove it.");
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(160.0).auto_shrink([false, true]).show(ui, |ui| {
                    let mut groups: Vec<&(String, KeywordGroup)> = prefs.keyword_groups.iter().filter(|(_, g)| !g.keywords.is_empty()).collect();
                    if prefs.keyword_groups.is_empty() {
                        ui.label(egui::RichText::new("No categories yet \u{2014} add one above.").font(theme::font_tiny()).color(theme::text_dim()));
                    }
                    groups.sort_by_key(|(_, g)| g.label.to_lowercase());
                    for (gkey, g) in groups {
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(&g.label).font(theme::font_ui()).color(theme::text_primary()));
                        ui.add_space(2.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
                            for kw in &g.keywords {
                                egui::Frame::new().fill(c("#242444")).show(ui, |ui| {
                                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                                    ui.horizontal(|ui| {
                                        let t = egui::RichText::new(kw).font(theme::font_tiny()).color(theme::text_muted());
                                        let r = ui.add(egui::Button::new(t).frame(false).min_size(Vec2::new(0.0, 22.0)));
                                        if r.clicked() {
                                            self.kw = kw.clone();
                                            self.cat = g.label.clone();
                                            self.err.clear();
                                            self.focus = true;
                                        }
                                        let x = egui::RichText::new("\u{d7}").font(theme::font_tiny()).color(egui::Color32::from_rgb(0xe0, 0x68, 0x5f));
                                        if ui.add(egui::Button::new(x).frame(false).min_size(Vec2::new(18.0, 22.0))).clicked() {
                                            delete = Some((gkey.clone(), kw.clone()));
                                        }
                                    });
                                });
                            }
                        });
                    }
                });
            });
            separator(ui);
            egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if plain_btn(ui, "Done", theme::text_primary(), 14.0) {
                        open = false;
                    }
                    ui.add_space(6.0);
                    if save_btn(ui, "Save") {
                        save = true;
                    }
                });
            });
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                save = true;
            }
        });
        if resp.should_close() {
            open = false;
        }
        let mut changed = false;
        if let Some((gk, kw)) = delete {
            if let Some(i) = prefs.keyword_groups.iter().position(|(k, _)| *k == gk) {
                let g = &mut prefs.keyword_groups[i].1;
                g.keywords.retain(|k| *k != kw);
                // the user removed the last keyword: the one place a group is really deleted
                if g.keywords.is_empty() {
                    prefs.keyword_groups.remove(i);
                }
            }
            lib.recompute_categories(prefs);
            prefs.save();
            changed = true;
        }
        if save && self.save(prefs, lib) {
            changed = true;
        }
        if changed {
            lib.visible_count = crate::library::VISIBLE_START;
        }
        (open, changed)
    }

    fn save(&mut self, prefs: &mut Prefs, lib: &mut Library) -> bool {
        let kw_raw = self.kw.trim().to_owned();
        let cat_raw = self.cat.trim().to_owned();
        if kw_raw.is_empty() || cat_raw.is_empty() {
            self.err = "Both fields are required.".into();
            return false;
        }
        let kw_norm = normalize_game(&kw_raw);
        if kw_norm.is_empty() {
            self.err = "Keyword must contain letters or numbers.".into();
            return false;
        }
        // a keyword lives in one category only: detach it first
        for (_, g) in prefs.keyword_groups.iter_mut() {
            g.keywords.retain(|k| *k != kw_norm);
        }
        // match the typed category by name: keyword groups first, then auto-detected ones
        let want = cat_raw.to_lowercase();
        let mut cat_key = prefs.keyword_groups.iter().find(|(_, g)| g.label.to_lowercase() == want).map(|(k, _)| k.clone());
        if cat_key.is_none() {
            cat_key = lib.cats.norm_map.iter().find(|(k, v)| k.as_str() != NONE_KEY && v.to_lowercase() == want).map(|(k, _)| k.clone());
        }
        let target = cat_key.unwrap_or_else(|| normalize_game(&cat_raw));
        if let Some(g) = prefs.group_mut(&target) {
            if !g.keywords.contains(&kw_norm) {
                g.keywords.push(kw_norm);
            }
        } else {
            // new group; reuses an auto-detected key so its clips keep matching
            prefs.keyword_groups.push((target.clone(), KeywordGroup { label: cat_raw, keywords: vec![kw_norm], extra: Default::default() }));
            prefs.set_rename(&target, None);
        }
        lib.recompute_categories(prefs);
        prune_game_filter(prefs, lib);
        prefs.save();
        self.err.clear();
        self.kw.clear();
        self.cat.clear();
        self.focus = true;
        true
    }
}
