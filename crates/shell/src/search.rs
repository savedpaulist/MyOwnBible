//! Сценарий «поиск по Библии» (architecture.md §5.3, этап 7).
//!
//! Поиск читает опубликованные индексы, пока воркер строит новые.

use std::collections::{BTreeMap, HashMap};

use anyhow::Result;

use bible_domain::ids::{BookNumber, ModuleId};
use bible_domain::ports::{ModuleCatalog, ModuleKind, ModuleMeta};
use bible_domain::search::{self, Corpus};
use mybible::{CommentarySearchRow, DictionarySearchRow};

use crate::services::AppServices;

/// Одна находка для панели поиска.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub module: ModuleId,
    pub book: BookNumber,
    pub chapter: u16,
    pub verse: u16,
    /// Текст стиха для показа (без служебного хвоста Strong's-токенов).
    pub text: String,
    /// «Ин 3:16» — короткое имя книги из самого модуля + координаты.
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct DictionarySearchHit {
    pub module: ModuleId,
    pub topic: String,
    pub lexeme: Option<String>,
    pub snippet: String,
}

#[derive(Debug, Clone)]
pub struct CommentarySearchHit {
    pub module: ModuleId,
    pub book: BookNumber,
    pub chapter_from: u16,
    pub verse_from: u16,
    pub chapter_to: Option<u16>,
    pub verse_to: Option<u16>,
    pub label: String,
    pub snippet: String,
}

/// Результаты одного модуля. Группы сортируются по идентификатору источника.
#[derive(Debug, Clone)]
pub struct SearchModuleGroup<T> {
    pub module: ModuleId,
    pub description: Option<String>,
    pub hits: Vec<T>,
}

/// Результат сценария глобального поиска.
#[derive(Debug, Clone)]
pub struct SearchResultsViewModel {
    pub input: String,
    pub highlight_terms: Vec<String>,
    /// Плоская совместимая выборка библейских стихов.
    pub hits: Vec<SearchHit>,
    pub verse_groups: Vec<SearchModuleGroup<SearchHit>>,
    pub dictionary_groups: Vec<SearchModuleGroup<DictionarySearchHit>>,
    pub commentary_groups: Vec<SearchModuleGroup<CommentarySearchHit>>,
    pub hit_count: usize,
    /// Модули, индекс которых прямо сейчас строит другой поток.
    pub building: Vec<String>,
}

/// Сценарий «найти стихи»: разбор запроса → свежесть индекса → поиск.
pub fn search_bible(
    services: &AppServices,
    input: &str,
    module_filter: Option<&ModuleId>,
    corpus: Corpus,
) -> Result<SearchResultsViewModel> {
    search_sources(services, input, corpus, |id| {
        module_filter.is_none_or(|filter| id == filter)
    })
}

