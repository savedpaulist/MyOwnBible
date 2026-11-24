//! Реестр модулей MyBible: сканирование каталога, классификация,
//! пул read-only соединений, запросы книг и стихов (architecture.md §4.1).
//!
//! Перенесено из форка (`bible/src/module.rs`) с отрезанным GPUI:
//! реестр теперь обычная структура, владение — через `Arc` в `shell`.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use parking_lot::Mutex;

use bible_domain::ids::{BookNumber, ModuleId, VerseRef};
use bible_domain::ports::{ModuleCatalog, ModuleKind, ModuleMeta, RawVerse, VerseSource};

use crate::db::ReadOnlyDb;

/// Полная словарная статья: `definition` плюс необязательные колонки
/// (перенос идеи `DictionaryColumns`/`columns_for` из `dictionary.rs`
/// форка). Колонки lexeme/transliteration/pronunciation/short_definition
/// есть не во всех модулях — проверяются PRAGMA table_info и кэшируются.
#[derive(Debug, Clone, Default)]
pub struct DictionaryEntry {
    pub definition: String,
    pub lexeme: Option<String>,
    pub transliteration: Option<String>,
    pub pronunciation: Option<String>,
    pub short_definition: Option<String>,
}

/// Какие необязательные колонки есть у таблицы `dictionary` модуля.
#[derive(Debug, Clone, Copy, Default)]
struct DictionaryColumns {
    has_lexeme: bool,
    has_transliteration: bool,
    has_pronunciation: bool,
    has_short_definition: bool,
}

impl DictionaryColumns {
    /// SELECT-список с NULL-заглушками под отсутствующие колонки:
    /// форма результата не зависит от схемы конкретного модуля.
    fn select_clause(self) -> String {
        let pick = |present: bool, name: &str| -> String {
            if present {
                name.to_string()
            } else {
                format!("NULL AS {name}")
            }
        };
        [
            "definition".to_string(),
            pick(self.has_lexeme, "lexeme"),
            pick(self.has_transliteration, "transliteration"),
            pick(self.has_pronunciation, "pronunciation"),
            pick(self.has_short_definition, "short_definition"),
        ]
        .join(", ")
    }
}

#[derive(Debug, Clone)]
pub struct BookEntry {
    pub book_number: i64,
    pub short_name: String,
    pub long_name: String,
}

#[derive(Debug, Clone)]
pub struct CommentaryEntry {
    pub book: BookNumber,
    pub chapter_from: u16,
    pub verse_from: u16,
    pub chapter_to: u16,
    pub verse_to: u16,
    pub text: String,
}

pub struct ModuleRegistry {
    modules_dir: PathBuf,
    modules: HashMap<ModuleId, Arc<ModuleMeta>>,
    pool: HashMap<ModuleId, Arc<ReadOnlyDb>>,
    /// Кэш книг по модулю — читается один раз при первом обращении.
    books: HashMap<ModuleId, Vec<BookEntry>>,
    chapter_counts: HashMap<(ModuleId, BookNumber), u16>,
    /// Кэш схемы dictionary по модулю (PRAGMA table_info читается один раз).
    dict_columns: HashMap<ModuleId, DictionaryColumns>,
    /// Сводный индекс имён книг всех переводов (ключ — normalize_book_key):
    /// разбор ссылок «Ин 3:16» из заметок и будущих пикеров.
    book_index: HashMap<String, i64>,
    /// Сводный список книг всех переводов (дедуп по book_number).
    book_entries: Vec<BookEntry>,
}

impl ModuleRegistry {
    /// Сканирует каталог модулей. Ошибки отдельных файлов не роняют скан —
    /// битый модуль пропускается с предупреждением в лог.
    pub fn scan(modules_dir: &Path) -> Self {
        let mut registry = Self {
            modules_dir: modules_dir.to_path_buf(),
            modules: HashMap::new(),
            pool: HashMap::new(),
            books: HashMap::new(),
            chapter_counts: HashMap::new(),
            dict_columns: HashMap::new(),
            book_index: HashMap::new(),
            book_entries: Vec::new(),
        };
        registry.rescan();
        registry
    }

