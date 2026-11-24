//! Идентификаторы и ссылки (architecture.md §3.1).

use serde::{Deserialize, Serialize};
use std::fmt;

/// Идентификатор модуля = stem файла модуля MyBible ("RST", "strongs", "TSK").
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ModuleId(pub String);

impl fmt::Display for ModuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ModuleId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

/// Номер книги в каноне MyBible (10..=730).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BookNumber(pub i64);

/// Граница Ветхого/Нового завета в нумерации MyBible:
/// book_number < границы — ВЗ, >= — НЗ.
pub const OLD_TESTAMENT_BOUNDARY: i64 = 470;

/// Каноническая ссылка на стих или диапазон. Независима от модуля,
/// но координаты всегда в традиции нумерации какого-то конкретного модуля —
/// при переходе между модулями прогоняется через `verse_map::translate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VerseRef {
    pub book: BookNumber,
    pub chapter: u16,
    pub verse: u16,
    pub end_verse: Option<u16>,
}

impl VerseRef {
    pub fn single(book: BookNumber, chapter: u16, verse: u16) -> Self {
        Self {
            book,
            chapter,
            verse,
            end_verse: None,
        }
    }
}

/// Привязка пользовательских данных: модуль + каноническая ссылка
/// (+ позиция слова для аннотаций уровня слова). Ключ всех аннотаций.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AnchoredRef {
    pub module: ModuleId,
    pub reference: VerseRef,
    pub word_index: Option<u32>,
}
