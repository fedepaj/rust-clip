pub mod app;
pub mod tray;

use eframe::egui;
use flume::{Sender, Receiver};
use crate::events::{UiCommand, CoreEvent};
use tray_icon::menu::MenuEvent;

pub fn run_gui(tx: Sender<UiCommand>, rx: Receiver<CoreEvent>) -> anyhow::Result<()> {
    // 1. Inizializza Tray
    let tray = tray::AppTray::new()?;
    
    // Salviamo gli ID per usarli nel thread
    let show_id = tray.menu_item_show.id().clone();
    let quit_id = tray.menu_item_quit.id().clone();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 500.0])
            .with_resizable(false)
            .with_close_button(true)
            .with_visible(true), // Parte visibile
        ..Default::default()
    };

    eframe::run_native(
        "RustClip",
        options,
        Box::new(move |cc| {
            // --- FIX WINDOWS: TRAY LISTENER THREAD ---
            // Cloniamo il contesto grafico per poterlo comandare dal thread
            let ctx = cc.egui_ctx.clone();
            let tx_clone = tx.clone(); // Per mandare Quit al backend
            
            std::thread::spawn(move || {
                // Questo loop gira su un thread separato e non si blocca mai
                while let Ok(event) = MenuEvent::receiver().recv() {
                    if event.id == show_id {
                        // Forza la finestra ad apparire e andare in primo piano
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                        ctx.request_repaint();
                    } 
                    else if event.id == quit_id {
                        // Invia segnale di stop e chiudi
                        let _ = tx_clone.send(UiCommand::Quit);
                        std::process::exit(0);
                    }
                }
            });
            // ------------------------------------------

            Ok(Box::new(app::RustClipApp::new(cc, tx, rx, tray)))
        }),
    ).map_err(|e| anyhow::anyhow!("Errore GUI: {}", e))
}