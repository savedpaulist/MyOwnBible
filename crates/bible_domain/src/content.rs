//! Нормализованная модель содержимого (architecture.md §3.7).
//!
//! Контракт между парсером MyBible (Infrastructure, `mybible::html`) и
//! отображением (Presentation, `bible_ui`): UI никогда не видит сырой HTML
//! MyBible, только это дерево. Backend рендера (TextView на нормализованном
//! HTML или собственный рендерер элементов) — деталь Presentation.

use crate::link::LinkTarget;

#[derive(Debug, Clone, PartialEq)]
pub enum ContentBlock {
    Paragraph(Vec<ContentInline>),
    Heading {
        level: u8,
        content: Vec<ContentInline>,
    },
    List {
        ordered: bool,
        items: Vec<Vec<ContentInline>>,
    },
    Quote(Vec<ContentInline>),
    Table {
        head: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Rule,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ContentInline {
    Text {
        text: String,
        marks: TextMarks,
    },
    Link {
        target: LinkTarget,
        label: String,
    },
    /// Семантическая ссылка на номер Strong's (не «голый <S>»).
    StrongRef(StrongNumber),
    Break,
}

/// Номер Strong's из разметки источника.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrongNumber {
    /// 'G' (греч.) или 'H' (евр.); None — источник префикс не указал.
    pub prefix: Option<char>,
    pub number: String,
}

impl StrongNumber {
    /// Строковый вид темы словаря: «G25», «H123».
    pub fn topic(&self) -> String {
        match self.prefix {
            Some(prefix) => format!("{prefix}{}", self.number),
            None => self.number.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextMarks {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// `<sup>`/`<sub>` из источника: TextView их не поддерживает, поэтому
    /// парсер нормализует их в этот флаг (рендер — меньшим/приглушённым
    /// кеглем, backend-зависимо).
    pub small: bool,
    /// `<font color="…">`: сырое CSS-значение. TextView-backend цвет текста
    /// не поддерживает — поле сохранено для собственного рендерера.
    pub color: Option<String>,
}
