//! FTS5-индекс поиска (architecture.md §4.1.4).
//!
//! Каждый модуль имеет собственную базу FTS5. Переводы индексируются по
//! одной строке на стих. Индексируемый текст —
//! clean_text из `bible_domain::verse::parse_verse_full`, плюс номера
//! Strong's токенами («G25», «H123») в конец строки: «любовь G26»
//! находится обычным MATCH без отдельного executor'а. Булева логика,
//! фразы и префиксы — силами самого FTS5 (см. bible_domain::search).
//!
//! Своя схема, свои миграции: индекс — производная данных модулей,
//! при несовпадении версии схемы перестраивается целиком.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use sqlez::connection::Connection;

use bible_domain::content::{ContentBlock, ContentInline};
use bible_domain::ids::{BookNumber, ModuleId};
use bible_domain::ports::{ModuleKind, ModuleMeta};
use bible_domain::verse;

use crate::db::ReadOnlyDb;

/// Версия схемы индекса. Несовпадение — полный сброс (индекс восстановим).
const SCHEMA_VERSION: i64 = 3;

/// Граница Ветхого/Нового завета в нумерации MyBible — единый источник
/// в домене (bible_domain::ids), здесь реэкспорт для удобства.
pub use bible_domain::ids::OLD_TESTAMENT_BOUNDARY;

/// Одна строка результата поиска.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRow {
    pub module: ModuleId,
    pub book: BookNumber,
    pub chapter: u16,
    pub verse: u16,
    /// Индексированный текст (clean_text + strong-токены).
    pub text: String,
}

/// Статус индекса модуля из мета-таблицы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexMeta {
    /// mtime файла модуля (unix-секунды) на момент построения.
    pub mtime: i64,
    pub built_at: i64,
}

/// Одна находка в словаре.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictionarySearchRow {
    pub module: ModuleId,
    pub topic: String,
    pub lexeme: Option<String>,
    pub snippet: String,
}

/// Одна находка в комментарии к месту Писания.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentarySearchRow {
    pub module: ModuleId,
    pub book: BookNumber,
    pub chapter_from: u16,
    pub verse_from: u16,
    pub chapter_to: Option<u16>,
    pub verse_to: Option<u16>,
    pub snippet: String,
}

pub struct SearchIndex {
    directory: PathBuf,
    failures: Mutex<HashMap<PathBuf, String>>,
    #[cfg(windows)]
    published: parking_lot::RwLock<()>,
}

impl SearchIndex {
    pub fn open(legacy_path: &Path) -> Result<Arc<Self>> {
        let directory = legacy_path.with_extension("modules");
        std::fs::create_dir_all(&directory)?;
        Ok(Arc::new(Self {
            directory,
            failures: Mutex::new(HashMap::new()),
            #[cfg(windows)]
            published: parking_lot::RwLock::new(()),
        }))
    }

    fn path(&self, module: &ModuleId) -> PathBuf {
        // A fixed-size name works for long module IDs and case-insensitive filesystems.
        let name: String = Sha256::digest(module.0.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        self.directory.join(format!("{name}.sqlite"))
    }

    fn signature(meta: &ModuleMeta) -> Result<(String, i64)> {
        let file = std::fs::metadata(&meta.path)?;
        let modified = file.modified()?;
        let seconds = modified
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs() as i64);
        Ok((
            format!(
                "{SCHEMA_VERSION}:{:?}:{modified:?}:{}",
                meta.path,
                file.len()
            ),
            seconds,
        ))
    }

    fn open_module(&self, module: &ModuleId) -> Result<Option<IndexDb>> {
        let path = self.path(module);
        if !path.try_exists()? {
            return Ok(None);
        }
        let connection =
            Connection::open_file_readonly(path.to_str().context("invalid index path")?)?;
        Ok(Some(IndexDb {
            inner: Mutex::new(connection),
        }))
    }

