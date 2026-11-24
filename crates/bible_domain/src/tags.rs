//! Теги стихов — именованные цветные группы канонических мест
//! (перенос tags/verse_tags форка, этап 21 polish-plan.md).
//!
//! Привязка модуль-независимая (как у цепочек): тег ставится на
//! каноническое место (book, chapter, verse) и виден в любом переводе.
//! Цвет — имя из палитры `annotations::PALETTE`: имена стабильны
//! (хранятся в БД), конкретные HSL подбирает тема в UI.

use serde::{Deserialize, Serialize};

/// Именованная группа стихов. `id` назначает БД (autoincrement) и
/// он стабилен: на него ссылаются строки verse_tags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub id: i64,
    pub name: String,
    /// Имя цвета из палитры `crate::annotations::PALETTE`.
    pub color: String,
}

impl Tag {
    pub fn new(id: i64, name: impl Into<String>, color: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            color: color.into(),
        }
    }
}

/// Цвет нового тега, пока пользователь не выбрал другой.
pub const DEFAULT_COLOR: &str = "yellow";

/// Допустимое имя цвета — только из палитры. Неизвестные имена из
/// старых данных не отбрасываем (UI покажет их акцентом темы), но
/// создавать новые теги с ними нельзя.
pub fn is_valid_color(name: &str) -> bool {
    crate::annotations::PALETTE.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_color_is_in_palette() {
        assert!(is_valid_color(DEFAULT_COLOR));
        assert!(!is_valid_color("magenta"));
    }
}
