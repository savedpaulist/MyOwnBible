//! Сценарий «контекст стиха» (architecture.md §6.4, переработка
//! `word_panel.rs` форка): всё, что правая панель показывает о текущем
//! стихе — другие переводы, параллельные ссылки по всем модулям,
//! комментарии по всем модулям. UI только рисует.
//!
//! Как и в форке, данные вычисляются на каждый запрос (стих сменился →
//! пересчёт): запросы дешёвые, кэширование не нужно.

use bible_domain::content::ContentBlock;
use bible_domain::ids::{ModuleId, VerseRef};
use bible_domain::ports::{ModuleCatalog, ModuleKind};
use bible_domain::verse;

use crate::services::{AppServices, CurrentLocation};

/// Один перевод того же стиха.
#[derive(Debug, Clone)]
pub struct TranslationEntry {
    pub module: ModuleId,
    /// Локализованное имя модуля (description), иначе id.
    pub label: String,
    /// Чистый текст стиха (без Strong's/морфологии).
    pub text: String,
}

/// Цель параллельной ссылки с готовой подписью.
#[derive(Debug, Clone)]
pub struct CrossRefTarget {
    pub reference: VerseRef,
    /// «Рим 4:1–5» (имя книги — из активного перевода).
    pub label: String,
    pub votes: i64,
}

/// Ссылки одного модуля (группа сворачивается в UI по модулю).
#[derive(Debug, Clone)]
pub struct CrossRefGroup {
    pub module: ModuleId,
    pub label: String,
    pub targets: Vec<CrossRefTarget>,
}

/// Комментарий одного модуля, покрывающий стих.
#[derive(Debug, Clone)]
pub struct CommentaryEntry {
    pub module: ModuleId,
    pub label: String,
    /// Диапазон стихов записи: «3–5».
    pub anchor: String,
    pub blocks: Vec<ContentBlock>,
}

/// Всё о текущем стихе для правой панели.
#[derive(Debug, Clone)]
pub struct VerseContextViewModel {
    /// Подпись места: «На 1:18».
    pub reference_label: String,
    /// Активный перевод (его текст в translations не повторяется).
    pub module: ModuleId,
    pub translations: Vec<TranslationEntry>,
    pub cross_refs: Vec<CrossRefGroup>,
    pub user_cross_refs: Vec<CrossRefTarget>,
    pub commentary: Vec<CommentaryEntry>,
}

