use serde::{Serialize, Deserialize};
use std::path::PathBuf;
use std::fs;
use anyhow::Result;
use directories::ProjectDirs;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppConfig {
    pub device_name: String,
    pub notifications_enabled: bool,
    pub auto_start: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        // Fix: Logica semplificata per ottenere il nome host
        let name = hostname::get()
            .ok()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "RustClip Device".to_string());

        Self {
            device_name: name,
            notifications_enabled: true,
            auto_start: false,
        }
    }
}

impl AppConfig {
    fn get_path() -> Result<PathBuf> {
        let proj = ProjectDirs::from("com", "rustclip", "rust-clip")
            .ok_or_else(|| anyhow::anyhow!("Impossibile determinare cartella config"))?;
        
        let config_dir = proj.config_dir();
        if !config_dir.exists() {
            fs::create_dir_all(config_dir)?;
        }
        
        Ok(config_dir.join("config.json"))
    }

    pub fn load() -> Self {
        if let Ok(path) = Self::get_path() {
            println!("📂 Config Path: {:?}", path);
            if let Ok(content) = fs::read_to_string(path) {
                if let Ok(cfg) = serde_json::from_str(&content) {
                    return cfg;
                }
            }
        }
        Self::default()
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::get_path()?;
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }
}