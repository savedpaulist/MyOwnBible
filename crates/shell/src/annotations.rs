//! Сценарии аннотаций (architecture.md §5.3: add_annotation / remove_annotation).
//!
//! Чтение показывает книгу целиком, поэтому модель тоже книжная:
//! `book_annotations` забирает данные всех модулей из хранилища и
//! раскладывает по стихам. Правило §3.6: «свои» аннотации (активный
//! модуль) оформляются точно, «чужие» схлопываются в индикатор
//! «есть пользовательские данные» на уровне стиха.

use std::collections::BTreeMap;

use anyhow::Result;
use bible_domain::annotations::{Annotation, AnnotationKind, VerseComment};
use bible_domain::ids::{AnchoredRef, BookNumber, ModuleId};

use crate::services::AppServices;

/// Оформление одного стиха. Цвета — имена палитры
/// (`bible_domain::annotations::PALETTE`), в HSL их превращает тема в UI.
#[derive(Debug, Default, Clone)]
pub struct VerseMarks {
    /// Фон стиха (highlight, word_index = None).
    pub highlight: Option<String>,
    /// Подчёркивание стиха.
    pub underline: Option<String>,
    /// Цвет текста всего стиха (word_color, word_index = None).
    pub text_color: Option<String>,
    /// Пословные аннотации: (word_index, вид, цвет).
    pub words: Vec<(u32, AnnotationKind, String)>,
    /// Есть мой комментарий (активный модуль).
    pub own_comment: bool,
    /// Есть аннотации или комментарии в других модулях — только индикатор.
    pub foreign: bool,
}

/// Модель аннотаций книги для BibleTextView.
#[derive(Debug, Default)]
pub struct BookAnnotations {
    pub marks: BTreeMap<(u16, u16), VerseMarks>,
}

impl BookAnnotations {
    pub fn marks_of(&self, chapter: u16, verse: u16) -> Option<&VerseMarks> {
        self.marks.get(&(chapter, verse))
    }

    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }
}

/// Чистое раскладывание по стихам (без I/O — удобно тестировать).
pub fn split_annotations(
    module: &ModuleId,
    annotations: Vec<Annotation>,
    comments: Vec<VerseComment>,
) -> BookAnnotations {
    let mut marks: BTreeMap<(u16, u16), VerseMarks> = BTreeMap::new();
    for annotation in annotations {
        let reference = &annotation.anchor.reference;
        let entry = marks
            .entry((reference.chapter, reference.verse))
            .or_default();
        let own = annotation.anchor.module == *module;
        match (own, annotation.anchor.word_index) {
            (true, None) => match annotation.kind {
                AnnotationKind::Highlight => entry.highlight = Some(annotation.style.color),
                AnnotationKind::Underline => entry.underline = Some(annotation.style.color),
                // Цвет текста всего стиха (меню «Оформление стиха»).
                AnnotationKind::WordColor => entry.text_color = Some(annotation.style.color),
            },
            (true, Some(word_index)) => {
                entry
                    .words
                    .push((word_index, annotation.kind, annotation.style.color));
            }
            (false, _) => entry.foreign = true,
        }
    }
    for comment in comments {
        let reference = &comment.anchor.reference;
        let entry = marks
            .entry((reference.chapter, reference.verse))
            .or_default();
        if comment.anchor.module == *module {
            entry.own_comment = true;
        } else {
            entry.foreign = true;
        }
    }
    BookAnnotations { marks }
}

/// Сценарий «аннотации книги»: хранилище → модель чтения.
/// Ошибки чтения — в лог и пустая модель: текст важнее украшений.
pub fn book_annotations(
    services: &AppServices,
    module: &ModuleId,
    book: BookNumber,
) -> BookAnnotations {
    let Some(store) = services.annotations.as_ref() else {
        return BookAnnotations::default();
    };
    let annotations = store.for_book(book).unwrap_or_else(|error| {
        log::warn!("annotations for book {} failed: {error:#}", book.0);
        Vec::new()
    });
    let comments = store.comments_for_book(book).unwrap_or_else(|error| {
        log::warn!("comments for book {} failed: {error:#}", book.0);
        Vec::new()
    });
    split_annotations(module, annotations, comments)
}