    /// Полное пересканирование каталога (кнопка «Обновить модули»).
    /// Пул соединений и кэш книг сбрасываются целиком; удалённые с диска
    /// модули исчезают из каталога (architecture.md §4.1.2).
    pub fn rescan(&mut self) {
        self.modules.clear();
        self.pool.clear();
        self.books.clear();
        self.chapter_counts.clear();
        self.dict_columns.clear();
        self.book_index.clear();
        self.book_entries.clear();

        let entries = match scan_modules_dir(&self.modules_dir) {
            Ok(entries) => entries,
            Err(error) => {
                log::warn!(
                    "modules_dir {} could not be scanned: {error:#}",
                    self.modules_dir.display()
                );
                return;
            }
        };

        for path in entries {
            match classify_module(&path) {
                Ok(meta) => {
                    // Сводный индекс имён книг строим только по переводам.
                    if meta.kind == ModuleKind::BibleTranslation {
                        if let Err(error) = self.merge_books_from(&meta) {
                            log::warn!("books index: {}: {error:#}", meta.id);
                        }
                    }
                    self.modules.insert(meta.id.clone(), Arc::new(meta));
                }
                Err(error) => {
                    log::warn!("skipping module {}: {error:#}", path.display());
                }
            }
        }
        log::info!(
            "scanned {} modules in {}",
            self.modules.len(),
            self.modules_dir.display()
        );
    }

    /// Сменить каталог модулей и пересканировать его (экран настроек,
    /// architecture.md §9). Пул и кэши сбрасываются так же, как при rescan.
    pub fn rescan_dir(&mut self, modules_dir: &Path) {
        if self.modules_dir != modules_dir {
            log::info!(
                "modules dir: {} -> {}",
                self.modules_dir.display(),
                modules_dir.display()
            );
            self.modules_dir = modules_dir.to_path_buf();
        }
        self.rescan();
    }

    /// Книга по имени (любая локализация любого загруженного перевода).
    pub fn lookup_book(&self, name: &str) -> Option<i64> {
        self.book_index
            .get(&bible_domain::reference::normalize_book_key(name))
            .copied()
    }

    /// Сводный список книг всех переводов в каноническом порядке.
    pub fn book_entries(&self) -> &[BookEntry] {
        &self.book_entries
    }

    /// Слить имена книг модуля в сводный индекс (как merge_books_from форка).
    fn merge_books_from(&mut self, meta: &ModuleMeta) -> Result<()> {
        use bible_domain::reference::normalize_book_key;
        let db = ReadOnlyDb::open(&meta.path)?;
        let books = db.with(|conn| {
            conn.select::<(i64, String, String)>(
                "SELECT book_number, short_name, long_name FROM books \
                 ORDER BY book_number",
            )?()
        })?;
        for (book_number, short_name, long_name) in books {
            self.book_index
                .entry(normalize_book_key(&short_name))
                .or_insert(book_number);
            self.book_index
                .entry(normalize_book_key(&long_name))
                .or_insert(book_number);
            if !self
                .book_entries
                .iter()
                .any(|entry| entry.book_number == book_number)
            {
                self.book_entries.push(BookEntry {
                    book_number,
                    short_name,
                    long_name,
                });
            }
        }
        self.book_entries.sort_by_key(|entry| entry.book_number);
        Ok(())
    }

    pub fn db_for(&mut self, module_id: &ModuleId) -> Option<Arc<ReadOnlyDb>> {
        if let Some(db) = self.pool.get(module_id) {
            return Some(db.clone());
        }
        let meta = self.modules.get(module_id)?.clone();
        match ReadOnlyDb::open(&meta.path) {
            Ok(db) => {
                self.pool.insert(module_id.clone(), db.clone());
                Some(db)
            }
            Err(error) => {
                log::error!(
                    "failed to open module {} at {}: {error:#}",
                    meta.id,
                    meta.path.display()
                );
                None
            }
        }
    }

    /// Книги модуля в каноническом порядке; имена — на языке самого модуля.
    pub fn books_of(&mut self, module_id: &ModuleId) -> Vec<BookEntry> {
        if let Some(books) = self.books.get(module_id) {
            return books.clone();
        }
        let Some(db) = self.db_for(module_id) else {
            return Vec::new();
        };
        let rows: Result<Vec<(i64, String, String)>> = db.with(|conn| {
            let mut select = conn
                .select::<(i64, String, String)>(
                    "SELECT book_number, short_name, long_name FROM books \
                     ORDER BY book_number",
                )
                .context("preparing SELECT books")?;
            select().context("running SELECT books")
        });
        match rows {
            Ok(rows) => {
                let books: Vec<BookEntry> = rows
                    .into_iter()
                    .map(|(book_number, short_name, long_name)| BookEntry {
                        book_number,
                        short_name,
                        long_name,
                    })
                    .collect();
                self.books.insert(module_id.clone(), books.clone());
                books
            }
            Err(error) => {
                log::warn!("failed to read books for module {module_id}: {error:#}");
                Vec::new()
            }
        }
    }