pub fn search_sources(
    services: &AppServices,
    input: &str,
    corpus: Corpus,
    included: impl Fn(&ModuleId) -> bool,
) -> Result<SearchResultsViewModel> {
    let query =
        search::parse_query(input).ok_or_else(|| anyhow::anyhow!("пустой поисковый запрос"))?;
    let index = services
        .search_index
        .clone()
        .ok_or_else(|| anyhow::anyhow!("поисковый индекс недоступен"))?;

    let catalog = services.registry.modules();
    let all_translations: Vec<&ModuleMeta> = catalog
        .iter()
        .filter(|m| m.kind == ModuleKind::BibleTranslation)
        .collect();
    let translations: Vec<&ModuleMeta> = all_translations
        .iter()
        .copied()
        .filter(|m| included(&m.id))
        .collect();
    let dictionaries: Vec<&ModuleMeta> = catalog
        .iter()
        .filter(|m| {
            matches!(
                m.kind,
                ModuleKind::StrongsDictionary | ModuleKind::GeneralDictionary
            )
        })
        .filter(|m| included(&m.id))
        .collect();
    let commentaries: Vec<&ModuleMeta> = catalog
        .iter()
        .filter(|m| m.kind == ModuleKind::Commentary)
        .filter(|m| included(&m.id))
        .collect();

    let selected = translations
        .iter()
        .chain(&dictionaries)
        .chain(&commentaries);
    queue_indexes(
        services,
        selected.clone().map(|meta| (**meta).clone()),
        false,
    );
    let building = {
        let pending = services.index_building.lock();
        selected
            .filter(|meta| pending.contains(meta.id.0.as_str()))
            .map(|meta| meta.id.to_string())
            .collect()
    };

    let mut short_names: HashMap<(String, i64), String> = HashMap::new();
    let mut fallback_book_names: HashMap<i64, String> = HashMap::new();
    {
        let mut registry = services.registry.lock();
        for meta in &all_translations {
            for entry in registry.books_of(&meta.id) {
                short_names.insert(
                    (meta.id.to_string(), entry.book_number),
                    entry.short_name.clone(),
                );
                fallback_book_names
                    .entry(entry.book_number)
                    .or_insert(entry.short_name);
            }
        }
    }

    let mut all_verse_hits = Vec::new();
    for meta in &translations {
        for row in index.search_module(meta, &query.match_expr, corpus.book_range())? {
            let book_name = short_names
                .get(&(row.module.to_string(), row.book.0))
                .cloned()
                .unwrap_or_else(|| format!("Кн{}", row.book.0));
            all_verse_hits.push(SearchHit {
                label: format!("{book_name} {}:{}", row.chapter, row.verse),
                text: strip_strong_tail(&row.text),
                module: row.module,
                book: row.book,
                chapter: row.chapter,
                verse: row.verse,
            });
        }
    }
    all_verse_hits.sort_by(|a, b| {
        a.module
            .cmp(&b.module)
            .then_with(|| a.book.cmp(&b.book))
            .then_with(|| a.chapter.cmp(&b.chapter))
            .then_with(|| a.verse.cmp(&b.verse))
    });

    let mut dictionary_hits = Vec::new();
    for meta in &dictionaries {
        let rows = index.search_dictionary_module(meta, &query.match_expr)?;
        dictionary_hits.extend(rows.into_iter().map(|row: DictionarySearchRow| {
            DictionarySearchHit {
                module: row.module,
                topic: row.topic,
                lexeme: row.lexeme,
                snippet: row.snippet,
            }
        }));
    }

    let mut commentary_hits = Vec::new();
    for meta in &commentaries {
        let rows = index.search_commentary_module(meta, &query.match_expr)?;
        commentary_hits.extend(rows.into_iter().map(|row: CommentarySearchRow| {
            let short = fallback_book_names
                .get(&row.book.0)
                .cloned()
                .unwrap_or_else(|| format!("Кн{}", row.book.0));
            let range = match (row.chapter_to, row.verse_to) {
                (Some(chapter), Some(verse))
                    if chapter != row.chapter_from || verse != row.verse_from =>
                {
                    format!("–{chapter}:{verse}")
                }
                (_, Some(verse)) if verse != row.verse_from => format!("–{verse}"),
                _ => String::new(),
            };
            CommentarySearchHit {
                module: row.module,
                book: row.book,
                chapter_from: row.chapter_from,
                verse_from: row.verse_from,
                chapter_to: row.chapter_to,
                verse_to: row.verse_to,
                label: format!("{short} {}:{}{range}", row.chapter_from, row.verse_from),
                snippet: row.snippet,
            }
        }));
    }

    let descriptions: HashMap<String, Option<String>> = catalog
        .iter()
        .map(|meta| (meta.id.to_string(), meta.description.clone()))
        .collect();
    let verse_groups = group_hits_by_module(all_verse_hits.clone(), &descriptions, |hit| {
        hit.module.clone()
    });
    let dictionary_groups =
        group_hits_by_module(dictionary_hits, &descriptions, |hit| hit.module.clone());
    let commentary_groups =
        group_hits_by_module(commentary_hits, &descriptions, |hit| hit.module.clone());

    let hit_count = verse_groups
        .iter()
        .map(|group| group.hits.len())
        .sum::<usize>()
        + dictionary_groups
            .iter()
            .map(|group| group.hits.len())
            .sum::<usize>()
        + commentary_groups
            .iter()
            .map(|group| group.hits.len())
            .sum::<usize>();

    Ok(SearchResultsViewModel {
        input: input.to_string(),
        highlight_terms: query.highlight_terms,
        hits: all_verse_hits,
        verse_groups,
        dictionary_groups,
        commentary_groups,
        hit_count,
        building,
    })
}

fn group_hits_by_module<T: Clone>(
    hits: Vec<T>,
    descriptions: &HashMap<String, Option<String>>,
    module_of: impl Fn(&T) -> ModuleId,
) -> Vec<SearchModuleGroup<T>> {
    let mut grouped: BTreeMap<String, (ModuleId, Vec<T>)> = BTreeMap::new();
    for hit in hits {
        let module = module_of(&hit);
        grouped
            .entry(module.to_string())
            .or_insert_with(|| (module.clone(), Vec::new()))
            .1
            .push(hit);
    }
    grouped
        .into_iter()
        .map(|(key, (module, hits))| SearchModuleGroup {
            module,
            description: descriptions.get(&key).cloned().flatten(),
            hits,
        })
        .collect()
}

/// Warm the catalogue after opening the window and after explicit rescans.
pub fn warm_indexes(services: &AppServices) {
    queue_indexes(services, services.registry.modules(), false);
}

/// Rebuild every searchable module on an explicit user request.
pub fn force_reindex_all(services: &AppServices) {
    queue_indexes(services, services.registry.modules(), true);
}

