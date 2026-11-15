//! Colors and egui style. The full "Vibrant Neon" theme from FeatherPlayer comes in step 4.

use eframe::egui::{self, Color32};

pub const ACCENT: Color32 = Color32::from_rgb(0x38, 0xd5, 0xff);
pub const BAR_BG: Color32 = Color32::from_rgb(0x12, 0x14, 0x1c);
pub const LINE: Color32 = Color32::from_rgb(0x3a, 0x3f, 0x52);
pub const MUTED: Color32 = Color32::from_rgb(0x9a, 0xa1, 0xb5);
pub const ERROR: Color32 = Color32::from_rgb(255, 120, 120);
pub const BUTTON_BG: Color32 = Color32::from_rgb(0x20, 0x24, 0x33);
pub const BUTTON_HOVER: Color32 = Color32::from_rgb(0x2a, 0x2f, 0x42);

pub fn apply(ctx: &egui::Context) {
    ctx.set_visuals(egui::Visuals::dark());
}
