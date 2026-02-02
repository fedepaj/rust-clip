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
pub struct PeerInfo {
    pub name: String,
    pub ip: SocketAddr,
    pub device_id: String,
    pub last_seen: std::time::SystemTime,
}

#[derive(Debug, Clone)]
pub enum CoreEvent {
    Log(LogEntry),
    // Updated to transport PeerInfo instead of tuple
    PeersUpdated(Vec<PeerInfo>),
    IdentityLoaded(RingIdentity),
    ServiceStateChanged { running: bool },
    // Decoupled notification request
    Notify { title: String, body: String },
}

#[derive(Debug, Clone)]
pub enum UiCommand {
    SetPaused(bool),
    UpdateConfig(AppConfig), // <--- NUOVO: Salva nuova config
    #[allow(dead_code)] JoinRing(String),
    #[allow(dead_code)] GenerateNewIdentity,
    Quit,
}