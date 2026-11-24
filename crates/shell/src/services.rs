//! Сервисы приложения и сценарии использования (architecture.md §5.2–5.3).
//!
//! GPUI здесь нет: `AppServices` — обычная структура; регистрация её как
//! глобала происходит в bible_ui (composition root).

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use parking_lot::{Mutex, RwLock};

use app_state::settings::Settings;
use bible_domain::ids::{BookNumber, ModuleId};
use bible_domain::ports::{AnnotationStore, ModuleCatalog, ModuleKind};
use bible_domain::verse::{self, CleanVerse, VerseParseOptions};
use mybible::{ModuleRegistry, SharedRegistry};

/// Текущее место чтения: пишет панель чтения (при открытии книги и при
/// прокрутке), читают панели словаря/комментариев/параллельных ссылок.
#[derive(Debug, Clone)]
pub struct CurrentLocation {
    pub module: ModuleId,
    pub book: BookNumber,
    pub book_title: String,
    /// Короткое имя книги из модуля («Ин», «2Пар») — для бегунка места
    /// и заголовка вкладки (architecture.md §6.6, этап 20).
    pub book_short: String,
    pub chapter: u16,
    pub verse: u16,
}

/// Все сервисы приложения. Создаётся один раз в main до открытия окна.
pub struct AppServices {
    /// Настройки (settings.json). RwLock: экран настроек правит их на лету
    /// (architecture.md §9), читатели берут снимок через read().
    pub settings: RwLock<Settings>,
    pub registry: SharedRegistry,
    pub current_location: Arc<Mutex<Option<CurrentLocation>>>,
    /// Хранилище аннотаций (app_state.db). None — БД не открылась,
    /// приложение живёт без пользовательских пометок (ошибка в логе).
    pub state_db: Option<Arc<app_state::db::StateDb>>,
    pub user_cross_refs: Option<Arc<app_state::user_cross_refs::UserCrossRefs>>,
    pub annotations: Option<Arc<dyn AnnotationStore>>,
    /// Хранилище логических цепочек (та же app_state.db, общий StateDb).
    /// None — БД не открылась (ошибка в логе), цепочки отключены.
    pub chains: Option<Arc<app_state::chains::SqliteChainStore>>,
    /// Хранилище тегов стихов (та же app_state.db, этап 21). None —
    /// БД не открылась, теги отключены.
    pub tags: Option<Arc<dyn bible_domain::ports::TagStore>>,
    /// Каталог FTS5-индексов модулей. None — не открылся (ошибка в логе),
    /// приложение живёт без поиска.
    pub search_index: Option<Arc<mybible::SearchIndex>>,
    /// Модули, индекс которых прямо сейчас перестраивается (in-flight
    /// guard: два потока не строят один модуль одновременно).
    pub index_building: Arc<Mutex<HashSet<String>>>,
    /// Индекс заметок (notes_dir/notes.db). None — notes_dir не задан
    /// в настройках или индекс не открылся (ошибка в логе).
    /// Заменяется целиком при смене notes_dir в настройках.
    pub notes: RwLock<Option<Arc<notes_core::index::NotesIndex>>>,
    /// Watcher хранилища заметок — держим, чтобы не остановился.
    /// Внешние правки (Obsidian) переиндексируются через него.
    /// Пересоздаётся вместе с индексом при смене notes_dir.
    pub _notes_watcher: Mutex<Option<notes_core::watcher::VaultWatcher>>,
}