fn queue_indexes(
    services: &AppServices,
    modules: impl IntoIterator<Item = ModuleMeta>,
    force: bool,
) {
    static WORKER: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    let Some(index) = services.search_index.clone() else {
        return;
    };
    let pending = services.index_building.clone();
    let jobs: Vec<_> = modules
        .into_iter()
        .filter(|meta| {
            matches!(
                meta.kind,
                ModuleKind::BibleTranslation
                    | ModuleKind::StrongsDictionary
                    | ModuleKind::GeneralDictionary
                    | ModuleKind::Commentary
            ) && (force || index.needs_refresh(meta))
                && pending.lock().insert(meta.id.to_string())
        })
        .collect();
    if jobs.is_empty() {
        return;
    }
    let worker_pending = pending.clone();
    let ids: Vec<_> = jobs.iter().map(|meta| meta.id.to_string()).collect();
    if let Err(error) = std::thread::Builder::new()
        .name("bible-index".into())
        .spawn(move || {
            lower_index_worker_priority();
            let _worker = WORKER.lock();
            for meta in jobs {
                let result = if force {
                    index.force_refresh(&meta)
                } else {
                    index.refresh(&meta)
                };
                if let Err(error) = result {
                    log::warn!("index rebuild failed for {}: {error:#}", meta.id);
                }
                worker_pending.lock().remove(meta.id.0.as_str());
            }
        })
    {
        log::error!("could not start index worker: {error:#}");
        pending.lock().retain(|id| !ids.contains(id));
    }
}

fn lower_index_worker_priority() {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(class: u32, relative_priority: i32) -> i32;
        }
        const QOS_CLASS_UTILITY: u32 = 0x11;
        // Utility is below the interactive and default scheduling classes.
        unsafe { pthread_set_qos_class_self_np(QOS_CLASS_UTILITY, 0) };
    }
    #[cfg(target_os = "linux")]
    {
        unsafe extern "C" {
            fn nice(increment: i32) -> i32;
        }
        // On Linux the nice adjustment applies to the calling thread.
        unsafe { nice(10) };
    }
    #[cfg(windows)]
    {
        #[link(name = "Kernel32")]
        unsafe extern "system" {
            fn GetCurrentThread() -> *mut std::ffi::c_void;
            fn SetThreadPriority(thread: *mut std::ffi::c_void, priority: i32) -> i32;
        }
        const THREAD_PRIORITY_BELOW_NORMAL: i32 = -1;
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) };
    }
}

/// Убирает служебный хвост Strong's-токенов из индексированного текста
/// (формат — см. mybible::search_index::index_text): токены вида «G25»,
/// «H123» в конце строки в выдаче не показываем.
fn strip_strong_tail(text: &str) -> String {
    let mut words: Vec<&str> = text.split_whitespace().collect();
    while let Some(last) = words.last() {
        if is_strong_token(last) {
            words.pop();
        } else {
            break;
        }
    }
    words.join(" ")
}