/// Поставить/заменить аннотацию (upsert по месту и виду).
pub fn set_annotation(
    services: &AppServices,
    anchor: AnchoredRef,
    kind: AnnotationKind,
    color: impl Into<String>,
) -> Result<()> {
    let store = services
        .annotations
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("annotations store is unavailable"))?;
    store.put(&Annotation::new(anchor, kind, color))
}

/// Снять аннотацию данного вида с места.
pub fn clear_annotation(
    services: &AppServices,
    anchor: &AnchoredRef,
    kind: AnnotationKind,
) -> Result<()> {
    let store = services
        .annotations
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("annotations store is unavailable"))?;
    store.remove(anchor, kind)?;
    Ok(())
}

/// Мой комментарий к стиху (активный модуль).
pub fn verse_comment(services: &AppServices, anchor: &AnchoredRef) -> Option<VerseComment> {
    services
        .annotations
        .as_ref()
        .and_then(|store| store.comment(anchor).ok())
        .flatten()
}

/// Сохранить комментарий к стиху; пустое тело (после trim) = удалить.
pub fn save_verse_comment(services: &AppServices, anchor: AnchoredRef, body: &str) -> Result<()> {
    let store = services
        .annotations
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("annotations store is unavailable"))?;
    let body = body.trim();
    if body.is_empty() {
        store.remove_comment(&anchor)?;
        return Ok(());
    }
    let created_at = store.comment(&anchor)?.map_or(0, |saved| saved.created_at);
    store.put_comment(&VerseComment {
        anchor,
        body: body.to_string(),
        note_relpath: None,
        created_at,
        updated_at: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bible_domain::ids::VerseRef;

    fn anchor(module: &str, chapter: u16, verse: u16, word_index: Option<u32>) -> AnchoredRef {
        AnchoredRef {
            module: ModuleId::from(module),
            reference: VerseRef::single(BookNumber(40), chapter, verse),
            word_index,
        }
    }

    fn comment(module: &str, verse: u16) -> VerseComment {
        VerseComment {
            anchor: anchor(module, 1, verse, None),
            body: "текст".to_string(),
            note_relpath: None,
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn own_annotations_are_precise() {
        let module = ModuleId::from("RST");
        let annotations = vec![
            Annotation::new(
                anchor("RST", 1, 5, None),
                AnnotationKind::Highlight,
                "yellow",
            ),
            Annotation::new(
                anchor("RST", 1, 5, Some(2)),
                AnnotationKind::WordColor,
                "orange",
            ),
            Annotation::new(
                anchor("RST", 2, 1, None),
                AnnotationKind::Underline,
                "green",
            ),
        ];
        let book = split_annotations(&module, annotations, vec![comment("RST", 5)]);

        let marks = book.marks_of(1, 5).unwrap();
        assert_eq!(marks.highlight.as_deref(), Some("yellow"));
        assert_eq!(marks.words.len(), 1);
        assert_eq!(marks.words[0].0, 2);
        assert!(marks.own_comment);
        assert!(!marks.foreign);
        assert!(marks.text_color.is_none());

        let marks = book.marks_of(2, 1).unwrap();
        assert_eq!(marks.underline.as_deref(), Some("green"));
        assert!(marks.highlight.is_none());

        assert!(book.marks_of(3, 3).is_none());
    }

    #[test]
    fn foreign_annotations_collapse_to_indicator() {
        let module = ModuleId::from("RST");
        let annotations = vec![
            Annotation::new(
                anchor("KJV", 1, 5, None),
                AnnotationKind::Highlight,
                "yellow",
            ),
            Annotation::new(
                anchor("KJV", 1, 5, Some(0)),
                AnnotationKind::WordColor,
                "blue",
            ),
        ];
        let book = split_annotations(&module, annotations, vec![comment("NRT", 5)]);
        let marks = book.marks_of(1, 5).unwrap();
        assert!(marks.highlight.is_none());
        assert!(marks.words.is_empty());
        assert!(!marks.own_comment);
        assert!(marks.foreign);
    }
}
