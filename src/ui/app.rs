use eframe::egui;
use crate::events::{UiCommand, CoreEvent};
use flume::{Sender, Receiver};

#[derive(PartialEq)]
enum Tab {
    Dashboard,
    Settings,
}

pub struct RustClipApp {
    tx_to_core: Sender<UiCommand>,
    rx_from_core: Receiver<CoreEvent>,
    
    current_tab: Tab,
    logs: Vec<String>,
    is_paused: bool,
    
    // Mettiamo #[allow(dead_code)] temporaneamente finché non implementiamo la UI completa
    #[allow(dead_code)]
    show_mnemonic: bool,
    my_ring_id: String,
}

impl RustClipApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, tx: Sender<UiCommand>, rx: Receiver<CoreEvent>) -> Self {
        Self {
            tx_to_core: tx,
            rx_from_core: rx,
            current_tab: Tab::Dashboard,
            logs: vec![String::from("App avviata. In attesa del Core...")],
            is_paused: false,
            show_mnemonic: false,
            my_ring_id: String::from("Caricamento..."),
        }
    }

    fn handle_backend_events(&mut self) {
        while let Ok(event) = self.rx_from_core.try_recv() {
            match event {
                CoreEvent::Log(entry) => {
                    let icon = match entry.level {
                        crate::events::LogLevel::Info => "ℹ️",
                        crate::events::LogLevel::Success => "✅",
                        crate::events::LogLevel::Warn => "⚠️",
                        crate::events::LogLevel::Error => "❌",
                    };
                    self.logs.push(format!("{} {} - {}", entry.timestamp, icon, entry.message));
                    if self.logs.len() > 50 { self.logs.remove(0); }
                }
                CoreEvent::PeersUpdated(_peers) => {}
                CoreEvent::IdentityLoaded(id) => {
                    self.my_ring_id = id.discovery_id; 
                }
                CoreEvent::ServiceStateChanged { running } => {
                    self.is_paused = !running;
                }
            }
        }
    }
}

impl eframe::App for RustClipApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_backend_events();

        egui::CentralPanel::default().show(ctx, |ui| {
            // HEADER
            ui.horizontal(|ui| {
                ui.heading("RustClip 🦀");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.is_paused {
                        ui.label(egui::RichText::new("PAUSA").color(egui::Color32::RED));
                    } else {
                        ui.label(egui::RichText::new("ATTIVO").color(egui::Color32::GREEN));
                    }
                });
            });
            ui.separator();

            // TABS
            ui.horizontal(|ui| {
                if ui.selectable_label(self.current_tab == Tab::Dashboard, "📊 Dashboard").clicked() {
                    self.current_tab = Tab::Dashboard;
                }
                if ui.selectable_label(self.current_tab == Tab::Settings, "⚙️ Impostazioni").clicked() {
                    self.current_tab = Tab::Settings;
                }
            });
            ui.separator();

            // CONTENUTO
            match self.current_tab {
                Tab::Dashboard => {
                    ui.label("Dispositivi connessi:");
                    ui.indent("peers", |ui| {
                        ui.label("🖥️ (In attesa di peer...)"); 
                    });
                    
                    ui.add_space(20.0);
                    
                    if ui.button(if self.is_paused { "▶️ RIPRENDI SYNC" } else { "⏸️ METTI IN PAUSA" }).clicked() {
                        self.is_paused = !self.is_paused;
                        let _ = self.tx_to_core.send(UiCommand::SetPaused(self.is_paused));
                    }
                    
                    ui.add_space(20.0);
                    ui.label("Log Eventi:");
                    egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                        for log in &self.logs {
                            ui.label(log);
                        }
                    });
                },
                Tab::Settings => {
                    ui.label(format!("Il tuo ID Pubblico: {}", self.my_ring_id));
                    ui.add_space(10.0);
                    
                    if ui.button("🚪 Esci e Chiudi Applicazione").clicked() {
                        let _ = self.tx_to_core.send(UiCommand::Quit);
                        std::process::exit(0);
                    }
                }
            }
        });
        
        ctx.request_repaint();
    }
}