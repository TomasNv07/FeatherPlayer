//! Theme engine, ported from FeatherPlayer (THEMES, `_build_theme_map`, `_c`).
//!
//! Every colour in the Python file is the original "Midnight" palette, translated through the
//! active theme by `_c("#rrggbb")`. Here `c("#rrggbb")` does the same. The theme name comes from
//! `settings.json` ("theme", default "Obsidian"), custom accents from "accent_overrides".

use crate::prefs::{self, JsonGet};
use eframe::egui::{self, Color32, FontFamily, FontId};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

struct ThemeDef {
    name: &'static str,
    identity: bool,
    hue: f64,
    sat: f64,
    l_scale: f64,
    l_shift: f64,
    accent: &'static str,
}

const fn t(name: &'static str, hue: f64, sat: f64, l_scale: f64, l_shift: f64, accent: &'static str) -> ThemeDef {
    ThemeDef { name, identity: false, hue, sat, l_scale, l_shift, accent }
}

const THEMES: [ThemeDef; 8] = [
    ThemeDef { name: "Midnight", identity: true, hue: 0.0, sat: 0.0, l_scale: 1.0, l_shift: 0.0, accent: "#FF2A6D" },
    t("Graphite", 0.0, 0.0, 1.0, 0.010, "#E8B923"),
    t("Ocean", 214.0, 1.05, 1.0, 0.0, "#2F8CFF"),
    t("Forest", 152.0, 0.90, 1.0, 0.0, "#2FD28A"),
    t("Ember", 18.0, 0.75, 1.0, 0.0, "#F2662B"),
    t("Amethyst", 268.0, 1.05, 1.0, 0.0, "#A06BFF"),
    t("Obsidian", 0.0, 0.0, 0.85, -0.050, "#00C9A7"),
    t("Mocha", 28.0, 0.55, 1.0, 0.0, "#D4A373"),
];
#[allow(dead_code)] // the theme picker (settings) uses it
pub const THEME_ORDER: [&str; 8] = ["Obsidian", "Midnight", "Graphite", "Ocean", "Forest", "Ember", "Amethyst", "Mocha"];
pub const DEFAULT_THEME: &str = "Obsidian";

const BASE_SURFACES: [&str; 14] = [
    "#0f0f1a", "#13131f", "#1a1a2e", "#242444", "#2a2a4a", "#2c2c50", "#161628", "#33335a", "#3a3a5e", "#45456e", "#52527e",
    "#2a2a3d", "#23233a", "#12121e",
];
const BASE_TEXT: [&str; 2] = ["#ababc2", "#8b8b9e"];
const BASE_ACCENT: &str = "#ff2a6d";
const BASE_ACCENT_HOVER: &str = "#ff5c8d";
const BASE_ACCENT_PRESS: &str = "#cc1f57";
/// Colours deliberately NOT themed; themed colours may never equal one of these.
const RESERVED: [&str; 19] = [
    "#000000", "#ffffff", "#111111", "#333333", "#bbbbbb", "#dddddd", "#e0e0e0", "#f1f1f1", "#e05555", "#e0685f", "#e0a05f",
    "#7ec97e", "#c8c864", "#ccc364", "#e8454a", "#e8c84a", "#9e2a2a", "#7a1f1f", "#0a1a18",
];

// ---- colorsys port (exact, so themed colours match the Python app) --------------------------

fn rgb_to_hls(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let maxc = r.max(g).max(b);
    let minc = r.min(g).min(b);
    let sumc = maxc + minc;
    let rangec = maxc - minc;
    let l = sumc / 2.0;
    if minc == maxc {
        return (0.0, l, 0.0);
    }
    let s = if l <= 0.5 { rangec / sumc } else { rangec / (2.0 - maxc - minc) };
    let rc = (maxc - r) / rangec;
    let gc = (maxc - g) / rangec;
    let bc = (maxc - b) / rangec;
    let h = if r == maxc {
        bc - gc
    } else if g == maxc {
        2.0 + rc - bc
    } else {
        4.0 + gc - rc
    };
    ((h / 6.0).rem_euclid(1.0), l, s)
}

fn hls_v(m1: f64, m2: f64, hue: f64) -> f64 {
    let hue = hue.rem_euclid(1.0);
    if hue < 1.0 / 6.0 {
        m1 + (m2 - m1) * hue * 6.0
    } else if hue < 0.5 {
        m2
    } else if hue < 2.0 / 3.0 {
        m1 + (m2 - m1) * (2.0 / 3.0 - hue) * 6.0
    } else {
        m1
    }
}

