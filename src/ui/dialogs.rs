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
            ui.set_max_width(BODY_W + 48.0);
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
            ui.set_max_width(BODY_W + 48.0);
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

// ---------------------------------------------------------------------------------------------
// Confirm dialog (Python `_dark_dialog`, kind="yesno")
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    Yes,
    Extra,
    Extra2,
    No,
}

pub struct ConfirmDialog {
    pub title: String,
    pub head: String,
    pub details: Vec<String>,
    pub note: String,
    pub yes_text: String,
    /// Optional dark red button left of the confirm button (result `Extra`).
    pub extra_text: Option<String>,
    /// Optional button left of that (result `Extra2`), neutral or dark red.
    pub extra2_text: Option<String>,
    pub extra2_neutral: bool,
    /// "+ N more" under the details.
    pub more: usize,
    /// kind="error": amber, only an OK button.
    pub error: bool,
    /// Optional checkbox under the note (label, state).
    pub check: Option<(String, std::cell::Cell<bool>)>,
}

impl Default for ConfirmDialog {
    fn default() -> Self {
        ConfirmDialog { title: String::new(), head: String::new(), details: Vec::new(), note: String::new(), yes_text: "Delete".into(), extra_text: None, extra2_text: None, extra2_neutral: false, more: 0, error: false, check: None }
    }
}

