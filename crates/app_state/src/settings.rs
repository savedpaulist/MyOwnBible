//! Настройки приложения: settings.json (architecture.md §9).
//!
//! Файл читается при старте; отсутствующие ключи дополняются дефолтами
//! (`serde(default)`), при первом запуске файл записывается целиком.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Язык интерфейса. Контент модулей Библии не переводится.
    pub interface_language: InterfaceLanguage,
    /// Каталог модулей MyBible (*.sqlite3).
    pub modules_dir: PathBuf,
    /// Каталог заметок (Markdown-хранилище). None — заметки отключены.
    pub notes_dir: Option<PathBuf>,
    /// Перевод по умолчанию (ModuleId, напр. "RST").
    pub default_translation: Option<String>,
    pub new_tabs_use_current_module: bool,
    pub tab_size: TabSize,
    /// Показывать критический аппарат ({…}, 「…», <n>…) в тексте.
    pub show_critical_text: bool,
    pub verse_preview_module: Option<String>,
    pub dictionary_font_family: String,
    pub dictionary_font_size: f32,
    pub left_panel_font_family: String,
    pub left_panel_font_size: f32,
    pub icon_size: f32,
    /// Номера стихов в тексте (v1: номер рисуется элементом, флаг на будущее).
    pub show_verse_numbers: bool,
    /// Куда открывать ссылки на места Писания (перекрёстные, поиск,
    /// палитра перехода): текущая вкладка чтения или новая (§5.4).
    pub link_open_mode: LinkOpenMode,
    /// Закреплённый Strong's-словарь для карточки слова (§6.4). None —
    /// автовыбор: первый Strong's-словарь каталога, где нашлась статья.
    pub preferred_strongs_dictionary: Option<String>,
    /// Имя темы оформления из реестра gpui-component (этап 13,
    /// polish-plan.md). Темы регистрирует bible_ui::theme из
    /// assets/themes/everforest.json; неизвестное имя — мягкий откат
    /// к дефолтной.
    pub theme: String,
    /// Левая менюшка свёрнута в узкую полосу иконок (этап 15).
    pub sidebar_collapsed: bool,
    /// Выбранный пункт левой менюшки (этап 15):
    /// «dictionaries» | «commentaries» | «notes».
    pub sidebar_section: String,
    /// Левая менюшка видима (кнопка в Status bar, этап 15).
    pub sidebar_visible: bool,
    /// Основной шрифт интерфейса. Пусто — системный шрифт темы.
    pub ui_font_family: String,
    /// Базовый размер шрифта интерфейса в px (до общего масштаба окна).
    pub ui_font_size: f32,
    /// Шрифт текста Библии. Пусто — наследовать основной шрифт интерфейса.
    pub reading_font_family: String,
    /// Размер текста Библии в px. 0.0 — использовать базовый размер темы.
    pub reading_font_size: f32,
    /// Общий масштаб интерфейса и типографики (1.0 = 100%).
    pub interface_scale: f32,
    /// Семейство шрифта боковых панелей. Пусто — наследовать основной шрифт.
    pub side_panel_font_family: String,
    /// Базовый размер текста боковых панелей в px.
    pub side_panel_font_size: f32,
    /// Дополнительный масштаб типографики боковых панелей (1.0 = 100%).
    pub side_panel_font_scale: f32,
    /// Межстрочный ритм в тексте Библии (множитель вертикальных интервалов).
    pub reading_line_spacing: f32,
    /// Максимальная ширина колонки текста Библии в px. 0 — без ограничения.
    pub reading_column_width: f32,
    pub morphology_color_scheme: MorphologyColorScheme,
    pub morphology_colors: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceLanguage {
    #[default]
    Russian,
    English,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MorphologyColorScheme {
    #[default]
    Off,
    PartsOfSpeech,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabSize {
    Compact,
    #[default]
    Normal,
    Large,
}

/// Режим открытия библейских ссылок (architecture.md §5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkOpenMode {
    /// Активная вкладка чтения переходит по ссылке (поведение до вкладок).
    #[default]
    ReuseCurrent,
    /// Каждая ссылка — новая вкладка чтения.
    NewTab,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            interface_language: InterfaceLanguage::Russian,
            modules_dir: default_modules_dir(),
            notes_dir: None,
            default_translation: None,
            new_tabs_use_current_module: false,
            tab_size: TabSize::Normal,
            show_critical_text: false,
            verse_preview_module: None,
            dictionary_font_family: String::new(),
            dictionary_font_size: 0.,
            left_panel_font_family: String::new(),
            left_panel_font_size: 0.,
            icon_size: 18.,
            show_verse_numbers: true,
            link_open_mode: LinkOpenMode::default(),
            preferred_strongs_dictionary: None,
            theme: default_theme(),
            sidebar_collapsed: false,
            sidebar_section: "dictionaries".to_string(),
            sidebar_visible: true,
            ui_font_family: String::new(),
            ui_font_size: 16.0,
            reading_font_family: String::new(),
            reading_font_size: 0.0,
            interface_scale: 1.0,
            side_panel_font_family: String::new(),
            side_panel_font_size: 14.0,
            side_panel_font_scale: 1.0,
            reading_line_spacing: 1.0,
            reading_column_width: 0.0,
            morphology_color_scheme: MorphologyColorScheme::Off,
            morphology_colors: Default::default(),
        }
    }
}

