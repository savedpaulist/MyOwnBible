//! Сценарии заметок (architecture.md §5.3, этап 8).
//!
//! Тонкий слой над `notes_core`: AppServices владеет `Arc<NotesIndex>` и
//! watcher'ом; сценарии возвращают готовые ViewModel. Правило источника
//! истины: файл на диске главный, notes.db — только индекс.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;

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
    let relpath = if relpath.ends_with(".md") {
        relpath.to_string()
    } else {
        format!("{}.md", relpath.trim())
    };
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
}