impl ConfirmDialog {
    /// Returns Some(choice) once a button (or Escape / the x) was used.
    pub fn show(&self, ctx: &egui::Context) -> Option<Confirm> {
        let red = egui::Color32::from_rgb(0xe8, 0x45, 0x4a);
        let amber = egui::Color32::from_rgb(0xe8, 0xc8, 0x4a);
        let tone = if self.error { amber } else { red };
        let mut res = None;
        let frame = egui::Frame::new().fill(theme::card_bg()).stroke(egui::Stroke::new(1.0_f32, theme::border())).corner_radius(8).inner_margin(egui::Margin::ZERO);
        let resp = egui::Modal::new(egui::Id::new("confirm_dialog")).frame(frame).show(ctx, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            // dialog width: the body (icon + 340 text column) or the button row, whichever is wider,
            // so the title bar and accent line always span the whole dialog
            let btn_w = |ui: &egui::Ui, t: &str, font: egui::FontId, padx: f32| widgets::text_width(ui, t, &font) + padx * 2.0;
            let footer_w = if self.error {
                btn_w(ui, "OK", theme::font_ui(), 18.0)
            } else {
                let mut w = btn_w(ui, "Cancel", theme::font_ui(), 16.0) + 8.0 + btn_w(ui, &self.yes_text, theme::font_bold(), 18.0);
                for t in [&self.extra_text, &self.extra2_text].into_iter().flatten() {
                    w += 8.0 + btn_w(ui, t, theme::font_bold(), 16.0);
                }
                w
            } + 32.0;
            let dw = footer_w.max(446.0).ceil();
            let col_w = dw - 106.0; // minus 2x22 margin, 46 icon, 16 gap
            ui.set_max_width(dw);
            // title bar like the main window: BG + accent line
            let (bar, _) = ui.allocate_exact_size(Vec2::new(dw, 37.0), egui::Sense::hover());
            ui.painter().rect_filled(bar, egui::CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 }, theme::bg());
            ui.painter().text(bar.left_center() + Vec2::new(16.0, 0.0), egui::Align2::LEFT_CENTER, &self.title, theme::font_bold(), theme::text_primary());
            let xr = egui::Rect::from_center_size(bar.right_center() - Vec2::new(6.0 + 18.5, 0.0), Vec2::new(37.0, 29.0));
            let x = ui.interact(xr, ui.id().with("x"), egui::Sense::click());
            if x.hovered() {
                ui.painter().rect_filled(xr, 6.0, theme::mix(theme::bg(), red, 0.24));
            }
            super::icons::paint(ui, xr.center(), "x", 14.0, if x.hovered() { egui::Color32::from_rgb(0xff, 0x6b, 0x6b) } else { theme::text_muted() }, false, 1.9);
            if x.clicked() {
                res = Some(Confirm::No);
            }
            let (line, _) = ui.allocate_exact_size(Vec2::new(dw, 2.0), egui::Sense::hover());
            ui.painter().rect_filled(line, 0.0, theme::accent());
            egui::Frame::new().inner_margin(egui::Margin::symmetric(22, 20)).show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    let (b, _) = ui.allocate_exact_size(Vec2::splat(46.0), egui::Sense::hover());
                    ui.painter().rect_filled(b, 14.0, theme::mix(theme::card_bg(), tone, 0.45));
                    ui.painter().rect_filled(b.shrink(1.0), 13.0, theme::mix(theme::card_bg(), tone, 0.16));
                    let icon = if !self.error && self.yes_text == "Delete" { "trash" } else { "alert" };
                    super::icons::paint(ui, b.center(), icon, 22.0, tone, false, 1.9);
                    ui.add_space(16.0);
                    ui.vertical(|ui| {
                        ui.set_width(col_w);
                        ui.add(egui::Label::new(egui::RichText::new(&self.head).font(egui::FontId::new(11.0 * 96.0 / 72.0, theme::bold_family())).color(theme::text_primary())).wrap());
                        if !self.details.is_empty() {
                            ui.add_space(12.0);
                            egui::Frame::new().fill(c("#161628")).stroke(egui::Stroke::new(1.0_f32, theme::border())).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
                                ui.set_width(col_w - 24.0);
                                for d in &self.details {
                                    let t = widgets::ellipsize(ui, d, &theme::font_ui(), col_w - 30.0, true);
                                    ui.label(egui::RichText::new(t).font(theme::font_ui()).color(theme::text_muted()));
                                }
                                if self.more > 0 {
                                    ui.add_space(3.0);
                                    ui.label(egui::RichText::new(format!("+ {} more", self.more)).font(theme::font_tiny()).color(theme::text_dim()));
                                }
                            });
                        }
                        if !self.note.is_empty() {
                            ui.add_space(12.0);
                            ui.add(egui::Label::new(egui::RichText::new(&self.note).font(theme::font_ui()).color(theme::text_dim())).wrap());
                        }
                        if let Some((label, on)) = &self.check {
                            ui.add_space(12.0);
                            let font = theme::font_ui();
                            let w = (26.0 + widgets::text_width(ui, label, &font)).min(col_w);
                            let (r, resp) = ui.allocate_exact_size(Vec2::new(w, 20.0), egui::Sense::click());
                            if resp.clicked() {
                                on.set(!on.get());
                            }
                            let b = egui::Rect::from_center_size(egui::pos2(r.left() + 8.0, r.center().y), Vec2::splat(16.0));
                            let p = ui.painter();
                            if on.get() {
                                p.rect_filled(b, 5.0, theme::accent());
                                super::icons::paint(ui, b.center(), "check", 12.0, theme::accent_fg(), false, 2.6);
                            } else {
                                let border = if resp.hovered() { theme::accent() } else { c("#52527e") };
                                p.rect(b, 5.0, c("#242444"), egui::Stroke::new(1.0_f32, border), egui::StrokeKind::Inside);
                            }
                            let fg = if resp.hovered() || on.get() { theme::text_primary() } else { theme::text_muted() };
                            let t = widgets::ellipsize(ui, label, &font, col_w - 26.0, true);
                            ui.painter().text(egui::pos2(r.left() + 26.0, r.center().y), egui::Align2::LEFT_CENTER, t, font, fg);
                            if resp.hovered() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                        }
                    });
                });
            });
            let (d, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), egui::Sense::hover());
            ui.painter().rect_filled(d, 0.0, theme::divider());
            egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.error {
                        if rounded_button(ui, "OK", theme::text_primary(), c("#242444"), c("#2A2A4A"), theme::font_ui(), 18.0, 7.0).clicked() {
                            res = Some(Confirm::No);
                        }
                        return;
                    }
                    if rounded_button(ui, "Cancel", theme::text_primary(), c("#242444"), c("#2A2A4A"), theme::font_ui(), 16.0, 7.0).clicked() {
                        res = Some(Confirm::No);
                    }
                    ui.add_space(8.0);
                    if rounded_button(ui, &self.yes_text, egui::Color32::WHITE, egui::Color32::from_rgb(0xc9, 0x35, 0x3c), red, theme::font_bold(), 18.0, 7.0).clicked() {
                        res = Some(Confirm::Yes);
                    }
                    let dark_red = (egui::Color32::from_rgb(0x8f, 0x2a, 0x30), egui::Color32::from_rgb(0xc9, 0x35, 0x3c));
                    if let Some(t) = &self.extra_text {
                        ui.add_space(8.0);
                        if rounded_button(ui, t, egui::Color32::WHITE, dark_red.0, dark_red.1, theme::font_bold(), 16.0, 7.0).clicked() {
                            res = Some(Confirm::Extra);
                        }
                    }
                    if let Some(t) = &self.extra2_text {
                        ui.add_space(8.0);
                        let (b, h) = if self.extra2_neutral { (c("#2f2f58"), c("#3a3a6a")) } else { dark_red };
                        if rounded_button(ui, t, egui::Color32::WHITE, b, h, theme::font_bold(), 16.0, 7.0).clicked() {
                            res = Some(Confirm::Extra2);
                        }
                    }
                });
            });
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                res = Some(if self.error { Confirm::No } else { Confirm::Yes });
            }
        });
        if res.is_none() && resp.should_close() {
            res = Some(Confirm::No);
        }
        res
    }
}