/// Дефолтная тема — тёмная Everforest Hard (этап 13). Строка, а не enum:
/// реестр тем живёт в bible_ui (gpui), app_state о нём не знает.
fn default_theme() -> String {
    "Everforest Dark Hard".to_string()
}

/// `~/Documents/MyBibleModules` — там лежат модули у пользователя.
fn default_modules_dir() -> PathBuf {
    let home = if cfg!(target_os = "windows") {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    } else {
        std::env::var_os("HOME").map(PathBuf::from)
    };
    home.map(|h| h.join("Documents").join("MyBibleModules"))
        .unwrap_or_else(|| PathBuf::from("MyBibleModules"))
}

/// Загрузить настройки. Если основной файл отсутствует или повреждён,
/// сначала пробуем восстановить последний Windows backup, и только затем
/// создаём настройки по умолчанию.
pub fn load() -> Settings {
    let path = paths::settings_file();
    match load_from_file(&path) {
        Ok(settings) => settings,
        Err(primary_error) => {
            // On Windows a save uses a short-lived .bak while replacing the
            // destination. Recover it after an interrupted shutdown instead
            // of silently resetting the user's preferences to defaults.
            let backup = path.with_extension("json.bak");
            if backup.exists() {
                match load_from_file(&backup) {
                    Ok(settings) => {
                        log::warn!(
                            "restored settings.json from backup after reading the primary file failed: {primary_error:#}"
                        );
                        if let Err(error) = save(&settings) {
                            log::warn!("failed to restore settings.json from backup: {error:#}");
                        }
                        return settings;
                    }
                    Err(backup_error) => log::error!(
                        "settings.json and its backup are unreadable ({primary_error:#}; backup: {backup_error:#})"
                    ),
                }
            } else if primary_error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            {
                log::info!("settings.json does not exist; creating defaults");
            } else {
                log::error!("cannot read settings.json: {primary_error:#}; using defaults");
            }

            let settings = Settings::default();
            if let Err(error) = save(&settings) {
                log::warn!("failed to write settings.json: {error:#}");
            }
            settings
        }
    }
}

fn load_from_file(path: &std::path::Path) -> anyhow::Result<Settings> {
    let json = std::fs::read_to_string(path)?;
    // Versions before the dropdown contract was fixed wrote display labels
    // ("80 %", "В новой вкладке") instead of stable values. Repair those
    // known representations before serde sees them, so one old control does
    // not make us discard the entire settings file.
    let mut value: serde_json::Value = serde_json::from_str(&json)?;
    if let Some(object) = value.as_object_mut() {
        normalize_legacy_dropdown_values(object);
    }
    Ok(serde_json::from_value::<Settings>(value)?)
}

