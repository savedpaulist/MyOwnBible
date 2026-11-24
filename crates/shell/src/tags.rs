//! Сценарии тегов стихов (этап 21 polish-plan.md).
//!
//! Теги модуль-независимы (как цепочки): ставятся на каноническое место
//! и видны в любом переводе. Чтение грузит книгу целиком — маркеры
//! собираются одним проходом `book_tag_marks` (детали тегов только для
//! помеченных стихов, их немного).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use anyhow::Result;
use bible_domain::ids::{BookNumber, VerseRef};
use bible_domain::ports::TagStore;
use bible_domain::tags::Tag;

use crate::services::AppServices;

fn store(services: &AppServices) -> Result<&Arc<dyn TagStore>> {
    services
        .tags
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("tags store is unavailable"))
}

/// Все теги по алфавиту. Ошибки — в лог и пустой список (пикер и
/// менюшка не должны падать из-за хранилища).
pub fn list_tags(services: &AppServices) -> Vec<Tag> {
    services
        .tags
        .as_ref()
        .map(|store| {
            store.list().unwrap_or_else(|error| {
                log::warn!("list tags failed: {error:#}");
                Vec::new()
            })
        })
        .unwrap_or_default()
}

/// Число стихов по каждому тегу (список в менюшке).
pub fn tag_verse_counts(services: &AppServices) -> HashMap<i64, i64> {
    services
        .tags
        .as_ref()
        .map(|store| {
            store.verse_counts().unwrap_or_else(|error| {
                log::warn!("tag verse counts failed: {error:#}");
                HashMap::new()
            })
        })
        .unwrap_or_default()
}

/// Создать тег (или обновить цвет существующего с тем же именем).
pub fn create_tag(services: &AppServices, name: &str, color: &str) -> Result<Tag> {
    let name = name.trim();
    anyhow::ensure!(!name.is_empty(), "tag name is empty");
    store(services)?.create(name, color)
}

pub fn rename_tag(services: &AppServices, id: i64, name: &str) -> Result<()> {
    let name = name.trim();
    anyhow::ensure!(!name.is_empty(), "tag name is empty");
    store(services)?.rename(id, name)
}

pub fn set_tag_color(services: &AppServices, id: i64, color: &str) -> Result<()> {
    store(services)?.set_color(id, color)
}

pub fn delete_tag(services: &AppServices, id: i64) -> Result<()> {
    store(services)?.delete(id)
}

/// Теги конкретного стиха (чипы пикера).
pub fn tags_of_verse(services: &AppServices, reference: &VerseRef) -> Vec<Tag> {
    services
        .tags
        .as_ref()
        .map(|store| {
            store.tags_of_verse(reference).unwrap_or_else(|error| {
                log::warn!("tags of verse failed: {error:#}");
                Vec::new()
            })
        })
        .unwrap_or_default()
}

/// Тогл тега на стихе. Возвращает новое состояние: true = тег стоит.
pub fn toggle_verse_tag(services: &AppServices, tag_id: i64, reference: &VerseRef) -> Result<bool> {
    let store = store(services)?;
    let selected = store
        .tags_of_verse(reference)?
        .iter()
        .any(|tag| tag.id == tag_id);
    if selected {
        store.remove_verse(tag_id, reference)?;
        Ok(false)
    } else {
        store.add_verse(tag_id, reference)?;
        Ok(true)
    }
}

/// Все места с тегом, по канону (список стихов тега в менюшке).
pub fn verses_of_tag(services: &AppServices, tag_id: i64) -> Vec<VerseRef> {
    services
        .tags
        .as_ref()
        .map(|store| {
            store.verses_of_tag(tag_id).unwrap_or_else(|error| {
                log::warn!("verses of tag failed: {error:#}");
                Vec::new()
            })
        })
        .unwrap_or_default()
}

/// Маркеры тегов книги для чтения: (глава, стих) → теги (для цветных
/// точек в строке стиха). Книга грузится целиком — один запрос мест
/// плюс детали только помеченных стихов.
pub fn book_tag_marks(services: &AppServices, book: BookNumber) -> BTreeMap<(u16, u16), Vec<Tag>> {
    let mut marks: BTreeMap<(u16, u16), Vec<Tag>> = BTreeMap::new();
    let Some(store) = services.tags.as_ref() else {
        return marks;
    };
    let places = store
        .verses_with_tags_in_book(book)
        .unwrap_or_else(|error| {
            log::warn!("verses with tags in book {} failed: {error:#}", book.0);
            Vec::new()
        });
    for place in places {
        let tags = store.tags_of_verse(&place).unwrap_or_else(|error| {
            log::warn!("tags of verse failed: {error:#}");
            Vec::new()
        });
        if !tags.is_empty() {
            marks.insert((place.chapter, place.verse), tags);
        }
    }
    marks
}
