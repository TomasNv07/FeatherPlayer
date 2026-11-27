//! Settings page (Python `_build_settings_page`): replaces the main window while open.
//! All of Python's sections: Appearance (theme dropdown, accent colour picker), Starred clips,
//! Cloud sync (+ Share clip info for ClipSync, Rust only), Phone (ClipSync over the network, Rust only), Playback, Audio, Trimming, Clips, Window, and the Keyboard
//! shortcuts / Hidden features cards.

use super::widgets::{self, ellipsize, Ghost, Row};
use crate::library::{Library, NONE_KEY};
use crate::lan::Lan;
use crate::sync::{cloud_kind, Cloud, DotKind};
use crate::theme::{self, c};
use crate::tracks::AudioTracks;
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Vec2};

pub enum SettingsAction {
    Close,
    /// "Update now" / "Try again" in the update block.
    Update,
    StarCopy(bool),
    ChooseStarFolder,
    CloudToggle(bool),
    ChooseCloudFolder,
    CloudMode(&'static str),
    CloudShareInfo(bool),
    LanToggle(bool),
    LanNewCode,
    CloudGame(String),
    CloudSync,
    CloudDeleteAll,
    Loop(bool),
    StartAlways(bool),
    TrimGpu(bool),
    RecycleBin(bool),
    RememberSize(bool),
    CloseToTray(bool),
    StartWithWindows(bool),
    AudioEnabled(bool),
    DefaultTrack(usize),
    /// Default volume of track n: (value, slider released).
    DefaultVol(usize, f64, bool),
    Theme(String),
    /// Custom accent for the active theme ("#rrggbb"), None = the theme's own.
    Accent(Option<String>),
    AutoRefresh(bool),
    /// "Load more" step one choice down (-1) or up (+1).
    SeeMore(i32),
    ClearCache,
}

/// Values the page shows (the rest is read from `Cloud`).
pub struct SettingsValues<'a> {
    pub star_copy: bool,
    pub star_folder: &'a str,
    pub loop_clip: bool,
    pub start_always: bool,
    pub trim_gpu: bool,
    pub recycle_bin: bool,
    pub remember_size: bool,
    pub close_to_tray: bool,
    pub start_with_windows: bool,
    pub audio: &'a AudioTracks,
    pub auto_refresh: bool,
    pub see_more_step: usize,
    /// Thumbnail cache line ("Calculating…", "N thumbnails • size for the current folder").
    pub cache_info: &'a str,
    /// Update check result (block at the top while an update is known).
    pub update: &'a crate::update::Info,
}

#[derive(Default)]
pub struct SettingsPage {
    games_popup: Option<Rect>,
    /// Theme dropdown open, anchored to this field.
    theme_popup: Option<Rect>,
    accent: Option<AccentDialog>,
}

/// Python `_open_accent_dialog`: saturation/value square, hue bar, hex entry, preview.
struct AccentDialog {
    theme: String,
    default: String,
    h: f64,
    s: f64,
    v: f64,
    hex: String,
    /// SV square texture and the hue it was drawn for.
    sv_tex: Option<(f64, egui::TextureHandle)>,
    hue_tex: Option<egui::TextureHandle>,
}

const SVW: usize = 280;
const SVH: usize = 170;

impl AccentDialog {
    fn new(theme: &str) -> AccentDialog {
        let default = theme::default_accent(theme);
        let start = theme::custom_accent(theme).unwrap_or_else(|| default.clone());
        let (r, g, b) = theme::rgb_of(&start).unwrap_or((1.0, 0.0, 0.0));
        let (h, s, v) = theme::rgb_to_hsv(r, g, b);
        AccentDialog { theme: theme.to_owned(), default, h, s, v, hex: start.to_uppercase(), sv_tex: None, hue_tex: None }
    }

    fn hex_now(&self) -> String {
        theme::hex_of(theme::hsv_to_rgb(self.h, self.s, self.v))
    }

    /// The typed hex if valid ("#" optional), else the picked colour.
    fn chosen(&self) -> String {
        let t = self.hex.trim();
        let t = if t.starts_with('#') { t.to_owned() } else { format!("#{t}") };
        if t.len() == 7 && theme::rgb_of(&t).is_some() { t.to_lowercase() } else { self.hex_now() }
    }

    /// Python `_sv_image`: white at the left, the pure hue at the right, black at the bottom.
    fn sv_texture(&mut self, ctx: &egui::Context) -> egui::TextureHandle {
        if let Some((h, t)) = &self.sv_tex {
            if *h == self.h {
                return t.clone();
            }
        }
        let (br, bg, bb) = theme::hsv_to_rgb(self.h, 1.0, 1.0);
        let mut px = Vec::with_capacity(SVW * SVH * 4);
        for y in 0..SVH {
            let dark = 1.0 - y as f64 / (SVH - 1) as f64;
            for x in 0..SVW {
                let k = x as f64 / (SVW - 1) as f64;
                let ch = |base: f64| (((base * k + (1.0 - k)) * dark) * 255.0).round() as u8;
                px.extend_from_slice(&[ch(br), ch(bg), ch(bb), 255]);
            }
        }
        let img = egui::ColorImage::from_rgba_unmultiplied([SVW, SVH], &px);
        let t = ctx.load_texture("accent_sv", img, egui::TextureOptions::LINEAR);
        self.sv_tex = Some((self.h, t.clone()));
        t
    }