    pub fn needs_refresh(&self, meta: &ModuleMeta) -> bool {
        let Ok((signature, _)) = Self::signature(meta) else {
            return false;
        };
        if self.failures.lock().get(&meta.path) == Some(&signature)
            || std::fs::read_to_string(self.path(&meta.id).with_extension("failed"))
                .is_ok_and(|failed| failed == signature)
        {
            return false;
        }
        #[cfg(windows)]
        let _published = self.published.read();
        let stored = (|| -> Result<Option<(String, String)>> {
            let Some(db) = self.open_module(&meta.id)? else {
                return Ok(None);
            };
            let conn = db.inner.lock();
            conn.select_row(
                "SELECT a.value, b.value FROM meta a, meta b \
                 WHERE a.key = 'source_signature' AND b.key = 'schema_version'",
            )?()
        })();
        !matches!(stored, Ok(Some((source, version)))
            if source == signature && version == SCHEMA_VERSION.to_string())
    }

    /// Build in a private file, then publish the complete database in one move.
    /// Each query sees a complete snapshot.
    pub fn refresh(&self, meta: &ModuleMeta) -> Result<()> {
        self.refresh_inner(meta, false)
    }

    /// Rebuild an index even when the source signature has not changed.
    /// Used only by the explicit "Reindex modules" user action.
    pub fn force_refresh(&self, meta: &ModuleMeta) -> Result<()> {
        self.refresh_inner(meta, true)
    }

    fn refresh_inner(&self, meta: &ModuleMeta, force: bool) -> Result<()> {
        if !force && !self.needs_refresh(meta) {
            return Ok(());
        }
        let (signature, mtime) = Self::signature(meta)?;
        let result = (|| -> Result<()> {
            let temp = tempfile::NamedTempFile::new_in(&self.directory)?.into_temp_path();
            let connection =
                Connection::open_file(temp.to_str().context("invalid temporary index path")?);
            if !connection.persistent() {
                bail!("cannot open temporary index file");
            }
            let db = IndexDb::init(connection)?;
            let result = (|| -> Result<usize> {
                let source = ReadOnlyDb::open(&meta.path)?;
                Ok(match meta.kind {
                    ModuleKind::BibleTranslation => {
                        db.rebuild_module(meta, &source, mtime, |_, _| {})?
                    }
                    ModuleKind::StrongsDictionary | ModuleKind::GeneralDictionary => {
                        db.rebuild_dictionary_module(meta, &source, mtime)?
                    }
                    ModuleKind::Commentary => db.rebuild_commentary_module(meta, &source, mtime)?,
                    _ => 0,
                })
            })();
            if let Err(error) = result {
                let marker = self.path(&meta.id).with_extension("failed");
                if let Err(marker_error) = std::fs::write(marker, &signature) {
                    log::debug!(
                        "could not cache index failure for {}: {marker_error}",
                        meta.id
                    );
                }
                return Err(error);
            }
            db.inner
                .lock()
                .exec_bound("INSERT INTO meta VALUES ('source_signature', ?)")?(
                signature.as_str(),
            )?;
            drop(db);
            if Self::signature(meta)?.0 != signature {
                bail!("module {} changed during indexing", meta.id);
            }
            // On Windows, coordinate replacement with SQLite file handles.
            #[cfg(windows)]
            let _published = self.published.write();
            temp.persist(self.path(&meta.id))?;
            Ok(())
        })();
        if result.is_err() {
            self.failures.lock().insert(meta.path.clone(), signature);
        } else {
            self.failures.lock().remove(&meta.path);
        }
        result
    }

    pub fn module_meta(&self, module: &ModuleId) -> Result<Option<IndexMeta>> {
        #[cfg(windows)]
        let _published = self.published.read();
        match self.open_module(module)? {
            Some(db) => db.module_meta(module),
            None => Ok(None),
        }
    }

    fn search_ready<T>(
        &self,
        meta: &ModuleMeta,
        search: impl FnOnce(&IndexDb) -> Result<Vec<T>>,
    ) -> Result<Vec<T>> {
        let Ok((signature, _)) = Self::signature(meta) else {
            return Ok(Vec::new());
        };
        #[cfg(windows)]
        let _published = self.published.read();
        let Some(db) = self.open_module(&meta.id)? else {
            return Ok(Vec::new());
        };
        let stored = db
            .inner
            .lock()
            .select_row::<String>("SELECT value FROM meta WHERE key = 'source_signature'")
            .and_then(|mut read| read());
        if !matches!(stored, Ok(Some(stored)) if stored == signature) {
            return Ok(Vec::new());
        }
        search(&db)
    }

