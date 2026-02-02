use tray_icon::{TrayIconBuilder, TrayIcon};
use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
use anyhow::Result;
use image::GenericImageView; // Importante per leggere le dimensioni

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

        // Costruiamo il menu
        tray_menu.append(&menu_item_show)?;
        tray_menu.append(&PredefinedMenuItem::separator())?;
        tray_menu.append(&menu_item_quit)?;

        // Carichiamo l'icona incastonata
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

fn load_icon() -> Result<tray_icon::Icon> {
    // Leggiamo il file a tempo di compilazione. 
    // Il percorso è relativo al file sorgente corrente (src/ui/tray.rs)
    // Quindi ../../assets/icon.png punta alla root/assets/icon.png
    let icon_bytes = include_bytes!("../../assets/icon.png");

    // Decodifichiamo il PNG/ICO dalla memoria
    let image = image::load_from_memory(icon_bytes)
        .map_err(|e| anyhow::anyhow!("Errore decodifica icona: {}", e))?;
    
    let (width, height) = image.dimensions();
    let rgba = image.to_rgba8().into_raw();

    tray_icon::Icon::from_rgba(rgba, width, height)
        .map_err(|e| anyhow::anyhow!("Errore creazione tray icon: {}", e))
}