fn hls_to_rgb(h: f64, l: f64, s: f64) -> (f64, f64, f64) {
    if s == 0.0 {
        return (l, l, l);
    }
    let m2 = if l <= 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let m1 = 2.0 * l - m2;
    (hls_v(m1, m2, h + 1.0 / 3.0), hls_v(m1, m2, h), hls_v(m1, m2, h - 1.0 / 3.0))
}

fn parse_hex(h: &str) -> Option<(u8, u8, u8)> {
    let h = h.strip_prefix('#')?;
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some((p(0)?, p(2)?, p(4)?))
}

fn hex_to_rgb(h: &str) -> (f64, f64, f64) {
    let (r, g, b) = parse_hex(h).unwrap_or((0, 0, 0));
    (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0)
}

/// Python `_rgb_to_hex`: `int(round(v * 255))`, and Python rounds half to even.
fn rgb_to_hex((r, g, b): (f64, f64, f64)) -> String {
    let c = |v: f64| (v * 255.0).round_ties_even().clamp(0.0, 255.0) as u8;
    format!("#{:02x}{:02x}{:02x}", c(r), c(g), c(b))
}

/// Nudge a colour one step at a time until it is neither already used nor reserved.
fn unique(used: &mut HashSet<String>, hexv: &str) -> String {
    let mut hexv = hexv.to_lowercase();
    let (mut r, mut g, mut b) = parse_hex(&hexv).map(|(r, g, b)| (r as i32, g as i32, b as i32)).unwrap_or((0, 0, 0));
    let mut step = 1;
    for _ in 0..600 {
        if !used.contains(&hexv) && !RESERVED.contains(&hexv.as_str()) {
            break;
        }
        if r.max(g).max(b) >= 255 {
            step = -1;
        }
        r = (r + step).clamp(0, 255);
        g = (g + step).clamp(0, 255);
        b = (b + step).clamp(0, 255);
        hexv = format!("#{r:02x}{g:02x}{b:02x}");
    }
    used.insert(hexv.clone());
    hexv
}

fn set_accent_trio(m: &mut HashMap<String, String>, acc: &str, used: &mut HashSet<String>) {
    let (r, g, b) = hex_to_rgb(acc);
    let (h, l, s) = rgb_to_hls(r, g, b);
    m.insert(BASE_ACCENT.into(), unique(used, acc));
    m.insert(BASE_ACCENT_HOVER.into(), unique(used, &rgb_to_hex(hls_to_rgb(h, (l + 0.09).min(0.88), s))));
    m.insert(BASE_ACCENT_PRESS.into(), unique(used, &rgb_to_hex(hls_to_rgb(h, (l - 0.10).max(0.10), s))));
}

/// `{base colour -> themed colour}` for every themeable colour (Python `_build_theme_map`).
fn build_theme_map(name: &str, accent_override: Option<&str>) -> HashMap<String, String> {
    let t = THEMES.iter().find(|t| t.name == name).unwrap_or(&THEMES[0]);
    let mut m = HashMap::new();
    if t.identity {
        for c in BASE_SURFACES.iter().chain(BASE_TEXT.iter()).chain([BASE_ACCENT, BASE_ACCENT_HOVER, BASE_ACCENT_PRESS].iter()) {
            m.insert(c.to_string(), c.to_string());
        }
        if let Some(ov) = accent_override {
            let mut used: HashSet<String> = BASE_SURFACES.iter().chain(BASE_TEXT.iter()).map(|s| s.to_string()).collect();
            set_accent_trio(&mut m, ov, &mut used);
        }
        return m;
    }
    let hue = t.hue / 360.0;
    let mut used = HashSet::new();
    for c in BASE_SURFACES {
        let (r, g, b) = hex_to_rgb(c);
        let (_, l, s) = rgb_to_hls(r, g, b);
        let v = rgb_to_hex(hls_to_rgb(hue, (l * t.l_scale + t.l_shift).clamp(0.0, 1.0), (s * t.sat).clamp(0.0, 1.0)));
        m.insert(c.to_string(), unique(&mut used, &v));
    }
    for c in BASE_TEXT {
        let (r, g, b) = hex_to_rgb(c);
        let (_, l, s) = rgb_to_hls(r, g, b);
        let v = rgb_to_hex(hls_to_rgb(hue, l, (s * t.sat).clamp(0.0, 1.0)));
        m.insert(c.to_string(), unique(&mut used, &v));
    }
    let acc = accent_override.map(str::to_owned).unwrap_or_else(|| t.accent.to_lowercase());
    set_accent_trio(&mut m, &acc, &mut used);
    m
}