    /// Все стихи книги одним запросом: (chapter, verse, raw_text) по порядку.
    /// Основа режима «книга целиком» (architecture.md §8.1).
    pub fn verses_of_book(
        &mut self,
        module_id: &ModuleId,
        book: BookNumber,
    ) -> Result<Vec<(u16, u16, String)>> {
        let db = self
            .db_for(module_id)
            .ok_or_else(|| anyhow::anyhow!("module {module_id} is not available"))?;
        db.with(|conn| {
            let mut stmt = conn.select_bound::<i64, (i64, i64, String)>(
                "SELECT chapter, verse, text FROM verses \
                 WHERE book_number = ? ORDER BY chapter, verse",
            )?;
            let rows = stmt(book.0)?;
            Ok(rows
                .into_iter()
                .map(|(chapter, verse, text)| (chapter as u16, verse as u16, text))
                .collect())
        })
    }
    /// Число глав книги (MAX(chapter)); 0, если модуль недоступен.
    /// Нужен навигатору «книга → глава → стих» (architecture.md §6.2).
    pub fn chapter_count_of(&mut self, module_id: &ModuleId, book: BookNumber) -> u16 {
        let key = (module_id.clone(), book);
        if let Some(count) = self.chapter_counts.get(&key) {
            return *count;
        }
        let Some(db) = self.db_for(module_id) else {
            return 0;
        };
        let count = db.with(|conn| {
            let mut stmt = conn.select_bound::<i64, Option<i64>>(
                "SELECT MAX(chapter) FROM verses WHERE book_number = ?",
            )?;
            let rows = stmt(book.0)?;
            Ok(rows.into_iter().flatten().next().unwrap_or(0) as u16)
        });
        if let Ok(count) = count {
            self.chapter_counts.insert(key, count);
            count
        } else {
            0
        }
    }

    /// Число стихов главы (MAX(verse)); 0, если модуль недоступен.
    pub fn verse_count_of(&mut self, module_id: &ModuleId, book: BookNumber, chapter: u16) -> u16 {
        let Some(db) = self.db_for(module_id) else {
            return 0;
        };
        db.with(|conn| {
            let mut stmt = conn.select_bound::<(i64, i64), Option<i64>>(
                "SELECT MAX(verse) FROM verses WHERE book_number = ? AND chapter = ?",
            )?;
            let rows = stmt((book.0, chapter as i64))?;
            Ok(rows.into_iter().flatten().next().unwrap_or(0) as u16)
        })
        .unwrap_or(0)
    }

    /// Номера присутствующих стихов главы без загрузки их текста.
    pub fn verse_numbers_of(
        &mut self,
        module: &ModuleId,
        book: BookNumber,
        chapter: u16,
    ) -> Vec<u16> {
        let Some(db) = self.db_for(module) else {
            return Vec::new();
        };
        db.with(|conn| {
            let rows: Vec<i64> = conn.select_bound(
                "SELECT verse FROM verses WHERE book_number = ? AND chapter = ? ORDER BY verse",
            )?((book.0, i64::from(chapter)))?;
            Ok(rows
                .into_iter()
                .filter_map(|verse| u16::try_from(verse).ok())
                .collect())
        })
        .unwrap_or_default()
    }

    /// Темы словаря с фильтром-подстрокой (LIKE %..%), ограничение LIMIT.
    /// Пустой фильтр — первые `limit` тем по алфавиту.
    pub fn dictionary_topics(
        &mut self,
        module_id: &ModuleId,
        filter: &str,
        limit: usize,
    ) -> Vec<String> {
        let Some(db) = self.db_for(module_id) else {
            return Vec::new();
        };
        let result = if filter.is_empty() {
            db.with(|conn| {
                let mut stmt = conn.select_bound::<i64, String>(
                    "SELECT topic FROM dictionary ORDER BY topic LIMIT ?",
                )?;
                stmt(limit as i64)
            })
        } else {
            let pattern = format!("%{}%", filter.replace('%', ""));
            db.with(|conn| {
                let mut stmt = conn.select_bound::<(String, i64), String>(
                    "SELECT topic FROM dictionary WHERE topic LIKE ? \
                     ORDER BY topic LIMIT ?",
                )?;
                stmt((pattern, limit as i64))
            })
        };
        match result {
            Ok(topics) => topics,
            Err(error) => {
                log::warn!("dictionary topics failed for {module_id}: {error:#}");
                Vec::new()
            }
        }
    }

    /// Статья словаря по теме (сырой текст с разметкой).
    pub fn dictionary_entry(&mut self, module_id: &ModuleId, topic: &str) -> Option<String> {
        self.dictionary_entry_full(module_id, topic)
            .map(|entry| entry.definition)
    }