    /// Python `_hue_bar_image`.
    fn hue_texture(&mut self, ctx: &egui::Context) -> egui::TextureHandle {
        self.hue_tex
            .get_or_insert_with(|| {
                let px: Vec<u8> = (0..SVW)
                    .flat_map(|i| {
                        let (r, g, b) = theme::hsv_to_rgb(i as f64 / (SVW - 1) as f64, 1.0, 1.0);
                        [(r * 255.0).round() as u8, (g * 255.0).round() as u8, (b * 255.0).round() as u8, 255]
                    })
                    .collect();
                let img = egui::ColorImage::from_rgba_unmultiplied([SVW, 1], &px);
                ctx.load_texture("accent_hue", img, egui::TextureOptions::LINEAR)
            })
            .clone()
    }

    /// Some(Some(colour or None)) when saved / reset, Some(None) when cancelled.
    fn show(&mut self, ctx: &egui::Context) -> Option<Option<Option<String>>> {
        let mut out = None;
        let sv = self.sv_texture(ctx);
        let hue = self.hue_texture(ctx);
        let frame = egui::Frame::new().fill(theme::card_bg()).stroke(egui::Stroke::new(1.0_f32, c("#33335a"))).corner_radius(10).inner_margin(egui::Margin::ZERO);
        let resp = egui::Modal::new(egui::Id::new("accent_dialog")).frame(frame).show(ctx, |ui| {
            // right-to-left rows would otherwise stretch the modal to egui's default area width
            ui.set_max_width(SVW as f32 + 48.0);
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            egui::Frame::new().inner_margin(egui::Margin::symmetric(24, 20)).show(ui, |ui| {
                ui.label(egui::RichText::new(format!("Accent color for {}", self.theme)).font(theme::font_ui()).color(theme::text_primary()));
                ui.add_space(12.0);
                // saturation / value square
                let (r, resp) = ui.allocate_exact_size(Vec2::new(SVW as f32, SVH as f32), Sense::click_and_drag());
                egui::Image::new(&sv).corner_radius(8).paint_at(ui, r);
                if resp.hovered() || resp.dragged() {
                    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if resp.is_pointer_button_down_on() {
                    if let Some(p) = resp.interact_pointer_pos() {
                        self.s = (((p.x - r.left()) / (SVW - 1) as f32) as f64).clamp(0.0, 1.0);
                        self.v = 1.0 - (((p.y - r.top()) / (SVH - 1) as f32) as f64).clamp(0.0, 1.0);
                        self.hex = self.hex_now().to_uppercase();
                    }
                }
                let m = Pos2::new(r.left() + self.s as f32 * (SVW - 1) as f32, r.top() + (1.0 - self.v as f32) * (SVH - 1) as f32);
                ui.painter().circle_stroke(m, 8.0, egui::Stroke::new(1.0_f32, Color32::BLACK));
                ui.painter().circle_stroke(m, 6.5, egui::Stroke::new(2.0_f32, Color32::WHITE));
                ui.add_space(10.0);
                // hue bar
                let (hr, hresp) = ui.allocate_exact_size(Vec2::new(SVW as f32, 22.0), Sense::click_and_drag());
                let bar = Rect::from_center_size(hr.center(), Vec2::new(SVW as f32, 12.0));
                egui::Image::new(&hue).corner_radius(6).paint_at(ui, bar);
                if hresp.hovered() || hresp.dragged() {
                    ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                if hresp.is_pointer_button_down_on() {
                    if let Some(p) = hresp.interact_pointer_pos() {
                        self.h = (((p.x - hr.left()) / (SVW - 1) as f32) as f64).clamp(0.0, 1.0);
                        self.hex = self.hex_now().to_uppercase();
                    }
                }
                let x = (hr.left() + self.h as f32 * (SVW - 1) as f32).clamp(hr.left() + 3.0, hr.right() - 3.0);
                ui.painter().rect_filled(Rect::from_min_max(Pos2::new(x - 3.0, hr.top() + 1.0), Pos2::new(x + 3.0, hr.bottom() - 1.0)), 3.0, Color32::BLACK);
                ui.painter().rect_filled(Rect::from_min_max(Pos2::new(x - 2.0, hr.top() + 2.0), Pos2::new(x + 2.0, hr.bottom() - 2.0)), 2.0, Color32::WHITE);
                ui.add_space(14.0);
                // preview, hex entry, theme default
                ui.horizontal(|ui| {
                    ui.set_width(SVW as f32);
                    let (pr, _) = ui.allocate_exact_size(Vec2::new(44.0, 30.0), Sense::hover());
                    ui.painter().rect_filled(pr, 8.0, c("#52527e"));
                    ui.painter().rect_filled(pr.shrink(1.0), 7.0, theme::raw(&self.hex_now()));
                    ui.add_space(12.0);
                    let before = self.hex.clone();
                    widgets::entry(ui, &mut self.hex, 90.0);
                    if self.hex != before {
                        // Python `on_entry`: a valid hex moves the picker
                        let t = self.hex.trim();
                        let t = if t.starts_with('#') { t.to_owned() } else { format!("#{t}") };
                        if t.len() == 7 {
                            if let Some((r, g, b)) = theme::rgb_of(&t) {
                                let (h, s, v) = theme::rgb_to_hsv(r, g, b);
                                if s > 0.0 && v > 0.0 {
                                    self.h = h;
                                }
                                self.s = s;
                                self.v = v;
                            }
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(format!("Theme default  {}", self.default.to_uppercase())).font(theme::font_tiny()).color(theme::text_dim()));
                    });
                });
            });
            let (d, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
            ui.painter().rect_filled(d, 0.0, c("#2A2A4A"));
            egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    if widgets::rounded_button(ui, "Use theme default", theme::text_muted(), c("#242444"), c("#2A2A4A"), theme::font_ui(), 10.0, 6.0).clicked() {
                        out = Some(Some(None));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::rounded_button(ui, "Cancel", theme::text_primary(), c("#242444"), c("#2A2A4A"), theme::font_ui(), 14.0, 6.0).clicked() {
                            out = Some(None);
                        }
                        ui.add_space(6.0);
                        if widgets::rounded_button(ui, "Save", theme::accent_fg(), theme::accent(), c("#FF5C8D"), theme::font_bold(), 14.0, 6.0).clicked() {
                            out = Some(Some(self.save_value()));
                        }
                    });
                });
            });
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                out = Some(Some(self.save_value()));
            }
        });
        if out.is_none() && resp.should_close() {
            out = Some(None);
        }
        out
    }

    /// Python `_save`: the theme's own accent counts as "no custom accent".
    fn save_value(&self) -> Option<String> {
        let chosen = self.chosen();
        (chosen != self.default).then_some(chosen)
    }
}