/// Custom accents saved in settings.json (`accent_overrides`: {theme: "#rrggbb"}).
fn accent_override(settings: &serde_json::Map<String, Value>, theme: &str) -> Option<String> {
    let v = settings.get("accent_overrides")?.as_object()?.get(theme)?.as_str()?;
    (v.len() == 7 && parse_hex(v).is_some()).then(|| v.to_lowercase())
}

// ---- active theme ---------------------------------------------------------------------------

struct Active {
    #[allow(dead_code)]
    name: String,
    map: HashMap<u32, Color32>,
}

static ACTIVE: RwLock<Option<Arc<Active>>> = RwLock::new(None);

fn key(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) << 16 | (g as u32) << 8 | b as u32
}

/// Make `name` the active theme (unknown names fall back to Obsidian, like Python).
pub fn set_active(name: &str) {
    let name = if THEMES.iter().any(|t| t.name == name) { name } else { DEFAULT_THEME };
    let ov = accent_override(&prefs::read_settings(), name);
    let mut map = HashMap::new();
    for (k, v) in build_theme_map(name, ov.as_deref()) {
        if let (Some((r, g, b)), Some((r2, g2, b2))) = (parse_hex(&k), parse_hex(&v)) {
            map.insert(key(r, g, b), Color32::from_rgb(r2, g2, b2));
        }
    }
    *ACTIVE.write().unwrap() = Some(Arc::new(Active { name: name.to_owned(), map }));
}

#[allow(dead_code)]
pub fn active_name() -> String {
    ACTIVE.read().unwrap().as_ref().map(|a| a.name.clone()).unwrap_or_else(|| DEFAULT_THEME.into())
}

/// Translate a base-palette colour through the active theme (Python `_c`).
pub fn c(hex: &str) -> Color32 {
    let Some((r, g, b)) = parse_hex(hex) else { return Color32::MAGENTA };
    let a = ACTIVE.read().unwrap();
    a.as_ref().and_then(|a| a.map.get(&key(r, g, b)).copied()).unwrap_or(Color32::from_rgb(r, g, b))
}

// ---- palette (Python globals) ---------------------------------------------------------------
pub fn bg() -> Color32 { c("#0F0F1A") }
pub fn sidebar_bg() -> Color32 { c("#13131f") }
pub fn card_bg() -> Color32 { c("#1A1A2E") }
pub fn card_hover() -> Color32 { c("#242444") }
pub fn accent() -> Color32 { c("#FF2A6D") }
pub fn text_primary() -> Color32 { c("#F1F1F1") }
pub fn text_muted() -> Color32 { c("#ABABC2") }
pub fn text_dim() -> Color32 { c("#8B8B9E") }
pub fn player_bg() -> Color32 { c("#000000") }
pub fn bar_track() -> Color32 { c("#2A2A4A") }
pub fn divider() -> Color32 { c("#2A2A3D") }
pub fn border() -> Color32 { c("#2A2A3D") }
pub const ERROR: Color32 = Color32::from_rgb(0xe0, 0x55, 0x55);