impl AppServices {
    /// Читает settings.json, сканирует каталог модулей, открывает app_state.db.
    pub fn bootstrap() -> Self {
        let mut settings = app_state::settings::load();
        log::info!("modules dir: {}", settings.modules_dir.display());
        let registry = SharedRegistry::new(ModuleRegistry::scan(&settings.modules_dir));
        if let Some(configured) = settings.default_translation.as_ref() {
            let translations: Vec<_> = registry
                .modules()
                .into_iter()
                .filter(|meta| meta.kind == ModuleKind::BibleTranslation)
                .collect();
            if !translations.iter().any(|meta| &meta.id.0 == configured) {
                let mut matches = translations
                    .into_iter()
                    .filter(|meta| meta.description.as_ref() == Some(configured));
                if let (Some(meta), None) = (matches.next(), matches.next()) {
                    settings.default_translation = Some(meta.id.0);
                }
            }
        }
        // Одно соединение StateDb на все хранилища (аннотации, цепочки,
        // теги) — миграции и PRAGMA применяются один раз.
        let (state_db, annotations, chains, tags) = match app_state::db::StateDb::open_default() {
            Ok(state_db) => {
                let annotations: Arc<dyn AnnotationStore> = Arc::new(
                    app_state::annotations::SqliteAnnotationStore::new(state_db.clone()),
                );
                let chains = Arc::new(app_state::chains::SqliteChainStore::new(state_db.clone()));
                let tags: Arc<dyn bible_domain::ports::TagStore> =
                    Arc::new(app_state::tags::SqliteTagStore::new(state_db.clone()));
                (Some(state_db), Some(annotations), Some(chains), Some(tags))
            }
            Err(error) => {
                log::error!("state db unavailable: {error:#}");
                (None, None, None, None)
            }
        };
        let user_cross_refs = app_state::user_cross_refs::UserCrossRefs::open(
            &app_state::paths::data_dir().join("My Cross References.crossreferences.SQLite3"),
        )
        .map_err(|error| log::error!("user cross references unavailable: {error:#}"))
        .ok();
        let search_index = match mybible::SearchIndex::open(&app_state::paths::index_db_file()) {
            Ok(index) => Some(index),
            Err(error) => {
                log::error!("search index unavailable: {error:#}");
                None
            }
        };
        let (notes, notes_watcher) = crate::notes::init(&settings, &registry);
        Self {
            settings: RwLock::new(settings),
            registry,
            current_location: Arc::new(Mutex::new(None)),
            state_db,
            user_cross_refs,
            annotations,
            chains,
            tags,
            search_index,
            index_building: Arc::new(Mutex::new(HashSet::new())),
            notes: RwLock::new(notes),
            _notes_watcher: Mutex::new(notes_watcher),
        }
    }

    pub fn parse_options(&self) -> VerseParseOptions {
        let settings = self.settings.read();
        VerseParseOptions {
            show_verse_numbers: settings.show_verse_numbers,
            show_strong_numbers_inline: false,
            show_morphology_inline: false,
            show_critical_text: settings.show_critical_text,
        }
    }

    /// Перевод для UI по умолчанию: настройка, иначе первый по алфавиту.
    pub fn default_translation(&self) -> Option<ModuleId> {
        let configured = self.settings.read().default_translation.clone();
        if let Some(id) = configured {
            let id = ModuleId(id);
            if self
                .registry
                .module(&id)
                .is_some_and(|meta| meta.kind == ModuleKind::BibleTranslation)
            {
                return Some(id);
            }
        }
        self.registry
            .modules()
            .into_iter()
            .find(|m| m.kind == ModuleKind::BibleTranslation)
            .map(|m| m.id)
    }

    /// Последнее место чтения из session.json.
    ///
    /// Возвращает None, если позиции нет или модуль исчез из каталога
    /// (файл удалили/переименовали с прошлого запуска).
    pub fn restore_reading_position(&self) -> Option<(ModuleId, BookNumber, u16, u16)> {
        let saved = app_state::session::load().reading_position?;
        let module = ModuleId(saved.module);
        if self.registry.module(&module).is_none() {
            log::warn!("session: module {module} not found, ignoring saved position");
            return None;
        }
        Some((module, BookNumber(saved.book), saved.chapter, saved.verse))
    }

    /// Запоминает текущее место чтения. Ошибки записи — только в лог:
    /// потерять позицию не страшно, ронять чтение из-за этого нельзя.
    pub fn save_reading_position(
        &self,
        module: &ModuleId,
        book: BookNumber,
        chapter: u16,
        verse: u16,
    ) {
        let mut state = app_state::session::load();
        state.reading_position = Some(app_state::session::SavedPosition {
            module: module.to_string(),
            book: book.0,
            chapter,
            verse,
        });
        if let Err(error) = app_state::session::save(&state) {
            log::warn!("failed to save session: {error}");
        }
    }
}

/// Один стих в модели книги: распарсенный текст + координаты.
#[derive(Debug, Clone)]
pub struct VerseItem {
    pub chapter: u16,
    pub verse: u16,
    pub clean: CleanVerse,
    pub has_critical: bool,
}