// ---------------------------------------------------------------------------------------------
// Update window (owner 2026-11-28): download progress, then "installing" until the installer
// closes the app
// ---------------------------------------------------------------------------------------------

pub enum UpdateAct {
    Cancel,
    Retry,
    Close,
}

pub fn update_window(ctx: &egui::Context, info: &crate::update::Info) -> Option<UpdateAct> {
    use crate::update::Phase;
    let mut act = None;
    let frame = egui::Frame::new().fill(theme::card_bg()).stroke(egui::Stroke::new(1.0_f32, theme::border())).corner_radius(10).inner_margin(egui::Margin::ZERO);
    egui::Modal::new(egui::Id::new("update_window")).frame(frame).show(ctx, |ui| {
        let w = 400.0;
        ui.set_max_width(w);
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        // title bar like the other dialogs: BG + accent line
        let (bar, _) = ui.allocate_exact_size(Vec2::new(w, 37.0), egui::Sense::hover());
        ui.painter().rect_filled(bar, egui::CornerRadius { nw: 10, ne: 10, sw: 0, se: 0 }, theme::bg());
        ui.painter().text(bar.left_center() + Vec2::new(16.0, 0.0), egui::Align2::LEFT_CENTER, "Updating Feather Player", theme::font_bold(), theme::text_primary());
        let (line, _) = ui.allocate_exact_size(Vec2::new(w, 2.0), egui::Sense::hover());
        ui.painter().rect_filled(line, 0.0, theme::accent());
        egui::Frame::new().inner_margin(egui::Margin::symmetric(20, 18)).show(ui, |ui| {
            ui.set_width(w - 40.0);
            let (head, frac, detail, detail_col): (String, Option<f32>, String, egui::Color32) = match &info.phase {
                Phase::Downloading(got, total) => {
                    let mb = |b: u64| b as f64 / 1024.0 / 1024.0;
                    let f = if *total > 0 { (*got as f32 / *total as f32).clamp(0.0, 1.0) } else { 0.0 };
                    let d = if *total > 0 { format!("{:.0} %   {:.1} / {:.1} MB", f * 100.0, mb(*got), mb(*total)) } else { format!("{:.1} MB", mb(*got)) };
                    (format!("Downloading version {}\u{2026}", info.latest), Some(f), d, theme::text_muted())
                }
                Phase::Ready(_) | Phase::Installing => (
                    format!("Installing version {}\u{2026}", info.latest),
                    None,
                    "Feather Player closes and the new version opens by itself in a moment.".to_owned(),
                    theme::text_muted(),
                ),
                Phase::Error(e) => ("The update didn't work".to_owned(), Some(0.0), e.clone(), theme::ERROR),
                _ => (String::new(), Some(0.0), String::new(), theme::text_muted()),
            };
            ui.label(egui::RichText::new(head).font(theme::font_ui()).color(theme::text_primary()));
            ui.add_space(12.0);
            // progress bar (None = installing: an accent block sliding back and forth)
            let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 10.0), egui::Sense::hover());
            let p = ui.painter();
            p.rect_filled(r, 5.0, theme::bar_track());
            match frac {
                Some(f) if f > 0.0 => {
                    let fill = egui::Rect::from_min_max(r.min, egui::pos2(r.left() + r.width() * f, r.bottom()));
                    p.rect_filled(fill, 5.0, theme::accent());
                }
                Some(_) => {}
                None => {
                    let t = ui.input(|i| i.time) as f32;
                    let seg = r.width() * 0.3;
                    let x = r.left() + (r.width() - seg) * (0.5 + 0.5 * (t * 2.2).sin());
                    p.rect_filled(egui::Rect::from_min_max(egui::pos2(x, r.top()), egui::pos2(x + seg, r.bottom())), 5.0, theme::accent());
                    ui.ctx().request_repaint();
                }
            }
            ui.add_space(8.0);
            ui.add(egui::Label::new(egui::RichText::new(detail).font(theme::font_tiny()).color(detail_col)).wrap());
        });
        // buttons
        let buttons: &[(&str, u8)] = match info.phase {
            Phase::Downloading(..) => &[("Cancel", 0)],
            Phase::Error(_) => &[("Close", 2), ("Try again", 1)],
            _ => &[],
        };
        if !buttons.is_empty() {
            let (d, _) = ui.allocate_exact_size(Vec2::new(w, 1.0), egui::Sense::hover());
            ui.painter().rect_filled(d, 0.0, theme::divider());
            egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
                ui.set_width(w - 32.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    for (i, (text, id)) in buttons.iter().rev().enumerate() {
                        if i > 0 {
                            ui.add_space(8.0);
                        }
                        let accent = *id == 1;
                        let (fg, bg, hov) = if accent {
                            (theme::accent_fg(), theme::accent(), theme::mix(theme::accent(), egui::Color32::WHITE, 0.15))
                        } else {
                            (theme::text_primary(), c("#242444"), c("#2A2A4A"))
                        };
                        if rounded_button(ui, text, fg, bg, hov, if accent { theme::font_bold() } else { theme::font_ui() }, 16.0, 7.0).clicked() {
                            act = Some(match id {
                                0 => UpdateAct::Cancel,
                                1 => UpdateAct::Retry,
                                _ => UpdateAct::Close,
                            });
                        }
                    }
                });
            });
        }
    });
    act
}