/// Readable text colour on top of the accent (Python `_accent_fg`).
pub fn accent_fg() -> Color32 {
    let a = accent();
    let lin = |v: u8| {
        let v = v as f64 / 255.0;
        if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    let lum = 0.2126 * lin(a.r()) + 0.7152 * lin(a.g()) + 0.0722 * lin(a.b());
    if lum > 0.30 { Color32::from_rgb(0x11, 0x11, 0x11) } else { Color32::WHITE }
}

pub const CTRL_H: f32 = 30.0;
pub const RADIUS: f32 = 8.0;

// ---- fonts: Segoe UI like the Python app (tk point sizes at 96 dpi) -------------------------
const PT: f32 = 96.0 / 72.0;
pub fn font_ui() -> FontId { FontId::proportional(9.0 * PT) }
pub fn font_bold() -> FontId { FontId::new(9.0 * PT, bold_family()) }
pub fn font_title() -> FontId { FontId::new(11.0 * PT, bold_family()) }
pub fn font_tiny() -> FontId { FontId::proportional(8.0 * PT) }
pub fn font_pt(pt: f32) -> FontId { FontId::proportional(pt * PT) }
pub fn bold_family() -> FontFamily { FontFamily::Name("bold".into()) }

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let dir = std::env::var_os("WINDIR").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into()).join("Fonts");
    let mut load = |name: &str, file: &str| -> bool {
        match std::fs::read(dir.join(file)) {
            Ok(b) => {
                fonts.font_data.insert(name.to_owned(), Arc::new(egui::FontData::from_owned(b)));
                true
            }
            Err(_) => false,
        }
    };
    let regular = load("segoe", "segoeui.ttf");
    let bold = load("segoe-bold", "segoeuib.ttf");
    let symbol = load("segoe-symbol", "seguisym.ttf");
    let mut prop = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if symbol {
        prop.insert(0, "segoe-symbol".into());
    }
    if regular {
        prop.insert(0, "segoe".into());
    }
    let mut boldf = prop.clone();
    if bold {
        boldf.insert(0, "segoe-bold".into());
    }
    fonts.families.insert(FontFamily::Proportional, prop);
    fonts.families.insert(bold_family(), boldf);
    ctx.set_fonts(fonts);
}

/// Load the theme from settings.json, install fonts and egui visuals.
pub fn apply(ctx: &egui::Context) {
    let s = prefs::read_settings();
    set_active(&s.get_str("theme", DEFAULT_THEME));
    install_fonts(ctx);
    restyle(ctx);
}

/// egui's own widgets (scrollbars, sliders, popups) in theme colours.
pub fn restyle(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = bg();
    v.window_fill = card_bg();
    v.extreme_bg_color = bg();
    v.faint_bg_color = card_bg();
    v.selection.bg_fill = accent();
    v.hyperlink_color = accent();
    v.override_text_color = None;
    v.widgets.noninteractive.bg_fill = card_bg();
    v.widgets.noninteractive.fg_stroke.color = text_muted();
    v.widgets.inactive.bg_fill = c("#242444");
    v.widgets.inactive.weak_bg_fill = c("#242444");
    v.widgets.inactive.fg_stroke.color = text_muted();
    v.widgets.hovered.bg_fill = c("#2A2A4A");
    v.widgets.hovered.weak_bg_fill = c("#2A2A4A");
    v.widgets.active.bg_fill = c("#33335a");
    v.widgets.active.weak_bg_fill = c("#33335a");
    v.window_stroke.color = c("#33335a");
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.text_styles.insert(egui::TextStyle::Body, font_ui());
        s.text_styles.insert(egui::TextStyle::Button, font_ui());
        s.text_styles.insert(egui::TextStyle::Small, font_tiny());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midnight_is_identity() {
        let m = build_theme_map("Midnight", None);
        assert_eq!(m["#242444"], "#242444");
        assert_eq!(m[BASE_ACCENT], BASE_ACCENT);
    }

    #[test]
    fn themed_colours_are_unique_and_not_reserved() {
        for t in THEME_ORDER {
            let m = build_theme_map(t, None);
            let vals: HashSet<_> = m.values().collect();
            assert_eq!(vals.len(), m.len(), "{t}");
            assert!(m.values().all(|v| !RESERVED.contains(&v.as_str())), "{t}");
        }
    }

    #[test]
    fn obsidian_accent() {
        assert_eq!(build_theme_map("Obsidian", None)[BASE_ACCENT], "#00c9a7");
    }
}

#[cfg(test)]
mod python_compare {
    /// Dumps every theme map for comparison with the Python app:
    /// `FP_THEME_DUMP=out.json cargo test -- --ignored theme_dump`
    #[test]
    #[ignore]
    fn theme_dump() {
        let path = std::env::var("FP_THEME_DUMP").expect("FP_THEME_DUMP");
        let mut all = serde_json::Map::new();
        for t in super::THEME_ORDER {
            let m: serde_json::Map<String, serde_json::Value> = super::build_theme_map(t, None).into_iter().map(|(k, v)| (k, v.into())).collect();
            all.insert(t.into(), m.into());
        }
        std::fs::write(path, serde_json::to_string(&all).unwrap()).unwrap();
    }
}