fn normalize_legacy_dropdown_values(settings: &mut serde_json::Map<String, serde_json::Value>) {
    let labels = [
        ("link_open_mode", "В текущей вкладке", "reuse_current"),
        ("link_open_mode", "В новой вкладке", "new_tab"),
        ("reading_column_width", "Автоматически", "0"),
        ("reading_column_width", "720 px — компактно", "720"),
        ("reading_column_width", "880 px — стандарт", "880"),
        ("reading_column_width", "1040 px — широко", "1040"),
        ("reading_column_width", "1200 px — очень широко", "1200"),
        ("reading_line_spacing", "Плотно", "0.70"),
        ("reading_line_spacing", "Компактно", "0.85"),
        ("reading_line_spacing", "Обычно", "1.00"),
        ("reading_line_spacing", "Свободно", "1.25"),
        ("reading_line_spacing", "Очень свободно", "1.50"),
        ("reading_font_size", "По умолчанию", "0"),
        ("ui_font_family", "По умолчанию", ""),
        ("reading_font_family", "По умолчанию", ""),
        ("side_panel_font_family", "По умолчанию", ""),
    ];
    for (key, label, value) in labels {
        if settings.get(key).and_then(serde_json::Value::as_str) == Some(label) {
            settings.insert(
                key.to_string(),
                serde_json::Value::String(value.to_string()),
            );
        }
    }
    for key in [
        "ui_font_family",
        "reading_font_family",
        "side_panel_font_family",
    ] {
        if let Some(family) = settings
            .get(key)
            .and_then(serde_json::Value::as_str)
            .and_then(|name| name.strip_suffix(" (не найден сейчас)"))
        {
            settings.insert(
                key.to_string(),
                serde_json::Value::String(family.to_string()),
            );
        }
    }
    if settings
        .get("link_open_mode")
        .is_some_and(|value| !matches!(value.as_str(), Some("reuse_current" | "new_tab")))
    {
        settings.remove("link_open_mode");
    }
    let defaults = Settings::default();
    for (key, fallback) in [
        ("interface_scale", defaults.interface_scale),
        ("side_panel_font_scale", defaults.side_panel_font_scale),
        ("reading_column_width", defaults.reading_column_width),
        ("reading_line_spacing", defaults.reading_line_spacing),
        ("reading_font_size", defaults.reading_font_size),
        ("ui_font_size", defaults.ui_font_size),
        ("side_panel_font_size", defaults.side_panel_font_size),
    ] {
        let Some(text) = settings.get(key).and_then(serde_json::Value::as_str) else {
            continue;
        };
        let normalized = text.trim().trim_end_matches('%').trim();
        let number = normalized
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite());
        let value = number.map_or(fallback, |value| {
            if text.contains('%') {
                value / 100.0
            } else {
                value
            }
        });
        settings.insert(key.to_string(), serde_json::json!(value));
    }
    if settings
        .get("default_translation")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value.trim().is_empty() || value == "Авто — первый доступный перевод")
    {
        settings.remove("default_translation");
    }
}

pub fn save(settings: &Settings) -> anyhow::Result<()> {
    use std::io::Write as _;

    let path = paths::settings_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let json = serde_json::to_vec_pretty(settings)?;
    let temp_path = path.with_extension("json.tmp");
    #[cfg(windows)]
    let backup_path = path.with_extension("json.bak");

    {
        let mut file = std::fs::File::create(&temp_path)?;
        file.write_all(&json)?;
        file.sync_all()?;
    }

    // Keep a copy of the last known-good file while replacing it. On Windows
    // std::fs::rename does not replace an existing destination, so use the
    // native replace-capable move operation instead of moving the destination
    // out of the way first (which left a window where settings.json vanished).
    #[cfg(windows)]
    if path.exists() {
        if let Err(error) = std::fs::copy(&path, &backup_path) {
            log::warn!(
                "could not refresh settings backup {}: {error}; continuing with atomic replacement",
                backup_path.display()
            );
            // Never let an old backup masquerade as the latest settings if
            // making a fresh copy failed.
            let _ = std::fs::remove_file(&backup_path);
        }
    }

    if let Err(error) = replace_settings_file(&temp_path, &path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error.into());
    }

    // Verify the exact file that will be read on the next launch. This also
    // catches unexpected filesystem/antivirus interference before publishing
    // the new Settings snapshot in shell::settings::update.
    let persisted = std::fs::read(&path)?;
    if persisted != json {
        anyhow::bail!(
            "settings file verification failed after writing {}",
            path.display()
        );
    }

    #[cfg(windows)]
    if backup_path.exists() {
        if let Err(error) = std::fs::remove_file(&backup_path) {
            log::warn!(
                "could not remove settings backup {}: {error}",
                backup_path.display()
            );
        }
    }

    Ok(())
}

