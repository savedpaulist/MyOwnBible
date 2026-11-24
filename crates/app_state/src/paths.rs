//! Расположение данных приложения по платформам (architecture.md §4.2.1).

use std::path::PathBuf;
use std::sync::RwLock;

/// Тестовый override корня данных: сценарные тесты настроек пишут
/// settings.json, не трогая реальный %APPDATA%/BibleOWN. Тесты, ставящие
/// override, обязаны сериализоваться общим мьютексом и снимать override.
static DATA_DIR_OVERRIDE: RwLock<Option<PathBuf>> = RwLock::new(None);

pub fn set_data_dir_override(dir: Option<PathBuf>) {
    *DATA_DIR_OVERRIDE.write().unwrap() = dir;
}

/// Корневой каталог данных приложения.
///
/// Windows: `%APPDATA%/BibleOWN`
/// macOS:   `~/Library/Application Support/BibleOWN`
/// Linux:   `$XDG_DATA_HOME/bibleown` или `~/.local/share/bibleown`
pub fn data_dir() -> PathBuf {
    if let Some(dir) = DATA_DIR_OVERRIDE.read().unwrap().as_ref() {
        return dir.clone();
    }
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })
    };
    base.unwrap_or_else(|| PathBuf::from(".")).join("BibleOWN")
}

/// Раскладка панелей (DockAreaState в JSON).
pub fn layout_file() -> PathBuf {
    data_dir().join("layout.json")
}

/// Собственная БД состояния (этапы 2+).
pub fn state_db_file() -> PathBuf {
    data_dir().join("app_state.db")
}

/// Настройки (этапы 2+).
pub fn settings_file() -> PathBuf {
    data_dir().join("settings.json")
}

/// Сессия: последнее место чтения и др. (частые записи, JSON).
pub fn session_file() -> PathBuf {
    data_dir().join("session.json")
}

/// Базовый путь для FTS5-индексов. Файлы модулей лежат рядом в index.modules.
pub fn index_db_file() -> PathBuf {
    data_dir().join("index.db")
}
