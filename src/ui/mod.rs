pub mod app;
pub mod tray;

use eframe::egui;
use flume::{Sender, Receiver};
use crate::events::{UiCommand, CoreEvent};
use tray_icon::menu::MenuEvent;
use std::fs;
use std::sync::Arc; // <--- Importiamo Arc

pub fn run_gui(tx: Sender<UiCommand>, rx: Receiver<CoreEvent>) -> anyhow::Result<()> {
    let tray = tray::AppTray::new()?;
    
    let show_id = tray.menu_item_show.id().clone();
    let quit_id = tray.menu_item_quit.id().clone();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 550.0])
            .with_resizable(false)
            .with_close_button(true)
            .with_visible(true),
        ..Default::default()
    };

    eframe::run_native(
        "RustClip",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            let tx_clone = tx.clone();
            
            // --- FIX FONT SYSTEM ---
            configure_fonts(&ctx);
            // -----------------------

            // TRAY THREAD
            std::thread::spawn(move || {
                while let Ok(event) = MenuEvent::receiver().recv() {
                    if event.id == show_id {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                        ctx.request_repaint();
                    } 
                    else if event.id == quit_id {
                        let _ = tx_clone.send(UiCommand::Quit);
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
        println!("🎨 Caricato font Emoji di sistema: {}", font_path);
        
        // --- FIX QUI SOTTO ---
        // Avvolgiamo in Arc::new(...) come richiesto dal compilatore
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
    } else {
        eprintln!("⚠️ Impossibile trovare il font Emoji in: {}", font_path);
    }

    ctx.set_fonts(fonts);
}