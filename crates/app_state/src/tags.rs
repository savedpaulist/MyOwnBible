//! Реализация `bible_domain::ports::TagStore` поверх app_state.db
//! (этап 21 polish-plan.md). Весь SQL тегов живёт только здесь.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use bible_domain::ids::{BookNumber, VerseRef};
use bible_domain::ports::TagStore;
use bible_domain::tags::Tag;

use crate::db::StateDb;

pub struct SqliteTagStore {
    db: Arc<StateDb>,
}

impl SqliteTagStore {
    pub fn new(db: Arc<StateDb>) -> Self {
        Self { db }
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

type TagRow = (i64, String, String);

fn tag_from_row((id, name, color): TagRow) -> Tag {
    Tag { id, name, color }
}

fn reference_binds(reference: &VerseRef) -> (i64, i64, i64) {
    (
        reference.book.0,
        reference.chapter as i64,
        reference.verse as i64,
    )
}

fn reference_from_row((book, chapter, verse): (i64, i64, i64)) -> VerseRef {
    VerseRef::single(BookNumber(book), chapter as u16, verse as u16)
}

impl TagStore for SqliteTagStore {
    fn list(&self) -> Result<Vec<Tag>> {
        self.db.with(|connection| {
            let rows = connection.select::<TagRow>("SELECT id, name, color FROM tags")?()?;
            let mut tags: Vec<Tag> = rows.into_iter().map(tag_from_row).collect();
            tags.sort_by_key(|tag| tag.name.to_lowercase());
            Ok(tags)
        })
    }

    fn create(&self, name: &str, color: &str) -> Result<Tag> {
        self.db.with(|connection| {
            connection.exec_bound(
                "INSERT INTO tags(name, color) VALUES (?, ?) \
                 ON CONFLICT(name) DO UPDATE SET color = excluded.color",
            )?((name, color))?;
            let row = connection.select_row_bound::<&str, TagRow>(
                "SELECT id, name, color FROM tags WHERE name = ?",
            )?(name)?;
            row.map(tag_from_row)
                .ok_or_else(|| anyhow::anyhow!("tag {name:?} disappeared after upsert"))
        })
    }

    fn rename(&self, id: i64, name: &str) -> Result<()> {
        self.db.with(|connection| {
            connection.exec_bound("UPDATE tags SET name = ? WHERE id = ?")?((name, id))?;
            Ok(())
        })
    }

    fn set_color(&self, id: i64, color: &str) -> Result<()> {
        self.db.with(|connection| {
            connection.exec_bound("UPDATE tags SET color = ? WHERE id = ?")?((color, id))?;
            Ok(())
        })
    }

    fn delete(&self, id: i64) -> Result<()> {
        self.db.with(|connection| {
            connection.exec_bound("DELETE FROM tags WHERE id = ?")?(id)?;
            Ok(())
        })
    }

    fn add_verse(&self, tag_id: i64, reference: &VerseRef) -> Result<()> {
        self.db.with(|connection| {
            connection.exec_bound(
                "INSERT OR IGNORE INTO verse_tags(book, chapter, verse, tag_id, created_at) \
                 VALUES (?, ?, ?, ?, ?)",
            )?((
                reference.book.0,
                reference.chapter as i64,
                reference.verse as i64,
                tag_id,
                now_unix(),
            ))?;
            Ok(())
        })
    }

    fn remove_verse(&self, tag_id: i64, reference: &VerseRef) -> Result<bool> {
        self.db.with(|connection| {
            let existed: Option<i64> = connection.select_row_bound(
                "SELECT COUNT(*) FROM verse_tags \
                 WHERE book = ? AND chapter = ? AND verse = ? AND tag_id = ?",
            )?((
                reference.book.0,
                reference.chapter as i64,
                reference.verse as i64,
                tag_id,
            ))?;
            connection.exec_bound(
                "DELETE FROM verse_tags \
                 WHERE book = ? AND chapter = ? AND verse = ? AND tag_id = ?",
            )?((
                reference.book.0,
                reference.chapter as i64,
                reference.verse as i64,
                tag_id,
            ))?;
            Ok(existed.unwrap_or(0) > 0)
        })
    }

    fn tags_of_verse(&self, reference: &VerseRef) -> Result<Vec<Tag>> {
        self.db.with(|connection| {
            let rows = connection.select_bound::<(i64, i64, i64), TagRow>(
                "SELECT t.id, t.name, t.color \
                 FROM verse_tags vt JOIN tags t ON t.id = vt.tag_id \
                 WHERE vt.book = ? AND vt.chapter = ? AND vt.verse = ? \
                 ORDER BY t.name",
            )?(reference_binds(reference))?;
            Ok(rows.into_iter().map(tag_from_row).collect())
        })
    }

    fn verses_of_tag(&self, tag_id: i64) -> Result<Vec<VerseRef>> {
        self.db.with(|connection| {
            let rows = connection.select_bound::<i64, (i64, i64, i64)>(
                "SELECT book, chapter, verse FROM verse_tags \
                 WHERE tag_id = ? ORDER BY book, chapter, verse",
            )?(tag_id)?;
            Ok(rows.into_iter().map(reference_from_row).collect())
        })
    }

    fn verses_with_tags_in_book(&self, book: BookNumber) -> Result<Vec<VerseRef>> {
        self.db.with(|connection| {
            let rows = connection.select_bound::<i64, (i64, i64, i64)>(
                "SELECT DISTINCT book, chapter, verse FROM verse_tags \
                 WHERE book = ? ORDER BY chapter, verse",
            )?(book.0)?;
            Ok(rows.into_iter().map(reference_from_row).collect())
        })
    }

    fn verse_counts(&self) -> Result<HashMap<i64, i64>> {
        self.db.with(|connection| {
            let rows = connection
                .select::<(i64, i64)>("SELECT tag_id, COUNT(*) FROM verse_tags GROUP BY tag_id")?(
            )?;
            Ok(rows.into_iter().collect())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SqliteTagStore {
        SqliteTagStore::new(StateDb::in_memory().unwrap())
    }

    fn john316() -> VerseRef {
        VerseRef::single(BookNumber(500), 3, 16)
    }

    #[test]
    fn tag_lifecycle() {
        let store = store();
        let tag = store.create("милость", "green").unwrap();
        assert!(tag.id > 0);
        assert_eq!(store.list().unwrap().len(), 1);

        // Upsert по имени: тот же id, новый цвет.
        let same = store.create("милость", "blue").unwrap();
        assert_eq!(same.id, tag.id);
        assert_eq!(same.color, "blue");
        assert_eq!(store.list().unwrap().len(), 1);

        store.rename(tag.id, "благодать").unwrap();
        assert_eq!(store.list().unwrap()[0].name, "благодать");

        store.delete(tag.id).unwrap();
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn verse_membership() {
        let store = store();
        let tag = store.create("обетование", "yellow").unwrap();
        store.add_verse(tag.id, &john316()).unwrap();
        // Повторная постановка идемпотентна.
        store.add_verse(tag.id, &john316()).unwrap();

        let tags = store.tags_of_verse(&john316()).unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "обетование");

        assert_eq!(store.verses_of_tag(tag.id).unwrap(), vec![john316()]);
        assert_eq!(
            store.verses_with_tags_in_book(BookNumber(500)).unwrap(),
            vec![john316()]
        );
        assert_eq!(store.verse_counts().unwrap().get(&tag.id), Some(&1));

        assert!(store.remove_verse(tag.id, &john316()).unwrap());
        assert!(!store.remove_verse(tag.id, &john316()).unwrap());
        assert!(store.tags_of_verse(&john316()).unwrap().is_empty());
    }

    #[test]
    fn delete_cascades_to_verses() {
        let store = store();
        let tag = store.create("временный", "pink").unwrap();
        store.add_verse(tag.id, &john316()).unwrap();
        store.delete(tag.id).unwrap();
        assert!(
            store
                .verses_with_tags_in_book(BookNumber(500))
                .unwrap()
                .is_empty()
        );
    }
}
