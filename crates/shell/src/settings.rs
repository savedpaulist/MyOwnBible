//! Сценарии настроек (architecture.md §9, этап 9).
//!
//! Разделение ответственности: `update` — дешёвая правка состояния и файла
//! (вызывается на каждое изменение поля экрана настроек); тяжёлые побочные
//! эффекты — `rescan_modules` и `reinit_notes` — вызываются явно
//! (кнопка «Применить» у соответствующего поля), чтобы полунабранный путь
//! в текстовом поле не дёргал перескан каталога на каждое нажатие клавиши.

use anyhow::Result;
use bible_domain::ports::ModuleCatalog as _;

pub use app_state::settings::{
    CopyReferencePosition, InterfaceLanguage, LinkOpenMode, MorphologyColorScheme, Settings,
    TabSize,
};

use crate::services::AppServices;

static SETTINGS_SAVE: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// Location of the persisted settings file. Presentation code may display
/// this path, but does not need to know which storage adapter owns it.
pub fn file_path() -> std::path::PathBuf {
    app_state::paths::settings_file()
}

/// Снимок текущих настроек (для экрана настроек).
pub fn current(services: &AppServices) -> Settings {
    services.settings.read().clone()
}

/// Применить правку к настройкам и сохранить settings.json.
pub fn update(services: &AppServices, patch: impl FnOnce(&mut Settings)) -> Result<()> {
    let _save = SETTINGS_SAVE.lock();
    // Сначала собираем новый снимок, затем сохраняем его на диск и только
    // после успешной записи публикуем в памяти. Так UI не показывает
    // «применённое» значение, которое на самом деле не удалось сохранить.
    // fsync и проверочное чтение файла могут быть медленными; render не
    // должен ждать их под write-lock.
    let mut candidate = services.settings.read().clone();
    patch(&mut candidate);

    if let Err(error) = app_state::settings::save(&candidate) {
        log::error!(
            "failed to persist application settings at {}: {error:#}",
            app_state::paths::settings_file().display()
        );
        return Err(error);
    }
    log::debug!(
        "application settings saved to {}",
        app_state::paths::settings_file().display()
    );

    *services.settings.write() = candidate;
    Ok(())
}

/// Пересканировать каталог модулей из настроек (кнопка «Применить»
/// у поля modules_dir). Возвращает число найденных модулей.
pub fn rescan_modules(services: &AppServices) -> usize {
    let dir = services.settings.read().modules_dir.clone();
    services.registry.lock().rescan_dir(&dir);
    services.registry.modules().len()
}