    /// Полная статья словаря с необязательными колонками (лексема,
    /// транслитерация, произношение, краткое определение) — для карточки
    /// слова (architecture.md §6.4). Схема модуля проверяется PRAGMA
    /// один раз и кэшируется; отсутствующие колонки приходят NULL.
    pub fn dictionary_entry_full(
        &mut self,
        module_id: &ModuleId,
        topic: &str,
    ) -> Option<DictionaryEntry> {
        let columns = self.dictionary_columns(module_id)?;
        let db = self.db_for(module_id)?;
        let sql = format!(
            "SELECT {} FROM dictionary WHERE topic = ?",
            columns.select_clause()
        );
        db.with(|conn| {
            let mut stmt = conn.select_row_bound::<String, (
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
            )>(&sql)?;
            stmt(topic.to_string())
        })
        .unwrap_or_else(|error| {
            log::warn!("dictionary entry failed for {module_id}: {error:#}");
            None
        })
        .map(
            |(definition, lexeme, transliteration, pronunciation, short_definition)| {
                DictionaryEntry {
                    definition,
                    lexeme,
                    transliteration,
                    pronunciation,
                    short_definition,
                }
            },
        )
    }

    /// Схема таблицы dictionary модуля (кэшируется; перенос `columns_for`
    /// из форка). None — модуль недоступен или таблицы нет.
    fn dictionary_columns(&mut self, module_id: &ModuleId) -> Option<DictionaryColumns> {
        if let Some(columns) = self.dict_columns.get(module_id) {
            return Some(*columns);
        }
        let db = self.db_for(module_id)?;
        let columns = db
            .with(|conn| {
                let rows = conn.select::<(i64, String, String, i64, Option<String>, i64)>(
                    "PRAGMA table_info(dictionary)",
                )?()?;
                let mut columns = DictionaryColumns::default();
                for row in rows {
                    match row.1.as_str() {
                        "lexeme" => columns.has_lexeme = true,
                        "transliteration" => columns.has_transliteration = true,
                        "pronunciation" => columns.has_pronunciation = true,
                        "short_definition" => columns.has_short_definition = true,
                        _ => {}
                    }
                }
                Ok(columns)
            })
            .unwrap_or_else(|error| {
                log::warn!("dictionary schema failed for {module_id}: {error:#}");
                DictionaryColumns::default()
            });
        self.dict_columns.insert(module_id.clone(), columns);
        Some(columns)
    }

    /// Индекс комментариев книги одним запросом: (chapter, verse_from,
    /// verse_to) — для надстрочных маркеров в тексте (architecture.md
    /// §6.3.2). Тексты комментариев здесь не читаются.
    pub fn commentary_index_of_book(
        &mut self,
        module_id: &ModuleId,
        book: BookNumber,
    ) -> Vec<(u16, u16, u16)> {
        let Some(db) = self.db_for(module_id) else {
            return Vec::new();
        };
        db.with(|conn| {
            let mut stmt = conn.select_bound::<i64, (i64, i64, i64)>(
                "SELECT chapter_number_from, verse_number_from, verse_number_to \
                 FROM commentaries WHERE book_number = ? \
                 ORDER BY chapter_number_from, verse_number_from",
            )?;
            let rows = stmt(book.0)?;
            Ok(rows
                .into_iter()
                .map(|(chapter, from, to)| (chapter as u16, from as u16, to as u16))
                .collect())
        })
        .unwrap_or_else(|error| {
            log::warn!("commentary index failed for {module_id}: {error:#}");
            Vec::new()
        })
    }

    /// Комментарии к главе: (verse_from, verse_to, text) по порядку.
    /// Схема MyBible: book_number, chapter_number_from/to, verse_number_from/to.
    pub fn commentary_for_chapter(
        &mut self,
        module_id: &ModuleId,
        book: BookNumber,
        chapter: u16,
    ) -> Vec<(u16, u16, String)> {
        let Some(db) = self.db_for(module_id) else {
            return Vec::new();
        };
        db.with(|conn| {
            let mut stmt = conn.select_bound::<(i64, i64), (i64, i64, String)>(
                "SELECT verse_number_from, verse_number_to, text FROM commentaries \
                 WHERE book_number = ? AND chapter_number_from = ? \
                 ORDER BY verse_number_from",
            )?;
            let rows = stmt((book.0, chapter as i64))?;
            Ok(rows
                .into_iter()
                .map(|(from, to, text)| (from as u16, to as u16, text))
                .collect())
        })
        .unwrap_or_else(|error| {
            log::warn!("commentary failed for {module_id}: {error:#}");
            Vec::new()
        })
    }