// ---------------------------------------------------------------------------------------------
// Share (Python `_open_share_dialog`)
// ---------------------------------------------------------------------------------------------

const SHARE_SIZES: [(&str, Option<f64>); 2] = [("Original", None), ("10 MB", Some(9.5))];

pub struct ShareDialog {
    src: String,
    t_start: f64,
    t_end: f64,
    ext: String,
    ffmpeg: std::path::PathBuf,
    snap: crate::tracks::AudioSnap,
    thumb: Option<egui::TextureHandle>,
    selected: usize,
    name: String,
    default_stem: String,
    ctl: crate::media::ShareCtl,
    /// The finished file got the typed name (Python `_finish` -> `_apply_name`).
    named: bool,
    /// "Copied!" / "Failed" on the copy button until then.
    copy_msg: Option<(&'static str, std::time::Instant)>,
}

/// Python `_safe_stem`: no characters Windows forbids, max 150, not a reserved device name.
fn safe_stem(typed: &str, default_stem: &str) -> String {
    let clean = |s: &str| -> String { s.chars().filter(|c| !"<>:\"/\\|?*".contains(*c) && (*c as u32) >= 0x20).collect::<String>().trim().to_owned() };
    let mut st: String = clean(typed).chars().take(150).collect();
    st = st.trim_end_matches(['.', ' ']).to_owned();
    if st.is_empty() {
        st = clean(default_stem).trim_end_matches(['.', ' ']).to_owned();
        if st.is_empty() {
            st = "clip".into();
        }
    }
    let first = st.split('.').next().unwrap_or("").to_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&first.as_str()) || ((first.starts_with("COM") || first.starts_with("LPT")) && first.len() == 4 && matches!(first.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        st.push('_');
    }
    st
}

#[cfg(test)]
mod share_tests {
    use super::safe_stem;

