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
    #[serde(default)] // default false
    pub autostart: bool,
    #[serde(default = "default_true")]
    pub ai_can_add_commands: bool,
    #[serde(default = "default_true")]
    pub ai_can_add_projects: bool,
    #[serde(default)] // default false
    pub show_command_count: bool,
    #[serde(default = "default_true")]
    pub show_ram: bool,
    #[serde(default = "default_true")]
    pub show_port: bool,
    #[serde(flatten)]
    #[ts(skip)]
    pub kit: KitSettings,
}

fn default_port() -> u16 {
    6969
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            api_port: default_port(),
            autostart: false,
            ai_can_add_commands: true,
            ai_can_add_projects: true,
            show_command_count: false,
            show_ram: true,
            show_port: true,
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

#[cfg(test)]
mod tests {
    use super::Settings;

    #[test]
    fn omitted_dashboard_prefs_use_defaults() {
        // An empty settings object must deserialize to the locked defaults:
        // count off, RAM/port on.
        let s: Settings = serde_json::from_str("{}").expect("deserialize {}");
        assert_eq!(s.show_command_count, false);
        assert_eq!(s.show_ram, true);
        assert_eq!(s.show_port, true);
    }
}
