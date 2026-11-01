//! Налаштування програми: обрані пристрої й останній голос.
//! Лежать у %APPDATA%\Holosomat\ (на Windows).

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::preset::Preset;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub input_id: Option<String>,
    pub output_id: Option<String>,
    pub monitor_id: Option<String>,
    pub monitor_enabled: bool,
    pub effect_on: Option<bool>,
    /// Поточний голос з усіма підкрученими повзунками.
    pub current: Option<Preset>,
}

pub struct Paths {
    pub settings_file: PathBuf,
    pub presets_dir: PathBuf,
}

impl Paths {
    pub fn detect() -> Self {
        let base = directories::ProjectDirs::from("", "", "Holosomat")
            .map(|d| d.config_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("holosomat-data"));
        Self {
            settings_file: base.join("settings.json"),
            presets_dir: base.join("presets"),
        }
    }
}

impl Settings {
    pub fn load(path: &PathBuf) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &PathBuf) {
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(path, json);
        }
    }
}
