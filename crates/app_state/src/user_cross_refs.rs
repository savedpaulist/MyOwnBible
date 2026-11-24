use std::{path::Path, sync::Arc};

use anyhow::{Context as _, Result};
use bible_domain::ids::{BookNumber, VerseRef};
use parking_lot::Mutex;
use sqlez::connection::Connection;

pub struct UserCrossRefs(Mutex<Connection>);

impl UserCrossRefs {
    pub fn open(path: &Path) -> Result<Arc<Self>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open_file(path.to_str().context("Некорректный путь к модулю")?);
        anyhow::ensure!(
            conn.persistent(),
            "Не удалось открыть файл параллельных мест."
        );
        // MyBible schema, so the database can also be opened as a cross-reference module.
        conn.exec("CREATE TABLE IF NOT EXISTS info (name TEXT PRIMARY KEY, value TEXT)")?()?;
        conn.exec("INSERT OR IGNORE INTO info(name, value) VALUES ('description', 'Мои параллельные места')")?()?;
        conn.exec(
            "CREATE TABLE IF NOT EXISTS cross_references (
                book INTEGER NOT NULL, chapter INTEGER NOT NULL,
                verse INTEGER NOT NULL, verse_end INTEGER NOT NULL DEFAULT 0,
                book_to INTEGER NOT NULL, chapter_to INTEGER NOT NULL,
                verse_to_start INTEGER NOT NULL, verse_to_end INTEGER NOT NULL DEFAULT 0,
                votes INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(book, chapter, verse, verse_end, book_to, chapter_to, verse_to_start, verse_to_end)
            )",
        )?()?;
        Ok(Arc::new(Self(Mutex::new(conn))))
    }

    pub fn sync_records(&self) -> Result<crate::sync_data::Records> {
        crate::sync_data::cross_refs_snapshot(&self.0.lock())
    }

    pub fn sync_replace(
        &self,
        key: &crate::sync_data::Key,
        expected: Option<&serde_json::Value>,
        target: Option<&serde_json::Value>,
    ) -> Result<bool> {
        crate::sync_data::cross_refs_replace(&self.0.lock(), key, expected, target)
    }

    pub fn for_verse(&self, source: VerseRef) -> Result<Vec<VerseRef>> {
        let conn = self.0.lock();
        let rows: Vec<(i64, i64, i64, i64)> = conn.select_bound(
            "SELECT book_to, chapter_to, verse_to_start, verse_to_end FROM cross_references
             WHERE book = ? AND chapter = ? AND ? BETWEEN verse AND MAX(verse_end, verse)
             ORDER BY rowid",
        )?((
            source.book.0,
            i64::from(source.chapter),
            i64::from(source.verse),
        ))?;
        Ok(rows
            .into_iter()
            .map(|(book, chapter, verse, end)| VerseRef {
                book: BookNumber(book),
                chapter: chapter as u16,
                verse: verse as u16,
                end_verse: (end > verse).then_some(end as u16),
            })
            .collect())
    }

    pub fn add(&self, source: VerseRef, target: VerseRef) -> Result<()> {
        anyhow::ensure!(source != target, "Выберите другой стих.");
        let conn = self.0.lock();
        conn.exec_bound(
            "INSERT OR IGNORE INTO cross_references
             (book, chapter, verse, verse_end, book_to, chapter_to, verse_to_start, verse_to_end)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )?(coordinates(source, target))?;
        Ok(())
    }

    pub fn remove(&self, source: VerseRef, target: VerseRef) -> Result<()> {
        let conn = self.0.lock();
        conn.exec_bound(
            "DELETE FROM cross_references WHERE book = ? AND chapter = ? AND verse = ? AND verse_end = ?
               AND book_to = ? AND chapter_to = ? AND verse_to_start = ? AND verse_to_end = ?",
        )?(coordinates(source, target))?;
        Ok(())
    }
}

fn coordinates(source: VerseRef, target: VerseRef) -> (i64, i64, i64, i64, i64, i64, i64, i64) {
    (
        source.book.0,
        i64::from(source.chapter),
        i64::from(source.verse),
        i64::from(source.end_verse.unwrap_or(0)),
        target.book.0,
        i64::from(target.chapter),
        i64::from(target.verse),
        i64::from(target.end_verse.unwrap_or(0)),
    )
}