pub fn verse_context(services: &AppServices, location: &CurrentLocation) -> VerseContextViewModel {
    let reference_label = format!(
        "{} {}:{}",
        location.book_title, location.chapter, location.verse
    );
    let verse_ref = VerseRef::single(location.book, location.chapter, location.verse);

    let modules = services.registry.modules();
    let mut registry = services.registry.lock();

    // Имена книг для подписей — из активного перевода.
    let books = registry.books_of(&location.module);
    let book_name = |book_number: i64| -> String {
        books
            .iter()
            .find(|book| book.book_number == book_number)
            .map(|book| book.short_name.clone())
            .unwrap_or_else(|| format!("Кн. {book_number}"))
    };

    // Другие переводы этого же стиха.
    let mut translations = Vec::new();
    for meta in modules
        .iter()
        .filter(|m| m.kind == ModuleKind::BibleTranslation && m.id != location.module)
    {
        if let Ok(raw) = registry.verse_range_text(&meta.id, &verse_ref) {
            let text = verse::parse_verse(&raw).clean_text;
            if text.trim().is_empty() {
                continue;
            }
            translations.push(TranslationEntry {
                module: meta.id.clone(),
                label: meta
                    .description
                    .clone()
                    .unwrap_or_else(|| meta.id.to_string()),
                text,
            });
        }
    }

    // Параллельные ссылки всех модулей, группами по модулю (без дедупликации,
    // как в форке: пользователь сворачивает источники сам).
    let mut cross_refs = Vec::new();
    for meta in modules
        .iter()
        .filter(|m| m.kind == ModuleKind::CrossReferences)
    {
        let rows = registry.cross_references_for(
            &meta.id,
            location.book,
            location.chapter,
            location.verse,
        );
        if rows.is_empty() {
            continue;
        }
        let targets = rows
            .into_iter()
            .map(|(book_to, chapter_to, verse_start, verse_end, votes)| {
                let verses = if verse_end > verse_start {
                    format!("{verse_start}–{verse_end}")
                } else {
                    verse_start.to_string()
                };
                CrossRefTarget {
                    reference: VerseRef {
                        book: bible_domain::ids::BookNumber(book_to),
                        chapter: chapter_to as u16,
                        verse: verse_start as u16,
                        end_verse: (verse_end > verse_start).then_some(verse_end as u16),
                    },
                    label: format!("{} {chapter_to}:{verses}", book_name(book_to)),
                    votes,
                }
            })
            .collect();
        cross_refs.push(CrossRefGroup {
            module: meta.id.clone(),
            label: meta
                .description
                .clone()
                .unwrap_or_else(|| meta.id.to_string()),
            targets,
        });
    }

    // Комментарии всех модулей, покрывающие стих (диапазон from..=to).
    let mut commentary = Vec::new();
    for meta in modules.iter().filter(|m| m.kind == ModuleKind::Commentary) {
        let rows = registry.commentary_for_chapter(&meta.id, location.book, location.chapter);
        for (verse_from, verse_to, raw) in rows {
            let covered =
                location.verse >= verse_from && location.verse <= verse_to.max(verse_from);
            if !covered {
                continue;
            }
            let anchor = if verse_to > verse_from {
                format!("{verse_from}–{verse_to}")
            } else {
                verse_from.to_string()
            };
            commentary.push(CommentaryEntry {
                module: meta.id.clone(),
                label: meta
                    .description
                    .clone()
                    .unwrap_or_else(|| meta.id.to_string()),
                anchor,
                blocks: mybible::html::parse_article(&raw, &meta.id),
            });
        }
    }

    drop(registry);
    let user_cross_refs = user_cross_refs(services, location);

    VerseContextViewModel {
        reference_label,
        module: location.module.clone(),
        translations,
        cross_refs,
        user_cross_refs,
        commentary,
    }
}

pub fn user_cross_refs(services: &AppServices, location: &CurrentLocation) -> Vec<CrossRefTarget> {
    let Some(store) = &services.user_cross_refs else {
        return Vec::new();
    };
    let source = VerseRef::single(location.book, location.chapter, location.verse);
    let references = store.for_verse(source).unwrap_or_else(|error| {
        log::error!("read personal cross-references: {error:#}");
        Vec::new()
    });
    if references.is_empty() {
        return Vec::new();
    }
    let mut registry = services.registry.lock();
    let preferred = registry.books_of(&location.module);
    references
        .into_iter()
        .map(|reference| {
            let book = preferred
                .iter()
                .chain(registry.book_entries())
                .find(|book| book.book_number == reference.book.0)
                .map(|book| book.short_name.clone())
                .unwrap_or_else(|| format!("Кн. {}", reference.book.0));
            let verses = reference.end_verse.map_or_else(
                || reference.verse.to_string(),
                |end| format!("{}–{end}", reference.verse),
            );
            CrossRefTarget {
                label: format!("{book} {}:{verses}", reference.chapter),
                reference,
                votes: 0,
            }
        })
        .collect()
}

pub fn add_user_cross_ref(
    services: &AppServices,
    source: VerseRef,
    target: VerseRef,
) -> anyhow::Result<()> {
    services
        .user_cross_refs
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("База параллельных мест недоступна."))?
        .add(source, target)
}

pub fn remove_user_cross_ref(
    services: &AppServices,
    source: VerseRef,
    target: VerseRef,
) -> anyhow::Result<()> {
    services
        .user_cross_refs
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("База параллельных мест недоступна."))?
        .remove(source, target)
}