/// «G25», «H123»: ASCII-заглавные буквы + цифры, без других символов.
fn is_strong_token(word: &str) -> bool {
    let split = word
        .find(|c: char| c.is_ascii_digit())
        .unwrap_or(word.len());
    let (letters, digits) = word.split_at(split);
    !letters.is_empty()
        && letters.chars().all(|c| c.is_ascii_uppercase())
        && !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use parking_lot::Mutex;
    use sqlez::connection::Connection;

    use mybible::{ModuleRegistry, SearchIndex, SharedRegistry};

    /// Временный каталог без внешних зависимостей.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "bibleown-search-test-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Модуль-фикстура: две книги НЗ с парой стихов.
    fn write_module(dir: &Path, name: &str) {
        let path = dir.join(format!("{name}.sqlite3"));
        let connection = Connection::open_file(path.to_str().unwrap());
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
                "INSERT INTO verses VALUES\
                   (500, 3, 16, 'Ибо так возлюбил Бог мир'),\
                   (500, 3, 17, 'Ибо не послал Бог Сына Своего в мир'),\
                   (430, 1, 1, 'В начале сотворил Бог небо и землю')",
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
            .exec("INSERT INTO books VALUES(500, 'Ин', 'Иоанн'), (430, 'Быт', 'Бытие')")
            .unwrap()()
        .unwrap();
        drop(connection);
    }

    fn services_unindexed(dir: &Path) -> AppServices {
        AppServices {
            settings: parking_lot::RwLock::new(app_state::settings::Settings::default()),
            registry: SharedRegistry::new(ModuleRegistry::scan(dir)),
            current_location: Arc::new(Mutex::new(None)),
            state_db: None,
            user_cross_refs: None,
            annotations: None,
            chains: None,
            tags: None,
            search_index: Some(SearchIndex::open(&dir.join("index.db")).unwrap()),
            index_building: Arc::new(Mutex::new(std::collections::HashSet::new())),
            notes: parking_lot::RwLock::new(None),
            _notes_watcher: Mutex::new(None),
        }
    }

    fn services(dir: &Path) -> AppServices {
        let services = services_unindexed(dir);
        let index = services.search_index.as_ref().unwrap();
        for meta in services.registry.modules() {
            index.refresh(&meta).unwrap();
        }
        services
    }

    #[test]
    fn search_builds_index_and_finds_verses() {
        let temp = TempDir::new("basic");
        write_module(&temp.0, "T");
        let services = services_unindexed(&temp.0);
        let query = "возлюбил* мир";
        let first = search_bible(&services, query, None, Corpus::All).unwrap();
        if first.hits.is_empty() {
            assert_eq!(first.building, ["T"]);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !services.index_building.lock().is_empty() {
            assert!(std::time::Instant::now() < deadline, "index did not finish");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let result = search_bible(&services, query, None, Corpus::All).unwrap();
        assert_eq!(result.hits.len(), 1);
        let hit = &result.hits[0];
        assert_eq!(hit.module, ModuleId::from("T"));
        assert_eq!(hit.book, BookNumber(500));
        assert_eq!((hit.chapter, hit.verse), (3, 16));
        assert_eq!(hit.label, "Ин 3:16");
        assert_eq!(hit.text, "Ибо так возлюбил Бог мир");
        assert!(result.building.is_empty());
    }

    #[test]
    fn second_search_reuses_fresh_index() {
        let temp = TempDir::new("fresh");
        write_module(&temp.0, "T");
        let services = services(&temp.0);
        let index = services.search_index.clone().unwrap();

        search_bible(&services, "мир", None, Corpus::All).unwrap();
        let meta = index.module_meta(&ModuleId::from("T")).unwrap().unwrap();

        // Повторный поиск не перестраивает (built_at не меняется).
        search_bible(&services, "Бог", None, Corpus::All).unwrap();
        let meta2 = index.module_meta(&ModuleId::from("T")).unwrap().unwrap();
        assert_eq!(meta.built_at, meta2.built_at);
    }

    #[test]
    fn corpus_filter_limits_testament() {
        let temp = TempDir::new("corpus");
        write_module(&temp.0, "T");
        let services = services(&temp.0);

        let nt = search_bible(&services, "Бог", None, Corpus::NewTestament).unwrap();
        assert_eq!(nt.hits.len(), 2);
        assert!(nt.hits.iter().all(|h| h.book.0 >= 470));

        let ot = search_bible(&services, "Бог", None, Corpus::OldTestament).unwrap();
        assert_eq!(ot.hits.len(), 1);
        assert_eq!(ot.hits[0].label, "Быт 1:1");
    }

    #[test]
    fn removed_module_hits_disappear() {
        let temp = TempDir::new("invalidate");
        write_module(&temp.0, "T");
        write_module(&temp.0, "T2");
        let services = services(&temp.0);

        let result = search_bible(&services, "мир", None, Corpus::All).unwrap();
        assert_eq!(result.hits.len(), 4); // по 2 стиха в каждом модуле

        // Модуль удалили с диска → перескан → хитов T2 больше нет.
        // На Windows открытый реестром файл не удалить: сначала rescan
        // (сбрасывает пул соединений), потом удаление, потом rescan.
        services.registry.lock().rescan();
        std::fs::remove_file(temp.0.join("T2.sqlite3")).unwrap();
        services.registry.lock().rescan();
        let result = search_bible(&services, "мир", None, Corpus::All).unwrap();
        assert_eq!(result.hits.len(), 2);
        assert!(result.hits.iter().all(|h| h.module == ModuleId::from("T")));
    }

    #[test]
    fn module_filter_limits_results() {
        let temp = TempDir::new("filter");
        write_module(&temp.0, "T");
        write_module(&temp.0, "T2");
        let services = services(&temp.0);

        let result =
            search_bible(&services, "мир", Some(&ModuleId::from("T2")), Corpus::All).unwrap();
        assert_eq!(result.hits.len(), 2);
        assert!(result.hits.iter().all(|h| h.module == ModuleId::from("T2")));
    }

    #[test]
    fn empty_query_is_error() {
        let temp = TempDir::new("empty");
        write_module(&temp.0, "T");
        let services = services(&temp.0);
        assert!(search_bible(&services, "   ", None, Corpus::All).is_err());
    }

    #[test]
    fn strong_tail_is_stripped() {
        assert_eq!(strip_strong_tail("Слово важное G25 G26"), "Слово важное");
        assert_eq!(strip_strong_tail("Обычный текст"), "Обычный текст");
        assert!(is_strong_token("H7225"));
        assert!(!is_strong_token("мир"));
        assert!(!is_strong_token("3:16"));
    }
}
