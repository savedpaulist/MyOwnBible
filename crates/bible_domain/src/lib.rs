//! Domain: чистые типы и правила предметной области.
//!
//! Запрещено (проверяется ревью и `cargo tree`): gpui, gpui-kit, sqlez,
//! notify, walkdir и любое I/O. Здесь только типы и чистые функции.
//! См. architecture.md §3.

pub mod annotations;
pub mod book_aliases;
pub mod chains;
pub mod content;
pub mod ids;
pub mod link;
pub mod normalize;
pub mod ports;
pub mod reference;
pub mod search;
pub mod tags;
pub mod verse;
