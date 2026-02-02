use tray_icon::{TrayIconBuilder, TrayIcon};
use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
use anyhow::Result;

pub struct AppTray {
    pub icon: TrayIcon,
    pub menu_item_show: MenuItem,
    pub menu_item_quit: MenuItem,
}

impl AppTray {
    pub fn new() -> Result<Self> {
        let tray_menu = Menu::new();
        
        let menu_item_show = MenuItem::new("Apri Dashboard", true, None);
        let menu_item_quit = MenuItem::new("Esci (Quit)", true, None);

        tray_menu.append(&menu_item_show)?;
        tray_menu.append(&PredefinedMenuItem::separator())?;
        tray_menu.append(&menu_item_quit)?;

        // Carichiamo un'icona (Qui generiamo un quadrato rosso finto per test)
        // In produzione caricheremo un PNG.
        let icon = load_icon()?;

        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_tooltip("RustClip")
            .with_icon(icon)
            .build()?;

        Ok(Self {
            icon: tray_icon,
            menu_item_show,
            menu_item_quit,
        })
    }
}

// Genera un'icona "pixel art" al volo (4 red dots)
fn load_icon() -> Result<tray_icon::Icon> {
    let width = 64;
    let height = 64;
    let mut rgba = Vec::new();
    for _ in 0..height {
        for _ in 0..width {
            // Un bel colore arancione Rust (R, G, B, A)
            rgba.extend_from_slice(&[255, 100, 0, 255]);
        }
    }
    tray_icon::Icon::from_rgba(rgba, width, height)
        .map_err(|e| anyhow::anyhow!("Icon error: {}", e))
}