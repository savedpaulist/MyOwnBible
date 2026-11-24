//! Порты к Infrastructure (architecture.md §3.8).
//!
//! Application зависит только от этих traits; реализации живут в
//! `mybible`, `notes_core`, `app_state`. Тесты Application — на моках.

use crate::annotations::{Annotation, AnnotationKind, VerseComment};
use crate::ids::{AnchoredRef, BookNumber, ModuleId, VerseRef};
use crate::tags::Tag;
use anyhow::Result;
use std::collections::HashMap;

/// Метаданные модуля MyBible (см. mybible::scanner за построением).
#[derive(Debug, Clone)]
pub struct ModuleMeta {
    pub id: ModuleId,
    pub path: std::path::PathBuf,
    pub kind: ModuleKind,
    pub language: Option<String>,
    pub description: Option<String>,
    pub strong_prefix: Option<String>,
    /// Традиция нумерации: true = LXX/синодальная, false = MT.
    pub russian_numbering: bool,
    pub has_morphology_table: bool,
    pub morph_uses_applicable_to: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleKind {
    BibleTranslation,
    StrongsDictionary,
    GeneralDictionary,
    Commentary,
    CrossReferences,
    Unknown,
}

pub trait ModuleCatalog: Send + Sync {
    fn modules(&self) -> Vec<ModuleMeta>;
    fn module(&self, id: &ModuleId) -> Option<ModuleMeta>;
}

/// Сырой стих из модуля (ещё не распарсен: с `<S>`/`<m>` разметкой).
#[derive(Debug, Clone)]
pub struct RawVerse {
    pub verse: u16,
    pub text: String,
}

pub trait VerseSource: Send + Sync {
    fn chapter(&self, module: &ModuleId, book: BookNumber, chapter: u16) -> Result<Vec<RawVerse>>;
    fn verse_range(&self, module: &ModuleId, reference: &VerseRef) -> Result<String>;
}

/// Хранилище пользовательских аннотаций (architecture.md §3.6, §4.2.2).
/// Реализация — `app_state` (app_state.db). Запросы главы отдают данные
/// ВСЕХ модулей: слой подготовки модели сам делит их на «свои» (точное
/// оформление) и «чужие» (индикатор на уровне стиха).
pub trait AnnotationStore: Send + Sync {
    /// Поставить/заменить аннотацию (upsert по anchor+kind).
    fn put(&self, annotation: &Annotation) -> Result<()>;
    /// Убрать аннотацию данного вида с данного места. true = была и удалена.
    fn remove(&self, anchor: &AnchoredRef, kind: AnnotationKind) -> Result<bool>;
    /// Все аннотации книги по всем модулям (чтение показывает книгу целиком;
    /// деление «свои/чужие» — на слое подготовки модели).
    fn for_book(&self, book: BookNumber) -> Result<Vec<Annotation>>;

    /// Комментарий к стиху/слову.
    fn comment(&self, anchor: &AnchoredRef) -> Result<Option<VerseComment>>;
    /// Поставить/заменить комментарий (пустое тело не сохраняем —
    /// вызывающий код должен звать remove_comment).
    fn put_comment(&self, comment: &VerseComment) -> Result<()>;
    fn remove_comment(&self, anchor: &AnchoredRef) -> Result<bool>;
    /// Все комментарии книги по всем модулям.
    fn comments_for_book(&self, book: BookNumber) -> Result<Vec<VerseComment>>;
}

// DictionarySource / CommentarySource / CrossReferenceSource / SearchIndex
// добавляются по мере этапов плана (architecture.md §12).

/// Хранилище тегов стихов (этап 21 polish-plan.md). Реализация —
/// `app_state` (та же app_state.db). Привязка модуль-независимая:
/// запросы отдают канонические места без module_id.
pub trait TagStore: Send + Sync {
    /// Все теги, по алфавиту (без учёта регистра).
    fn list(&self) -> Result<Vec<Tag>>;
    /// Создать тег или обновить цвет существующего с тем же именем
    /// (upsert по имени, как tag_upsert форка). Возвращает тег с id.
    fn create(&self, name: &str, color: &str) -> Result<Tag>;
    fn rename(&self, id: i64, name: &str) -> Result<()>;
    fn set_color(&self, id: i64, color: &str) -> Result<()>;
    /// Каскадно снимает тег со всех стихов (FK ON DELETE CASCADE).
    fn delete(&self, id: i64) -> Result<()>;

    /// Поставить тег на место (идемпотентно — INSERT OR IGNORE).
    fn add_verse(&self, tag_id: i64, reference: &VerseRef) -> Result<()>;
    /// Снять тег с места. true = строка была и удалена.
    fn remove_verse(&self, tag_id: i64, reference: &VerseRef) -> Result<bool>;
    /// Теги конкретного стиха (чипы пикера, маркер в чтении).
    fn tags_of_verse(&self, reference: &VerseRef) -> Result<Vec<Tag>>;
    /// Все места с этим тегом, по канону (book, chapter, verse).
    fn verses_of_tag(&self, tag_id: i64) -> Result<Vec<VerseRef>>;
    /// Места книги, у которых есть хотя бы один тег (маркеры чтения:
    /// книга грузится целиком).
    fn verses_with_tags_in_book(&self, book: BookNumber) -> Result<Vec<VerseRef>>;
    /// Число стихов по каждому тегу (список в менюшке без N запросов).
    fn verse_counts(&self) -> Result<HashMap<i64, i64>>;
}
