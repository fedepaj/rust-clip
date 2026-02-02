use eframe::egui;
use crate::events::{UiCommand, CoreEvent, LogEntry};
use flume::{Sender, Receiver};
use std::net::SocketAddr;

#[derive(PartialEq)]
enum Tab { Dashboard, Settings }

pub struct RustClipApp {
    tx: Sender<UiCommand>,
    rx: Receiver<CoreEvent>,
    
    current_tab: Tab,
    logs: Vec<String>,
    is_paused: bool,
    peers: Vec<(String, SocketAddr)>, // Lista locale per la GUI
    
    // Form Inputs
    my_ring_id: String,
    join_phrase: String,
    show_mnemonic_window: bool,
}

impl RustClipApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, tx: Sender<UiCommand>, rx: Receiver<CoreEvent>) -> Self {
        Self {
            tx, rx,
            current_tab: Tab::Dashboard,
            logs: vec![],
            is_paused: false,
            peers: vec![],
            my_ring_id: "Caricamento...".into(),
            join_phrase: String::new(),
            show_mnemonic_window: false,
        }
    }

    fn update_state(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                CoreEvent::Log(entry) => {
                    self.logs.push(format!("[{}] {}", entry.timestamp, entry.message));
                }
                CoreEvent::PeersUpdated(list) => {
                    self.peers = list;
                }
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
        self.update_state();

        egui::CentralPanel::default().show(ctx, |ui| {
            // HEADER
            ui.horizontal(|ui| {
                ui.heading("RustClip 🦀");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.is_paused {
                        ui.label(egui::RichText::new("🔴 PAUSA").strong().color(egui::Color32::RED));
                    } else {
                        ui.label(egui::RichText::new("🟢 ATTIVO").strong().color(egui::Color32::GREEN));
                    }
                });
            });
            ui.separator();

            // TABS
            ui.horizontal(|ui| {
                if ui.selectable_label(self.current_tab == Tab::Dashboard, "📊 Dashboard").clicked() { self.current_tab = Tab::Dashboard; }
                if ui.selectable_label(self.current_tab == Tab::Settings, "⚙️ Impostazioni").clicked() { self.current_tab = Tab::Settings; }
            });
            ui.separator();

            // BODY
            match self.current_tab {
                Tab::Dashboard => {
                    // PAUSE BUTTON
                    let btn_text = if self.is_paused { "▶️ RIPRENDI SINCRONIZZAZIONE" } else { "⏸️ METTI IN PAUSA" };
                    if ui.add(egui::Button::new(btn_text).min_size(egui::vec2(0.0, 30.0))).clicked() {
                        let _ = self.tx.send(UiCommand::SetPaused(!self.is_paused));
                    }
                    
                    ui.add_space(15.0);
                    
                    // PEER LIST
                    ui.label(egui::RichText::new("Dispositivi Connessi:").strong());
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        if self.peers.is_empty() {
                            ui.label("nessun dispositivo trovato...");
                        } else {
                            for (name, ip) in &self.peers {
                                ui.horizontal(|ui| {
                                    ui.label("🖥️");
                                    ui.label(egui::RichText::new(name).strong());
                                    ui.label(format!("({})", ip));
                                });
                            }
                        }
                    });

                    ui.add_space(15.0);

                    // LOGS
                    ui.label("Log Eventi:");
                    egui::ScrollArea::vertical().max_height(150.0).stick_to_bottom(true).show(ui, |ui| {
                        for log in &self.logs {
                            ui.monospace(log);
                        }
                    });
                },
                Tab::Settings => {
                    ui.heading("Gestione Ring");
                    ui.label(format!("ID Pubblico: {}", self.my_ring_id));
                    
                    ui.add_space(20.0);
                    ui.separator();
                    
                    ui.label(egui::RichText::new("Unisciti a un altro Ring").strong());
                    ui.text_edit_multiline(&mut self.join_phrase);
                    if ui.button("🔗 Unisciti (Join Ring)").clicked() {
                        if !self.join_phrase.is_empty() {
                            let _ = self.tx.send(UiCommand::JoinRing(self.join_phrase.clone()));
                            self.join_phrase.clear();
                        }
                    }

                    ui.add_space(20.0);
                    ui.separator();

                    ui.label(egui::RichText::new("Zona Pericolo").strong().color(egui::Color32::RED));
                    if ui.button("⚠️ Genera Nuova Identità").clicked() {
                        let _ = self.tx.send(UiCommand::GenerateNewIdentity);
                    }
                    
                    ui.add_space(20.0);
                    if ui.button("🚪 Esci (Quit)").clicked() {
                        let _ = self.tx.send(UiCommand::Quit);
                    }
                }
            }
        });
        
        ctx.request_repaint(); // Refresh continuo per animazioni/log fluidi
    }
}