    pub fn search_module(
        &self,
        meta: &ModuleMeta,
        query: &str,
        range: Option<(i64, i64)>,
    ) -> Result<Vec<SearchRow>> {
        self.search_ready(meta, |db| db.search(query, None, range, usize::MAX))
    }

    pub fn search_dictionary_module(
        &self,
        meta: &ModuleMeta,
        query: &str,
    ) -> Result<Vec<DictionarySearchRow>> {
        self.search_ready(meta, |db| {
            db.search_dictionary_module(&meta.id, query, usize::MAX)
        })
    }

    pub fn search_commentary_module(
        &self,
        meta: &ModuleMeta,
        query: &str,
    ) -> Result<Vec<CommentarySearchRow>> {
        self.search_ready(meta, |db| {
            db.search_commentary_module(&meta.id, query, usize::MAX)
        })
    }
}

struct IndexDb {
    inner: Mutex<Connection>,
}

impl IndexDb {
    /// In-memory индекс для тестов.
    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_memory(None))
    }

    fn init(connection: Connection) -> Result<Self> {
        connection.exec(
            "CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT",
        )?()?;
        connection.exec(
            "CREATE TABLE IF NOT EXISTS index_meta(\
                module_id TEXT PRIMARY KEY, mtime INTEGER NOT NULL, built_at INTEGER NOT NULL\
            ) STRICT",
        )?()?;
        let version: Option<String> =
            connection.select_row("SELECT value FROM meta WHERE key = 'schema_version'")?()?;
        let version: i64 = version.as_deref().unwrap_or("0").parse().unwrap_or(0);
        if version > SCHEMA_VERSION {
            bail!(
                "index.db schema_version {version} новее поддерживаемой {SCHEMA_VERSION} — \
                 обновите приложение"
            );
        }
        if version < SCHEMA_VERSION {
            // Индекс — производная данных: при смене схемы перестраиваем с нуля.
            connection.exec("DROP TABLE IF EXISTS verses_fts")?()?;
            connection.exec("DROP TABLE IF EXISTS dictionary_fts")?()?;
            connection.exec("DROP TABLE IF EXISTS commentary_fts")?()?;
            connection.exec("DROP TABLE IF EXISTS aux_index_meta")?()?;
            connection.exec("DELETE FROM index_meta")?()?;
        }
        connection.exec(
            "CREATE VIRTUAL TABLE IF NOT EXISTS verses_fts USING fts5(\
                module_id UNINDEXED, book UNINDEXED, chapter UNINDEXED, verse UNINDEXED, \
                text, tokenize='unicode61 remove_diacritics 2')",
        )?()?;
        connection.exec_bound(
            "INSERT INTO meta(key, value) VALUES ('schema_version', ?) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )?(SCHEMA_VERSION.to_string())?;
        connection.exec(
            "CREATE VIRTUAL TABLE IF NOT EXISTS dictionary_fts USING fts5(\
                module_id UNINDEXED, topic, lexeme, definition_text, \
                tokenize='unicode61 remove_diacritics 2', columnsize=0)",
        )?()?;
        connection.exec(
            "CREATE VIRTUAL TABLE IF NOT EXISTS commentary_fts USING fts5(\
                module_id UNINDEXED, book UNINDEXED, chapter_from UNINDEXED, \
                verse_from UNINDEXED, chapter_to UNINDEXED, verse_to UNINDEXED, text, \
                tokenize='unicode61 remove_diacritics 2', columnsize=0)",
        )?()?;
        connection.exec(
            "CREATE TABLE IF NOT EXISTS aux_index_meta(\
                module_id TEXT NOT NULL, kind TEXT NOT NULL, mtime INTEGER NOT NULL, \
                built_at INTEGER NOT NULL, PRIMARY KEY(module_id, kind)) STRICT",
        )?()?;
        Ok(Self {
            inner: Mutex::new(connection),
        })
    }

    /// Метаданные индекса модуля (None — модуль не индексировался).
    pub fn module_meta(&self, module: &ModuleId) -> Result<Option<IndexMeta>> {
        let connection = self.inner.lock();
        let row = connection.select_row_bound::<&str, (i64, i64)>(
            "SELECT mtime, built_at FROM index_meta WHERE module_id = ?",
        )?(module.0.as_str())?;
        Ok(row.map(|(mtime, built_at)| IndexMeta { mtime, built_at }))
    }

    /// Перестроить индекс модуля целиком (DELETE + INSERT + meta
    /// одним savepoint'ом). `mtime` — mtime файла модуля, секунды.
    /// `progress(done, total)` зовётся после каждой книги.
    /// Возвращает число проиндексированных стихов.
    pub fn rebuild_module(
        &self,
        meta: &ModuleMeta,
        db: &ReadOnlyDb,
        mtime: i64,
        progress: impl Fn(usize, usize),
    ) -> Result<usize> {
        let verses = db.with(|connection| {
            connection.select::<(i64, i64, i64, String)>(
                "SELECT book_number, chapter, verse, text FROM verses \
                 ORDER BY book_number, chapter, verse",
            )?()
        })?;
        if verses.is_empty() {
            bail!("module {} has no verses", meta.id);
        }

        let connection = self.inner.lock();
        let module_id = meta.id.to_string();
        let mut indexed = 0usize;
        connection.with_savepoint("rebuild_module", || {
            connection.exec_bound("DELETE FROM verses_fts WHERE module_id = ?")?(
                module_id.as_str(),
            )?;
            let mut insert = connection.exec_bound::<(&str, i64, i64, i64, &str)>(
                "INSERT INTO verses_fts(module_id, book, chapter, verse, text) \
                 VALUES (?, ?, ?, ?, ?)",
            )?;
            let total = verses.len();
            let mut last_book = verses.first().map(|(book, ..)| *book).unwrap_or(0);
            for (book, chapter, verse_no, raw) in &verses {
                let clean = verse::parse_verse_full(raw, false, false, false);
                let text = index_text(&clean, meta.strong_prefix.as_deref(), *book);
                insert((
                    module_id.as_str(),
                    *book,
                    *chapter,
                    *verse_no,
                    text.as_str(),
                ))?;
                indexed += 1;
                if *book != last_book {
                    last_book = *book;
                    progress(indexed, total);
                }
            }
            progress(indexed, total);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs() as i64)
                .unwrap_or(0);
            connection.exec_bound(
                "INSERT INTO index_meta(module_id, mtime, built_at) VALUES (?, ?, ?) \
                 ON CONFLICT(module_id) DO UPDATE SET mtime = excluded.mtime, \
                     built_at = excluded.built_at",
            )?((module_id.as_str(), mtime, now))?;
            Ok(())
        })?;
        Ok(indexed)
    }

    /// Построить/обновить полнотекстовый индекс словаря.
    pub fn rebuild_dictionary_module(
        &self,
        meta: &ModuleMeta,
        db: &ReadOnlyDb,
        mtime: i64,
    ) -> Result<usize> {
        if !matches!(
            meta.kind,
            ModuleKind::StrongsDictionary | ModuleKind::GeneralDictionary
        ) {
            bail!("module {} is not a dictionary", meta.id);
        }

        let columns = db.with(|source| {
            let names =
                source.select::<String>("SELECT name FROM pragma_table_info('dictionary')")?()?;
            Ok((
                names.iter().any(|name| name == "lexeme"),
                names.iter().any(|name| name == "definition"),
            ))
        })?;
        let lexeme_col = if columns.0 {
            "lexeme"
        } else {
            "NULL AS lexeme"
        };
        let definition_col = if columns.1 {
            "definition"
        } else {
            "NULL AS definition"
        };
        let sql =
            format!("SELECT topic, {lexeme_col}, {definition_col} FROM dictionary ORDER BY topic");
        let rows = db.with(|source| {
            let mut select = source.select::<(String, Option<String>, Option<String>)>(&sql)?;
            select()
        })?;

        let module_id = meta.id.to_string();
        let connection = self.inner.lock();
        let mut indexed = 0usize;
        connection.with_savepoint("rebuild_dictionary_module", || {
            connection.exec_bound("DELETE FROM dictionary_fts WHERE module_id = ?")?(
                module_id.as_str(),
            )?;
            let mut insert = connection.exec_bound::<(&str, &str, Option<&str>, &str)>(
                "INSERT INTO dictionary_fts(module_id, topic, lexeme, definition_text) \
                 VALUES (?, ?, ?, ?)",
            )?;
            for (topic, lexeme, definition) in &rows {
                let plain =
                    article_search_text(definition.as_deref().unwrap_or_default(), &meta.id);
                let lexeme = lexeme.as_deref().filter(|value| !value.trim().is_empty());
                insert((module_id.as_str(), topic.as_str(), lexeme, plain.as_str()))?;
                indexed += 1;
            }
            record_aux_index(&connection, &module_id, "dictionary", mtime)?;
            Ok(())
        })?;
        Ok(indexed)
    }

    /// Построить/обновить полнотекстовый индекс комментариев.
    pub fn rebuild_commentary_module(
        &self,
        meta: &ModuleMeta,
        db: &ReadOnlyDb,
        mtime: i64,
    ) -> Result<usize> {
        if meta.kind != ModuleKind::Commentary {
            bail!("module {} is not a commentary", meta.id);
        }
        let rows = db.with(|source| {
            let mut select = source
                .select::<(i64, i64, i64, Option<i64>, Option<i64>, Option<String>)>(
                    "SELECT book_number, chapter_number_from, verse_number_from, \
                        chapter_number_to, verse_number_to, text FROM commentaries",
                )?;
            select()
        })?;

        let module_id = meta.id.to_string();
        let connection = self.inner.lock();
        let mut indexed = 0usize;
        connection.with_savepoint("rebuild_commentary_module", || {
            connection.exec_bound("DELETE FROM commentary_fts WHERE module_id = ?")?(
                module_id.as_str(),
            )?;
            let mut insert = connection
                .exec_bound::<(&str, i64, i64, i64, Option<i64>, Option<i64>, &str)>(
                    "INSERT INTO commentary_fts(\
                    module_id, book, chapter_from, verse_from, chapter_to, verse_to, text\
                 ) VALUES (?, ?, ?, ?, ?, ?, ?)",
                )?;
            for (book, chapter_from, verse_from, chapter_to, verse_to, raw) in &rows {
                let text = article_search_text(raw.as_deref().unwrap_or_default(), &meta.id);
                if text.trim().is_empty() {
                    continue;
                }
                insert((
                    module_id.as_str(),
                    *book,
                    *chapter_from,
                    *verse_from,
                    *chapter_to,
                    *verse_to,
                    text.as_str(),
                ))?;
                indexed += 1;
            }
            record_aux_index(&connection, &module_id, "commentary", mtime)?;
            Ok(())
        })?;
        Ok(indexed)
    }

    /// Поиск по одному модулю словаря.
    pub fn search_dictionary_module(
        &self,
        module: &ModuleId,
        match_expr: &str,
        limit: usize,
    ) -> Result<Vec<DictionarySearchRow>> {
        let connection = self.inner.lock();
        let rows = connection
            .select_bound::<(&str, &str, i64), (String, Option<String>, Option<String>)>(
                "SELECT topic, lexeme, snippet(dictionary_fts, 3, '', '', '…', 48) \
             FROM dictionary_fts WHERE dictionary_fts MATCH ? AND module_id = ? \
             ORDER BY rank LIMIT ?",
            )?((match_expr, module.0.as_str(), limit as i64))?;
        Ok(rows
            .into_iter()
            .map(|(topic, lexeme, snippet)| DictionarySearchRow {
                module: module.clone(),
                topic,
                lexeme: lexeme.filter(|value| !value.trim().is_empty()),
                snippet: snippet.unwrap_or_default(),
            })
            .collect())
    }

    /// Поиск по одному модулю комментариев.
    pub fn search_commentary_module(
        &self,
        module: &ModuleId,
        match_expr: &str,
        limit: usize,
    ) -> Result<Vec<CommentarySearchRow>> {
        let connection = self.inner.lock();
        let rows = connection.select_bound::<(&str, &str, i64), (
            i64, i64, i64, Option<i64>, Option<i64>, Option<String>
        )>(
            "SELECT book, chapter_from, verse_from, chapter_to, verse_to, \
                    snippet(commentary_fts, 6, '', '', '…', 48) \
             FROM commentary_fts WHERE commentary_fts MATCH ? AND module_id = ? \
             ORDER BY rank LIMIT ?",
        )?((match_expr, module.0.as_str(), limit as i64))?;
        Ok(rows
            .into_iter()
            .map(
                |(book, chapter_from, verse_from, chapter_to, verse_to, snippet)| {
                    CommentarySearchRow {
                        module: module.clone(),
                        book: BookNumber(book),
                        chapter_from: chapter_from as u16,
                        verse_from: verse_from as u16,
                        chapter_to: chapter_to.map(|value| value as u16),
                        verse_to: verse_to.map(|value| value as u16),
                        snippet: snippet.unwrap_or_default(),
                    }
                },
            )
            .collect())
    }

    /// Удалить индекс модуля (модуль пропал из каталога модулей).
    #[cfg(test)]
    pub fn drop_module(&self, module: &ModuleId) -> Result<()> {
        let connection = self.inner.lock();
        connection.with_savepoint("drop_module", || {
            connection.exec_bound("DELETE FROM verses_fts WHERE module_id = ?")?(
                module.0.as_str(),
            )?;
            connection.exec_bound("DELETE FROM index_meta WHERE module_id = ?")?(
                module.0.as_str(),
            )?;
            connection.exec_bound("DELETE FROM dictionary_fts WHERE module_id = ?")?(
                module.0.as_str(),
            )?;
            connection.exec_bound("DELETE FROM commentary_fts WHERE module_id = ?")?(
                module.0.as_str(),
            )?;
            connection.exec_bound("DELETE FROM aux_index_meta WHERE module_id = ?")?(
                module.0.as_str(),
            )?;
            Ok(())
        })
    }

    /// Поиск по индексу: FTS5 MATCH + фильтры.
    ///
    /// - `match_expr` — готовое FTS5-выражение (строит bible_domain::search);
    /// - `modules` — None = все проиндексированные переводы;
    /// - `book_range` — None = весь канон, Some((min, max)) — включительно
    ///   (корпуса ВЗ/НЗ задаются границей OLD_TESTAMENT_BOUNDARY);
    /// - результат — в каноническом порядке (book, chapter, verse), LIMIT.
    pub fn search(
        &self,
        match_expr: &str,
        modules: Option<&[String]>,
        book_range: Option<(i64, i64)>,
        limit: usize,
    ) -> Result<Vec<SearchRow>> {
        let mut sql = "SELECT module_id, book, chapter, verse, text FROM verses_fts \
                       WHERE verses_fts MATCH ?"
            .to_string();
        if let Some(modules) = modules {
            let list: Vec<String> = modules
                .iter()
                .filter(|id| is_safe_module_id(id))
                .map(|id| format!("'{id}'"))
                .collect();
            if list.is_empty() {
                return Ok(Vec::new());
            }
            sql.push_str(&format!(" AND module_id IN ({})", list.join(", ")));
        }
        if let Some((min, max)) = book_range {
            sql.push_str(&format!(" AND book BETWEEN {min} AND {max}"));
        }
        sql.push_str(" ORDER BY book, chapter, verse LIMIT ?");

        let connection = self.inner.lock();
        let rows =
            connection.select_bound::<(&str, i64), (String, i64, i64, i64, String)>(&sql)?((
                match_expr,
                limit as i64,
            ))?;
        Ok(rows
            .into_iter()
            .map(|(module, book, chapter, verse, text)| SearchRow {
                module: ModuleId(module),
                book: BookNumber(book),
                chapter: chapter as u16,
                verse: verse as u16,
                text,
            })
            .collect())
    }
}

