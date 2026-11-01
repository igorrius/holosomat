//! Пресети: вбудовані + власні (JSON-файли в папці користувача).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::dsp::{SpectralMode, VoiceParams};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(flatten)]
    pub params: VoiceParams,
}

/// Пресети, які йдуть у комплекті.
/// Налаштовані під дитячий голос — якщо щось звучить не так, крути повзунки.
pub fn builtin() -> Vec<Preset> {
    let d = VoiceParams::default();
    let p = |name: &str, description: &str, params: VoiceParams| Preset {
        name: name.to_string(),
        description: description.to_string(),
        params,
    };
    vec![
        p("Звичайний голос", "Без ефектів", d),
        p(
            "Голос бати",
            "Низький і серйозний. «Так, а уроки зроблені?»",
            VoiceParams {
                pitch: -8.0,
                formant: -4.0,
                low_cut: 70.0,
                drive: 0.08,
                reverb_size: 0.2,
                reverb_mix: 0.08,
                volume_db: 0.0,
                ..d
            },
        ),
        p(
            "Велетень",
            "Величезний і повільний, з відлунням",
            VoiceParams {
                pitch: -12.0,
                formant: -7.0,
                reverb_size: 0.75,
                reverb_mix: 0.3,
                volume_db: -1.0,
                ..d
            },
        ),
        p(
            "Бурундук",
            "Пискляво і швидко",
            VoiceParams {
                pitch: 9.0,
                formant: 7.0,
                ..d
            },
        ),
        p(
            "Робот",
            "Металевий монотонний голос",
            VoiceParams {
                mode: SpectralMode::Robot,
                ring_freq: 60.0,
                ring_mix: 0.35,
                crush_bits: 10,
                high_cut: 8000.0,
                gate_db: -50.0,
                volume_db: 3.0,
                ..d
            },
        ),
        p(
            "Рація",
            "Як по радіо: шипить і хрипить",
            VoiceParams {
                low_cut: 500.0,
                high_cut: 2800.0,
                drive: 0.5,
                crush_rate: 2,
                gate_db: -45.0,
                volume_db: -1.0,
                ..d
            },
        ),
        p(
            "Монстр з печери",
            "Низьке гарчання з луною",
            VoiceParams {
                pitch: -9.0,
                formant: -6.0,
                drive: 0.25,
                echo_ms: 180.0,
                echo_feedback: 0.3,
                echo_mix: 0.2,
                reverb_size: 0.85,
                reverb_mix: 0.4,
                volume_db: -4.0,
                ..d
            },
        ),
        p(
            "Привид",
            "Моторошний шепіт",
            VoiceParams {
                mode: SpectralMode::Whisper,
                pitch: 2.0,
                echo_ms: 300.0,
                echo_feedback: 0.4,
                echo_mix: 0.3,
                reverb_size: 0.9,
                reverb_mix: 0.5,
                gate_db: -50.0,
                volume_db: -1.0,
                ..d
            },
        ),
        p(
            "Інопланетянин",
            "Дивний голос з іншої планети",
            VoiceParams {
                pitch: 4.0,
                formant: -5.0,
                ring_freq: 300.0,
                ring_mix: 0.35,
                echo_ms: 90.0,
                echo_feedback: 0.25,
                echo_mix: 0.2,
                volume_db: 4.0,
                ..d
            },
        ),
        p(
            "8-біт",
            "Як у старій приставці",
            VoiceParams {
                pitch: 3.0,
                crush_bits: 5,
                crush_rate: 6,
                high_cut: 6000.0,
                gate_db: -50.0,
                ..d
            },
        ),
        p(
            "Диктор",
            "Трохи нижче, як на радіо",
            VoiceParams {
                pitch: -4.0,
                formant: -2.0,
                low_cut: 80.0,
                reverb_size: 0.3,
                reverb_mix: 0.1,
                ..d
            },
        ),
    ]
}

/// Папка з власними пресетами користувача.
pub struct PresetStore {
    dir: PathBuf,
}

impl PresetStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Усі `.json` файли з папки, відсортовані за назвою.
    pub fn list(&self) -> Vec<(PathBuf, Preset)> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("json"))
                && let Ok(preset) = load_file(&path)
            {
                out.push((path, preset));
            }
        }
        out.sort_by_key(|a| a.1.name.to_lowercase());
        out
    }

    pub fn save(&self, preset: &Preset) -> Result<PathBuf, String> {
        fs::create_dir_all(&self.dir).map_err(|e| format!("Не вдалося створити папку: {e}"))?;
        let path = self.dir.join(format!("{}.json", file_stem(&preset.name)));
        let json = serde_json::to_string_pretty(preset).map_err(|e| e.to_string())?;
        fs::write(&path, json).map_err(|e| format!("Не вдалося записати файл: {e}"))?;
        Ok(path)
    }

    pub fn delete(&self, path: &Path) -> Result<(), String> {
        fs::remove_file(path).map_err(|e| format!("Не вдалося видалити: {e}"))
    }
}

pub fn load_file(path: &Path) -> Result<Preset, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut preset: Preset = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if preset.name.trim().is_empty() {
        preset.name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Без назви".into());
    }
    Ok(preset)
}

/// Безпечне ім'я файлу з назви пресету (кирилиця дозволена).
pub fn file_stem(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        "preset".into()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip_and_partial_files() {
        let original = builtin()
            .into_iter()
            .find(|p| p.name == "Голос бати")
            .unwrap();
        let json = serde_json::to_string_pretty(&original).unwrap();
        let back: Preset = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);

        // Файл, у якому вказано лише частину полів, теж має читатися.
        let partial: Preset = serde_json::from_str(r#"{"name":"Мій","pitch":-3}"#).unwrap();
        assert_eq!(partial.params.pitch, -3.0);
        assert_eq!(partial.params.crush_bits, 16);
    }

    #[test]
    fn store_save_list_delete() {
        let dir = std::env::temp_dir().join(format!("holosomat-test-{}", std::process::id()));
        let store = PresetStore::new(dir.clone());
        let mut p = builtin()[1].clone();
        p.name = "Тест: пресет/1".into();
        let path = store.save(&p).unwrap();
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("Тест_ пресет_1")
        );
        let listed = store.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].1, p);
        store.delete(&path).unwrap();
        assert!(store.list().is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn builtin_names_are_unique() {
        let names: std::collections::HashSet<_> = builtin().into_iter().map(|p| p.name).collect();
        assert_eq!(names.len(), builtin().len());
    }
}
