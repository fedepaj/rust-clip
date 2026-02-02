pub mod app;
pub mod tray; // <--- NUOVO MODULO

use eframe::egui;
use flume::{Sender, Receiver};
use crate::events::{UiCommand, CoreEvent};

pub fn run_gui(tx: Sender<UiCommand>, rx: Receiver<CoreEvent>) -> anyhow::Result<()> {
    // 1. Inizializza la Tray Icon
    // Nota: La Tray deve vivere quanto l'app, quindi la passiamo alla struct App
    let tray = tray::AppTray::new()?;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 500.0])
            .with_resizable(false)
            // Importante: Gestiremo noi la chiusura
            .with_close_button(true), 
        ..Default::default()
    };

    eframe::run_native(
        "RustClip",
        options,
        // Passiamo la tray all'app
        Box::new(|cc| Ok(Box::new(app::RustClipApp::new(cc, tx, rx, tray)))),
    ).map_err(|e| anyhow::anyhow!("Errore GUI: {}", e))
}