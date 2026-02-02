pub mod app;
pub mod tray;

use eframe::egui;
use flume::{Sender, Receiver};
use crate::events::{UiCommand, CoreEvent};
use tray_icon::menu::MenuEvent;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

pub fn run_gui(tx: Sender<UiCommand>, rx: Receiver<CoreEvent>) -> anyhow::Result<()> {
    let tray = tray::AppTray::new()?;
    let show_id = tray.menu_item_show.id().clone();
    let quit_id = tray.menu_item_quit.id().clone();

    // FLAG CONDIVISO: "La finestra deve aprirsi?"
    let show_request = Arc::new(AtomicBool::new(false));
    let show_request_thread = show_request.clone();

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
            
            // --- TRAY THREAD (SVEGLIA) ---
            std::thread::spawn(move || {
                while let Ok(event) = MenuEvent::receiver().recv() {
                    if event.id == show_id {
                        // 1. Alza il flag
                        show_request_thread.store(true, Ordering::Relaxed);
                        // 2. SVEGLIA IL MAIN THREAD
                        ctx.request_repaint();
                    } 
                    else if event.id == quit_id {
                        let _ = tx_clone.send(UiCommand::Quit);
                        std::process::exit(0);
                    }
                }
            });

            // Passiamo il flag all'app
            Ok(Box::new(app::RustClipApp::new(cc, tx, rx, tray, show_request)))
        }),
    ).map_err(|e| anyhow::anyhow!("Errore GUI: {}", e))
}