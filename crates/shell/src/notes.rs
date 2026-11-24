//! Сценарии заметок (architecture.md §5.3, этап 8).
//!
//! Тонкий слой над `notes_core`: AppServices владеет `Arc<NotesIndex>` и
//! watcher'ом; сценарии возвращают готовые ViewModel. Правило источника
//! истины: файл на диске главный, notes.db — только индекс.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use chrono::Local;
use serde::{Deserialize, Serialize};

use notes_core::index::NotesIndex;
use notes_core::store::NoteSearchHit;
use notes_core::vault;
use notes_core::watcher::{self, VaultWatcher};

use mybible::SharedRegistry;

pub use notes_core::backlinks::{LinkHit, LinkSourceRef};
pub use notes_core::index::IndexEvent;
pub use notes_core::wikilink;

/// Opaque application handle used by presentation code. The concrete index
/// and its database remain private to the application/infrastructure layers.
#[derive(Clone)]
pub struct NotesHandle(Arc<NotesIndex>);

pub struct NotesSubscription {
    _token: notes_core::index::SubscriptionToken,
}

impl NotesHandle {
    pub fn same_storage(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub fn subscribe(
        &self,
        callback: impl Fn(&IndexEvent) + Send + Sync + 'static,
    ) -> NotesSubscription {
        NotesSubscription {
            _token: self.0.subscribe(callback),
        }
    }
}

/// Current notes storage for UI use cases.
pub fn current(services: &crate::services::AppServices) -> Option<NotesHandle> {
    services.notes.read().clone().map(NotesHandle)
}

/// Открыть индекс заметок и запустить watcher. Вызывается из bootstrap,
/// когда в настройках задан notes_dir. Ошибки — в лог, приложение живёт
/// без заметок.
pub fn init(
    settings: &app_state::settings::Settings,
    registry: &SharedRegistry,
) -> (Option<Arc<NotesIndex>>, Option<VaultWatcher>) {
    let Some(notes_dir) = settings.notes_dir.clone() else {
        log::info!("notes_dir не задан — заметки отключены");
        return (None, None);
    };
    let registry = registry.clone();
    let resolver: Arc<notes_core::parser::BibleResolver> = Arc::new(move |raw| {
        bible_domain::reference::parse(raw, |name| registry.lock().lookup_book(name))
    });
    let index = match NotesIndex::open(notes_dir, Some(resolver)) {
        Ok(index) => Arc::new(index),
        Err(error) => {
            log::error!("notes index unavailable: {error:#}");
            return (None, None);
        }
    };
    let watcher = match start_watcher(&index) {
        Ok(watcher) => Some(watcher),
        Err(error) => {
            log::error!("notes watcher unavailable: {error:#}");
            None
        }
    };
    let scan_target = index.clone();
    std::thread::spawn(move || {
        if let Err(error) = scan_target.initial_scan() {
            log::error!("scan notes vault: {error:#}");
        }
    });
    (Some(index), watcher)
}

/// Watcher снаружи: внешние изменения (Obsidian!) переиндексируются,
/// события уходят подписчикам (UI обновляется по IndexEvent).
fn start_watcher(index: &Arc<NotesIndex>) -> Result<VaultWatcher> {
    let target = index.clone();
    let handle = watcher::watch(index.notes_dir(), move |events| {
        if let Err(error) = target.apply_watch_events(events) {
            log::error!("apply watch events: {error:#}");
        }
    })?;
    Ok(handle)
}

/// Строка списка заметок в панели.
#[derive(Debug, Clone)]
pub struct NoteSummary {
    pub relpath: String,
    pub title: String,
    pub aliases: Vec<String>,
}

/// Открытая заметка: тело читается с диска (источник истины).
#[derive(Debug, Clone)]
pub struct NoteDocument {
    pub relpath: String,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteFolder {
    pub relpath: String,
    pub name: String,
}

/// Reusable note recipe. Patterns support `{title}`, `{date}`, `{time}` and
/// `{datetime}` and are expanded only when a note is created.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NoteTemplate {
    pub id: String,
    pub name: String,
    pub title_pattern: String,
    pub body_pattern: String,
    #[serde(default)]
    pub folder: String,
}

impl NoteTemplate {
    pub fn blank() -> Self {
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            name: "Новый шаблон".to_owned(),
            title_pattern: "{title}".to_owned(),
            body_pattern: "# {title}\n\n".to_owned(),
            folder: String::new(),
        }
    }
}

