use eframe::egui;
use crate::events::{UiCommand, CoreEvent};
use flume::{Sender, Receiver};
use std::net::SocketAddr;
use crate::ui::tray::AppTray;
use tray_icon::menu::MenuEvent; 

#[derive(PartialEq)]
enum Tab { Dashboard, Settings }

pub struct RustClipApp {
    tx: Sender<UiCommand>,
    rx: Receiver<CoreEvent>,
    tray: AppTray,
    
    current_tab: Tab,
    logs: Vec<String>,
    is_paused: bool,
    peers: Vec<(String, SocketAddr)>,
    
    my_ring_id: String,
    join_phrase: String,
    // show_mnemonic_window: bool, // (Commentato per ora se non usato)
}

impl RustClipApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, tx: Sender<UiCommand>, rx: Receiver<CoreEvent>, tray: AppTray) -> Self {
        Self {
            tx, rx, tray,
            current_tab: Tab::Dashboard,
            logs: vec![],
            is_paused: false,
            peers: vec![],
            my_ring_id: "Caricamento...".into(),
            join_phrase: String::new(),
            // show_mnemonic_window: false,
        }
    }

    fn update_state(&mut self) {
        // Leggi messaggi dal Backend
        while let Ok(event) = self.rx.try_recv() {
            match event {
                CoreEvent::Log(entry) => self.logs.push(format!("[{}] {}", entry.timestamp, entry.message)),
                CoreEvent::PeersUpdated(list) => self.peers = list,
                CoreEvent::IdentityLoaded(id) => self.my_ring_id = id.discovery_id,
                CoreEvent::ServiceStateChanged { running } => self.is_paused = !running,
            }
        }
    }
}

impl eframe::App for RustClipApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 1. Aggiorna stato dal backend
        self.update_state();

        // 2. CONTROLLO TRAY ICON (Fondamentale farlo qui)
        // Leggiamo gli eventi del menu di sistema
        if let Ok(event) = MenuEvent::receiver().try_recv() {
             if event.id == self.tray.menu_item_show.id() {
                 // ORDINE: Prima rendi visibile, poi porta in primo piano, poi richiedi repaint
                 ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                 ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                 ctx.request_repaint(); 
             }
             if event.id == self.tray.menu_item_quit.id() {
                 println!("Chiusura richiesta da Tray");
                 // Manda comando di stop al backend (opzionale) e chiudi
                 let _ = self.tx.send(UiCommand::Quit);
                 std::process::exit(0);
             }
        }

        // 3. GESTIONE CHIUSURA FINESTRA ("X" button)
        if ctx.input(|i| i.viewport().close_requested()) {
            // Invece di chiudere, nascondiamo
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        // 4. UI (Disegnamo solo se visibile per risparmiare GPU, 
        // ma Egui gestisce questo internamente se la finestra è Hidden)
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

            match self.current_tab {
                Tab::Dashboard => {
                    let btn_text = if self.is_paused { "▶️ RIPRENDI SYNC" } else { "⏸️ METTI IN PAUSA" };
                    if ui.add(egui::Button::new(btn_text).min_size(egui::vec2(0.0, 30.0))).clicked() {
                        let _ = self.tx.send(UiCommand::SetPaused(!self.is_paused));
                    }
                    
                    ui.add_space(15.0);
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
                    ui.label("Log Eventi:");
                    egui::ScrollArea::vertical().max_height(150.0).stick_to_bottom(true).show(ui, |ui| {
                        for log in &self.logs { ui.monospace(log); }
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
                    if ui.button("🚪 Esci (Quit)").clicked() {
                        let _ = self.tx.send(UiCommand::Quit);
                        std::process::exit(0);
                    }
                }
            }
        });
        
        // 5. IL FIX CRUCIALE (HEARTBEAT)
        // Questo dice a Egui: "Anche se non succede nulla, svegliati tra 500ms"
        // Questo permette di controllare il canale Tray anche se la finestra è nascosta.
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}