    /// Complete commentary catalogue for the sidebar browser. The query stays
    /// in infrastructure so presentation never knows the SQLite schema.
    pub fn commentary_entries(&mut self, module_id: &ModuleId) -> Vec<CommentaryEntry> {
        let Some(db) = self.db_for(module_id) else {
            return Vec::new();
        };
        db.with(|conn| {
            let rows = conn.select::<(i64, i64, i64, Option<i64>, Option<i64>, String)>(
                "SELECT book_number, chapter_number_from, verse_number_from, \
                        chapter_number_to, verse_number_to, text \
                 FROM commentaries \
                 ORDER BY book_number, chapter_number_from, verse_number_from",
            )?()?;
            Ok(rows
                .into_iter()
                .map(
                    |(book, chapter_from, verse_from, chapter_to, verse_to, text)| {
                        CommentaryEntry {
                            book: BookNumber(book),
                            chapter_from: chapter_from as u16,
                            verse_from: verse_from as u16,
                            chapter_to: chapter_to.unwrap_or(chapter_from) as u16,
                            verse_to: verse_to.unwrap_or(verse_from) as u16,
                            text,
                        }
                    },
                )
                .collect())
        })
        .unwrap_or_else(|error| {
            log::warn!("commentary catalogue failed for {module_id}: {error:#}");
            Vec::new()
        })
    }

    /// Параллельные ссылки для стиха: (book_to, chapter_to, verse_start,
    /// verse_end, votes), отсортированы по votes (вес в MyBible).
    pub fn cross_references_for(
        &mut self,
        module_id: &ModuleId,
        book: BookNumber,
        chapter: u16,
        verse: u16,
    ) -> Vec<(i64, i64, i64, i64, i64)> {
        let Some(db) = self.db_for(module_id) else {
            return Vec::new();
        };
        db.with(|conn| {
            let mut stmt = conn.select_bound::<(i64, i64, i64), (i64, i64, i64, i64, i64)>(
                "SELECT book_to, chapter_to, verse_to_start, verse_to_end, votes \
                     FROM cross_references \
                     WHERE book = ? AND chapter = ? \
                       AND ? BETWEEN verse AND CASE WHEN verse_end > verse \
                           THEN verse_end ELSE verse END \
                     ORDER BY votes DESC",
            )?;
            stmt((book.0, chapter as i64, verse as i64))
        })
        .unwrap_or_else(|error| {
            log::warn!("cross references failed for {module_id}: {error:#}");
            Vec::new()
        })
    }

    /// Расшифровка морфологического кода через `morphology_indications`
    /// модуля (architecture.md §6.4). None — нет таблицы или модуль
    /// недоступен; неизвестный код возвращается как есть (Ok-ветка resolve).
    pub fn resolve_morphology(&mut self, module_id: &ModuleId, code: &str) -> Option<String> {
        let meta = self.modules.get(module_id)?.clone();
        if !meta.has_morphology_table {
            return None;
        }
        let db = self.db_for(module_id)?;
        match crate::morphology::resolve(&db, code, meta.morph_uses_applicable_to) {
            Ok(gloss) if !gloss.is_empty() => Some(gloss),
            Ok(_) => None,
            Err(error) => {
                log::warn!("morphology resolve failed for {module_id}: {error:#}");
                None
            }
        }
    }

    /// Стихи одной главы.
    pub fn chapter_of(
        &mut self,
        module_id: &ModuleId,
        book: BookNumber,
        chapter: u16,
    ) -> Result<Vec<RawVerse>> {
        let db = self
            .db_for(module_id)
            .ok_or_else(|| anyhow::anyhow!("module {module_id} is not available"))?;
        db.with(|conn| {
            let mut stmt = conn.select_bound::<(i64, i64), (i64, String)>(
                "SELECT verse, text FROM verses \
                 WHERE book_number = ? AND chapter = ? ORDER BY verse",
            )?;
            let rows = stmt((book.0, chapter as i64))?;
            Ok(rows
                .into_iter()
                .map(|(verse, text)| RawVerse {
                    verse: verse as u16,
                    text,
                })
                .collect())
        })
    }

    /// Текст диапазона стихов одной строкой (для hover-превью и цитирования).
    pub fn verse_range_text(
        &mut self,
        module_id: &ModuleId,
        reference: &VerseRef,
    ) -> Result<String> {
        let db = self
            .db_for(module_id)
            .ok_or_else(|| anyhow::anyhow!("module {module_id} is not available"))?;
        Self::read_verse_range(&db, module_id, reference)
    }

    fn read_verse_range(
        db: &ReadOnlyDb,
        module_id: &ModuleId,
        reference: &VerseRef,
    ) -> Result<String> {
        let from = i64::from(reference.verse);
        let to = reference.end_verse.map(i64::from).unwrap_or(from);
        let rows: Vec<String> = db.with(|conn| {
            let mut stmt = conn.select_bound::<(i64, i64, i64, i64), String>(
                "SELECT text FROM verses \
                 WHERE book_number = ? AND chapter = ? AND verse >= ? AND verse <= ? \
                 ORDER BY verse",
            )?;
            stmt((reference.book.0, reference.chapter as i64, from, to))
        })?;
        anyhow::ensure!(
            !rows.is_empty(),
            "no verses for {reference:?} in {module_id}"
        );
        Ok(rows.join(" "))
    }
}