/// Python `_paint_swatch`: the theme's three colours as small circles.
fn paint_swatch(ui: &egui::Ui, left: f32, cy: f32, name: &str) {
    for (i, col) in theme::swatch_colors(name).into_iter().enumerate() {
        let ctr = Pos2::new(left + 1.0 + i as f32 * 15.0 + 7.0, cy);
        ui.painter().circle_filled(ctr, 7.0, col);
        ui.painter().circle_stroke(ctr, 7.0, egui::Stroke::new(1.0_f32, c("#52527e")));
    }
}

/// Python `_hint_card`: one row per item with the keys in a small box on the right.
fn hint_card(ui: &mut egui::Ui, title: &str, items: &[(&str, &str, &str)]) {
    section(ui, title, |ui| {
        for (i, (keys, label, desc)) in items.iter().enumerate() {
            if i > 0 {
                divider(ui);
            }
            row(ui, label, desc, |ui| {
                egui::Frame::new().fill(c("#161628")).inner_margin(egui::Margin::symmetric(9, 4)).show(ui, |ui| {
                    ui.label(egui::RichText::new(*keys).font(theme::font_tiny()).color(theme::text_primary()));
                });
            });
        }
    });
}

/// Update block at the top of Settings (owner 2026-11-28): accent border, new version, and
/// "Update now" (silent install, see `update`).
fn update_block(ui: &mut egui::Ui, info: &crate::update::Info, acts: &mut Vec<SettingsAction>) {
    use crate::update::Phase;
    egui::Frame::new().fill(theme::card_bg()).stroke(egui::Stroke::new(1.5_f32, theme::accent())).corner_radius(10).inner_margin(egui::Margin::symmetric(16, 14)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            ui.vertical(|ui| {
                ui.label(egui::RichText::new("UPDATE AVAILABLE").font(theme::font_bold()).color(theme::accent()));
                ui.add_space(4.0);
                let line = format!("Feather Player {} is out. You have {}.", info.latest, crate::update::VERSION);
                ui.label(egui::RichText::new(line).font(theme::font_ui()).color(theme::text_primary()));
                let (sub, col) = match &info.phase {
                    Phase::Downloading(..) => ("Downloading the update\u{2026}".to_owned(), theme::text_muted()),
                    Phase::Ready(_) | Phase::Installing => ("Installing \u{2014} Feather Player restarts by itself.".to_owned(), theme::text_muted()),
                    Phase::Error(e) => (e.clone(), theme::ERROR),
                    _ => ("Installs in the background and restarts Feather Player. Your clips and settings stay.".to_owned(), theme::text_dim()),
                };
                ui.add_space(3.0);
                ui.add(egui::Label::new(egui::RichText::new(sub).font(theme::font_tiny()).color(col)).wrap());
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (text, enabled) = match info.phase {
                    Phase::Available => ("Update now", true),
                    Phase::Error(_) => ("Try again", true),
                    Phase::Downloading(..) => ("Downloading\u{2026}", false),
                    _ => ("Installing\u{2026}", false),
                };
                let (bg, hov) = if enabled { (theme::accent(), theme::mix(theme::accent(), Color32::WHITE, 0.15)) } else { (c("#2A2A4A"), c("#2A2A4A")) };
                let fg = if enabled { theme::accent_fg() } else { theme::text_muted() };
                if widgets::rounded_button(ui, text, fg, bg, hov, theme::font_bold(), 16.0, 7.0).clicked() && enabled {
                    acts.push(SettingsAction::Update);
                }
            });
        });
    });
}

