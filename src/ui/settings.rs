//! Settings page (Python `_build_settings_page`): replaces the main window while open.
//! Ported so far: Starred clips, Cloud sync, Phone sync, Playback, Trimming, Clips (Recycle
//! Bin). Appearance, Audio, auto-refresh, Load more, thumbnail cache, Window and the shortcut
//! cards come with their features.

use super::widgets::{self, ellipsize, Ghost, Row};
use crate::library::{Library, NONE_KEY};
use crate::sync::{cloud_kind, phone_kind, Cloud, DotKind, Phone};
use crate::theme::{self, c};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Vec2};

pub enum SettingsAction {
    Close,
    StarCopy(bool),
    ChooseStarFolder,
    CloudToggle(bool),
    ChooseCloudFolder,
    CloudMode(&'static str),
    CloudGame(String),
    CloudSync,
    CloudDeleteAll,
    PhoneToggle(bool),
    PhoneKind(&'static str),
    PhoneSync,
    Loop(bool),
    StartAlways(bool),
    TrimGpu(bool),
    RecycleBin(bool),
}

/// Values the page shows (the rest is read from `Cloud` / `Phone`).
pub struct SettingsValues<'a> {
    pub star_copy: bool,
    pub star_folder: &'a str,
    pub loop_clip: bool,
    pub start_always: bool,
    pub trim_gpu: bool,
    pub recycle_bin: bool,
}

#[derive(Default)]
pub struct SettingsPage {
    games_popup: Option<Rect>,
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
    pub fn show(&mut self, ctx: &egui::Context, v: &SettingsValues, cloud: &Cloud, phone: &mut Phone, lib: &Library) -> Vec<SettingsAction> {
        let mut acts = Vec::new();
        let mut games_btn = Rect::NOTHING;
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
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let w = ui.available_width();
                let col_w = (w - 48.0).clamp(300.0, 620.0);
                ui.add_space(26.0);
                ui.horizontal(|ui| {
                    ui.add_space(((w - col_w) / 2.0).max(0.0));
                    ui.vertical(|ui| {
                        ui.set_width(col_w);
                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                        let path_text = |ui: &egui::Ui, p: &str| if p.is_empty() { "No folder chosen yet".to_owned() } else { ellipsize(ui, p, &theme::font_tiny(), 330.0, false) };

                        section(ui, "STARRED CLIPS", |ui| {
                            row(ui, "Copy starred clips to a folder", "When you star a clip, a copy is saved to the folder below", |ui| {
                                if toggle(ui, v.star_copy) {
                                    acts.push(SettingsAction::StarCopy(!v.star_copy));
                                }
                            });
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

                        section(ui, "PHONE SYNC", |ui| {
                            row(ui, "Sync clips to phone", "Send clips to the ClipSync app on your phone", |ui| {
                                if toggle(ui, phone.enabled) {
                                    acts.push(SettingsAction::PhoneToggle(!phone.enabled));
                                }
                            });
                            if phone.enabled {
                                divider(ui);
                                row(ui, "Pairing code", "Shown at the top of the ClipSync app on your phone", |ui| {
                                    widgets::entry(ui, &mut phone.code, 90.0);
                                });
                                divider(ui);
                                row(ui, "Phone address (optional)", "Leave empty: your phone is found automatically on the same network", |ui| {
                                    widgets::entry(ui, &mut phone.manual_ip, 140.0);
                                });
                                divider(ui);
                                row(ui, "Clips to send", "Pick one or more. All clips sends every clip in your folders", |ui| {
                                    field(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing = Vec2::ZERO;
                                            let opts = [("all", "All clips"), ("trimmed", "Trimmed"), ("liked", "Liked"), ("starred", "Starred")];
                                            for (i, (key, label)) in opts.into_iter().enumerate() {
                                                if i > 0 {
                                                    widgets::pill_divider(ui);
                                                }
                                                let fg = if phone.kinds.contains(key) { theme::accent() } else { theme::text_muted() };
                                                if Ghost::new(label).font(theme::font_ui()).fg(fg).show(ui).clicked() {
                                                    acts.push(SettingsAction::PhoneKind(key));
                                                }
                                            }
                                        });
                                    });
                                });
                                divider(ui);
                                let st = phone.status();
                                if status_row(ui, &st, phone_kind(&st)) {
                                    acts.push(SettingsAction::PhoneSync);
                                }
                            }
                        });

                        section(ui, "PLAYBACK", |ui| {
                            row(ui, "Loop clips", "Replay the clip when it ends (a trimmed clip loops its trimmed part)", |ui| {
                                if toggle(ui, v.loop_clip) {
                                    acts.push(SettingsAction::Loop(!v.loop_clip));
                                }
                            });
                            divider(ui);
                            row(ui, "Always start clips from the beginning", "Long clips normally open in the middle; this makes every clip start at 0:00", |ui| {
                                if toggle(ui, v.start_always) {
                                    acts.push(SettingsAction::StartAlways(!v.start_always));
                                }
                            });
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
                        });
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
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.games_popup.is_some() {
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