    #[test]
    fn safe_names() {
        assert_eq!(safe_stem("my: clip?.", "d"), "my clip");
        assert_eq!(safe_stem("   ", "Apex 2024"), "Apex 2024");
        assert_eq!(safe_stem("<>", "??"), "clip");
        assert_eq!(safe_stem("con", "d"), "con_");
        assert_eq!(safe_stem("COM3.part", "d"), "COM3.part_");
        assert_eq!(safe_stem("COM0", "d"), "COM0");
        assert_eq!(safe_stem(&"a".repeat(200), "d").len(), 150);
    }
}

impl ShareDialog {
    /// Opens and starts the "Original" encode right away, like Python.
    pub fn new(ctx: &egui::Context, src: String, range: (f64, f64), ffmpeg: std::path::PathBuf, snap: crate::tracks::AudioSnap, thumb: Option<egui::TextureHandle>) -> ShareDialog {
        let name = scanner::basename(&src);
        let (stem, ext) = match name.rfind('.') {
            Some(i) if i > 0 => (&name[..i], name[i..].to_lowercase()),
            _ => (name, String::new()),
        };
        // shared clips are always mp4 (an .mkv source is remuxed)
        let ext = if ext.is_empty() || ext == ".mkv" { ".mp4".to_owned() } else { ext };
        let default_stem = if stem.is_empty() { "clip".to_owned() } else { stem.to_owned() };
        let mut d = ShareDialog {
            src,
            t_start: range.0,
            t_end: range.1,
            ext,
            ffmpeg,
            snap,
            thumb,
            selected: 0,
            name: default_stem.clone(),
            default_stem,
            ctl: crate::media::ShareCtl::default(),
            named: false,
            copy_msg: None,
        };
        d.start(ctx);
        d
    }

    fn start(&mut self, ctx: &egui::Context) {
        if self.ctl.state.lock().unwrap().encoding {
            return;
        }
        self.named = false;
        self.copy_msg = None;
        let job = crate::media::ShareJob {
            src: self.src.clone(),
            t_start: self.t_start,
            t_end: self.t_end,
            mb_lim: SHARE_SIZES[self.selected].1,
            ext: self.ext.clone(),
            ffmpeg: self.ffmpeg.clone(),
            snap: self.snap.clone(),
        };
        crate::media::start_share(job, self.ctl.clone(), ctx.clone());
    }

    /// Python `_apply_name`: rename the finished file to the typed name (it is alone in its
    /// own temp folder, so the name can't clash). Returns the file's path.
    fn apply_name(&mut self) -> Option<String> {
        let mut st = self.ctl.state.lock().unwrap();
        let cur = st.output.clone()?;
        if st.encoding || !std::path::Path::new(&cur).exists() {
            return Some(cur);
        }
        let dir = std::path::Path::new(&cur).parent()?.to_path_buf();
        let new = dir.join(format!("{}{}", safe_stem(&self.name, &self.default_stem), self.ext)).to_string_lossy().into_owned();
        if new != cur && std::fs::rename(&cur, &new).is_ok() {
            st.output = Some(new.clone());
            return Some(new);
        }
        Some(cur)
    }

    /// Kill the encode and delete the temp file (app exit).
    pub fn close(&mut self) {
        self.ctl.close();
    }