/// Python `ToggleSwitch` (42x24), returns true when clicked.
fn toggle(ui: &mut egui::Ui, on: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(42.0, 24.0), Sense::click());
    let t = ui.ctx().animate_bool(resp.id, on);
    let off_track = if resp.hovered() { c("#33335a") } else { c("#2A2A4A") };
    let track = theme::mix(off_track, theme::accent(), t);
    let border = theme::mix(c("#52527e"), theme::accent(), t);
    let p = ui.painter();
    p.rect_filled(rect, 12.0, border);
    p.rect_filled(rect.shrink(1.0), 11.0, track);
    let d = 24.0 - 8.0;
    let x = rect.left() + 4.0 + t * (42.0 - 8.0 - d);
    p.circle_filled(Pos2::new(x + d / 2.0, rect.top() + 4.0 + d / 2.0), d / 2.0, theme::mix(theme::text_muted(), Color32::WHITE, t));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.clicked()
}

/// Glowing status dot (Python `_cloud_dot_img`).
fn status_dot(ui: &mut egui::Ui, kind: DotKind) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
    let col = match kind {
        DotKind::Ok => Color32::from_rgb(0x35, 0xd0, 0x7f),
        DotKind::Bad => Color32::from_rgb(0xff, 0x4d, 0x5a),
        DotKind::Busy => Color32::from_rgb(0xf5, 0xb9, 0x42),
        DotKind::Off => Color32::from_rgb(0x6b, 0x6b, 0x80),
    };
    let p = ui.painter();
    let cpos = rect.center();
    if kind != DotKind::Off {
        for (r, a) in [(9.0, 0.10), (7.5, 0.16), (6.2, 0.24)] {
            p.circle_filled(cpos, r, col.gamma_multiply(a));
        }
    }
    p.circle_filled(cpos, 3.6, col);
    p.circle_filled(cpos - Vec2::new(1.1, 1.3), 1.3, Color32::from_white_alpha(90));
}

fn section(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.label(egui::RichText::new(title).font(theme::font_bold()).color(theme::text_muted()));
    ui.add_space(8.0);
    egui::Frame::new().fill(theme::card_bg()).stroke(egui::Stroke::new(1.0_f32, theme::border())).corner_radius(10).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        add(ui);
    });
    ui.add_space(22.0);
}

/// One settings row: label + description left, the control (added by `right`) on the right.
fn row(ui: &mut egui::Ui, label: &str, desc: &str, right: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 14)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(label).font(theme::font_bold()).color(theme::text_primary()));
                if !desc.is_empty() {
                    ui.add_space(2.0);
                    ui.label(egui::RichText::new(desc).font(theme::font_tiny()).color(theme::text_muted()));
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), right);
        });
    });
}

fn divider(ui: &mut egui::Ui) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(r.shrink2(Vec2::new(16.0, 0.0)), 0.0, theme::divider());
}

/// Small pill (#161628) around a control, like the Python settings buttons.
fn field<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new().fill(c("#161628")).stroke(egui::Stroke::new(1.0_f32, theme::border())).corner_radius(8).inner_margin(egui::Margin::same(3)).show(ui, add).inner
}

/// Status row: glowing dot + text under the label, Sync button on the right.
fn status_row(ui: &mut egui::Ui, text: &str, kind: DotKind) -> bool {
    let mut sync = false;
    egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 14)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(egui::RichText::new("Status").font(theme::font_bold()).color(theme::text_primary()));
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(2.0, 0.0);
                    status_dot(ui, kind);
                    ui.add(egui::Label::new(egui::RichText::new(text).font(theme::font_tiny()).color(theme::text_muted())).wrap());
                });
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                field(ui, |ui| {
                    if Ghost::new("Sync").font(theme::font_ui()).fg(theme::text_primary()).padx(14.0).show(ui).clicked() {
                        sync = true;
                    }
                });
            });
        });
    });
    sync
}

