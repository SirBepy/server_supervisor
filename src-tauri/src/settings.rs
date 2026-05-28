use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tauri_kit_settings::KitSettings;
use ts_rs::TS;

const FILE: &str = "settings.json";

/// App settings, persisted as `<app-data>/settings.json` via the kit store.
/// Kit-reserved keys (theme, auto-update) are flattened in so the kit settings
/// UI can read/write them on the same object.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct Settings {
    #[serde(default = "default_port")]
    pub api_port: u16,
    #[serde(default)]
    pub autostart: bool,
    #[serde(flatten)]
    #[ts(skip)]
    pub kit: KitSettings,
}

fn default_port() -> u16 {
    7717
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            api_port: default_port(),
            autostart: false,
            kit: KitSettings::default(),
        }
    }
}

pub fn load(app: &AppHandle) -> Settings {
    tauri_kit_settings::load_for(app, FILE).unwrap_or_default()
}

pub fn save(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    tauri_kit_settings::save_for(app, FILE, settings).map_err(|e| e.to_string())
}