/// Newtype-обёртка для совместного владения реестром (пул требует &mut).
/// Newtype нужен, чтобы реализовать порты Domain: orphan rules не дают
/// писать impl чужого трейта для type-alias над `Arc`.
#[derive(Clone)]
pub struct SharedRegistry(pub Arc<Mutex<ModuleRegistry>>);

impl SharedRegistry {
    pub fn new(registry: ModuleRegistry) -> Self {
        Self(Arc::new(Mutex::new(registry)))
    }

    pub fn lock(&self) -> parking_lot::MutexGuard<'_, ModuleRegistry> {
        self.0.lock()
    }

    /// Search the display text of dictionary entries without using the reader's
    /// connection or holding the registry lock. Return every matching topic.
    pub fn search_dictionary_topics(
        &self,
        module: &ModuleMeta,
        query: &str,
        include_definition: bool,
        cancelled: impl Fn() -> bool,
    ) -> Result<Vec<String>> {
        use sqlez::statement::Statement;

        let db = ReadOnlyDb::open(&module.path)?;
        let query = query.trim().to_lowercase();
        db.with(|connection| {
            let has_definition = include_definition
                && !query.is_empty()
                && connection
                    .select::<String>("SELECT name FROM pragma_table_info('dictionary')")?(
                )?
                .iter()
                .any(|name| name == "definition");
            let sql = if has_definition {
                "SELECT topic, definition FROM dictionary ORDER BY topic"
            } else {
                "SELECT topic, NULL FROM dictionary ORDER BY topic"
            };
            let mut stmt = Statement::prepare(connection, sql)?;
            let mut topics = Vec::new();
            while !cancelled() {
                let Some((topic, body)) = stmt.next_row::<(String, Option<String>)>()? else {
                    break;
                };
                if topic.to_lowercase().contains(&query)
                    || body.is_some_and(|body| {
                        crate::search_index::article_search_text(&body, &module.id)
                            .to_lowercase()
                            .contains(&query)
                    })
                {
                    topics.push(topic);
                }
            }
            Ok(topics)
        })
    }
}

/// Порты Domain реализуются для разделяемой обёртки: Application и UI
/// работают с `SharedRegistry` через трейты, не зная о пуле.
impl ModuleCatalog for SharedRegistry {
    fn modules(&self) -> Vec<ModuleMeta> {
        let registry = self.lock();
        let mut out: Vec<ModuleMeta> = registry
            .modules
            .values()
            .map(|meta| (**meta).clone())
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    fn module(&self, id: &ModuleId) -> Option<ModuleMeta> {
        self.lock().modules.get(id).map(|meta| (**meta).clone())
    }
}

impl VerseSource for SharedRegistry {
    fn chapter(&self, module: &ModuleId, book: BookNumber, chapter: u16) -> Result<Vec<RawVerse>> {
        self.lock().chapter_of(module, book, chapter)
    }

    fn verse_range(&self, module: &ModuleId, reference: &VerseRef) -> Result<String> {
        let db = self
            .lock()
            .db_for(module)
            .ok_or_else(|| anyhow::anyhow!("module {module} is not available"))?;
        ModuleRegistry::read_verse_range(&db, module, reference)
    }
}

fn scan_modules_dir(modules_dir: &Path) -> Result<Vec<PathBuf>> {
    let read_dir = std::fs::read_dir(modules_dir)
        .with_context(|| format!("reading {}", modules_dir.display()))?;
    let mut paths = Vec::new();
    for entry in read_dir {
        let entry = entry.context("reading directory entry")?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        match path
            .extension()
            .and_then(OsStr::to_str)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("sqlite3") | Some("sqlite") => paths.push(path),
            _ => {}
        }
    }
    paths.sort();
    Ok(paths)
}