fn record_aux_index(
    connection: &Connection,
    module_id: &str,
    kind: &str,
    mtime: i64,
) -> Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    connection.exec_bound(
        "INSERT INTO aux_index_meta(module_id, kind, mtime, built_at) VALUES (?, ?, ?, ?) \
         ON CONFLICT(module_id, kind) DO UPDATE SET \
            mtime = excluded.mtime, built_at = excluded.built_at",
    )?((module_id, kind, mtime, now))?;
    Ok(())
}

/// Превращает HTML-статью MyBible в поисковый текст через тот же
/// структурный парсер, который используется при отображении статей.
/// Сохраняем заголовки, текст абзацев, списки, таблицы, подписи ссылок и
/// Strong-номера; теги и атрибуты в индекс не попадают.
pub(crate) fn article_search_text(raw: &str, module: &ModuleId) -> String {
    fn append_inline(inlines: &[ContentInline], out: &mut Vec<String>) {
        for inline in inlines {
            match inline {
                ContentInline::Text { text, .. } => out.push(text.clone()),
                ContentInline::Link { label, .. } => out.push(label.clone()),
                ContentInline::StrongRef(number) => out.push(number.topic()),
                ContentInline::Break => out.push(" ".to_string()),
            }
        }
    }

    let blocks = crate::html::parse_article(raw, module);
    let mut pieces = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Paragraph(inlines)
            | ContentBlock::Heading {
                content: inlines, ..
            }
            | ContentBlock::Quote(inlines) => append_inline(&inlines, &mut pieces),
            ContentBlock::List { items, .. } => {
                for item in items {
                    append_inline(&item, &mut pieces);
                }
            }
            ContentBlock::Table { head, rows } => {
                pieces.extend(head);
                for row in rows {
                    pieces.extend(row);
                }
            }
            ContentBlock::Rule => {}
        }
    }

    pieces
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Индексируемый текст стиха: чистый текст + Strong's-токены.
fn index_text(clean: &verse::CleanVerse, strong_prefix: Option<&str>, book: i64) -> String {
    let mut text = clean.clean_text.clone();
    let mut first = true;
    for token in &clean.tokens {
        let Some(number) = &token.strong else {
            continue;
        };
        if first {
            text.push(' ');
            first = false;
        } else {
            text.push(' ');
        }
        let prefix = strong_prefix.unwrap_or(if book < OLD_TESTAMENT_BOUNDARY {
            "H"
        } else {
            "G"
        });
        text.push_str(prefix);
        text.push_str(number);
    }
    text
}