/// Модель книги целиком для BibleTextView (architecture.md §8.1).
#[derive(Debug)]
pub struct BookViewModel {
    pub module: ModuleId,
    pub book: BookNumber,
    /// Название книги из самого модуля (для заголовка вкладки).
    pub title: String,
    /// Короткое имя книги из модуля («2Пар») — бегунок места и
    /// заголовок вкладки (этап 20).
    pub book_short: String,
    pub items: Vec<VerseItem>,
}

impl BookViewModel {
    /// Линейный индекс стиха (chapter, verse) в items.
    pub fn index_of(&self, chapter: u16, verse: u16) -> Option<usize> {
        self.items
            .iter()
            .position(|item| item.chapter == chapter && item.verse == verse)
    }
}

/// Карточка слова для панели «Слово» (architecture.md §6.4).
/// Собирается целиком в shell: UI только рисует.
#[derive(Debug, Clone)]
pub struct WordDescription {
    /// Само слово как в тексте.
    pub word: String,
    pub module: ModuleId,
    /// Книга — для повторного запроса при смене Strong's-словаря.
    pub book: BookNumber,
    pub book_title: String,
    pub chapter: u16,
    pub verse: u16,
    /// Разобранный токен слова — для повторного запроса (redescribe).
    pub token: bible_domain::verse::VerseToken,
    /// Номер Strong's с префиксом («G25», «H123»).
    pub strong: Option<String>,
    /// Словарь, из которого взята статья (источник; для «открыть в словаре»).
    pub strongs_dict: Option<ModuleId>,
    /// Тема статьи в словаре-источнике (может отличаться от `strong`:
    /// часть модулей хранит топики голыми номерами).
    pub strong_topic: Option<String>,
    /// Статья словаря-источника (сырой текст с разметкой MyBible).
    pub strong_entry: Option<String>,
    /// Лексема (словарная форма) — из колонки lexeme, если она есть
    /// в словаре-источнике (§6.4, перенос WordDetail форка).
    pub lexeme: Option<String>,
    pub transliteration: Option<String>,
    pub pronunciation: Option<String>,
    /// Краткое определение (колонка short_definition).
    pub short_definition: Option<String>,
    /// Морфологический код как в источнике («V-AAI-3S»).
    pub morph_code: Option<String>,
    /// Расшифровка кода через morphology_indications словаря.
    pub morph_gloss: Option<String>,
}

/// Отрезает буквенный префикс номера Strong's («G25» → «25»; перенос
/// `strip_strong_prefix` из dictionary.rs форка).
fn strip_strong_prefix(value: &str) -> String {
    value
        .trim()
        .trim_start_matches(|c: char| c.is_ascii_alphabetic())
        .to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BiblicalScript {
    Greek,
    Hebrew,
    Other,
}

fn biblical_script(word: &str) -> BiblicalScript {
    let mut greek = 0usize;
    let mut hebrew = 0usize;
    for ch in word.chars() {
        let code = ch as u32;
        if (0x0370..=0x03ff).contains(&code) || (0x1f00..=0x1fff).contains(&code) {
            greek += 1;
        } else if (0x0590..=0x05ff).contains(&code) || (0xfb1d..=0xfb4f).contains(&code) {
            hebrew += 1;
        }
    }
    match greek.cmp(&hebrew) {
        std::cmp::Ordering::Greater if greek > 0 => BiblicalScript::Greek,
        std::cmp::Ordering::Less if hebrew > 0 => BiblicalScript::Hebrew,
        _ => BiblicalScript::Other,
    }
}

fn expected_strong_prefix(word: &str, book: BookNumber) -> char {
    match biblical_script(word) {
        BiblicalScript::Greek => 'G',
        BiblicalScript::Hebrew => 'H',
        BiblicalScript::Other if book.0 < 470 => 'H',
        BiblicalScript::Other => 'G',
    }
}

/// Candidate topics for one dictionary. The opposite biblical language is
/// deliberately never tried: `G25` and `H25` are unrelated entries.
fn strong_topic_candidates(
    expected_prefix: char,
    strong_clean: &str,
    raw: Option<&str>,
) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::with_capacity(3);
    candidates.push(format!("{expected_prefix}{strong_clean}"));
    candidates.push(strong_clean.to_string());
    if let Some(raw) = raw
        && raw != strong_clean
        && raw.chars().next().is_none_or(|prefix| {
            !matches!(prefix, 'G' | 'g' | 'H' | 'h')
                || prefix.eq_ignore_ascii_case(&expected_prefix)
        })
        && !candidates.iter().any(|c| c == raw)
    {
        candidates.push(raw.to_string());
    }
    candidates.dedup();
    candidates
}

