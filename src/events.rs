use std::net::SocketAddr;
use crate::core::identity::RingIdentity;

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
    #[allow(dead_code)] GenerateNewIdentity,
    #[allow(dead_code)] JoinRing(String),
    Quit,
}