/// Переинициализировать хранилище заметок из настроек (кнопка «Применить»
/// у поля notes_dir): старый watcher останавливается (drop), индекс
/// заменяется целиком. Возвращает, включены ли заметки после применения.
pub fn reinit_notes(services: &AppServices) -> bool {
    let snapshot = services.settings.read().clone();
    let (index, watcher) = crate::notes::init(&snapshot, &services.registry);
    let enabled = index.is_some();
    *services.notes.write() = index;
    *services._notes_watcher.lock() = watcher;
    enabled
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::{Arc, Mutex as StdMutex};

    use mybible::{ModuleRegistry, SharedRegistry};
    use parking_lot::Mutex;

    /// Тесты правят глобальный override каталога данных (settings.json
    /// пишется в него) — сериализуем их общим мьютексом.
    static DATA_DIR_LOCK: StdMutex<()> = StdMutex::new(());

    /// Модуль-фикстура: verses + books — классифицируется как перевод.
    fn write_module(dir: &Path, name: &str) {
        let path = dir.join(format!("{name}.sqlite3"));
        let connection = sqlez::connection::Connection::open_file(path.to_str().unwrap());
        connection
            .exec("CREATE TABLE info(name TEXT, value TEXT)")
            .unwrap()()
        .unwrap();
        connection
            .exec("INSERT INTO info VALUES('language', 'ru')")
            .unwrap()()
        .unwrap();
        connection
            .exec(
                "CREATE TABLE verses(\
                   book_number INTEGER, chapter INTEGER, verse INTEGER, text TEXT)",
            )
            .unwrap()()
        .unwrap();
        connection
            .exec(
                "CREATE TABLE books(\
                   book_number INTEGER, short_name TEXT, long_name TEXT)",
            )
            .unwrap()()
        .unwrap();
        connection
            .exec("INSERT INTO books VALUES(500, 'Ин', 'Иоанн')")
            .unwrap()()
        .unwrap();
        drop(connection);
    }

    fn services(modules_dir: &Path) -> AppServices {
        let mut settings = Settings::default();
        settings.modules_dir = modules_dir.to_path_buf();
        AppServices {
            settings: parking_lot::RwLock::new(settings),
            registry: SharedRegistry::new(ModuleRegistry::scan(modules_dir)),
            current_location: Arc::new(Mutex::new(None)),
            state_db: None,
            user_cross_refs: None,
            annotations: None,
            chains: None,
            tags: None,
            search_index: None,
            index_building: Arc::new(Mutex::new(std::collections::HashSet::new())),
            notes: parking_lot::RwLock::new(None),
            _notes_watcher: Mutex::new(None),
        }
    }

    #[test]
    fn update_persists_and_rescan_switches_modules() {
        let _guard = DATA_DIR_LOCK.lock().unwrap();
        let data = tempfile::tempdir().unwrap();
        app_state::paths::set_data_dir_override(Some(data.path().to_path_buf()));

        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        write_module(dir_a.path(), "AAA");
        write_module(dir_b.path(), "BBB");

        let services = services(dir_a.path());
        assert_eq!(rescan_modules(&services), 1);

        update(&services, |s| {
            s.modules_dir = dir_b.path().to_path_buf();
        })
        .unwrap();
        // Файл записан и читается обратно тем же load().
        let saved = app_state::settings::load();
        assert_eq!(saved.modules_dir, dir_b.path());
        // Перескан применяет новый каталог: старый модуль уходит.
        assert_eq!(rescan_modules(&services), 1);
        let modules = services.registry.modules();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].id, bible_domain::ids::ModuleId::from("BBB"));

        app_state::paths::set_data_dir_override(None);
    }

    #[test]
    fn update_persists_independent_typography_profiles() {
        let _guard = DATA_DIR_LOCK.lock().unwrap();
        let data = tempfile::tempdir().unwrap();
        app_state::paths::set_data_dir_override(Some(data.path().to_path_buf()));

        let modules = tempfile::tempdir().unwrap();
        let services = services(modules.path());
        update(&services, |settings| {
            settings.ui_font_family = "Inter".to_string();
            settings.ui_font_size = 18.0;
            settings.reading_font_family = "Noto Serif Hebrew".to_string();
            settings.reading_font_size = 24.0;
            settings.side_panel_font_family = "Atkinson Hyperlegible".to_string();
            settings.side_panel_font_size = 15.0;
            settings.side_panel_font_scale = 1.2;
            settings.interface_scale = 1.1;
        })
        .unwrap();

        let restored = app_state::settings::load();
        assert_eq!(restored.ui_font_family, "Inter");
        assert_eq!(restored.ui_font_size, 18.0);
        assert_eq!(restored.reading_font_family, "Noto Serif Hebrew");
        assert_eq!(restored.reading_font_size, 24.0);
        assert_eq!(restored.side_panel_font_family, "Atkinson Hyperlegible");
        assert_eq!(restored.side_panel_font_size, 15.0);
        assert_eq!(restored.side_panel_font_scale, 1.2);
        assert_eq!(restored.interface_scale, 1.1);

        app_state::paths::set_data_dir_override(None);
    }

    #[test]
    fn reinit_notes_toggles_storage() {
        let _guard = DATA_DIR_LOCK.lock().unwrap();
        let data = tempfile::tempdir().unwrap();
        app_state::paths::set_data_dir_override(Some(data.path().to_path_buf()));

        let modules = tempfile::tempdir().unwrap();
        let vault = tempfile::tempdir().unwrap();
        let services = services(modules.path());

        // Заметки выключены: индекса нет, reinit подтверждает.
        assert!(!reinit_notes(&services));
        assert!(services.notes.read().is_none());

        // Включаем: reinit создаёт индекс и watcher.
        update(&services, |s| {
            s.notes_dir = Some(vault.path().to_path_buf());
        })
        .unwrap();
        assert!(reinit_notes(&services));
        assert!(services.notes.read().is_some());
        assert!(services._notes_watcher.lock().is_some());

        // Выключаем: индекс и watcher заменяются на None.
        update(&services, |s| s.notes_dir = None).unwrap();
        assert!(!reinit_notes(&services));
        assert!(services.notes.read().is_none());
        assert!(services._notes_watcher.lock().is_none());

        app_state::paths::set_data_dir_override(None);
    }
}
