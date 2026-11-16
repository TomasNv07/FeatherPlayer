//! System-tray icon (Python `_show_tray_icon`, pystray): only exists while the window is
//! hidden in the tray. Left click or "Open Feather Player" opens the window, "Quit" quits.

use eframe::egui;
use std::sync::mpsc::Receiver;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrayMsg {
    Open,
    Quit,
}

const OPEN_ID: &str = "open";
const QUIT_ID: &str = "quit";

pub struct Tray {
    icon: Option<TrayIcon>,
    pub rx: Receiver<TrayMsg>,
}

/// `pics/<name>` next to the exe (Python `resource_path`), or in the data folder.
pub fn pic_path(name: &str) -> Option<std::path::PathBuf> {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
    exe_dir.into_iter().chain(std::iter::once(crate::prefs::data_dir().to_path_buf())).map(|d| d.join("pics").join(name)).find(|p| p.is_file())
}

/// pics/feather2.png at 64x64, else a plain accent square (Python's fallback).
pub fn icon_rgba() -> (Vec<u8>, u32, u32) {
    if let Some(img) = pic_path("feather2.png").and_then(|p| image::open(p).ok()) {
        let img = image::imageops::resize(&img.to_rgba8(), 64, 64, image::imageops::FilterType::Triangle);
        return (img.into_raw(), 64, 64);
    }
    let a = crate::theme::accent();
    let px = [a.r(), a.g(), a.b(), 255];
    (px.iter().copied().cycle().take(64 * 64 * 4).collect(), 64, 64)
}

impl Tray {
    pub fn new(ctx: egui::Context) -> Tray {
        let (tx, rx) = std::sync::mpsc::channel();
        // The handlers run on the UI thread while it pumps messages; the app reads `rx`.
        let (tx2, ctx2) = (tx.clone(), ctx.clone());
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let m = match e.id.0.as_str() {
                OPEN_ID => TrayMsg::Open,
                QUIT_ID => TrayMsg::Quit,
                _ => return,
            };
            let _ = tx2.send(m);
            ctx2.request_repaint();
        }));
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            // pystray's default item: a left click opens the window
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                let _ = tx.send(TrayMsg::Open);
                ctx.request_repaint();
            }
        }));
        Tray { icon: None, rx }
    }

    /// Show the icon. False if it could not be created.
    pub fn show(&mut self) -> bool {
        if self.icon.is_some() {
            return true;
        }
        let menu = Menu::new();
        let open = MenuItem::with_id(MenuId::new(OPEN_ID), "Open Feather Player", true, None);
        let quit = MenuItem::with_id(MenuId::new(QUIT_ID), "Quit", true, None);
        if menu.append_items(&[&open, &quit]).is_err() {
            return false;
        }
        let (rgba, w, h) = icon_rgba();
        let Ok(icon) = Icon::from_rgba(rgba, w, h) else { return false };
        match TrayIconBuilder::new().with_menu(Box::new(menu)).with_menu_on_left_click(false).with_tooltip("Feather Player").with_icon(icon).build() {
            Ok(t) => {
                self.icon = Some(t);
                true
            }
            Err(_) => false,
        }
    }

    /// Remove the icon (dropping it removes it from the tray).
    pub fn hide(&mut self) {
        self.icon = None;
    }
}
