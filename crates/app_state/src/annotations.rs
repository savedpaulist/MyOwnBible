//! Реализация `bible_domain::ports::AnnotationStore` поверх app_state.db
//! (architecture.md §4.2.2). Весь SQL аннотаций живёт только здесь.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use bible_domain::annotations::{Annotation, AnnotationKind, AnnotationStyle, VerseComment};
use bible_domain::ids::{AnchoredRef, BookNumber, ModuleId, VerseRef};
use bible_domain::ports::AnnotationStore;

use crate::db::StateDb;

/// -1 в колонке word_index = аннотация на весь стих (NULL в первичном
/// ключе STRICT-таблицы неудобен, держим явный сентинел).
const WHOLE_VERSE: i64 = -1;

pub struct SqliteAnnotationStore {
    db: Arc<StateDb>,
}

impl SqliteAnnotationStore {
    pub fn new(db: Arc<StateDb>) -> Self {
        Self { db }
    }

    /// Открыть хранилище в каталоге данных приложения.
    pub fn open_default() -> Result<Self> {
        Ok(Self::new(StateDb::open_default()?))
    }
}

fn word_index_to_db(word_index: Option<u32>) -> i64 {
    word_index.map_or(WHOLE_VERSE, |index| index as i64)
}

fn word_index_from_db(value: i64) -> Option<u32> {
    (value != WHOLE_VERSE).then_some(value as u32)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

type AnnotationRow = (String, i64, i64, i64, i64, String, String, i64);

fn annotation_from_row(row: AnnotationRow) -> Result<Annotation> {
    let (module, book, chapter, verse, word_index, kind, color, updated_at) = row;
    Ok(Annotation {
        anchor: AnchoredRef {
            module: ModuleId(module),
            reference: VerseRef::single(BookNumber(book), chapter as u16, verse as u16),
            word_index: word_index_from_db(word_index),
        },
        kind: AnnotationKind::parse(&kind)
            .with_context(|| format!("unknown annotation kind {kind:?} in app_state.db"))?,
        style: AnnotationStyle { color },
        updated_at,
    })
}

type CommentRow = (String, i64, i64, i64, i64, Option<String>, String, i64, i64);

fn comment_from_row(row: CommentRow) -> VerseComment {
    let (module, book, chapter, verse, word_index, note_relpath, body, created_at, updated_at) =
        row;
    VerseComment {
        anchor: AnchoredRef {
            module: ModuleId(module),
            reference: VerseRef::single(BookNumber(book), chapter as u16, verse as u16),
            word_index: word_index_from_db(word_index),
        },
        body,
        note_relpath,
        created_at,
        updated_at,
    }
}

/// Бинды адреса (module, book, chapter, verse, word_index) в порядке колонок.
fn anchor_binds(anchor: &AnchoredRef) -> (String, i64, i64, i64, i64) {
    (
        anchor.module.to_string(),
        anchor.reference.book.0,
        anchor.reference.chapter as i64,
        anchor.reference.verse as i64,
        word_index_to_db(anchor.word_index),
    )
}

impl AnnotationStore for SqliteAnnotationStore {
    fn put(&self, annotation: &Annotation) -> Result<()> {
        let mut annotation = annotation.clone();
        annotation.updated_at = now_unix();
        self.db.with(|connection| {
            connection.exec_bound(
                "INSERT INTO annotations(\
                    module_id, book, chapter, verse, word_index, kind, color, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(module_id, book, chapter, verse, word_index, kind) \
                 DO UPDATE SET color = excluded.color, updated_at = excluded.updated_at",
            )?((
                annotation.anchor.module.to_string(),
                annotation.anchor.reference.book.0,
                annotation.anchor.reference.chapter as i64,
                annotation.anchor.reference.verse as i64,
                word_index_to_db(annotation.anchor.word_index),
                annotation.kind.as_str(),
                annotation.style.color.as_str(),
                annotation.updated_at,
            ))
        })
    }

    fn remove(&self, anchor: &AnchoredRef, kind: AnnotationKind) -> Result<bool> {
        self.db.with(|connection| {
            let existed: Option<i64> = connection.select_row_bound(
                "SELECT COUNT(*) FROM annotations \
                 WHERE module_id = ? AND book = ? AND chapter = ? AND verse = ? \
                   AND word_index = ? AND kind = ?",
            )?({
                let (module, book, chapter, verse, word_index) = anchor_binds(anchor);
                (module, book, chapter, verse, word_index, kind.as_str())
            })?;
            connection.exec_bound(
                "DELETE FROM annotations \
                 WHERE module_id = ? AND book = ? AND chapter = ? AND verse = ? \
                   AND word_index = ? AND kind = ?",
            )?({
                let (module, book, chapter, verse, word_index) = anchor_binds(anchor);
                (module, book, chapter, verse, word_index, kind.as_str())
            })?;
            Ok(existed.unwrap_or(0) > 0)
        })
    }

    fn for_book(&self, book: BookNumber) -> Result<Vec<Annotation>> {
        self.db.with(|connection| {
            let rows = connection.select_bound::<i64, AnnotationRow>(
                "SELECT module_id, book, chapter, verse, word_index, kind, color, updated_at \
                 FROM annotations WHERE book = ? \
                 ORDER BY module_id, chapter, verse, word_index, kind",
            )?(book.0)?;
            rows.into_iter().map(annotation_from_row).collect()
        })
    }

    fn comment(&self, anchor: &AnchoredRef) -> Result<Option<VerseComment>> {
        self.db.with(|connection| {
            let row = connection.select_row_bound::<(String, i64, i64, i64, i64), CommentRow>(
                "SELECT module_id, book, chapter, verse, word_index, note_relpath, body, \
                        created_at, updated_at \
                 FROM verse_comments \
                 WHERE module_id = ? AND book = ? AND chapter = ? AND verse = ? AND word_index = ?",
            )?(anchor_binds(anchor))?;
            Ok(row.map(comment_from_row))
        })
    }

    fn put_comment(&self, comment: &VerseComment) -> Result<()> {
        let now = now_unix();
        let created_at = if comment.created_at > 0 {
            comment.created_at
        } else {
            now
        };
        self.db.with(|connection| {
            connection.exec_bound(
                "INSERT INTO verse_comments(\
                    module_id, book, chapter, verse, word_index, note_relpath, body, \
                    created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(module_id, book, chapter, verse, word_index) \
                 DO UPDATE SET note_relpath = excluded.note_relpath, \
                               body = excluded.body, \
                               updated_at = excluded.updated_at",
            )?((
                comment.anchor.module.to_string(),
                comment.anchor.reference.book.0,
                comment.anchor.reference.chapter as i64,
                comment.anchor.reference.verse as i64,
                word_index_to_db(comment.anchor.word_index),
                comment.note_relpath.as_deref(),
                comment.body.as_str(),
                created_at,
                now,
            ))
        })
    }

    fn remove_comment(&self, anchor: &AnchoredRef) -> Result<bool> {
        self.db.with(|connection| {
            let existed: Option<i64> = connection.select_row_bound(
                "SELECT COUNT(*) FROM verse_comments \
                 WHERE module_id = ? AND book = ? AND chapter = ? AND verse = ? AND word_index = ?",
            )?(anchor_binds(anchor))?;
            connection.exec_bound(
                "DELETE FROM verse_comments \
                 WHERE module_id = ? AND book = ? AND chapter = ? AND verse = ? AND word_index = ?",
            )?(anchor_binds(anchor))?;
            Ok(existed.unwrap_or(0) > 0)
        })
    }

    fn comments_for_book(&self, book: BookNumber) -> Result<Vec<VerseComment>> {
        self.db.with(|connection| {
            let rows = connection.select_bound::<i64, CommentRow>(
                "SELECT module_id, book, chapter, verse, word_index, note_relpath, body, \
                        created_at, updated_at \
                 FROM verse_comments WHERE book = ? \
                 ORDER BY module_id, chapter, verse, word_index",
            )?(book.0)?;
            Ok(rows.into_iter().map(comment_from_row).collect())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SqliteAnnotationStore {
        SqliteAnnotationStore::new(StateDb::in_memory().unwrap())
    }

    fn anchor(module: &str, verse: u16) -> AnchoredRef {
        AnchoredRef {
            module: ModuleId::from(module),
            reference: VerseRef::single(BookNumber(540), 3, verse),
            word_index: None,
        }
    }

    #[test]
    fn put_and_read_back_chapter() {
        let store = store();
        store
            .put(&Annotation::new(
                anchor("RST", 16),
                AnnotationKind::Highlight,
                "yellow",
            ))
            .unwrap();
        store
            .put(&Annotation::new(
                anchor("KJV", 16),
                AnnotationKind::Underline,
                "green",
            ))
            .unwrap();
        let all = store.for_book(BookNumber(540)).unwrap();
        assert_eq!(all.len(), 2);
        // Обе аннотации одной главы, разных модулей — как нужно индикаторам.
        assert!(all.iter().any(|a| a.anchor.module.0 == "RST"));
        assert!(all.iter().any(|a| a.anchor.module.0 == "KJV"));
        assert!(all.iter().all(|a| a.updated_at > 0));
    }

    #[test]
    fn put_upserts_style() {
        let store = store();
        store
            .put(&Annotation::new(
                anchor("RST", 1),
                AnnotationKind::Highlight,
                "yellow",
            ))
            .unwrap();
        store
            .put(&Annotation::new(
                anchor("RST", 1),
                AnnotationKind::Highlight,
                "blue",
            ))
            .unwrap();
        let all = store.for_book(BookNumber(540)).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].style.color, "blue");
    }

    #[test]
    fn remove_reports_existence() {
        let store = store();
        assert!(
            !store
                .remove(&anchor("RST", 1), AnnotationKind::Highlight)
                .unwrap()
        );
        store
            .put(&Annotation::new(
                anchor("RST", 1),
                AnnotationKind::Highlight,
                "yellow",
            ))
            .unwrap();
        assert!(
            store
                .remove(&anchor("RST", 1), AnnotationKind::Highlight)
                .unwrap()
        );
        assert!(store.for_book(BookNumber(540)).unwrap().is_empty());
    }

    #[test]
    fn word_level_annotation() {
        let store = store();
        let mut word_anchor = anchor("RST", 5);
        word_anchor.word_index = Some(3);
        store
            .put(&Annotation::new(
                word_anchor,
                AnnotationKind::WordColor,
                "orange",
            ))
            .unwrap();
        let all = store.for_book(BookNumber(540)).unwrap();
        assert_eq!(all[0].anchor.word_index, Some(3));
        assert_eq!(all[0].kind, AnnotationKind::WordColor);
    }

    #[test]
    fn comment_lifecycle() {
        let store = store();
        let anchor = anchor("RST", 16);
        assert!(store.comment(&anchor).unwrap().is_none());

        store
            .put_comment(&VerseComment {
                anchor: anchor.clone(),
                body: "любимый стих".to_string(),
                note_relpath: None,
                created_at: 0,
                updated_at: 0,
            })
            .unwrap();
        let saved = store.comment(&anchor).unwrap().unwrap();
        assert_eq!(saved.body, "любимый стих");
        assert!(saved.created_at > 0);
        let created_at = saved.created_at;

        // Перезапись сохраняет created_at исходного.
        store
            .put_comment(&VerseComment {
                anchor: anchor.clone(),
                body: "обновлённый".to_string(),
                note_relpath: None,
                created_at,
                updated_at: 0,
            })
            .unwrap();
        let saved = store.comment(&anchor).unwrap().unwrap();
        assert_eq!(saved.body, "обновлённый");
        assert_eq!(saved.created_at, created_at);

        assert!(store.remove_comment(&anchor).unwrap());
        assert!(store.comment(&anchor).unwrap().is_none());
        assert!(!store.remove_comment(&anchor).unwrap());
    }
}
