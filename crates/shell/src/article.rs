//! Сценарии справочных текстов (architecture.md §5.3, таблица портов):
//! статья словаря, комментарий к главе, параллельные ссылки к стиху.
//!
//! Все выдают ViewModel с деревом `ContentBlock` (§3.7) — UI только
//! рисует и не видит сырой HTML MyBible.

use bible_domain::content::{ContentBlock, ContentInline, TextMarks};
use bible_domain::ids::{BookNumber, ModuleId, VerseRef};
use bible_domain::link::LinkTarget;

use crate::services::{AppServices, CurrentLocation};

/// Статья словаря для DictionaryTextView.
#[derive(Debug, Clone)]
pub struct ArticleViewModel {
    pub module: ModuleId,
    pub topic: String,
    pub blocks: Vec<ContentBlock>,
}

/// Сценарий «открыть статью словаря». None — темы нет в модуле.
pub fn open_dictionary_article(
    services: &AppServices,
    module: &ModuleId,
    topic: &str,
) -> Option<ArticleViewModel> {
    let raw = services.registry.lock().dictionary_entry(module, topic)?;
    let blocks = parse_module_html(&raw, module);
    Some(ArticleViewModel {
        module: module.clone(),
        topic: topic.to_string(),
        blocks,
    })
}

/// Разбор разметки MyBible в дерево содержимого — для мест, где сырой
/// текст приходит не из `dictionary_entry` (например, strong_entry
/// в карточке слова). UI вызывает только это, `mybible` напрямую не трогает.
pub fn parse_module_html(raw: &str, module: &ModuleId) -> Vec<ContentBlock> {
    mybible::html::parse_article(raw, module)
}

/// Готовый справочный текст: комментарий к главе / ссылки к стиху.
#[derive(Debug, Clone)]
pub struct ReferenceViewModel {
    pub title: String,
    pub blocks: Vec<ContentBlock>,
}

impl ReferenceViewModel {
    fn plain(title: String, body: &str) -> Self {
        Self {
            title,
            blocks: vec![ContentBlock::Paragraph(vec![ContentInline::Text {
                text: body.to_string(),
                marks: TextMarks::default(),
            }])],
        }
    }
}

/// Комментарий модуля к главе из текущего места чтения.
pub fn open_commentary(
    services: &AppServices,
    module: &ModuleId,
    label: &str,
    location: &CurrentLocation,
) -> ReferenceViewModel {
    open_commentary_range(services, module, label, location, None)
}

pub fn open_commentary_range(
    services: &AppServices,
    module: &ModuleId,
    label: &str,
    location: &CurrentLocation,
    requested: Option<(u16, u16)>,
) -> ReferenceViewModel {
    let mut title = format!("{label} — {} {}", location.book_title, location.chapter);
    if let Some((first, last)) = requested {
        title.push_str(&format!(":{first}"));
        if last > first {
            title.push_str(&format!("–{last}"));
        }
    }
    let rows =
        services
            .registry
            .lock()
            .commentary_for_chapter(module, location.book, location.chapter);
    let mut blocks = Vec::new();
    for (verse_from, verse_to, raw) in &rows {
        if requested.is_some_and(|(first, last)| {
            *verse_from != 0 && (*verse_from > last || (*verse_to).max(*verse_from) < first)
        }) {
            continue;
        }
        let anchor = if *verse_to > *verse_from {
            format!("{verse_from}–{verse_to}")
        } else {
            verse_from.to_string()
        };
        blocks.push(ContentBlock::Paragraph(vec![ContentInline::Text {
            text: anchor,
            marks: TextMarks {
                bold: true,
                ..Default::default()
            },
        }]));
        blocks.extend(mybible::html::parse_article(raw, module));
    }
    if blocks.is_empty() {
        return ReferenceViewModel::plain(title, "Нет комментария к этому месту.");
    }
    ReferenceViewModel { title, blocks }
}

/// Параллельные ссылки к стиху из текущего места чтения.
/// Имена целевых книг — из активного перевода (location.module).
pub fn open_cross_references(
    services: &AppServices,
    module: &ModuleId,
    label: &str,
    location: &CurrentLocation,
) -> ReferenceViewModel {
    let title = format!(
        "{label} — {} {}:{}",
        location.book_title, location.chapter, location.verse
    );
    let mut registry = services.registry.lock();
    let rows =
        registry.cross_references_for(module, location.book, location.chapter, location.verse);
    if rows.is_empty() {
        return ReferenceViewModel::plain(title, "Нет параллельных мест для этого стиха.");
    }
    let books = registry.books_of(&location.module);
    let items = rows
        .iter()
        .map(|&(book_to, chapter_to, verse_start, verse_end, votes)| {
            let book_name = books
                .iter()
                .find(|book| book.book_number == book_to)
                .map(|book| book.short_name.clone())
                .unwrap_or_else(|| format!("Кн. {book_to}"));
            let verses = if verse_end > verse_start {
                format!("{verse_start}–{verse_end}")
            } else {
                verse_start.to_string()
            };
            let reference = VerseRef {
                book: BookNumber(book_to),
                chapter: chapter_to as u16,
                verse: verse_start as u16,
                end_verse: (verse_end > verse_start).then_some(verse_end as u16),
            };
            vec![
                ContentInline::Link {
                    target: LinkTarget::Bible(reference),
                    label: format!("{book_name} {chapter_to}:{verses}"),
                },
                ContentInline::Text {
                    text: format!(" ({votes})"),
                    marks: TextMarks {
                        small: true,
                        ..Default::default()
                    },
                },
            ]
        })
        .collect();
    ReferenceViewModel {
        title,
        blocks: vec![ContentBlock::List {
            ordered: false,
            items,
        }],
    }
}