/// Классификация модуля по набору таблиц и info (перенесено из форка).
fn classify_module(path: &Path) -> Result<ModuleMeta> {
    let id = path
        .file_stem()
        .and_then(OsStr::to_str)
        .map(str::to_string)
        .with_context(|| format!("module {} has no filename stem", path.display()))?;

    let db = ReadOnlyDb::open(path)?;
    db.with(|conn| {
        let info = read_info(conn)?;
        let tables = read_tables(conn)?;

        let has_dictionary = tables.iter().any(|name| name == "dictionary");
        let has_verses = tables.iter().any(|name| name == "verses");
        let has_commentaries = tables.iter().any(|name| name == "commentaries");
        let has_cross_references = tables.iter().any(|name| name == "cross_references");
        let has_morphology_table = tables.iter().any(|name| name == "morphology_indications");

        let is_strong = info
            .get("is_strong")
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let kind = if has_dictionary && is_strong {
            ModuleKind::StrongsDictionary
        } else if has_dictionary {
            ModuleKind::GeneralDictionary
        } else if has_verses {
            ModuleKind::BibleTranslation
        } else if has_commentaries {
            ModuleKind::Commentary
        } else if has_cross_references {
            ModuleKind::CrossReferences
        } else {
            ModuleKind::Unknown
        };

        let morph_uses_applicable_to = if has_morphology_table {
            morphology_has_applicable_to(conn)?
        } else {
            false
        };

        Ok::<ModuleMeta, anyhow::Error>(ModuleMeta {
            id: ModuleId(id),
            path: path.to_path_buf(),
            kind,
            language: info.get("language").cloned(),
            description: info.get("description").cloned(),
            strong_prefix: info.get("strong_numbers_prefix").cloned(),
            russian_numbering: info
                .get("russian_numbering")
                .map(|v| v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            has_morphology_table,
            morph_uses_applicable_to,
        })
    })
}

fn read_info(conn: &sqlez::connection::Connection) -> Result<HashMap<String, String>> {
    let rows = conn
        .select::<(String, String)>("SELECT name, value FROM info")
        .context("preparing SELECT FROM info")?()
    .context("running SELECT FROM info")?;
    Ok(rows.into_iter().collect())
}

fn read_tables(conn: &sqlez::connection::Connection) -> Result<Vec<String>> {
    let rows = conn
        .select::<String>("SELECT name FROM sqlite_master WHERE type='table'")
        .context("preparing SELECT FROM sqlite_master")?()
    .context("running SELECT FROM sqlite_master")?;
    Ok(rows)
}

fn morphology_has_applicable_to(conn: &sqlez::connection::Connection) -> Result<bool> {
    let columns = conn
        .select::<(i64, String, String, i64, Option<String>, i64)>(
            "PRAGMA table_info(morphology_indications)",
        )
        .context("preparing PRAGMA table_info")?()
    .context("running PRAGMA table_info")?;
    Ok(columns.iter().any(|column| column.1 == "applicable_to"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_dictionary_titles_and_optional_visible_definitions_without_a_cap() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("SL.sqlite3");
        let source = sqlez::connection::Connection::open_file(path.to_str().unwrap());
        source.exec("CREATE TABLE info(name TEXT, value TEXT)")?()?;
        source.exec("CREATE TABLE dictionary(topic TEXT, definition TEXT)")?()?;
        source.exec(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 610)
             INSERT INTO dictionary SELECT 'Номер ' || i, '<b>ЛОГОС</b>' FROM n",
        )?()?;
        source.exec(r#"INSERT INTO dictionary VALUES ('Логос', '<i title="Логос">Слово</i>')"#)?()?;
        drop(source);

        let registry = SharedRegistry::new(ModuleRegistry::scan(directory.path()));
        let module = registry.modules().pop().unwrap();
        assert_eq!(
            registry.search_dictionary_topics(&module, "логос", false, || false)?,
            ["Логос"]
        );
        assert_eq!(
            registry
                .search_dictionary_topics(&module, "логос", true, || false)?
                .len(),
            611
        );
        assert!(
            registry
                .search_dictionary_topics(&module, "title", true, || false)?
                .is_empty()
        );
        assert!(
            registry
                .search_dictionary_topics(&module, "", false, || true)?
                .is_empty()
        );
        Ok(())
    }

    /// Модуль-фикстура: verses + books — классифицируется как перевод,
    /// имя книги попадает в сводный индекс (merge_books_from).
    fn write_module(dir: &Path, name: &str, book_number: i64) {
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
            .exec(&format!(
                "INSERT INTO books VALUES({book_number}, 'Ин', 'Иоанн')"
            ))
            .unwrap()()
        .unwrap();
        drop(connection);
    }

    #[test]
    fn rescan_dir_switches_catalog_and_book_index() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        write_module(dir_a.path(), "AAA", 500);
        write_module(dir_b.path(), "BBB", 510);

        let mut registry = ModuleRegistry::scan(dir_a.path());
        assert!(registry.modules.contains_key(&ModuleId::from("AAA")));
        assert!(!registry.modules.contains_key(&ModuleId::from("BBB")));
        assert_eq!(registry.lookup_book("Ин"), Some(500));

        registry.rescan_dir(dir_b.path());
        assert!(!registry.modules.contains_key(&ModuleId::from("AAA")));
        assert!(registry.modules.contains_key(&ModuleId::from("BBB")));
        // Сводный индекс книг тоже перестроен — старый модуль не протекает.
        assert_eq!(registry.lookup_book("Ин"), Some(510));
        assert!(registry.book_entries().iter().all(|e| e.book_number == 510));

        // Повторный rescan_dir в тот же каталог — просто перескан.
        registry.rescan_dir(dir_b.path());
        assert!(registry.modules.contains_key(&ModuleId::from("BBB")));
    }

    /// Модуль-словарь: богатый — со всеми необязательными колонками,
    /// бедный — только topic/definition (обе схемы встречаются в MyBible).
    fn write_dictionary(dir: &Path, name: &str, rich: bool) {
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
                .exec(
                    "INSERT INTO dictionary VALUES(\
                       'G25', '<b>ἀγαπάω</b> — любить', 'ἀγαπάω', 'agapaō', \
                       'a-ga-PA-o', 'любить (волевой любовью)')",
                )
                .unwrap()()
            .unwrap();
        } else {
            connection
                .exec("CREATE TABLE dictionary(topic TEXT, definition TEXT)")
                .unwrap()()
            .unwrap();
            connection
                .exec("INSERT INTO dictionary VALUES('G25', 'любить')")
                .unwrap()()
            .unwrap();
        }
        drop(connection);
    }

    #[test]
    fn dictionary_entry_full_reads_optional_columns() {
        let dir = tempfile::tempdir().unwrap();
        write_dictionary(dir.path(), "RICH", true);
        write_dictionary(dir.path(), "POOR", false);

        let mut registry = ModuleRegistry::scan(dir.path());

        let rich = registry
            .dictionary_entry_full(&ModuleId::from("RICH.dictionary"), "G25")
            .expect("rich entry");
        assert_eq!(rich.lexeme.as_deref(), Some("ἀγαπάω"));
        assert_eq!(rich.transliteration.as_deref(), Some("agapaō"));
        assert_eq!(rich.pronunciation.as_deref(), Some("a-ga-PA-o"));
        assert_eq!(
            rich.short_definition.as_deref(),
            Some("любить (волевой любовью)")
        );
        assert!(rich.definition.contains("ἀγαπάω"));

        // Бедная схема: необязательные колонки приходят NULL, а не ошибкой.
        let poor = registry
            .dictionary_entry_full(&ModuleId::from("POOR.dictionary"), "G25")
            .expect("poor entry");
        assert_eq!(poor.definition, "любить");
        assert!(poor.lexeme.is_none());
        assert!(poor.transliteration.is_none());
        assert!(poor.short_definition.is_none());

        // Старый контракт (только definition) продолжает работать.
        assert_eq!(
            registry.dictionary_entry(&ModuleId::from("POOR.dictionary"), "G25"),
            Some("любить".to_string())
        );
        assert!(
            registry
                .dictionary_entry_full(&ModuleId::from("POOR.dictionary"), "H999")
                .is_none()
        );
    }

    /// Регрессия: `cross_references_for` падал «Failed to bind i64 at index 4»
    /// (в SQL три `?`, а привязывалось четыре значения) — ссылки не
    /// показывались вообще. Диапазонная строка (verse..verse_end) тоже
    /// должна покрывать стих внутри диапазона.
    #[test]
    fn cross_references_for_binds_and_filters_by_verse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TEST-x.crossreferences.sqlite3");
        let connection = sqlez::connection::Connection::open_file(path.to_str().unwrap());
        for statement in [
            "CREATE TABLE info(name TEXT, value TEXT)",
            "CREATE TABLE cross_references(\
                book INTEGER, chapter INTEGER, verse INTEGER, verse_end INTEGER, \
                book_to INTEGER, chapter_to INTEGER, verse_to_start INTEGER, \
                verse_to_end INTEGER, votes INTEGER)",
            // Стих 1:1 → Рим 4:1–5 (3 голоса) и Пс 2:7 (10 голосов).
            "INSERT INTO cross_references VALUES(10, 1, 1, 1, 520, 4, 1, 5, 3)",
            "INSERT INTO cross_references VALUES(10, 1, 1, 1, 230, 2, 7, 7, 10)",
            // Строка-диапазон 1:3–5 покрывает стих 4.
            "INSERT INTO cross_references VALUES(10, 1, 3, 5, 500, 1, 1, 1, 1)",
            // Чужая глава не попадает.
            "INSERT INTO cross_references VALUES(10, 2, 1, 1, 500, 9, 9, 9, 99)",
        ] {
            connection.exec(statement).unwrap()().unwrap();
        }
        drop(connection);

        let mut registry = ModuleRegistry::scan(dir.path());
        let module = ModuleId::from("TEST-x.crossreferences");

        let rows = registry.cross_references_for(&module, BookNumber(10), 1, 1);
        // Сортировка по votes DESC: сначала Пс 2:7.
        assert_eq!(rows, vec![(230, 2, 7, 7, 10), (520, 4, 1, 5, 3)]);

        let rows = registry.cross_references_for(&module, BookNumber(10), 1, 4);
        assert_eq!(rows, vec![(500, 1, 1, 1, 1)]);

        assert!(
            registry
                .cross_references_for(&module, BookNumber(10), 1, 2)
                .is_empty()
        );
    }
}
