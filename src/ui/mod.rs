pub mod app;
pub mod tray;

use eframe::egui;
use flume::{Sender, Receiver};
use crate::events::{UiCommand, CoreEvent};
use tray_icon::menu::MenuEvent;

pub fn run_gui(tx: Sender<UiCommand>, rx: Receiver<CoreEvent>) -> anyhow::Result<()> {
    // 1. Inizializza Tray
    let tray = tray::AppTray::new()?;
    
    // Salviamo gli ID
    let show_id = tray.menu_item_show.id().clone();
    let quit_id = tray.menu_item_quit.id().clone();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 500.0])
            .with_resizable(false)
            .with_close_button(true)
            .with_visible(true),
        ..Default::default()
    };

    eframe::run_native(
        "RustClip",
        options,
        Box::new(move |cc| {
            // Cloniamo il contesto per il thread
            let ctx = cc.egui_ctx.clone();
            let tx_clone = tx.clone();
            
            // --- TRAY LISTENER THREAD ---
            std::thread::spawn(move || {
                while let Ok(event) = MenuEvent::receiver().recv() {
                    if event.id == show_id {
                        // SEQUENZA DI RISVEGLIO ROBUSTA PER WINDOWS
                        // 1. Rendiamo visibile
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        // 2. Assicuriamoci che non sia minimizzata (importante su Win)
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        // 3. Portiamo in primo piano
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                        // 4. Forziamo il repaint
                        ctx.request_repaint();
                    } 
                    else if event.id == quit_id {
                        // Manda comando al backend per chiudere clean
                        let _ = tx_clone.send(UiCommand::Quit);
                        // Forza chiusura processo brutale (sicura su Windows per GUI apps)
                        std::process::exit(0);
                    }
                }
            });
            // ----------------------------

            Ok(Box::new(app::RustClipApp::new(cc, tx, rx, tray)))
        }),
    ).map_err(|e| anyhow::anyhow!("Errore GUI: {}", e))
}