/// Сценарий «описать слово»: Strong's + морфология. При явно выбранном
/// Strong's-словаре статья ищется только в нём; режим «Авто» проверяет
/// словари по приоритету, затем морфологию разрешает по доступным таблицам.
pub fn describe_word(
    services: &AppServices,
    module: &ModuleId,
    book: BookNumber,
    book_title: &str,
    chapter: u16,
    verse: u16,
    token: &bible_domain::verse::VerseToken,
) -> WordDescription {
    // The visible word is authoritative for original-language modules.
    // Translations fall back to the canonical testament boundary.
    let expected_prefix = expected_strong_prefix(&token.text, book);
    let strong = token.strong.as_ref().map(|number| {
        let number = strip_strong_prefix(number);
        format!("{expected_prefix}{number}")
    });

    // Режим «Авто» использует Strong's-словари раньше обычных;
    // внутри группы — по id для стабильности.
    let preferred = services
        .settings
        .read()
        .preferred_strongs_dictionary
        .clone();
    let mut dictionaries: Vec<bible_domain::ports::ModuleMeta> = services
        .registry
        .modules()
        .into_iter()
        .filter(|m| {
            matches!(
                m.kind,
                ModuleKind::StrongsDictionary | ModuleKind::GeneralDictionary
            )
        })
        .collect();
    dictionaries.sort_by(|a, b| {
        let rank = |m: &bible_domain::ports::ModuleMeta| {
            if Some(m.id.0.as_str()) == preferred.as_deref() {
                0
            } else if m.kind == ModuleKind::StrongsDictionary {
                1
            } else {
                2
            }
        };
        rank(a).cmp(&rank(b)).then_with(|| a.id.cmp(&b.id))
    });

    // Явный выбор — строго один источник без незаметного fallback в другие
    // словари. «Авто» сохраняет последовательный поиск по каталогу.
    let selected_strongs = preferred.as_deref().and_then(|id| {
        dictionaries
            .iter()
            .find(|dict| dict.id.0 == id && dict.kind == ModuleKind::StrongsDictionary)
    });
    let strong_search_order: Vec<&bible_domain::ports::ModuleMeta> =
        if let Some(dict) = selected_strongs {
            vec![dict]
        } else {
            dictionaries.iter().collect()
        };

    let mut registry = services.registry.lock();

    // Статья: первый словарь выбранного порядка, где найдётся тема.
    let mut source: Option<(ModuleId, String)> = None;
    let mut entry: Option<mybible::DictionaryEntry> = None;
    if let Some(strong) = &strong {
        let strong_clean = strip_strong_prefix(strong);
        for dict in &strong_search_order {
            for topic in
                strong_topic_candidates(expected_prefix, &strong_clean, token.strong.as_deref())
            {
                if let Some(found) = registry.dictionary_entry_full(&dict.id, &topic) {
                    source = Some((dict.id.clone(), topic));
                    entry = Some(found);
                    break;
                }
            }
            if entry.is_some() {
                break;
            }
        }
    }

    // Морфология: словарь-источник статьи, если у него есть таблица;
    // иначе первый словарь приоритета с morphology_indications (форк
    // проходит тот же priority-список).
    let morph_gloss = token.morph.as_ref().and_then(|code| {
        let mut order: Vec<&bible_domain::ports::ModuleMeta> = Vec::new();
        if let Some((source_id, _)) = &source
            && let Some(dict) = dictionaries.iter().find(|d| &d.id == source_id)
        {
            order.push(dict);
        }
        order.extend(dictionaries.iter().filter(|d| d.has_morphology_table));
        let mut seen = HashSet::new();
        order
            .into_iter()
            .filter(|d| d.has_morphology_table && seen.insert(d.id.clone()))
            .find_map(|d| registry.resolve_morphology(&d.id, code))
    });

    WordDescription {
        word: token.text.clone(),
        module: module.clone(),
        book,
        book_title: book_title.to_string(),
        chapter,
        verse,
        token: token.clone(),
        strong,
        strongs_dict: source.as_ref().map(|(id, _)| id.clone()),
        strong_topic: source.map(|(_, topic)| topic),
        strong_entry: entry.as_ref().map(|e| e.definition.clone()),
        lexeme: entry.as_ref().and_then(|e| e.lexeme.clone()),
        transliteration: entry.as_ref().and_then(|e| e.transliteration.clone()),
        pronunciation: entry.as_ref().and_then(|e| e.pronunciation.clone()),
        short_definition: entry.as_ref().and_then(|e| e.short_definition.clone()),
        morph_code: token.morph.clone(),
        morph_gloss,
    }
}

