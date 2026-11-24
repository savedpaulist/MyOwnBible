//! Сессия: последнее место чтения (architecture.md §7.2).
//!
//! Отдельно от settings.json: позиция пишется часто (при прокрутке),
//! настройки — только по явному изменению. Битый файл молча заменяется
//! дефолтом: потерять позицию чтения не страшно.

use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionState {
    /// Последнее место чтения (панель «Чтение»).
    pub reading_position: Option<SavedPosition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedPosition {
    /// ModuleId (stem файла модуля).
    pub module: String,
    pub book: i64,
    pub chapter: u16,
    pub verse: u16,
}

pub fn load() -> SessionState {
    std::fs::read_to_string(paths::session_file())
        .ok()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

pub fn save(state: &SessionState) -> anyhow::Result<()> {
    let path = paths::session_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(state)?;
    std::fs::write(path, json)?;
    Ok(())
}