    /// Returns false once closed.
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        let (phase, status, progress, encoding, has_output) = {
            let st = self.ctl.state.lock().unwrap();
            (st.phase, st.status.clone(), st.progress, st.encoding, st.output.is_some())
        };
        if phase == crate::media::SharePhase::Ready && !self.named {
            self.named = true;
            self.apply_name();
        }
        let mut open = true;
        let tw = 280.0;
        let th = (tw * 9.0 / 16.0_f32).floor();
        let resp = egui::Modal::new(egui::Id::new("share_dialog")).frame(dialog_frame()).show(ctx, |ui| {
            // the right-to-left footer would otherwise stretch the modal to egui's default area width
            ui.set_max_width(tw + 56.0);
            egui::Frame::new().inner_margin(egui::Margin::symmetric(28, 22)).show(ui, |ui| {
                ui.set_width(tw);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("SHARE ON DISCORD").font(theme::font_title()).color(theme::text_primary()));
                    ui.add_space(14.0);
                    ui.label(egui::RichText::new("Choose a File Size").font(theme::font_ui()).color(theme::text_primary()));
                    ui.add_space(6.0);
                });
                let bw: f32 = SHARE_SIZES.iter().map(|(l, _)| widgets::text_width(ui, l, &theme::font_ui()) + 24.0 + 6.0).sum::<f32>() - 6.0;
                ui.horizontal(|ui| {
                    ui.add_space(((tw - bw) / 2.0).max(0.0));
                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 0.0);
                    for (i, (label, _)) in SHARE_SIZES.iter().enumerate() {
                        let sel = self.selected == i;
                        let (fg, bg) = if sel { (theme::accent_fg(), theme::accent()) } else { (theme::text_primary(), c("#242444")) };
                        let w = widgets::text_width(ui, label, &theme::font_ui()) + 24.0;
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(w, 26.0), egui::Sense::click());
                        ui.painter().rect_filled(r, 0.0, bg);
                        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, *label, theme::font_ui(), fg);
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if resp.clicked() {
                            // Python `_select_size`: the choice changes, the encode only
                            // restarts when none is running
                            self.selected = i;
                            self.start(ui.ctx());
                        }
                    }
                });
                ui.add_space(14.0);
                // thumbnail card + progress line + status
                egui::Frame::new().fill(c("#1A1A2E")).show(ui, |ui| {
                    ui.set_width(tw);
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    let (r, _) = ui.allocate_exact_size(Vec2::new(tw, th), egui::Sense::hover());
                    ui.painter().rect_filled(r, 0.0, c("#0F0F1A"));
                    match &self.thumb {
                        Some(t) => {
                            ui.painter().image(t.id(), r, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                        }
                        None => {
                            ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "No preview", theme::font_pt(11.0), theme::text_dim());
                        }
                    }
                    let (pr, _) = ui.allocate_exact_size(Vec2::new(tw, 3.0), egui::Sense::hover());
                    ui.painter().rect_filled(pr, 0.0, c("#13131f"));
                    if let Some(f) = progress {
                        ui.painter().rect_filled(egui::Rect::from_min_size(pr.min, Vec2::new(tw * f.min(1.0), 3.0)), 0.0, theme::accent());
                    }
                    let col = match phase {
                        crate::media::SharePhase::Ready => c("#FFFFFF"),
                        crate::media::SharePhase::Error => c("#e05555"),
                        crate::media::SharePhase::Encoding => theme::text_muted(),
                    };
                    let (sr, _) = ui.allocate_exact_size(Vec2::new(tw, 45.0), egui::Sense::hover());
                    ui.painter().text(sr.center(), egui::Align2::CENTER_CENTER, &status, theme::font_tiny(), col);
                });
                ui.add_space(10.0);
                // file name: editable while and after encoding; the type is fixed
                let mut commit = false;
                let mut focused = false;
                let box_resp = egui::Frame::new().fill(c("#161628")).stroke(egui::Stroke::new(1.0_f32, c("#2A2A4A"))).inner_margin(egui::Margin { left: 10, right: 8, top: 7, bottom: 7 }).show(ui, |ui| {
                    ui.set_width(tw - 18.0);
                    ui.horizontal(|ui| {
                        let ext_w = widgets::text_width(ui, &self.ext, &theme::font_ui()) + 8.0;
                        let te = ui.add(egui::TextEdit::singleline(&mut self.name).font(theme::font_ui()).text_color(theme::text_primary()).frame(false).desired_width(tw - 18.0 - ext_w - 8.0));
                        focused = te.has_focus();
                        if te.lost_focus() {
                            commit = true; // Enter or clicking away
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(egui::RichText::new(&self.ext).font(theme::font_ui()).color(theme::text_dim()));
                        });
                    });
                });
                if focused {
                    ui.painter().rect_stroke(box_resp.response.rect, 0.0, egui::Stroke::new(1.0_f32, theme::accent()), egui::StrokeKind::Inside);
                }
                if commit {
                    self.name = safe_stem(&self.name, &self.default_stem);
                    self.apply_name();
                }
                ui.add_space(10.0);
                // copy (+ retry after a failure)
                let err = phase == crate::media::SharePhase::Error;
                let copy_text = match self.copy_msg {
                    Some((t, until)) if std::time::Instant::now() < until => {
                        ui.ctx().request_repaint_after(until - std::time::Instant::now());
                        t
                    }
                    _ => "Copy to clipboard",
                };
                let retry_w = if err { widgets::text_width(ui, "\u{21ba}  Retry", &theme::font_ui()) + 28.0 + 6.0 } else { 0.0 };
                ui.horizontal(|ui| {
                    ui.add_space(((tw - 160.0 - retry_w) / 2.0).max(0.0));
                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 0.0);
                    let (r, resp) = ui.allocate_exact_size(Vec2::new(160.0, 30.0), egui::Sense::click());
                    ui.painter().rect_filled(r, 7.0, if resp.hovered() { c("#2A2A4A") } else { c("#242444") });
                    ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, copy_text, theme::font_ui(), theme::text_primary());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        // Python `_do_copy`: the shared file once it exists, else the clip itself
                        let path = if has_output { self.apply_name().unwrap_or_else(|| self.src.clone()) } else { self.src.clone() };
                        let ok = crate::window::copy_file_to_clipboard(&path.replace('/', "\\"));
                        self.copy_msg = Some((if ok { "\u{2713}  Copied!" } else { "\u{2717}  Failed" }, std::time::Instant::now() + std::time::Duration::from_secs(2)));
                    }
                    if err && plain_btn(ui, "\u{21ba}  Retry", theme::text_primary(), 14.0) {
                        self.start(ui.ctx());
                    }
                });
            });
            let _ = encoding;
            separator(ui);
            egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 10)).show(ui, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if plain_btn(ui, "Close", theme::text_primary(), 14.0) {
                        open = false;
                    }
                });
            });
        });
        if resp.should_close() {
            open = false;
        }
        if !open {
            self.ctl.close();
        }
        open
    }
}