/// Повторное описание того же слова (смена Strong's-словаря в настройках):
/// все входные данные уже лежат в карточке.
pub fn redescribe(services: &AppServices, description: &WordDescription) -> WordDescription {
    describe_word(
        services,
        &description.module,
        description.book,
        &description.book_title,
        description.chapter,
        description.verse,
        &description.token,
    )
}

/// Сценарий «открыть книгу целиком»: читает все стихи, парсит каждый.
pub fn open_book(
    services: &AppServices,
    module: &ModuleId,
    book: BookNumber,
) -> Result<BookViewModel> {
    let options = services.parse_options();
    let mut registry = services.registry.lock();

    let book_entry = registry
        .books_of(module)
        .into_iter()
        .find(|entry| entry.book_number == book.0);
    let title = book_entry
        .as_ref()
        .map(|entry| entry.long_name.clone())
        .unwrap_or_else(|| format!("Book {}", book.0));
    let book_short = book_entry
        .map(|entry| entry.short_name)
        .unwrap_or_else(|| title.clone());

    let rows = registry.verses_of_book(module, book)?;
    anyhow::ensure!(
        !rows.is_empty(),
        "module {module} has no verses for book {}",
        book.0
    );

    let items = rows
        .into_iter()
        .map(|(chapter, verse_no, raw)| {
            let mut clean = verse::parse_verse_full(
                &raw,
                options.show_strong_numbers_inline,
                options.show_morphology_inline,
                false,
            );
            let has_critical = clean.had_hidden_critical;
            if has_critical && options.show_critical_text {
                clean = verse::parse_verse_full(
                    &raw,
                    options.show_strong_numbers_inline,
                    options.show_morphology_inline,
                    true,
                );
            }
            VerseItem {
                chapter,
                verse: verse_no,
                clean,
                has_critical,
            }
        })
        .collect();

    log::info!("opened {module} book {} ({title})", book.0);
    Ok(BookViewModel {
        module: module.clone(),
        book,
        title,
        book_short,
        items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use bible_domain::verse::VerseToken;

    /// Перевод с префиксом Strong's «G» (как греческие подстрочники).
    fn write_bible(dir: &Path, name: &str) {
        let path = dir.join(format!("{name}.sqlite3"));
        let connection = sqlez::connection::Connection::open_file(path.to_str().unwrap());
        connection
            .exec("CREATE TABLE info(name TEXT, value TEXT)")
            .unwrap()()
        .unwrap();
        connection
            .exec("INSERT INTO info VALUES('strong_numbers_prefix', 'G')")
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

    /// Strong's-словарь. rich — со всеми колонками и морфологией;
    /// bare_topics — топики голыми номерами («25»), иначе «G25».
    fn write_strongs(dir: &Path, name: &str, rich: bool, bare_topics: bool, definition: &str) {
        let path = dir.join(format!("{name}.dictionary.sqlite3"));
        let connection = sqlez::connection::Connection::open_file(path.to_str().unwrap());
        connection
            .exec("CREATE TABLE info(name TEXT, value TEXT)")
            .unwrap()()
        .unwrap();
        connection
            .exec("INSERT INTO info VALUES('is_strong', 'true')")
            .unwrap()()
        .unwrap();
        let topic = if bare_topics { "25" } else { "G25" };
        if rich {
            connection
                .exec(
                    "CREATE TABLE dictionary(\
                       topic TEXT, definition TEXT, lexeme TEXT, \
                       transliteration TEXT, pronunciation TEXT, short_definition TEXT)",
                )
                .unwrap()()
            .unwrap();
            connection
                .exec(&format!(
                    "INSERT INTO dictionary VALUES(\
                     '{topic}', '{definition}', 'ἀγαπάω', 'agapaō', 'a-ga-PA-o', 'любить')"
                ))
                .unwrap()()
            .unwrap();
            connection
                .exec("CREATE TABLE morphology_indications(indication TEXT, meaning TEXT)")
                .unwrap()()
            .unwrap();
            connection
                .exec(
                    "INSERT INTO morphology_indications VALUES\
                       ('V', 'Глагол'), ('-AAI', 'Аорист активный'), ('-3S', '3 лицо ед. ч.')",
                )
                .unwrap()()
            .unwrap();
        } else {
            connection
                .exec("CREATE TABLE dictionary(topic TEXT, definition TEXT)")
                .unwrap()()
            .unwrap();
            connection
                .exec(&format!(
                    "INSERT INTO dictionary VALUES('{topic}', '{definition}')"
                ))
                .unwrap()()
            .unwrap();
        }
        drop(connection);
    }

    fn services(modules_dir: &Path) -> AppServices {
        let mut settings = Settings::default();
        settings.modules_dir = modules_dir.to_path_buf();
        AppServices {
            settings: RwLock::new(settings),
            registry: SharedRegistry::new(ModuleRegistry::scan(modules_dir)),
            current_location: Arc::new(Mutex::new(None)),
            state_db: None,
            user_cross_refs: None,
            annotations: None,
            chains: None,
            tags: None,
            search_index: None,
            index_building: Arc::new(Mutex::new(HashSet::new())),
            notes: RwLock::new(None),
            _notes_watcher: Mutex::new(None),
        }
    }

    fn token() -> VerseToken {
        VerseToken {
            range: 0..8,
            text: "ἠγάπησεν".to_string(),
            strong: Some("25".to_string()),
            morph: Some("V-AAI-3S".to_string()),
            word_index_in_verse: 0,
        }
    }

    #[test]
    fn describe_word_reads_rich_columns_and_morphology() {
        let dir = tempfile::tempdir().unwrap();
        write_bible(dir.path(), "GRK");
        write_strongs(dir.path(), "STR", true, false, "полная статья");
        let services = services(dir.path());

        let description = describe_word(
            &services,
            &ModuleId::from("GRK"),
            BookNumber(500),
            "Иоанн",
            3,
            16,
            &token(),
        );
        assert_eq!(description.strong.as_deref(), Some("G25"));
        assert_eq!(
            description.strongs_dict,
            Some(ModuleId::from("STR.dictionary"))
        );
        assert_eq!(description.strong_topic.as_deref(), Some("G25"));
        assert_eq!(description.lexeme.as_deref(), Some("ἀγαπάω"));
        assert_eq!(description.transliteration.as_deref(), Some("agapaō"));
        assert_eq!(description.pronunciation.as_deref(), Some("a-ga-PA-o"));
        assert_eq!(description.short_definition.as_deref(), Some("любить"));
        assert_eq!(description.strong_entry.as_deref(), Some("полная статья"));
        assert_eq!(
            description.morph_gloss.as_deref(),
            Some("Глагол, Аорист активный, 3 лицо ед. ч.")
        );
    }

    #[test]
    fn describe_word_uses_testament_for_translated_words() {
        let dir = tempfile::tempdir().unwrap();
        write_bible(dir.path(), "RUS");
        let path = dir.path().join("RUS.sqlite3");
        let connection = sqlez::connection::Connection::open_file(path.to_str().unwrap());
        connection
            .exec("UPDATE info SET value = 'ru' WHERE name = 'strong_numbers_prefix'")
            .unwrap()()
        .unwrap();
        drop(connection);
        let services = services(dir.path());

        let mut translated = token();
        translated.text = "любовь".to_string();
        let old = describe_word(
            &services,
            &ModuleId::from("RUS"),
            BookNumber(10),
            "Бытие",
            1,
            1,
            &translated,
        );
        let new = describe_word(
            &services,
            &ModuleId::from("RUS"),
            BookNumber(500),
            "Иоанн",
            3,
            16,
            &translated,
        );
        assert_eq!(old.strong.as_deref(), Some("H25"));
        assert_eq!(new.strong.as_deref(), Some("G25"));
    }

    #[test]
    fn describe_word_uses_visible_biblical_script_before_testament() {
        let dir = tempfile::tempdir().unwrap();
        write_bible(dir.path(), "SRC");
        let services = services(dir.path());

        let greek = describe_word(
            &services,
            &ModuleId::from("SRC"),
            BookNumber(10),
            "Бытие",
            1,
            1,
            &token(),
        );
        let mut hebrew_token = token();
        hebrew_token.text = "אָהַב".to_string();
        let hebrew = describe_word(
            &services,
            &ModuleId::from("SRC"),
            BookNumber(500),
            "Иоанн",
            1,
            1,
            &hebrew_token,
        );
        assert_eq!(greek.strong.as_deref(), Some("G25"));
        assert_eq!(hebrew.strong.as_deref(), Some("H25"));
        assert_eq!(
            strong_topic_candidates('H', "25", Some("G25")),
            vec!["H25".to_string(), "25".to_string()]
        );
    }

    #[test]
    fn describe_word_never_substitutes_opposite_language_topic() {
        let dir = tempfile::tempdir().unwrap();
        write_bible(dir.path(), "RUS");
        write_strongs(dir.path(), "BOTH", false, false, "греческая статья");
        let dictionary = dir.path().join("BOTH.dictionary.sqlite3");
        let connection = sqlez::connection::Connection::open_file(dictionary.to_str().unwrap());
        connection
            .exec("INSERT INTO dictionary VALUES('H25', 'еврейская статья')")
            .unwrap()()
        .unwrap();
        drop(connection);
        let services = services(dir.path());

        // Даже если исходный модуль положил в токен G25, русский текст ВЗ
        // должен искать именно H25 в колонке dictionary.topic.
        let mut translated = token();
        translated.text = "любовь".to_string();
        translated.strong = Some("G25".to_string());
        let description = describe_word(
            &services,
            &ModuleId::from("RUS"),
            BookNumber(10),
            "Бытие",
            1,
            1,
            &translated,
        );
        assert_eq!(description.strong.as_deref(), Some("H25"));
        assert_eq!(description.strong_topic.as_deref(), Some("H25"));
        assert_eq!(
            description.strong_entry.as_deref(),
            Some("еврейская статья")
        );
    }

    #[test]
    fn describe_word_falls_back_to_bare_topic() {
        let dir = tempfile::tempdir().unwrap();
        write_bible(dir.path(), "GRK");
        // Словарь хранит топики голыми номерами — кандидаты достают статью.
        write_strongs(dir.path(), "BARE", false, true, "статья голого номера");
        let services = services(dir.path());

        let description = describe_word(
            &services,
            &ModuleId::from("GRK"),
            BookNumber(500),
            "Иоанн",
            3,
            16,
            &token(),
        );
        assert_eq!(
            description.strongs_dict,
            Some(ModuleId::from("BARE.dictionary"))
        );
        assert_eq!(description.strong_topic.as_deref(), Some("25"));
        assert_eq!(
            description.strong_entry.as_deref(),
            Some("статья голого номера")
        );
    }

    #[test]
    fn describe_word_preferred_dictionary_wins() {
        let dir = tempfile::tempdir().unwrap();
        write_bible(dir.path(), "GRK");
        write_strongs(dir.path(), "AAA", false, false, "статья AAA");
        write_strongs(dir.path(), "ZZZ", false, false, "статья ZZZ");
        let services = services(dir.path());
        // Без закрепления — первый по алфавиту.
        let description = describe_word(
            &services,
            &ModuleId::from("GRK"),
            BookNumber(500),
            "Иоанн",
            3,
            16,
            &token(),
        );
        assert_eq!(description.strong_entry.as_deref(), Some("статья AAA"));

        // Закреплённый в настройках словарь побеждает алфавитный порядок.
        // Правим состояние напрямую: settings::update писал бы settings.json
        // в каталог данных, а тесту файловый побочный эффект не нужен.
        services.settings.write().preferred_strongs_dictionary = Some("ZZZ.dictionary".to_string());
        let description = describe_word(
            &services,
            &ModuleId::from("GRK"),
            BookNumber(500),
            "Иоанн",
            3,
            16,
            &token(),
        );
        assert_eq!(description.strong_entry.as_deref(), Some("статья ZZZ"));
        assert_eq!(
            description.strongs_dict,
            Some(ModuleId::from("ZZZ.dictionary"))
        );
    }
}
