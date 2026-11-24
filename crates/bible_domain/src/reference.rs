//! Распознавание библейских ссылок из свободного текста: «John 3:16»,
//! «1 Cor 13:1-3», «Иоан 3», «Ин 3:1-5» → структурированная `Reference`.
//!
//! Порт `reference.rs` форка. **Отличие адаптации:** форк ходил напрямую в
//! `ModuleRegistry::book_index`; здесь поиск книги инъецирован замыканием
//! `lookup_book`, чтобы Domain не знал о реестре модулей. Fallback-цепочка
//! сохранена: реестр → греческая нормализация → статическая таблица алиасов
//! (book_aliases), поэтому `John 3:16` разбирается и против русского
//! реестра, и наоборот.

use crate::book_aliases;
use crate::verse::greek;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Reference {
    pub book_number: i64,
    pub chapter: u16,
    pub verse: Option<u16>,
    pub end_verse: Option<u16>,
}

/// Ключ имени книги в индексе реестра: без пробелов, lowercase.
/// Используется и при построении индекса (mybible), и при поиске.
pub fn normalize_book_key(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_whitespace() {
            continue;
        }
        for lower in ch.to_lowercase() {
            out.push(lower);
        }
    }
    out
}

/// Разбор ссылки. `lookup_book` — поиск номера книги по имени среди книг
/// загруженных модулей (mybible::ModuleRegistry::lookup_book).
/// None на любом нераспознанном вводе.
pub fn parse(input: &str, lookup_book: impl Fn(&str) -> Option<i64>) -> Option<Reference> {
    let normalised = strip_whitespace_around_separators(&collapse_whitespace(input.trim()));
    if normalised.is_empty() {
        return None;
    }

    let last_digit_run_start = normalised
        .char_indices()
        .rev()
        .take_while(|(_, ch)| {
            ch.is_ascii_digit() || matches!(ch, ':' | '-' | '\u{2013}' | '\u{2014}')
        })
        .last()
        .map(|(idx, _)| idx)?;

    let (head, tail) = normalised.split_at(last_digit_run_start);
    let head = head.trim();
    if head.is_empty() {
        return None;
    }

    let book_number = lookup_book(head)
        .or_else(|| lookup_book(&greek::normalize(head)))
        .or_else(|| {
            // Fallback: встроенная мультиязычная таблица алиасов.
            book_aliases::lookup(head)
        })?;

    let (chapter, verse, end_verse) = parse_tail(tail)?;
    Some(Reference {
        book_number,
        chapter,
        verse,
        end_verse,
    })
}

fn strip_whitespace_around_separators(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == ' ' {
            // Пробел перед разделителем — отбросить.
            if matches!(
                chars.peek(),
                Some(':') | Some('-') | Some('\u{2013}') | Some('\u{2014}')
            ) {
                continue;
            }
            // Пробел после разделителя — отбросить.
            if matches!(
                out.chars().last(),
                Some(':') | Some('-') | Some('\u{2013}') | Some('\u{2014}')
            ) {
                continue;
            }
            out.push(ch);
        } else {
            out.push(ch);
        }
    }
    out
}

fn parse_tail(tail: &str) -> Option<(u16, Option<u16>, Option<u16>)> {
    let normalised = tail.replace(['\u{2013}', '\u{2014}'], "-");
    let mut parts = normalised.splitn(2, ':');
    let chapter_str = parts.next()?.trim();
    let chapter: u16 = chapter_str.parse().ok()?;

    let Some(rest) = parts.next() else {
        return Some((chapter, None, None));
    };

    let mut verse_parts = rest.splitn(2, '-');
    let verse_str = verse_parts.next()?.trim();
    let verse: u16 = verse_str.parse().ok()?;
    let end_verse = match verse_parts.next() {
        Some(end_str) => {
            let end: u16 = end_str.trim().parse().ok()?;
            Some(end)
        }
        None => None,
    };
    Some((chapter, Some(verse), end_verse))
}

fn collapse_whitespace(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_was_space = false;
    for ch in value.chars() {
        if ch.is_whitespace() {
            if !last_was_space && !out.is_empty() {
                out.push(' ');
            }
            last_was_space = true;
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Фикстура реестра: пары (короткое, длинное, номер).
    fn lookup_with(entries: &[(&str, &str, i64)]) -> impl Fn(&str) -> Option<i64> {
        let mut index: HashMap<String, i64> = HashMap::new();
        for (short, long, number) in entries {
            index.entry(normalize_book_key(short)).or_insert(*number);
            index.entry(normalize_book_key(long)).or_insert(*number);
        }
        move |name| index.get(&normalize_book_key(name)).copied()
    }

    #[test]
    fn parses_book_chapter_verse() {
        let lookup = lookup_with(&[("John", "Gospel of John", 500)]);
        let reference = parse("John 3:16", lookup).unwrap();
        assert_eq!(reference.book_number, 500);
        assert_eq!(reference.chapter, 3);
        assert_eq!(reference.verse, Some(16));
        assert_eq!(reference.end_verse, None);
    }

    #[test]
    fn parses_book_chapter_only() {
        let lookup = lookup_with(&[("Gen", "Genesis", 10)]);
        let reference = parse("Gen 1", lookup).unwrap();
        assert_eq!(reference.book_number, 10);
        assert_eq!(reference.chapter, 1);
        assert_eq!(reference.verse, None);
    }

    #[test]
    fn parses_verse_range() {
        let lookup = lookup_with(&[("1 Cor", "1 Corinthians", 530)]);
        let reference = parse("1 Cor 13:1-3", lookup).unwrap();
        assert_eq!(reference.book_number, 530);
        assert_eq!(reference.chapter, 13);
        assert_eq!(reference.verse, Some(1));
        assert_eq!(reference.end_verse, Some(3));
    }

    #[test]
    fn parses_russian_short_name() {
        let lookup = lookup_with(&[("Ин", "Иоанн", 500)]);
        let reference = parse("Ин 3:16", lookup).unwrap();
        assert_eq!(reference.book_number, 500);
    }

    #[test]
    fn rejects_unknown_book() {
        let lookup = lookup_with(&[("John", "John", 500)]);
        assert!(parse("Foo 1:1", lookup).is_none());
    }

    #[test]
    fn rejects_garbage() {
        let lookup = lookup_with(&[]);
        assert!(parse("nonsense", lookup).is_none());
        assert!(parse("", lookup_with(&[])).is_none());
    }

    #[test]
    fn english_falls_back_to_alias_table_when_registry_is_russian_only() {
        let lookup = lookup_with(&[("Ин", "Иоанн", 500)]);
        let reference = parse("John 3:16", lookup).unwrap();
        assert_eq!(reference.book_number, 500);
    }

    #[test]
    fn russian_falls_back_to_alias_table_when_registry_is_english_only() {
        let lookup = lookup_with(&[("John", "Gospel of John", 500)]);
        let reference = parse("Ин 3:16", lookup).unwrap();
        assert_eq!(reference.book_number, 500);
    }

    #[test]
    fn handles_extra_whitespace() {
        let lookup = lookup_with(&[("Mt", "Matthew", 470)]);
        let reference = parse("  Mt   5  :  3  ", lookup).unwrap();
        assert_eq!(reference.chapter, 5);
        assert_eq!(reference.verse, Some(3));
    }
}
