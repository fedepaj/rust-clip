use eframe::egui;
use crate::events::{UiCommand, CoreEvent, PeerInfo};
use flume::{Sender, Receiver};
use crate::ui::tray::AppTray;
use crate::core::config::AppConfig; 
use notify_rust::Notification; // Notification da UI

#[derive(PartialEq)]
enum Tab { Dashboard, Settings }

pub struct RustClipApp {
    tx: Sender<UiCommand>,
    rx: Receiver<CoreEvent>,
    _tray: AppTray,
    
    current_tab: Tab,
    logs: Vec<String>,
    is_paused: bool,
    peers: Vec<PeerInfo>, // Updated from Tuple
    
    // Dati
    my_ring_id: String,
    my_mnemonic: String,
    config: AppConfig, 
    
    // UI State
    join_phrase: String,
    show_mnemonic: bool,
    show_confirmation: bool, // NUOVO
}

impl RustClipApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, tx: Sender<UiCommand>, rx: Receiver<CoreEvent>, tray: AppTray) -> Self {
        let config = AppConfig::load();
        let app = Self {
            tx, rx, 
            _tray: tray,
            current_tab: Tab::Dashboard,
            logs: vec![],
            is_paused: false,
            peers: vec![],
            my_ring_id: "Loading...".into(),
            my_mnemonic: String::new(),
            config: config.clone(), 
            join_phrase: String::new(),
            show_mnemonic: false,
            show_confirmation: false,
        };
        // Initialize locale
        rust_i18n::set_locale(&config.language);
        app
    }

    fn update_state(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                CoreEvent::Log(entry) => self.logs.push(format!("[{}] {}", entry.timestamp, entry.message)),
                CoreEvent::PeersUpdated(list) => self.peers = list,
                CoreEvent::IdentityLoaded(id) => {
                    self.my_ring_id = id.discovery_id;
                    self.my_mnemonic = id.mnemonic;
                },
                CoreEvent::ServiceStateChanged { running } => {
                    println!("UI: ServiceStateChanged -> running={}", running);
                    self.is_paused = !running;
                },
                CoreEvent::Notify { title, body } => {
                    // Visualizza notifica nativa
                    let _ = Notification::new().summary(&title).body(&body).show();
                }
            }
        }
    }
}

