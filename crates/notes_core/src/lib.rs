//! Infrastructure заметок (architecture.md §4.3).
//!
//! Перенос из форка (`bible_notes` минус GPUI-интеграция, этап 8):
//! файл на диске — источник истины, `notes.db` внутри хранилища — только
//! индекс (записи, заголовки, блоки, ссылки, теги, FTS5 по телу).
//!
//! Адаптации против форка (пометка в architecture.md):
//! - `bible::wikilink` перенесён сюда же (`wikilink`) — в форке он жил
//!   в bible-крейте из-за общего использования;
//! - разбор `[[bible:…]]`-целей инъецирован `parser::BibleResolver`
//!   (замыкание над bible_domain::reference::parse + mybible::lookup_book),
//!   notes_core от mybible не зависит;
//! - GPUI-глобал и rename-очередь из index.rs не перенесены (v1): события
//!   индекса — подписка `NotesIndex::subscribe`, переименование файла
//!   обрабатывается как пара Delete+Reindex.

pub mod backlinks;
pub mod frontmatter;
pub mod index;
pub mod parser;
pub mod store;
pub mod vault;
pub mod watcher;
pub mod wikilink;