#[cfg(windows)]
fn replace_settings_file(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing_name: *const u16, new_name: *const u16, flags: u32) -> i32;
    }

    let from_wide: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to_wide: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // Both paths are NUL-terminated UTF-16, and the buffers remain alive for
    // the duration of the Win32 call.
    let moved = unsafe {
        MoveFileExW(
            from_wide.as_ptr(),
            to_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_settings_file(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

#[cfg(test)]
mod tests {
    use super::*;

    static DATA_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn theme_defaults_to_everforest_dark_hard() {
        // Старый settings.json без ключа — тема по умолчанию.
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.theme, "Everforest Dark Hard");
    }

    #[test]
    fn interface_language_is_backward_compatible_and_round_trips() {
        let legacy: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy.interface_language, InterfaceLanguage::Russian);

        let mut settings = Settings::default();
        settings.interface_language = InterfaceLanguage::English;
        let restored: Settings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored.interface_language, InterfaceLanguage::English);
    }

    #[test]
    fn loading_legacy_dropdown_labels_preserves_other_settings() {
        let mut raw = serde_json::json!({
            "theme": "Custom",
            "interface_scale": "80 %",
            "side_panel_font_scale": "130 %",
            "reading_font_family": "По умолчанию",
            "reading_font_size": "По умолчанию",
            "reading_column_width": "880 px — стандарт",
            "reading_line_spacing": "Свободно",
            "link_open_mode": "В новой вкладке"
        });
        normalize_legacy_dropdown_values(raw.as_object_mut().unwrap());
        let settings: Settings = serde_json::from_value(raw).unwrap();
        assert_eq!(settings.theme, "Custom");
        assert_eq!(settings.interface_scale, 0.8);
        assert_eq!(settings.side_panel_font_scale, 1.3);
        assert_eq!(settings.reading_font_family, "");
        assert_eq!(settings.reading_font_size, 0.0);
        assert_eq!(settings.reading_column_width, 880.0);
        assert_eq!(settings.reading_line_spacing, 1.25);
        assert_eq!(settings.link_open_mode, LinkOpenMode::NewTab);
    }

    #[test]
    fn typography_settings_have_safe_defaults_and_load_legacy_config() {
        let settings: Settings = serde_json::from_str(
            r##"{"theme":"Everforest Dark Hard","palette_overrides":{"yellow":"#ffcc00"}}"##,
        )
        .unwrap();

        assert_eq!(settings.ui_font_family, "");
        assert_eq!(settings.ui_font_size, 16.0);
        assert_eq!(settings.reading_font_family, "");
        assert_eq!(settings.reading_font_size, 0.0);
        assert_eq!(settings.interface_scale, 1.0);
        assert_eq!(settings.side_panel_font_family, "");
        assert_eq!(settings.side_panel_font_size, 14.0);
        assert_eq!(settings.side_panel_font_scale, 1.0);
        assert_eq!(settings.reading_line_spacing, 1.0);
        assert_eq!(settings.reading_column_width, 0.0);
    }

    #[test]
    fn interrupted_windows_save_recovers_backup_instead_of_defaults() {
        let _guard = DATA_DIR_LOCK.lock().unwrap();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let data = std::env::temp_dir().join(format!(
            "bibleown-settings-backup-test-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&data).unwrap();
        paths::set_data_dir_override(Some(data.clone()));

        let mut settings = Settings::default();
        settings.ui_font_family = "Inter".to_string();
        settings.reading_font_family = "Noto Serif Hebrew".to_string();
        let backup = paths::settings_file().with_extension("json.bak");
        std::fs::write(&backup, serde_json::to_vec_pretty(&settings).unwrap()).unwrap();

        let restored = load();
        assert_eq!(restored.ui_font_family, "Inter");
        assert_eq!(restored.reading_font_family, "Noto Serif Hebrew");
        assert!(paths::settings_file().exists());

        paths::set_data_dir_override(None);
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn save_replaces_existing_settings_file_and_persists_all_profiles() {
        let _guard = DATA_DIR_LOCK.lock().unwrap();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let data = std::env::temp_dir().join(format!(
            "bibleown-settings-save-test-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&data).unwrap();
        paths::set_data_dir_override(Some(data.clone()));

        let mut first = Settings::default();
        first.ui_font_family = "Inter".to_string();
        save(&first).unwrap();

        let mut updated = first.clone();
        updated.theme = "Zed Night Theme".to_string();
        updated.ui_font_size = 18.0;
        updated.reading_font_family = "Noto Serif Hebrew".to_string();
        updated.reading_font_size = 24.0;
        updated.side_panel_font_family = "Atkinson Hyperlegible".to_string();
        updated.side_panel_font_size = 15.0;
        updated.interface_scale = 1.2;
        save(&updated).unwrap();

        let restored = load();
        assert_eq!(restored.theme, "Zed Night Theme");
        assert_eq!(restored.ui_font_family, "Inter");
        assert_eq!(restored.ui_font_size, 18.0);
        assert_eq!(restored.reading_font_family, "Noto Serif Hebrew");
        assert_eq!(restored.reading_font_size, 24.0);
        assert_eq!(restored.side_panel_font_family, "Atkinson Hyperlegible");
        assert_eq!(restored.side_panel_font_size, 15.0);
        assert_eq!(restored.interface_scale, 1.2);

        paths::set_data_dir_override(None);
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn custom_typography_round_trips_through_settings_json() {
        let mut settings = Settings::default();
        settings.ui_font_family = "Inter".to_string();
        settings.ui_font_size = 18.0;
        settings.reading_font_family = "Noto Serif Hebrew".to_string();
        settings.reading_font_size = 22.0;
        settings.side_panel_font_family = "Atkinson Hyperlegible".to_string();
        settings.side_panel_font_size = 15.0;
        settings.interface_scale = 1.2;

        let json = serde_json::to_string(&settings).unwrap();
        let restored: Settings = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.ui_font_family, "Inter");
        assert_eq!(restored.ui_font_size, 18.0);
        assert_eq!(restored.reading_font_family, "Noto Serif Hebrew");
        assert_eq!(restored.reading_font_size, 22.0);
        assert_eq!(restored.side_panel_font_family, "Atkinson Hyperlegible");
        assert_eq!(restored.side_panel_font_size, 15.0);
        assert_eq!(restored.interface_scale, 1.2);
    }

    #[test]
    fn link_open_mode_defaults_to_reuse_current() {
        // Старый settings.json без ключа — поведение как до вкладок.
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.link_open_mode, LinkOpenMode::ReuseCurrent);
        assert_eq!(LinkOpenMode::default(), LinkOpenMode::ReuseCurrent);
    }

    #[test]
    fn link_open_mode_serde_roundtrip() {
        assert_eq!(
            serde_json::to_string(&LinkOpenMode::NewTab).unwrap(),
            "\"new_tab\""
        );
        assert_eq!(
            serde_json::to_string(&LinkOpenMode::ReuseCurrent).unwrap(),
            "\"reuse_current\""
        );
        let settings: Settings = serde_json::from_str("{\"link_open_mode\": \"new_tab\"}").unwrap();
        assert_eq!(settings.link_open_mode, LinkOpenMode::NewTab);
        let json = serde_json::to_string(&settings).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.link_open_mode, LinkOpenMode::NewTab);
    }
}