impl SettingsPage {
    pub fn show(&mut self, ctx: &egui::Context, v: &SettingsValues, cloud: &Cloud, lan: &Lan, lib: &Library) -> Vec<SettingsAction> {
        let mut acts = Vec::new();
        let mut games_btn = Rect::NOTHING;
        let mut theme_field = Rect::NOTHING;
        // top bar: back + "Settings"
        egui::TopBottomPanel::top("settings_top").frame(egui::Frame::new().fill(theme::bg()).inner_margin(egui::Margin::symmetric(16, 10))).show_separator_line(false).show(ctx, |ui| {
            ui.horizontal(|ui| {
                widgets::pill_frame().show(ui, |ui| {
                    if Ghost::new("").icon("arrow-left", 18.0).padx(8.0).show(ui).on_hover_text("Back").clicked() {
                        acts.push(SettingsAction::Close);
                    }
                });
                ui.add_space(14.0);
                ui.label(egui::RichText::new("Settings").font(theme::font_title()).color(theme::text_primary()));
            });
        });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::bg()).stroke(egui::Stroke::new(1.0_f32, c("#242444")))).show(ctx, |ui| {
            // owner: egui's 40 px per wheel notch was too slow here; 3x = 120 px
            egui::ScrollArea::vertical().auto_shrink([false, false]).wheel_scroll_multiplier(Vec2::new(1.0, 3.0)).show(ui, |ui| {
                let w = ui.available_width();
                let col_w = (w - 48.0).clamp(300.0, 620.0);
                ui.add_space(26.0);
                ui.horizontal(|ui| {
                    ui.add_space(((w - col_w) / 2.0).max(0.0));
                    ui.vertical(|ui| {
                        ui.set_width(col_w);
                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                        let path_text = |ui: &egui::Ui, p: &str| if p.is_empty() { "No folder chosen yet".to_owned() } else { ellipsize(ui, p, &theme::font_tiny(), 330.0, false) };

                        if v.update.available() {
                            update_block(ui, v.update, &mut acts);
                            ui.add_space(22.0);
                        }

                        let cur = theme::active_name();
                        section(ui, "APPEARANCE", |ui| {
                            row(ui, "Theme", "Choose a color scheme for the app", |ui| {
                                let r = egui::Frame::new().fill(c("#161628")).stroke(egui::Stroke::new(1.0_f32, theme::border())).corner_radius(8).inner_margin(egui::Margin::same(3)).show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        // right to left: name button, swatch
                                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                                        let b = Ghost::new(&cur).font(theme::font_ui()).fg(theme::text_primary()).arrow().width(150.0).show(ui);
                                        let (sw, _) = ui.allocate_exact_size(Vec2::new(47.0, theme::CTRL_H - 4.0), Sense::hover());
                                        paint_swatch(ui, sw.left(), sw.center().y, &cur);
                                        ui.add_space(8.0);
                                        b
                                    })
                                    .inner
                                });
                                theme_field = r.response.rect;
                                if r.inner.clicked() {
                                    self.theme_popup = if self.theme_popup.is_some() { None } else { Some(r.response.rect) };
                                }
                            });
                            divider(ui);
                            let ov = theme::custom_accent(&cur);
                            let desc = if ov.is_some() { format!("Custom accent for {cur}") } else { format!("Using the {cur} theme's own accent") };
                            row(ui, "Accent color", &desc, |ui| {
                                field(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                                        // right to left: [Reset |] hex, swatch
                                        if ov.is_some() {
                                            if Ghost::new("Reset").icon("refresh", 16.0).font(theme::font_ui()).fg(theme::text_primary()).padx(10.0).show(ui).on_hover_text("Go back to the theme's own accent").clicked() {
                                                acts.push(SettingsAction::Accent(None));
                                            }
                                            widgets::pill_divider(ui);
                                        }
                                        let shown = ov.clone().unwrap_or_else(|| theme::default_accent(&cur)).to_uppercase();
                                        if Ghost::new(&shown).font(theme::font_ui()).fg(theme::text_primary()).padx(10.0).show(ui).on_hover_text("Pick a custom accent color").clicked() {
                                            self.accent = Some(AccentDialog::new(&cur));
                                        }
                                        let (sw, _) = ui.allocate_exact_size(Vec2::new(24.0, theme::CTRL_H - 4.0), Sense::hover());
                                        let ctr = Pos2::new(sw.left() + 12.0, sw.center().y);
                                        ui.painter().circle_filled(ctr, 7.0, theme::accent());
                                        ui.painter().circle_stroke(ctr, 7.0, egui::Stroke::new(1.0_f32, c("#52527e")));
                                        ui.add_space(8.0);
                                    });
                                });
                            });
                        });

                        section(ui, "STARRED CLIPS", |ui| {
                            row(ui, "Copy starred clips to a folder", "When you star a clip, a copy is saved to the folder below", |ui| {
                                if toggle(ui, v.star_copy) {
                                    acts.push(SettingsAction::StarCopy(!v.star_copy));
                                }
                            });
                            // owner 2026-11-27: the rows under a sync toggle only show while it is on
                            if !v.star_copy {
                                return;
                            }
                            divider(ui);
                            let desc = path_text(ui, v.star_folder);
                            row(ui, "Destination folder", &desc, |ui| {
                                field(ui, |ui| {
                                    let t = if v.star_folder.is_empty() { "Choose folder" } else { "Change folder" };
                                    if Ghost::new(t).icon("folder", 16.0).font(theme::font_ui()).fg(theme::text_primary()).show(ui).clicked() {
                                        acts.push(SettingsAction::ChooseStarFolder);
                                    }
                                });
                            });
                        });

                        section(ui, "CLOUD SYNC", |ui| {
                            row(ui, "Copy clips to a cloud folder", "Copies clips to the selected folder", |ui| {
                                if toggle(ui, cloud.enabled()) {
                                    acts.push(SettingsAction::CloudToggle(!cloud.enabled()));
                                }
                            });
                            if !cloud.enabled() {
                                self.games_popup = None;
                                return;
                            }
                            divider(ui);
                            let desc = path_text(ui, &cloud.folder);
                            row(ui, "Cloud folder", &desc, |ui| {
                                field(ui, |ui| {
                                    let t = if cloud.folder.is_empty() { "Choose folder" } else { "Change folder" };
                                    if Ghost::new(t).icon("folder", 16.0).font(theme::font_ui()).fg(theme::text_primary()).show(ui).clicked() {
                                        acts.push(SettingsAction::ChooseCloudFolder);
                                    }
                                });
                            });
                            divider(ui);
                            row(ui, "Games", "Only clips from the ticked games are copied", |ui| {
                                field(ui, |ui| {
                                    let r = Ghost::new(&games_text(cloud, lib)).font(theme::font_ui()).fg(theme::text_primary()).arrow().width(170.0).show(ui);
                                    games_btn = r.rect;
                                    if r.clicked() {
                                        self.games_popup = if self.games_popup.is_some() { None } else { Some(r.rect) };
                                    }
                                });
                            });
                            divider(ui);
                            row(ui, "Clips to copy", "New clips: only clips recorded from now on. All clips: also the old ones", |ui| {
                                field(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                                        // right to left
                                        for (i, (key, label)) in [("all", "All clips"), ("new", "New clips")].into_iter().enumerate() {
                                            if i > 0 {
                                                widgets::pill_divider(ui);
                                            }
                                            let fg = if cloud.mode == key { theme::accent() } else { theme::text_muted() };
                                            if Ghost::new(label).font(theme::font_ui()).fg(fg).show(ui).clicked() {
                                                acts.push(SettingsAction::CloudMode(key));
                                            }
                                        }
                                    });
                                });
                            });
                            divider(ui);
                            row(ui, "Share clip info with ClipSync", "Also puts clips.json (game, liked, starred, trims) and thumbnails in the cloud folder, so the ClipSync phone app can show them", |ui| {
                                if toggle(ui, cloud.share_info) {
                                    acts.push(SettingsAction::CloudShareInfo(!cloud.share_info));
                                }
                            });
                            divider(ui);
                            let st = cloud.status();
                            if status_row(ui, &st, cloud_kind(&st)) {
                                acts.push(SettingsAction::CloudSync);
                            }
                            divider(ui);
                            row(ui, "Delete cloud clips", "Deletes all the clips in the cloud folder. Your originals stay put", |ui| {
                                field(ui, |ui| {
                                    if Ghost::new("Delete all").icon("trash", 16.0).font(theme::font_ui()).fg(theme::text_primary()).show(ui).clicked() {
                                        acts.push(SettingsAction::CloudDeleteAll);
                                    }
                                });
                            });
                        });

                        section(ui, "PHONE (CLIPSYNC)", |ui| {
                            row(ui, "Share clips with ClipSync on this network", "The phone copies new clips straight from this PC when it's on the same Wi-Fi (the same clips as cloud sync), else from the cloud folder", |ui| {
                                if toggle(ui, lan.enabled) {
                                    acts.push(SettingsAction::LanToggle(!lan.enabled));
                                }
                            });
                            if lan.enabled {
                                divider(ui);
                                row(ui, "Pairing code", "Type this once in ClipSync's settings", |ui| {
                                    field(ui, |ui| {
                                        if Ghost::new("New code").icon("refresh", 16.0).font(theme::font_ui()).fg(theme::text_primary()).show(ui).on_hover_text("The phone has to type the new code").clicked() {
                                            acts.push(SettingsAction::LanNewCode);
                                        }
                                    });
                                    ui.add_space(10.0);
                                    ui.label(egui::RichText::new(&lan.code).font(theme::font_bold()).color(theme::accent()));
                                });
                                divider(ui);
                                let st = lan.status();
                                row(ui, "This PC", &format!("{}  •  {st}", lan.address()), |_ui| {});
                            }
                        });

                        section(ui, "PLAYBACK", |ui| {
                            row(ui, "Loop clips", "Replay the clip when it ends (a trimmed clip loops its trimmed part)", |ui| {
                                if toggle(ui, v.loop_clip) {
                                    acts.push(SettingsAction::Loop(!v.loop_clip));
                                }
                            });
                            divider(ui);
                            row(ui, "Always start clips from the beginning", "Long clips normally open in the middle; this makes every clip start at 0:00 (trimmed clips still start at their trim)", |ui| {
                                if toggle(ui, v.start_always) {
                                    acts.push(SettingsAction::StartAlways(!v.start_always));
                                }
                            });
                        });

                        section(ui, "AUDIO", |ui| {
                            row(ui, "Audio track controls", "Show the audio tracks button under the player and the default audio track settings below. Turn off to hide both; every clip then plays with the default audio tracks", |ui| {
                                if toggle(ui, v.audio.enabled) {
                                    acts.push(SettingsAction::AudioEnabled(!v.audio.enabled));
                                }
                            });
                            // everything below is hidden together with the audio tracks button
                            if v.audio.enabled {
                                divider(ui);
                                row(ui, "Default audio tracks", "Tracks played and the volume of each, by default, when a clip has several tracks", |_| {});
                                ui.horizontal(|ui| {
                                    ui.add_space(10.0);
                                    ui.vertical(|ui| {
                                        ui.set_width(340.0);
                                        ui.spacing_mut().item_spacing = Vec2::new(0.0, 2.0);
                                        for n in 1..=6 {
                                            let on = v.audio.defaults.contains(&n);
                                            let vol = v.audio.def_vol.get(&n).copied().unwrap_or(1.0);
                                            let r = widgets::track_row(ui, &format!("Track {n}"), on, vol, 30.0);
                                            if r.toggled {
                                                acts.push(SettingsAction::DefaultTrack(n));
                                            }
                                            if let Some((x, fin)) = r.volume {
                                                acts.push(SettingsAction::DefaultVol(n, x, fin));
                                            }
                                        }
                                        ui.add_space(12.0);
                                    });
                                });
                            }
                        });

                        section(ui, "TRIMMING", |ui| {
                            row(ui, "Use the GPU encoder when applying a trim", "Much faster on NVIDIA cards, but files can come out a bit bigger. Falls back to the CPU if no GPU works", |ui| {
                                if toggle(ui, v.trim_gpu) {
                                    acts.push(SettingsAction::TrimGpu(!v.trim_gpu));
                                }
                            });
                        });

                        section(ui, "CLIPS", |ui| {
                            row(ui, "Move deleted clips to the Recycle Bin", "Turn off to delete clips permanently instead", |ui| {
                                if toggle(ui, v.recycle_bin) {
                                    acts.push(SettingsAction::RecycleBin(!v.recycle_bin));
                                }
                            });
                            divider(ui);
                            row(ui, "Auto-refresh the folder", "New clips show up on their own once a recording finishes", |ui| {
                                if toggle(ui, v.auto_refresh) {
                                    acts.push(SettingsAction::AutoRefresh(!v.auto_refresh));
                                }
                            });
                            divider(ui);
                            row(ui, "Clips added by \"Load more\"", "How many more clips load each time you press Load more", |ui| {
                                field(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                                        // right to left: +, number, -
                                        if Ghost::new("+").font(theme::font_ui()).fg(theme::text_primary()).padx(12.0).show(ui).clicked() {
                                            acts.push(SettingsAction::SeeMore(1));
                                        }
                                        let (r, _) = ui.allocate_exact_size(Vec2::new(44.0, theme::CTRL_H - 4.0), Sense::hover());
                                        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, v.see_more_step.to_string(), theme::font_bold(), theme::text_primary());
                                        if Ghost::new("\u{2212}").font(theme::font_ui()).fg(theme::text_primary()).padx(12.0).show(ui).clicked() {
                                            acts.push(SettingsAction::SeeMore(-1));
                                        }
                                    });
                                });
                            });
                            divider(ui);
                            row(ui, "Thumbnail cache", v.cache_info, |ui| {
                                field(ui, |ui| {
                                    if Ghost::new("Clear cache").icon("trash", 16.0).font(theme::font_ui()).fg(theme::text_primary()).padx(12.0).show(ui).clicked() {
                                        acts.push(SettingsAction::ClearCache);
                                    }
                                });
                            });
                        });

                        section(ui, "WINDOW", |ui| {
                            row(ui, "Remember window size", "Open the app at the size you last used", |ui| {
                                if toggle(ui, v.remember_size) {
                                    acts.push(SettingsAction::RememberSize(!v.remember_size));
                                }
                            });
                            divider(ui);
                            row(ui, "Close button hides to the tray", "Turn off to quit the app when you close the window", |ui| {
                                if toggle(ui, v.close_to_tray) {
                                    acts.push(SettingsAction::CloseToTray(!v.close_to_tray));
                                }
                            });
                            divider(ui);
                            row(ui, "Start with Windows", "Launch Feather Player in the tray when you sign in", |ui| {
                                if toggle(ui, v.start_with_windows) {
                                    acts.push(SettingsAction::StartWithWindows(!v.start_with_windows));
                                }
                            });
                        });

                        hint_card(ui, "KEYBOARD SHORTCUTS", &[
                            ("Space", "Play / pause", "Pause or resume the open clip"),
                            ("\u{2190}", "Newer clip", "Jump to the next newer clip in the current filter"),
                            ("\u{2192}", "Older clip", "Jump to the next older clip in the current filter"),
                            ("M", "Mute / unmute", "Mute or unmute the volume"),
                            ("F", "Fullscreen", "Enter or leave fullscreen (Esc also leaves it)"),
                            ("Esc", "Close settings or a popup", "Also closes the theme and cloud game lists"),
                            ("Ctrl + Q", "Quit completely", "Quits even when the close button hides to the tray"),
                        ]);
                        hint_card(ui, "HIDDEN FEATURES", &[
                            ("Right-click clip", "Select clips for deleting", "Hold and drag over clips to select several"),
                            ("Right-click game", "Pick several games", "In the game list: adds or removes it and keeps the list open"),
                            ("Middle-click game", "Rename a game", "In the game list: renames that category"),
                        ]);
                        ui.add_space(26.0);
                    });
                });
            });
        });

        // cloud games dropdown (stays open while ticking)
        if let Some(anchor) = self.games_popup {
            let mut counts: std::collections::HashMap<&str, usize> = Default::default();
            for vp in &lib.videos {
                *counts.entry(lib.cats.key_of.get(vp).map(String::as_str).unwrap_or(NONE_KEY)).or_default() += 1;
            }
            let mut keys: Vec<String> = lib.cats.norm_map.keys().cloned().chain(cloud.games.iter().cloned()).collect();
            if counts.get(NONE_KEY).copied().unwrap_or(0) > 0 {
                keys.push(NONE_KEY.into());
            }
            keys.sort_by_key(|k| game_label(k, lib).to_lowercase());
            keys.dedup();
            let width = anchor.width().max(220.0);
            let pos = Pos2::new(anchor.right() - width + 3.0, anchor.bottom() + 7.0);
            let rect = widgets::popup(ctx, "cloud_games", pos, width, |ui| {
                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                    let all = Row::new("All games").height(30.0).check(cloud.games.is_empty()).right_text(lib.videos.len().to_string()).show(ui);
                    if all.clicked() {
                        acts.push(SettingsAction::CloudGame("All".into()));
                    }
                    for k in &keys {
                        let n = counts.get(k.as_str()).copied().unwrap_or(0);
                        let r = Row::new(&game_label(k, lib)).height(30.0).check(cloud.games.contains(k)).right_text(if n > 0 { n.to_string() } else { String::new() }).show(ui);
                        if r.clicked() {
                            acts.push(SettingsAction::CloudGame(k.clone()));
                        }
                    }
                });
            });
            if ctx.input(|i| i.pointer.any_pressed()) {
                if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                    if !rect.contains(p) && !games_btn.contains(p) {
                        self.games_popup = None;
                    }
                }
            }
        }
        // theme dropdown (Python `_toggle_theme_popup` + `ThemeRow`)
        if let Some(anchor) = self.theme_popup {
            let width = anchor.width().max(210.0);
            let pos = Pos2::new(anchor.right() - width, anchor.bottom() + 4.0);
            let cur = theme::active_name();
            let mut picked = None;
            let rect = widgets::popup(ctx, "theme_popup", pos, width, |ui| {
                for name in theme::THEME_ORDER {
                    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), Sense::click());
                    let active = name == cur;
                    let hl = r.shrink(1.0);
                    if resp.hovered() {
                        ui.painter().rect_filled(hl, 7.0, c("#2c2c50"));
                        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                    } else if active {
                        ui.painter().rect_filled(hl, 7.0, c("#2A2A4A"));
                    }
                    paint_swatch(ui, r.left() + 11.0, r.center().y, name);
                    let fg = if resp.hovered() || active { Color32::WHITE } else { theme::text_muted() };
                    ui.painter().text(Pos2::new(r.left() + 12.0 + 47.0 + 8.0, r.center().y), egui::Align2::LEFT_CENTER, name, theme::font_ui(), fg);
                    if active {
                        super::icons::paint(ui, Pos2::new(r.right() - 16.0, r.center().y), "check", 12.0, theme::accent(), false, 2.4);
                    }
                    if resp.clicked() {
                        picked = Some(name);
                    }
                }
            });
            if let Some(name) = picked {
                self.theme_popup = None;
                if name != cur {
                    acts.push(SettingsAction::Theme(name.to_owned()));
                }
            } else if ctx.input(|i| i.pointer.any_pressed()) {
                if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                    if !rect.contains(p) && !theme_field.contains(p) {
                        self.theme_popup = None;
                    }
                }
            }
        }
        if let Some(d) = &mut self.accent {
            if let Some(res) = d.show(ctx) {
                self.accent = None;
                if let Some(color) = res {
                    acts.push(SettingsAction::Accent(color));
                }
            }
            return acts; // the dialog handles Escape itself
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.theme_popup.is_some() {
                self.theme_popup = None;
            } else if self.games_popup.is_some() {
                self.games_popup = None;
            } else {
                acts.push(SettingsAction::Close);
            }
        }
        acts
    }
}

fn game_label(key: &str, lib: &Library) -> String {
    if key == NONE_KEY { "Moments".into() } else { lib.cats.label(key).to_owned() }
}

/// Python `_cloud_games_text`.
fn games_text(cloud: &Cloud, lib: &Library) -> String {
    if cloud.games.is_empty() {
        return "All games".into();
    }
    let mut names: Vec<String> = cloud.games.iter().map(|k| game_label(k, lib)).collect();
    names.sort_by_key(|n| n.to_lowercase());
    let joined = names.join(", ");
    if names.len() == 1 {
        names.remove(0)
    } else if names.len() == 2 && joined.chars().count() <= 20 {
        joined
    } else {
        format!("{} games", names.len())
    }
}