// ---------------------------------------------------------------------------------------------
// Compress (Python `_open_compress_dialog`)
// ---------------------------------------------------------------------------------------------

pub const SIZE_OPTIONS: [(&str, u32); 3] = [("10 MB", 10), ("20 MB", 20), ("50 MB", 50)];

pub struct CompressDialog {
    pub targets: Vec<String>,
    pub thumb: Option<egui::TextureHandle>,
    pub selected: Option<usize>,
    pub encoding: bool,
    pub progress: Option<f32>,
    pub status: String,
    pub status_color: egui::Color32,
    /// Per clip in a batch: true = done, false = failed (missing = waiting).
    pub done: std::collections::HashMap<String, bool>,
}

pub enum CompressAction {
    Start(u32),
    Close,
}

impl CompressDialog {
    pub fn new(targets: Vec<String>, thumb: Option<egui::TextureHandle>) -> Self {
        CompressDialog { targets, thumb, selected: None, encoding: false, progress: None, status: String::new(), status_color: theme::text_muted(), done: Default::default() }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<CompressAction> {
        let mut act = None;
        let n = self.targets.len();
        let tw = 280.0;
        let resp = egui::Modal::new(egui::Id::new("compress_dialog")).frame(dialog_frame()).show(ctx, |ui| {
            ui.set_max_width(tw + 56.0);
            egui::Frame::new().inner_margin(egui::Margin::symmetric(28, 22)).show(ui, |ui| {
                ui.set_width(tw);
                ui.vertical_centered(|ui| {
                    let title = if n == 1 { "COMPRESS CLIP".to_owned() } else { format!("COMPRESS {n} CLIPS") };
                    ui.label(egui::RichText::new(title).font(theme::font_title()).color(theme::text_primary()));
                    ui.add_space(14.0);
                    ui.label(egui::RichText::new("Choose a Target Size").font(theme::font_ui()).color(theme::text_primary()));
                    if n > 1 {
                        ui.label(egui::RichText::new("Applies to every selected clip below.").font(theme::font_tiny()).color(theme::text_dim()));
                    }
                    ui.add_space(6.0);
                });
                let bw: f32 = SIZE_OPTIONS.iter().map(|(l, _)| widgets::text_width(ui, l, &theme::font_ui()) + 28.0 + 6.0).sum::<f32>() - 6.0;
                ui.horizontal(|ui| {
                    ui.add_space(((tw - bw) / 2.0).max(0.0));
                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 0.0);
                    for (i, (label, mb)) in SIZE_OPTIONS.iter().enumerate() {
                        let sel = self.selected == Some(i);
                        let (fg, bg) = if sel { (theme::accent_fg(), theme::accent()) } else { (theme::text_primary(), c("#242444")) };
                        let w = widgets::text_width(ui, label, &theme::font_ui()) + 28.0;
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(w, 28.0), egui::Sense::click());
                        ui.painter().rect_filled(r, 0.0, bg);
                        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, *label, theme::font_ui(), fg);
                        if resp.clicked() && !self.encoding {
                            self.selected = Some(i);
                            act = Some(CompressAction::Start(*mb));
                        }
                    }
                });
                ui.add_space(14.0);
                // preview: the thumbnail, or the list of clips when batching
                egui::Frame::new().fill(c("#1A1A2E")).show(ui, |ui| {
                    ui.set_width(tw);
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    if n == 1 {
                        let th = (tw * 9.0 / 16.0).floor();
                        let (r, _) = ui.allocate_exact_size(Vec2::new(tw, th), egui::Sense::hover());
                        ui.painter().rect_filled(r, 0.0, c("#0F0F1A"));
                        match &self.thumb {
                            Some(t) => {
                                ui.painter().image(t.id(), r, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                            }
                            None => {
                                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "No preview", theme::font_pt(11.0), theme::text_dim());
                            }
                        }
                    } else {
                        egui::Frame::new().fill(c("#0F0F1A")).show(ui, |ui| {
                            ui.set_width(tw);
                            egui::ScrollArea::vertical().max_height(130.0).min_scrolled_height(130.0).auto_shrink([false, false]).show(ui, |ui| {
                                for t in &self.targets {
                                    let name = scanner::basename(t);
                                    let (text, col) = match self.done.get(t) {
                                        Some(true) => (format!("\u{2713}  {name}"), c("#ccc364")),
                                        Some(false) => (format!("\u{2717}  {name}"), c("#e05555")),
                                        None => (name.to_owned(), theme::text_muted()),
                                    };
                                    ui.horizontal(|ui| {
                                        ui.add_space(10.0);
                                        ui.label(egui::RichText::new(text).font(theme::font_tiny()).color(col));
                                    });
                                    ui.add_space(6.0);
                                }
                            });
                        });
                    }
                    // progress line + status
                    let (pr, _) = ui.allocate_exact_size(Vec2::new(tw, 3.0), egui::Sense::hover());
                    ui.painter().rect_filled(pr, 0.0, c("#13131f"));
                    if let Some(f) = self.progress {
                        let r = egui::Rect::from_min_size(pr.min, Vec2::new(tw * f.min(1.0), 3.0));
                        ui.painter().rect_filled(r, 0.0, theme::accent());
                    }
                    ui.add_space(6.0);
                    ui.vertical_centered(|ui| ui.label(egui::RichText::new(&self.status).font(theme::font_tiny()).color(self.status_color)));
                    ui.add_space(6.0);
                });
            });
            separator(ui);
            egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 10)).show(ui, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if plain_btn(ui, "Close", theme::text_primary(), 14.0) {
                        act = Some(CompressAction::Close);
                    }
                });
            });
        });
        if act.is_none() && resp.should_close() {
            act = Some(CompressAction::Close);
        }
        act
    }
}