impl eframe::App for RustClipApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.update_state();

        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            use rust_i18n::t;

            // HEADER
            ui.horizontal(|ui| {
                ui.heading(t!("app.title"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.is_paused {
                        ui.label(egui::RichText::new(t!("app.paused")).strong().color(egui::Color32::RED));
                    } else {
                        ui.label(egui::RichText::new(t!("app.active")).strong().color(egui::Color32::GREEN));
                    }
                });
            });
            ui.separator();

            // TABS
            ui.horizontal(|ui| {
                if ui.selectable_label(self.current_tab == Tab::Dashboard, t!("dashboard.tab")).clicked() { self.current_tab = Tab::Dashboard; }
                if ui.selectable_label(self.current_tab == Tab::Settings, t!("settings.tab")).clicked() { self.current_tab = Tab::Settings; }
            });
            ui.separator();

            match self.current_tab {
                Tab::Dashboard => {
                    let btn_text = if self.is_paused { t!("dashboard.resume_sync") } else { t!("dashboard.pause_sync") };
                    if ui.add(egui::Button::new(btn_text).min_size(egui::vec2(0.0, 30.0))).clicked() {
                        let _ = self.tx.send(UiCommand::SetPaused(!self.is_paused));
                    }
                    
                    ui.add_space(15.0);
                    ui.label(egui::RichText::new(t!("dashboard.connected_devices")).strong());
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        if self.peers.is_empty() {
                            ui.label(t!("dashboard.no_devices"));
                        } else {
                            for peer in &self.peers {
                                ui.horizontal(|ui| {
                                    ui.label("🖥️");
                                    ui.label(egui::RichText::new(&peer.name).strong());
                                    ui.label(format!("({})", peer.ip));
                                });
                            }
                        }
                    });
                    ui.add_space(15.0);
                    ui.label(t!("dashboard.logs"));
                    egui::ScrollArea::vertical().max_height(150.0).stick_to_bottom(true).show(ui, |ui| {
                        for log in &self.logs { ui.monospace(log); }
                    });
                },
                Tab::Settings => {
                    ui.heading(t!("settings.title"));
                    
                    ui.horizontal(|ui| {
                         ui.label(t!("settings.language"));
                         let current = self.config.language.clone();
                         egui::ComboBox::from_id_salt("lang_selector")
                             .selected_text(if current == "it" { "🇮🇹 Italiano" } else { "🇺🇸 English" })
                             .show_ui(ui, |ui| {
                                 let mut changed = false;
                                 if ui.selectable_value(&mut self.config.language, "en".to_string(), "🇺🇸 English").clicked() { changed = true; }
                                 if ui.selectable_value(&mut self.config.language, "it".to_string(), "🇮🇹 Italiano").clicked() { changed = true; }
                                 
                                 if changed {
                                     rust_i18n::set_locale(&self.config.language);
                                     let _ = self.tx.send(UiCommand::UpdateConfig(self.config.clone()));
                                 }
                             });
                    });

                    // --- NOME DISPOSITIVO ---
                    ui.horizontal(|ui| {
                        ui.label(t!("settings.device_name"));
                        if ui.text_edit_singleline(&mut self.config.device_name).lost_focus() {
                            let _ = self.tx.send(UiCommand::UpdateConfig(self.config.clone()));
                        }
                    });
                    
                    // --- NOTIFICHE ---
                    if ui.checkbox(&mut self.config.notifications_enabled, t!("settings.enable_notifications")).changed() {
                        let _ = self.tx.send(UiCommand::UpdateConfig(self.config.clone()));
                    }
                    
                    // --- AUTO START ---
                    if ui.checkbox(&mut self.config.auto_start, t!("settings.auto_start")).changed() {
                        let _ = self.tx.send(UiCommand::UpdateConfig(self.config.clone()));
                    }

                    ui.separator();
                    ui.add_space(10.0);

                    // --- CHIAVE SEGRETA ---
                    ui.label(egui::RichText::new(t!("settings.credentials")).strong());
                    ui.label(format!("{} {}", t!("settings.public_id"), self.my_ring_id));
                    ui.label(t!("settings.secret_key"));
                    ui.group(|ui| {
                        ui.horizontal_wrapped(|ui| {
                            if self.show_mnemonic {
                                ui.add(egui::Label::new(
                                    egui::RichText::new(&self.my_mnemonic).monospace()
                                ).wrap());
                            } else {
                                ui.label("*************************************************");
                            }
                        });
                        ui.add_space(5.0);
                        ui.horizontal(|ui| {
                            if ui.button(if self.show_mnemonic { t!("settings.hide_key") } else { t!("settings.show_key") }).clicked() {
                                self.show_mnemonic = !self.show_mnemonic;
                            }
                            if ui.button(t!("settings.copy_key")).clicked() {
                                ui.output_mut(|o| o.copied_text = self.my_mnemonic.clone());
                            }
                        });
                    });

                    ui.add_space(20.0);
                    ui.separator();
                    
                    ui.label(egui::RichText::new(t!("settings.join_ring")).strong());
                    ui.text_edit_multiline(&mut self.join_phrase);
                    if ui.button(t!("settings.join_btn")).clicked() {
                        if !self.join_phrase.is_empty() {
                            let _ = self.tx.send(UiCommand::JoinRing(self.join_phrase.clone()));
                            self.join_phrase.clear();
                        }
                    }

                    ui.add_space(20.0);
                    ui.separator();

                    ui.label(egui::RichText::new(t!("settings.danger_zone")).strong().color(egui::Color32::RED));
                    
                    if self.show_confirmation {
                        ui.label(egui::RichText::new(t!("settings.confirm_title")).heading().color(egui::Color32::RED));
                        ui.label(t!("settings.confirm_msg"));
                        
                        ui.horizontal(|ui| {
                            if ui.button(egui::RichText::new(t!("settings.confirm_yes")).color(egui::Color32::RED)).clicked() {
                                let _ = self.tx.send(UiCommand::GenerateNewIdentity);
                                self.show_confirmation = false;
                            }
                            if ui.button(t!("settings.confirm_cancel")).clicked() {
                                self.show_confirmation = false;
                            }
                        });
                    } else {
                        if ui.button(t!("settings.generate_id")).clicked() {
                            self.show_confirmation = true;
                        }
                    }
                    
                    ui.add_space(20.0);
                    if ui.button(t!("settings.quit")).clicked() {
                        let _ = self.tx.send(UiCommand::Quit);
                        std::process::exit(0);
                    }
                }
            }
        });
        
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}