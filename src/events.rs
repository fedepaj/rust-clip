use std::net::SocketAddr;
use crate::core::identity::RingIdentity;
use crate::core::config::AppConfig;

#[derive(Debug, Clone)]
pub enum LogLevel {
    Info,
    Success,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: LogLevel,
    pub message: String,
}

#[derive(Debug, Clone)]
pub enum CoreEvent {
    Log(LogEntry),
    #[allow(dead_code)] PeersUpdated(Vec<(String, SocketAddr)>),
    IdentityLoaded(RingIdentity),
    ServiceStateChanged { running: bool },
}

#[derive(Debug, Clone)]
pub enum UiCommand {
    SetPaused(bool),
    UpdateConfig(AppConfig), // <--- NUOVO: Salva nuova config
    #[allow(dead_code)] JoinRing(String),
    #[allow(dead_code)] GenerateNewIdentity,
    Quit,
}