pub mod app;
pub mod tray;

use eframe::egui;
use flume::{Sender, Receiver};
use crate::events::{UiCommand, CoreEvent};
// Rimosso MenuEvent da qui perché lo usiamo tramite il percorso completo nel thread
use std::fs;
use std::sync::Arc;
use image::GenericImageView; // Importante per usare .dimensions()

pub fn run_gui(tx: Sender<UiCommand>, rx: Receiver<CoreEvent>) -> anyhow::Result<()> {
    let tray = tray::AppTray::new()?;
    
    // --- CARICAMENTO ICONA PER FINESTRA ---
    let icon_bytes = include_bytes!("../../assets/icon.png");
    let image = image::load_from_memory(icon_bytes)
        .map_err(|e| anyhow::anyhow!("Errore decodifica icona: {}", e))?;
    
    // FIX: Usiamo il metodo sull'oggetto 'image'
    let (width, height) = image.dimensions();
    
    let icon_data = eframe::egui::IconData {
        rgba: image.to_rgba8().into_raw(),
        width: width as u32,
        height: height as u32,
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 550.0])
            .with_resizable(false)
            .with_icon(Arc::new(icon_data)) // Setta l'icona della finestra (Taskbar/Dock)
            .with_visible(true),
        ..Default::default()
    };

    eframe::run_native(
        "RustClip",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            configure_fonts(&ctx);
            
            let show_id = tray.menu_item_show.id().clone();
            let quit_id = tray.menu_item_quit.id().clone();
            let tx_t = tx.clone();
            let ctx_t = ctx.clone();

            // TRAY LISTENER THREAD
            std::thread::spawn(move || {
                // Usiamo il percorso completo per evitare warning sugli import
                while let Ok(event) = tray_icon::menu::MenuEvent::receiver().recv() {
                    if event.id == show_id {
                        println!("Tray: Restore requested - Sending Visible(true)");
                        ctx_t.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        
                        println!("Tray: Sending Minimized(false)");
                        ctx_t.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        
                        println!("Tray: Sending Focus");
                        ctx_t.send_viewport_cmd(egui::ViewportCommand::Focus);
                        
                        println!("Tray: Sending AlwaysOnTop");
                        ctx_t.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop));
                        
                        println!("Tray: Sending Normal Level");
                        ctx_t.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::Normal));
                        
                        println!("Tray: Requesting Repaint");
                        ctx_t.request_repaint();
                    } else if event.id == quit_id {
                        let _ = tx_t.send(UiCommand::Quit);
                        std::process::exit(0);
                    }
                }
            });

            Ok(Box::new(app::RustClipApp::new(cc, tx, rx, tray)))
        }),
    ).map_err(|e| anyhow::anyhow!("Errore GUI: {}", e))
}

fn configure_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    let font_path = if cfg!(target_os = "macos") {
        "/System/Library/Fonts/Apple Color Emoji.ttc"
    } else if cfg!(target_os = "windows") {
        "C:\\Windows\\Fonts\\seguiemj.ttf"
    } else {
        "/usr/share/fonts/noto/NotoColorEmoji.ttf" 
    };

    if let Ok(font_data) = fs::read(font_path) {
        fonts.font_data.insert(
            "system_emoji".to_owned(),
            Arc::new(egui::FontData::from_owned(font_data)), 
        );

        if let Some(vec) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            vec.push("system_emoji".to_owned());
        }
        
        if let Some(vec) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            vec.push("system_emoji".to_owned());
        }
    }

    ctx.set_fonts(fonts);
}