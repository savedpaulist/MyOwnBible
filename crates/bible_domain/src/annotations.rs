//! Аннотации — пользовательские данные поверх текста (architecture.md §3.6).
//!
//! Привязка всегда к паре модуль + каноническая ссылка (`AnchoredRef`),
//! плюс `word_index` для уровня слова. В «родном» модуле аннотация
//! оформляется точно; в другом переводе показывается только индикатор
//! «есть пользовательские данные» на уровне стиха.

use serde::{Deserialize, Serialize};

use crate::ids::AnchoredRef;

/// Вид аннотации. v1 — целыми стихами и словами: цветовое выделение,
/// подчёркивание, раскраска слова. Комментарии — отдельная сущность
/// (`VerseComment`), т.к. у них тело, а не стиль.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationKind {
    /// Цветовой фон стиха/слова.
    Highlight,
    /// Подчёркивание.
    Underline,
    /// Цвет текста слова.
    WordColor,
}

impl AnnotationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Highlight => "highlight",
            Self::Underline => "underline",
            Self::WordColor => "word_color",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "highlight" => Some(Self::Highlight),
            "underline" => Some(Self::Underline),
            "word_color" => Some(Self::WordColor),
            _ => None,
        }
    }
}

/// Именованная палитра выделений. Имена стабильны (хранятся в БД и в
/// session-независимых данных), конкретные HSL подбирает тема в UI.
pub const PALETTE: &[&str] = &["yellow", "green", "blue", "pink", "orange", "purple"];

/// Стиль аннотации — payload к виду. Держим именованный цвет, а не RGB:
/// палитра переживает смену темы оформления.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotationStyle {
    pub color: String,
}

impl AnnotationStyle {
    pub fn new(color: impl Into<String>) -> Self {
        Self {
            color: color.into(),
        }
    }
}

/// Одна аннотация. Уникальность — натуральным ключом
/// (anchor + kind): повторная установка того же вида на то же место
/// заменяет стиль, а не плодит дубли.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Annotation {
    pub anchor: AnchoredRef,
    pub kind: AnnotationKind,
    pub style: AnnotationStyle,
    /// Unix-секунды последнего изменения.
    pub updated_at: i64,
}

impl Annotation {
    pub fn new(anchor: AnchoredRef, kind: AnnotationKind, color: impl Into<String>) -> Self {
        Self {
            anchor,
            kind,
            style: AnnotationStyle::new(color),
            updated_at: 0,
        }
    }
}

/// Комментарий пользователя к стиху или слову (markdown-тело).
/// Разросшийся комментарий может быть вынесен в файл заметки —
/// тогда заполнен `note_relpath`, а тело остаётся аннотацией-заголовком.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerseComment {
    /// Stable identity: several notes may share the same verse anchor.
    pub id: String,
    pub anchor: AnchoredRef,
    pub body: String,
    pub note_relpath: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_roundtrip() {
        for kind in [
            AnnotationKind::Highlight,
            AnnotationKind::Underline,
            AnnotationKind::WordColor,
        ] {
            assert_eq!(AnnotationKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(AnnotationKind::parse("bogus"), None);
    }

    #[test]
    fn palette_colors_are_stable_names() {
        assert!(PALETTE.contains(&"yellow"));
        for color in PALETTE {
            assert!(color.chars().all(|c| c.is_ascii_lowercase()));
        }
    }
}