const TEMPLATES_FILE: &str = ".bibleown-note-templates.json";

fn safe_relative_path(raw: &str) -> Result<PathBuf> {
    let raw = raw.trim().replace('\\', "/");
    let path = Path::new(&raw);
    if raw.is_empty() || path.is_absolute() {
        bail!("укажите относительный путь внутри хранилища заметок");
    }
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) if !part.is_empty() => clean.push(part),
            _ => bail!("путь не должен содержать . или .."),
        }
    }
    Ok(clean)
}

fn slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|part| part.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
}

fn valid_folder(raw: &str) -> Result<PathBuf> {
    if raw.trim().is_empty() {
        return Ok(PathBuf::new());
    }
    safe_relative_path(raw)
}

pub fn list_folders(handle: &NotesHandle) -> Result<Vec<NoteFolder>> {
    let root = handle.0.notes_dir();
    let mut folders = walkdir::WalkDir::new(root)
        .follow_links(false)
        .min_depth(1)
        .into_iter()
        .filter_entry(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_dir())
        .filter_map(|entry| {
            let relative = entry.path().strip_prefix(root).ok()?;
            let relpath = slash_path(relative);
            let name = entry.file_name().to_string_lossy().into_owned();
            Some(NoteFolder { relpath, name })
        })
        .collect::<Vec<_>>();
    folders.sort_by(|a, b| {
        a.relpath
            .to_lowercase()
            .cmp(&b.relpath.to_lowercase())
            .then_with(|| a.relpath.cmp(&b.relpath))
    });
    Ok(folders)
}

pub fn create_folder(handle: &NotesHandle, relpath: &str) -> Result<NoteFolder> {
    let relative = safe_relative_path(relpath)?;
    let absolute = handle.0.notes_dir().join(&relative);
    std::fs::create_dir_all(&absolute)
        .with_context(|| format!("не удалось создать папку {}", absolute.display()))?;
    let name = relative
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(relpath)
        .to_owned();
    Ok(NoteFolder {
        relpath: slash_path(&relative),
        name,
    })
}