/// Идентификатор модуля в IN-список подставляется текстом — белый список
/// символов против инъекции (stem файла: буквы, цифры, '_', '-', '.').
fn is_safe_module_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bible_domain::ports::ModuleKind;

    fn module_meta(id: &str) -> ModuleMeta {
        ModuleMeta {
            id: ModuleId::from(id),
            path: std::path::PathBuf::from(format!("{id}.sqlite3")),
            kind: ModuleKind::BibleTranslation,
            language: None,
            description: None,
            strong_prefix: None,
            russian_numbering: true,
            has_morphology_table: false,
            morph_uses_applicable_to: false,
        }
    }

    /// Модуль-фикстура: одна книга ВЗ (Быт 10) и одна НЗ (Иоан 500+).
    fn fixture_db() -> ReadOnlyDb {
        let connection = Connection::open_memory(None);
        connection
            .exec("CREATE TABLE verses(book_number INTEGER, chapter INTEGER, verse INTEGER, text TEXT)")
            .unwrap()()
            .unwrap();
        let mut insert = connection
            .exec_bound::<(i64, i64, i64, &str)>("INSERT INTO verses VALUES (?, ?, ?, ?)")
            .unwrap();
        insert((10, 1, 1, "В начале сотворил Бог небо и землю.")).unwrap();
        insert((10, 1, 2, "И сказал Бог: да будет свет. И стал свет.")).unwrap();
        insert((
            500,
            3,
            16,
            "Ибо так возлюбил<S>G25</S> Бог мир, что отдал<S>G1325</S> Сына Своего Единственного.",
        ))
        .unwrap();
        insert((
            500,
            3,
            17,
            "Ибо не послал Бог Сына Своего в мир судить мир.",
        ))
        .unwrap();
        drop(insert);
        ReadOnlyDb::from_connection_for_tests(connection)
    }

    #[test]
    fn rebuild_and_search() {
        let index = IndexDb::in_memory().unwrap();
        let db = fixture_db();
        let indexed = index
            .rebuild_module(&module_meta("TST"), &db, 100, |_, _| {})
            .unwrap();
        assert_eq!(indexed, 4);

        let meta = index.module_meta(&ModuleId::from("TST")).unwrap().unwrap();
        assert_eq!(meta.mtime, 100);

        let hits = index.search("свет", None, None, 100).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].chapter, hits[0].verse), (1, 2));

        // Несколько слов — неявное AND (поведение FTS5).
        let hits = index.search("судить мир", None, None, 100).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].verse, 17);
    }

    #[test]
    fn published_index_retries_changed_sources_and_has_no_result_cap() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let index_path = dir.path().join("index.db");
        let index = SearchIndex::open(&index_path).unwrap();
        let mut meta = module_meta("Библия");
        meta.path = dir.path().join("source.sqlite3");

        let source = Connection::open_file(meta.path.to_str().unwrap());
        source.exec("CREATE TABLE verses(book_number, chapter, verse, text)")?()?;
        assert!(index.refresh(&meta).is_err());
        let reopened = SearchIndex::open(&index_path).unwrap();
        assert!(!reopened.needs_refresh(&meta));

        source.exec(
            "WITH RECURSIVE numbers(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM numbers WHERE n < 650) \
             INSERT INTO verses SELECT 10, 1, n, 'мир' FROM numbers",
        )?()?;
        drop(source);

        assert!(reopened.needs_refresh(&meta));
        assert!(reopened.search_module(&meta, "мир", None)?.is_empty());
        reopened.refresh(&meta)?;
        assert_eq!(reopened.search_module(&meta, "мир", None)?.len(), 650);
        assert!(!reopened.needs_refresh(&meta));
        Ok(())
    }

    #[test]
    fn strong_tokens_are_searchable() {
        let index = IndexDb::in_memory().unwrap();
        let db = fixture_db();
        index
            .rebuild_module(&module_meta("TST"), &db, 100, |_, _| {})
            .unwrap();
        // Книга НЗ — префикс G по умолчанию.
        let hits = index.search("G1325", None, None, 100).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].verse, 16);
    }

    #[test]
    fn filters_by_module_and_testament() {
        let index = IndexDb::in_memory().unwrap();
        let db = fixture_db();
        index
            .rebuild_module(&module_meta("AAA"), &db, 100, |_, _| {})
            .unwrap();
        index
            .rebuild_module(&module_meta("BBB"), &db, 100, |_, _| {})
            .unwrap();

        let hits = index
            .search("мир", Some(&["AAA".to_string()]), None, 100)
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|hit| hit.module.0 == "AAA"));

        // Только Новый Завет: 2 стиха в каждом из двух модулей.
        let hits = index
            .search("мир", None, Some((OLD_TESTAMENT_BOUNDARY, 1000)), 100)
            .unwrap();
        assert_eq!(hits.len(), 4);
        assert!(hits.iter().all(|hit| hit.book.0 >= OLD_TESTAMENT_BOUNDARY));
        // Ветхий Завет — «мир» в фикстуре не встречается.
        let hits = index
            .search("мир", None, Some((0, OLD_TESTAMENT_BOUNDARY - 1)), 100)
            .unwrap();
        assert!(hits.is_empty());

        // Небезопасный id отфильтровывается → пустой IN → нет хитов.
        let hits = index
            .search(
                "мир",
                Some(&["'; DROP TABLE verses_fts;--".to_string()]),
                None,
                100,
            )
            .unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn drop_module_removes_hits() {
        let index = IndexDb::in_memory().unwrap();
        let db = fixture_db();
        index
            .rebuild_module(&module_meta("TST"), &db, 100, |_, _| {})
            .unwrap();
        index.drop_module(&ModuleId::from("TST")).unwrap();
        assert!(index.search("мир", None, None, 100).unwrap().is_empty());
        assert!(index.module_meta(&ModuleId::from("TST")).unwrap().is_none());
    }

    #[test]
    fn rebuild_replaces_previous_index() {
        let index = IndexDb::in_memory().unwrap();
        let db = fixture_db();
        index
            .rebuild_module(&module_meta("TST"), &db, 100, |_, _| {})
            .unwrap();
        index
            .rebuild_module(&module_meta("TST"), &db, 200, |_, _| {})
            .unwrap();
        // Дублей нет: 4 стиха, свежий mtime.
        let hits = index.search("Бог", None, None, 100).unwrap();
        assert_eq!(hits.len(), 4);
        assert_eq!(
            index
                .module_meta(&ModuleId::from("TST"))
                .unwrap()
                .unwrap()
                .mtime,
            200
        );
    }

    #[test]
    fn dictionary_article_index_text_uses_structured_article_parser() {
        let module = ModuleId::from("CBTEL");
        let text = article_search_text(
            "<h2>Любовь</h2><p><b>Бог</b> так возлюбил мир &mdash; <a href=\"B:500 3:16\">Ин 3:16</a>; <sup>G25</sup></p><ul><li>милость</li><li>истина</li></ul>",
            &module,
        );
        assert!(text.contains("Любовь"));
        assert!(text.contains("Бог так возлюбил мир — Ин 3:16"));
        assert!(text.contains("G25"));
        assert!(text.contains("милость истина"));
        assert!(!text.contains("<p>"));
    }
}
