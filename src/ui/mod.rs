pub mod app;
pub mod tray;

use eframe::egui;
use flume::{Sender, Receiver};
use crate::events::{UiCommand, CoreEvent};
use tray_icon::menu::MenuEvent;

pub fn run_gui(tx: Sender<UiCommand>, rx: Receiver<CoreEvent>) -> anyhow::Result<()> {
    let tray = tray::AppTray::new()?;
    
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
            let ctx = cc.egui_ctx.clone();
            let tx_clone = tx.clone();
            
            // --- TRAY LISTENER THREAD ---
            std::thread::spawn(move || {
                while let Ok(event) = MenuEvent::receiver().recv() {
                    if event.id == show_id {
                        // FIX WINDOWS: Inviamo i comandi DIRETTAMENTE da qui
                        // Non aspettiamo il loop update()
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
            // ----------------------------

            // Non serve più passare flag atomici complessi
            Ok(Box::new(app::RustClipApp::new(cc, tx, rx, tray)))
        }),
    ).map_err(|e| anyhow::anyhow!("Errore GUI: {}", e))
}