pub fn move_note(handle: &NotesHandle, relpath: &str, folder: &str) -> Result<NoteDocument> {
    let source_relative = safe_relative_path(relpath)?;
    let folder = valid_folder(folder)?;
    let file_name = source_relative
        .file_name()
        .context("у заметки нет имени файла")?;
    let target_relative = folder.join(file_name);
    if source_relative == target_relative {
        return open_note(handle, relpath);
    }
    let source = handle.0.notes_dir().join(&source_relative);
    let target = handle.0.notes_dir().join(&target_relative);
    if target.exists() {
        bail!(
            "в папке уже есть заметка с именем {}",
            file_name.to_string_lossy()
        );
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    {
        let _guard = crate::sync::NOTE_WRITE.lock();
        std::fs::rename(&source, &target).with_context(|| {
            format!(
                "не удалось перенести {} в {}",
                source.display(),
                target.display()
            )
        })?;
    }
    handle
        .0
        .apply_watch_events(vec![watcher::WatchEvent::Rename {
            from: source,
            to: target,
        }])?;
    open_note(handle, &slash_path(&target_relative))
}

pub fn load_templates(handle: &NotesHandle) -> Result<Vec<NoteTemplate>> {
    let path = handle.0.notes_dir().join(TEMPLATES_FILE);
    if !path.exists() {
        return Ok(vec![NoteTemplate {
            id: uuid::Uuid::now_v7().to_string(),
            name: "Обычная заметка".to_owned(),
            title_pattern: "{title}".to_owned(),
            body_pattern: "# {title}\n\n".to_owned(),
            folder: String::new(),
        }]);
    }
    let bytes = std::fs::read(&path)?;
    serde_json::from_slice(&bytes).context("не удалось прочитать шаблоны заметок")
}

pub fn save_templates(handle: &NotesHandle, templates: &[NoteTemplate]) -> Result<()> {
    let path = handle.0.notes_dir().join(TEMPLATES_FILE);
    std::fs::write(path, serde_json::to_vec_pretty(templates)?)?;
    Ok(())
}

fn expand_template(pattern: &str, entered_title: &str, now: &chrono::DateTime<Local>) -> String {
    pattern
        .replace("{title}", entered_title.trim())
        .replace("{date}", &now.format("%Y-%m-%d").to_string())
        .replace("{time}", &now.format("%H-%M").to_string())
        .replace("{datetime}", &now.format("%Y-%m-%d %H-%M").to_string())
}

pub fn create_note_from_template(
    handle: &NotesHandle,
    template: &NoteTemplate,
    entered_title: &str,
) -> Result<NoteDocument> {
    let now = Local::now();
    let title = expand_template(&template.title_pattern, entered_title, &now);
    if title.trim().is_empty() {
        bail!("шаблон создал пустое название заметки");
    }
    let folder = valid_folder(&template.folder)?;
    let title_path = safe_relative_path(title.trim())?;
    let mut relative = folder.join(title_path);
    relative.set_extension("md");
    let relpath = slash_path(&relative);
    let absolute = handle.0.notes_dir().join(&relative);
    if absolute.exists() {
        bail!("заметка {relpath} уже существует");
    }
    let body = expand_template(&template.body_pattern, entered_title, &now);
    save_note(handle, &relpath, &body)?;
    open_note(handle, &relpath)
}

/// Сценарий «список заметок» для панели: заголовок + алиасы.
pub fn list_notes(handle: &NotesHandle) -> Result<Vec<NoteSummary>> {
    let index = &handle.0;
    let store = index.store();
    let mut aliases: std::collections::HashMap<i64, Vec<String>> = std::collections::HashMap::new();
    for (note_id, alias) in store.all_aliases()? {
        aliases.entry(note_id).or_default().push(alias);
    }
    let mut out: Vec<NoteSummary> = store
        .all_notes_summary()?
        .into_iter()
        .map(|(id, relpath, title)| NoteSummary {
            relpath,
            title,
            aliases: aliases.remove(&id).unwrap_or_default(),
        })
        .collect();
    out.sort_by(|a, b| a.relpath.cmp(&b.relpath));
    Ok(out)
}

/// Сценарий «открыть заметку»: тело — с диска, заголовок — из индекса
/// (если заметка ещё не просканирована — из имени файла).
pub fn open_note(handle: &NotesHandle, relpath: &str) -> Result<NoteDocument> {
    let index = &handle.0;
    let absolute = index.notes_dir().join(relpath);
    let body = vault::read_to_string(&absolute)?;
    let title = index
        .store()
        .get_note_by_relpath(relpath)?
        .map(|row| row.title)
        .unwrap_or_else(|| {
            relpath
                .rsplit('/')
                .next()
                .unwrap_or(relpath)
                .strip_suffix(".md")
                .unwrap_or(relpath)
                .to_string()
        });
    Ok(NoteDocument {
        relpath: relpath.to_string(),
        title,
        body,
    })
}

/// Сценарий «сохранить заметку»: пишем файл и сразу переиндексируем,
/// не дожидаясь debounce watcher'а (дедуп по подписи отсечёт повтор).
pub fn save_note(handle: &NotesHandle, relpath: &str, body: &str) -> Result<()> {
    let index = &handle.0;
    let absolute = index.notes_dir().join(relpath);
    {
        let _guard = crate::sync::NOTE_WRITE.lock();
        crate::sync::write_note(&absolute, body)?;
    }
    index.apply_watch_events(vec![watcher::WatchEvent::Upsert(absolute)])?;
    Ok(())
}

/// Save an editor buffer only if the on-disk version still matches what it opened.
pub fn save_note_if_current(
    handle: &NotesHandle,
    relpath: &str,
    expected: &str,
    body: &str,
) -> Result<bool> {
    let index = &handle.0;
    let absolute = index.notes_dir().join(relpath);
    {
        let _guard = crate::sync::NOTE_WRITE.lock();
        match std::fs::read_to_string(&absolute) {
            Ok(current) if current == expected => crate::sync::write_note(&absolute, body)?,
            Ok(current) if current == body => return Ok(true),
            Ok(_) => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        }
    }
    // The source file is saved even if refreshing the rebuildable index fails.
    if let Err(error) = index.apply_watch_events(vec![watcher::WatchEvent::Upsert(absolute)]) {
        log::warn!("refresh note index: {error:#}");
    }
    Ok(true)
}

/// Сценарий «создать заметку». relpath без «.md» дополняется.
/// Существующий файл не затирается.
pub fn create_note(handle: &NotesHandle, relpath: &str) -> Result<NoteDocument> {
    let relative = safe_relative_path(relpath)?;
    let relative = if relative.extension().is_some_and(|ext| ext == "md") {
        relative
    } else {
        let mut with_extension = relative;
        with_extension.set_extension("md");
        with_extension
    };
    // Avoid names such as `.md` after normalisation.
    if relative.file_stem().is_none_or(|stem| stem.is_empty()) {
        bail!("укажите название заметки");
    }
    let relpath = slash_path(&relative);
    let absolute = handle.0.notes_dir().join(&relpath);
    if !absolute.exists() {
        let title = relpath
            .rsplit('/')
            .next()
            .unwrap_or(&relpath)
            .strip_suffix(".md")
            .unwrap_or(&relpath);
        save_note(handle, &relpath, &format!("# {title}\n"))?;
    }
    open_note(handle, &relpath)
}

/// Create a uniquely named note for the "new tab" command.
pub fn create_untitled_note(handle: &NotesHandle) -> Result<NoteDocument> {
    let name = (1..)
        .map(|number| {
            if number == 1 {
                "Новая заметка.md".to_owned()
            } else {
                format!("Новая заметка {number}.md")
            }
        })
        .find(|name| !handle.0.notes_dir().join(name).exists())
        .expect("unbounded note-name sequence");
    create_note(handle, &name)
}

/// Сценарий «поиск по заметкам»: тот же DSL, что и поиск по Библии
/// (bible_domain::search → FTS5 MATCH), по телу заметок.
pub fn search_notes(handle: &NotesHandle, input: &str) -> Result<Vec<NoteSearchHit>> {
    let query = bible_domain::search::parse_query(input)
        .ok_or_else(|| anyhow::anyhow!("пустой поисковый запрос"))?;
    handle.0.store().search_notes_fts(&query.match_expr, 100)
}

/// Сценарий «обратные ссылки заметки»: кто ссылается на неё.
pub fn note_backlinks(handle: &NotesHandle, relpath: &str) -> Result<Vec<LinkHit>> {
    let index = &handle.0;
    let Some(id) = index.store().note_id_by_relpath(relpath)? else {
        return Ok(Vec::new());
    };
    notes_core::backlinks::backlinks_for_note(index.store(), id)
}

/// Сценарий «кто ссылается на стих» — для будущей секции панели
/// «Контекст» (заметки, упоминающие открытый стих).
pub fn verse_backlinks(
    handle: &NotesHandle,
    book: i64,
    chapter: i64,
    verse: i64,
) -> Result<Vec<LinkHit>> {
    notes_core::backlinks::backlinks_for_verse(handle.0.store(), book, chapter, verse)
}

/// Путь к файлу заметки (для «открыть во внешнем редакторе» и тестов).
pub fn absolute_path(handle: &NotesHandle, relpath: &str) -> PathBuf {
    handle.0.notes_dir().join(relpath)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use mybible::{ModuleRegistry, SharedRegistry};
    use sqlez::connection::Connection;

    /// Минимальный перевод-фикстура: книга «Ин» (500), чтобы resolver
    /// заметок разбирал «Ин 3:16».
    fn write_bible_module(dir: &Path) {
        let path = dir.join("T.sqlite3");
        let connection = Connection::open_file(path.to_str().unwrap());
        connection
            .exec("CREATE TABLE info(name TEXT, value TEXT)")
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
            .exec("INSERT INTO verses VALUES(500, 3, 16, 'текст')")
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

    fn services(modules_dir: &Path, notes_dir: &Path) -> crate::services::AppServices {
        let mut settings = app_state::settings::Settings::default();
        settings.modules_dir = modules_dir.to_path_buf();
        settings.notes_dir = Some(notes_dir.to_path_buf());
        let registry = SharedRegistry::new(ModuleRegistry::scan(modules_dir));
        let (notes, watcher) = init(&settings, &registry);
        crate::services::AppServices {
            settings: parking_lot::RwLock::new(settings),
            registry,
            current_location: Arc::new(parking_lot::Mutex::new(None)),
            state_db: None,
            user_cross_refs: None,
            annotations: None,
            chains: None,
            tags: None,
            search_index: None,
            index_building: Arc::new(parking_lot::Mutex::new(std::collections::HashSet::new())),
            notes: parking_lot::RwLock::new(notes),
            _notes_watcher: parking_lot::Mutex::new(watcher),
        }
    }

    #[test]
    fn notes_full_cycle() {
        let modules = tempfile::TempDir::new().unwrap();
        let vault_dir = tempfile::TempDir::new().unwrap();
        write_bible_module(modules.path());
        let services = services(modules.path(), vault_dir.path());
        let index = NotesHandle(services.notes.read().clone().unwrap());

        // Создание → файл на диске, запись в индексе.
        let doc = create_note(&index, "Theology/Faith").unwrap();
        assert_eq!(doc.relpath, "Theology/Faith.md");
        assert!(absolute_path(&index, &doc.relpath).exists());

        // Сохранение → переиндексация, поиск по телу находит.
        save_note(
            &index,
            &doc.relpath,
            "# Вера\n\nТекст про веру и [[bible:Ин 3:16]].\n",
        )
        .unwrap();
        let hits = search_notes(&index, "веру").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].relpath, "Theology/Faith.md");

        // Список заметок.
        let list = list_notes(&index).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Faith");

        // Библейская ссылка разрешилась через реестр → backlinks по стиху.
        let verse_hits = verse_backlinks(&index, 500, 3, 16).unwrap();
        assert_eq!(verse_hits.len(), 1);

        // Обратные ссылки самой заметки: создадим ссылающуюся.
        save_note(&index, "Other.md", "См. [[Theology/Faith]].\n").unwrap();
        let backlinks = note_backlinks(&index, &doc.relpath).unwrap();
        assert_eq!(backlinks.len(), 1);
    }

    #[test]
    fn init_without_notes_dir_disables_notes() {
        let modules = tempfile::TempDir::new().unwrap();
        let mut settings = app_state::settings::Settings::default();
        settings.modules_dir = modules.path().to_path_buf();
        settings.notes_dir = None;
        let registry = SharedRegistry::new(ModuleRegistry::scan(modules.path()));
        let (index, watcher) = init(&settings, &registry);
        assert!(index.is_none());
        assert!(watcher.is_none());
    }

    #[test]
    fn folders_move_notes_and_templates_are_real_files() {
        let modules = tempfile::TempDir::new().unwrap();
        let vault_dir = tempfile::TempDir::new().unwrap();
        let services = services(modules.path(), vault_dir.path());
        let index = NotesHandle(services.notes.read().clone().unwrap());

        create_folder(&index, "Богословие/Черновики").unwrap();
        let folders = list_folders(&index).unwrap();
        assert!(folders.iter().any(|folder| folder.relpath == "Богословие"));
        assert!(
            folders
                .iter()
                .any(|folder| folder.relpath == "Богословие/Черновики")
        );

        let note = create_note(&index, "Мысль").unwrap();
        let moved = move_note(&index, &note.relpath, "Богословие/Черновики").unwrap();
        assert_eq!(moved.relpath, "Богословие/Черновики/Мысль.md");
        assert!(!absolute_path(&index, "Мысль.md").exists());

        let template = NoteTemplate {
            id: "daily".into(),
            name: "Дневник".into(),
            title_pattern: "{date} {title}".into(),
            body_pattern: "# {title}\n\nСоздано: {date} {time}\n".into(),
            folder: "Дневник".into(),
        };
        save_templates(&index, std::slice::from_ref(&template)).unwrap();
        assert_eq!(load_templates(&index).unwrap(), vec![template.clone()]);
        let from_template = create_note_from_template(&index, &template, "Молитва").unwrap();
        assert!(from_template.relpath.starts_with("Дневник/"));
        assert!(from_template.relpath.ends_with(" Молитва.md"));
        assert!(from_template.body.contains("# Молитва"));
    }

    #[test]
    fn note_paths_cannot_escape_vault() {
        let modules = tempfile::TempDir::new().unwrap();
        let vault_dir = tempfile::TempDir::new().unwrap();
        let services = services(modules.path(), vault_dir.path());
        let index = NotesHandle(services.notes.read().clone().unwrap());
        assert!(create_note(&index, "../outside").is_err());
        assert!(create_folder(&index, "/outside").is_err());
    }
}
