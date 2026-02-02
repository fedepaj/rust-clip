pub mod app;

use eframe::egui;
use flume::{Sender, Receiver};
use crate::events::{UiCommand, CoreEvent};

pub fn run_gui(tx: Sender<UiCommand>, rx: Receiver<CoreEvent>) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 500.0])
            .with_resizable(false),
        ..Default::default()
    };

    eframe::run_native(
        "RustClip",
        options,
        // Eframe 0.30 richiede Ok(Box::new(...)) dentro la closure
        Box::new(|cc| Ok(Box::new(app::RustClipApp::new(cc, tx, rx)))),
    ).map_err(|e| anyhow::anyhow!("Errore GUI: {}", e))
}