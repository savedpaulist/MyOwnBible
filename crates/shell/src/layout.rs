//! Чтение/запись файла раскладки панелей (architecture.md §7.1).
//!
//! shell владеет файловым I/O для состояния UI; типы DockAreaState живут
//! в bible_ui, поэтому здесь — чистый текст JSON.

use anyhow::Result;

pub fn read_layout() -> Option<String> {
    std::fs::read_to_string(app_state::paths::layout_file()).ok()
}

pub fn write_layout(json: &str) -> Result<()> {
    let path = app_state::paths::layout_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, json)?;
    Ok(())
}
