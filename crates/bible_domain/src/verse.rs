//! Парсер текста стихов MyBible (architecture.md §3.2).
//!
//! Перенесён из форка (`crates/bible/src/verse_parser.rs`) с одним изменением
//! границы: вместо `BibleSettings` — явный `VerseParseOptions`, чтобы Domain
//! не знал о настройках приложения.
//!
//! Текст стиха в MyBible — обычный Unicode с тегами `<S>NUM</S>` (Strong's)
//! и `<m>CODE</m>` (морфология) после слова, а также критическим аппаратом
//! `{…}` / `「…」` и метками `<n>…</n>`. Парсер возвращает чистый текст и
//! боковую таблицу токенов: Strong's и морфология никогда не живут в тексте.

use std::ops::Range;

/// Слово или внутрисловный морфологический сегмент с боковыми атрибутами.
///
/// В модулях оригинальных языков одно слитное написание может содержать
/// несколько размеченных частей. Например, BHSEK+ хранит еврейский артикль
/// и основу слова как два соседних сегмента без пробела.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerseToken {
    /// Байтовый диапазон слова внутри `CleanVerse::clean_text`.
    pub range: Range<usize>,
    /// Само слово, равно `clean_text[range.clone()]`.
    pub text: String,
    /// Номер Strong's без префикса G/H (префикс — в ModuleMeta::strong_prefix).
    pub strong: Option<String>,
    /// Морфологический код как в источнике, например "V-AAI-3S".
    pub morph: Option<String>,
    /// Нулевой индекс орфографического слова среди слов своего стиха.
    /// Слитные морфологические сегменты имеют одинаковый индекс, чтобы
    /// пользовательские пословные аннотации продолжали относиться ко всему
    /// написанию и не смещались после более точного разбора.
    pub word_index_in_verse: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanVerse {
    pub clean_text: String,
    pub tokens: Vec<VerseToken>,
    /// Байтовые смещения начала каждого стиха в clean_text (для глав).
    /// Для одиночного стиха пусто.
    pub verse_starts: Vec<(u16, usize)>,
    /// Стих содержал критический аппарат, скрытый настройкой — повод
    /// показать маркер `[CRIT]` в конце строки.
    pub had_hidden_critical: bool,
}

/// Параметры парсинга, приходящие из настроек приложения.
#[derive(Debug, Clone, Copy)]
pub struct VerseParseOptions {
    pub show_verse_numbers: bool,
    pub show_strong_numbers_inline: bool,
    pub show_morphology_inline: bool,
    pub show_critical_text: bool,
}

impl Default for VerseParseOptions {
    fn default() -> Self {
        Self {
            show_verse_numbers: true,
            show_strong_numbers_inline: false,
            show_morphology_inline: false,
            show_critical_text: false,
        }
    }
}

/// Разбор одного стиха без inline-аннотаций.
pub fn parse_verse(raw: &str) -> CleanVerse {
    parse_verse_full(raw, false, false, false)
}

/// Вариант для FTS-индекса: всегда без критического аппарата, чтобы поиск
/// не находил стихи только по тексту редакционных помет.
pub fn parse_verse_for_index(raw: &str) -> CleanVerse {
    parse_verse_full(raw, false, false, false)
}

/// Разбор одного стиха с опциональными inline-маркерами ⟨STRONG⟩ / ⟨MORPH⟩
/// сразу после слова. Диапазоны токенов продолжают указывать на само слово.
pub fn parse_verse_annotated(
    raw: &str,
    show_strong_inline: bool,
    show_morph_inline: bool,
) -> CleanVerse {
    parse_verse_full(raw, show_strong_inline, show_morph_inline, true)
}

/// Полный разбор стиха. При `show_critical_text == false` всё между `{` и `}`
/// (или 「 и 」) удаляется из текста, а стих помечается `had_hidden_critical`.
pub fn parse_verse_full(
    raw: &str,
    show_strong_inline: bool,
    show_morph_inline: bool,
    show_critical_text: bool,
) -> CleanVerse {
    let mut clean_text = String::with_capacity(raw.len());
    let mut tokens: Vec<VerseToken> = Vec::new();
    let mut pending_word: Option<PendingWord> = None;
    let mut next_word_index = 0u32;
    let mut in_critical = false;
    let mut had_hidden_critical = false;

    let bytes = raw.as_bytes();
    let mut idx = 0;

    while idx < bytes.len() {
        let open_len = critical_open_len(bytes, idx);
        if open_len > 0 {
            finalize_word_with_annotation(
                &mut pending_word,
                &mut tokens,
                &mut clean_text,
                show_strong_inline,
                show_morph_inline,
            );
            if !show_critical_text {
                in_critical = true;
                had_hidden_critical = true;
            }
            idx += open_len;
            continue;
        }
        let close_len = critical_close_len(bytes, idx);
        if close_len > 0 {
            finalize_word_with_annotation(
                &mut pending_word,
                &mut tokens,
                &mut clean_text,
                show_strong_inline,
                show_morph_inline,
            );
            in_critical = false;
            idx += close_len;
            continue;
        }
        if in_critical {
            idx += utf8_char_len(bytes[idx]).max(1);
            continue;
        }

        if bytes[idx] == b'<' {
            if let Some(scan) = parse_tag(bytes, idx) {
                match scan.tag {
                    Tag::OpenStrong => {
                        let content = std::str::from_utf8(&bytes[scan.content])
                            .unwrap_or("")
                            .trim();
                        if !content.is_empty() {
                            attach_strong(&mut pending_word, &mut tokens, content);
                        }
                    }
                    Tag::OpenMorph => {
                        let content = std::str::from_utf8(&bytes[scan.content])
                            .unwrap_or("")
                            .trim();
                        if !content.is_empty() {
                            attach_morph(&mut pending_word, &mut tokens, content);
                        }
                    }
                    Tag::OpenNote => {
                        finalize_word_with_annotation(
                            &mut pending_word,
                            &mut tokens,
                            &mut clean_text,
                            show_strong_inline,
                            show_morph_inline,
                        );
                        if show_critical_text {
                            // Метки не вложены в MyBible — рекурсия ограничена.
                            let inner = std::str::from_utf8(&bytes[scan.content]).unwrap_or("");
                            let inner_parsed = parse_verse_full(
                                inner,
                                show_strong_inline,
                                show_morph_inline,
                                show_critical_text,
                            );
                            let token_offset = clean_text.len();
                            clean_text.push_str(&inner_parsed.clean_text);
                            let inner_word_count = inner_parsed
                                .tokens
                                .iter()
                                .map(|token| token.word_index_in_verse)
                                .max()
                                .map_or(0, |last| last + 1);
                            for mut tok in inner_parsed.tokens {
                                tok.range.start += token_offset;
                                tok.range.end += token_offset;
                                tok.word_index_in_verse += next_word_index;
                                tokens.push(tok);
                            }
                            next_word_index += inner_word_count;
                        } else {
                            had_hidden_critical = true;
                        }
                    }
                    Tag::OtherOpen | Tag::OtherClose => {}
                }
                idx = scan.end;
                continue;
            }
        }

        let ch_len = utf8_char_len(bytes[idx]);
        let ch_end = (idx + ch_len).min(bytes.len());
        let chunk = &raw[idx..ch_end];
        let ch = chunk.chars().next().unwrap_or('\u{FFFD}');

        if is_word_char(ch) {
            // MyBible-модули оригинальных языков могут размечать несколько
            // частей одного слитного написания независимо:
            // `ה<S>9006</S><m>art</m>שמים<S>8064</S>`. Новый буквенный
            // фрагмент после закрытой аннотации начинает следующий сегмент,
            // даже если между ними нет пробела.
            let continued_word_index = pending_word
                .as_ref()
                .filter(|word| word.has_inline_annotation)
                .map(|word| word.word_index_in_verse);
            if continued_word_index.is_some() {
                finalize_word_with_annotation(
                    &mut pending_word,
                    &mut tokens,
                    &mut clean_text,
                    show_strong_inline,
                    show_morph_inline,
                );
            }
            if pending_word.is_none() {
                let word_index_in_verse = continued_word_index.unwrap_or_else(|| {
                    let index = next_word_index;
                    next_word_index += 1;
                    index
                });
                pending_word = Some(PendingWord {
                    start: clean_text.len(),
                    strong: None,
                    morph: None,
                    has_inline_annotation: false,
                    word_index_in_verse,
                });
            }
            clean_text.push_str(chunk);
        } else {
            finalize_word_with_annotation(
                &mut pending_word,
                &mut tokens,
                &mut clean_text,
                show_strong_inline,
                show_morph_inline,
            );
            clean_text.push_str(chunk);
        }

        idx = ch_end;
    }

    finalize_word_with_annotation(
        &mut pending_word,
        &mut tokens,
        &mut clean_text,
        show_strong_inline,
        show_morph_inline,
    );

    CleanVerse {
        clean_text,
        tokens,
        verse_starts: Vec::new(),
        had_hidden_critical,
    }
}

/// Угловые скобки `「`/`」` (U+300C/U+300D) — вторая нотация критического
/// аппарата в греческих модулях MyBible, рядом с ASCII `{`/`}`.
const CORNER_OPEN: &[u8] = "\u{300C}".as_bytes();
const CORNER_CLOSE: &[u8] = "\u{300D}".as_bytes();

fn critical_open_len(bytes: &[u8], idx: usize) -> usize {
    if bytes[idx] == b'{' {
        1
    } else if bytes_match_at(bytes, idx, CORNER_OPEN) {
        CORNER_OPEN.len()
    } else {
        0
    }
}

fn critical_close_len(bytes: &[u8], idx: usize) -> usize {
    if bytes[idx] == b'}' {
        1
    } else if bytes_match_at(bytes, idx, CORNER_CLOSE) {
        CORNER_CLOSE.len()
    } else {
        0
    }
}

fn bytes_match_at(bytes: &[u8], idx: usize, needle: &[u8]) -> bool {
    bytes
        .get(idx..idx + needle.len())
        .is_some_and(|slice| slice == needle)
}

/// Маркеры конца стиха: есть перекрёстные ссылки / комментарии / цепочки /
/// теги. В новом UI рисуются как элементы в конце строки стиха (этап 6),
/// в текст не вставляются — функция лишь сообщает флаги.
#[derive(Debug, Clone, Copy, Default)]
pub struct VerseMarkers {
    pub has_xrefs: bool,
    pub has_comments: bool,
    pub has_chains: bool,
    pub has_tags: bool,
}

/// Сборка текста главы из пар `(номер стиха, сырой текст)` по возрастанию.
/// Диапазоны токенов — абсолютные по `clean_text`; `verse_starts` позволяет
/// бинарным поиском отвечать «на каком стихе смещение».
pub fn parse_chapter(verses: &[(u32, String)], options: &VerseParseOptions) -> CleanVerse {
    parse_chapter_with_markers(verses, options, |_| VerseMarkers::default())
}

/// Как `parse_chapter`, но с текстовыми маркерами конца стиха (`(xref)` и пр.).
/// В BibleOWN UI маркеры рисуются элементами; функция сохранена для
/// совместимости и текстовых сценариев (копирование, экспорт).
pub fn parse_chapter_with_markers(
    verses: &[(u32, String)],
    options: &VerseParseOptions,
    markers_for: impl Fn(u16) -> VerseMarkers,
) -> CleanVerse {
    let mut clean_text = String::new();
    let mut tokens: Vec<VerseToken> = Vec::new();
    let mut verse_starts: Vec<(u16, usize)> = Vec::with_capacity(verses.len());

    for (i, (verse_number, raw)) in verses.iter().enumerate() {
        if i > 0 {
            clean_text.push('\n');
        }
        let line_start = clean_text.len();
        let verse_u16 = *verse_number as u16;
        verse_starts.push((verse_u16, line_start));
        if options.show_verse_numbers {
            clean_text.push_str(&format!("{verse_number}  "));
        }
        let body_offset = clean_text.len();
        let parsed = parse_verse_full(
            raw,
            options.show_strong_numbers_inline,
            options.show_morphology_inline,
            options.show_critical_text,
        );
        let had_hidden_critical = parsed.had_hidden_critical;
        clean_text.push_str(&parsed.clean_text);
        for token in parsed.tokens {
            tokens.push(VerseToken {
                range: (token.range.start + body_offset)..(token.range.end + body_offset),
                ..token
            });
        }

        let markers = markers_for(verse_u16);
        if markers.has_xrefs {
            clean_text.push_str("  (xref)");
        }
        if markers.has_comments {
            clean_text.push_str("  (note)");
        }
        if markers.has_chains {
            clean_text.push_str("  (chain)");
        }
        if markers.has_tags {
            clean_text.push_str("  (tag)");
        }
        // `[CRIT]` — всегда последний маркер строки.
        if had_hidden_critical {
            clean_text.push_str("  [CRIT]");
        }
    }

    CleanVerse {
        clean_text,
        tokens,
        verse_starts,
        had_hidden_critical: false,
    }
}

struct PendingWord {
    start: usize,
    strong: Option<String>,
    morph: Option<String>,
    /// Встретился хотя бы один закрытый `<S>`/`<m>` после текущего текста.
    /// Отдельный флаг нужен для служебных BHSEK-кодов, которые намеренно не
    /// сохраняются как настоящие номера Strong's.
    has_inline_annotation: bool,
    word_index_in_verse: u32,
}

fn finalize_word_with_annotation(
    pending: &mut Option<PendingWord>,
    tokens: &mut Vec<VerseToken>,
    clean_text: &mut String,
    show_strong_inline: bool,
    show_morph_inline: bool,
) {
    let Some(word) = pending.take() else { return };
    let end = clean_text.len();
    if end <= word.start {
        return;
    }
    let strong = word.strong.clone();
    let morph = word.morph.clone();
    tokens.push(VerseToken {
        range: word.start..end,
        text: clean_text[word.start..end].to_string(),
        strong: word.strong,
        morph: word.morph,
        word_index_in_verse: word.word_index_in_verse,
    });

    let visible_strong = strong.as_deref().filter(|_| show_strong_inline);
    let visible_morph = morph.as_deref().filter(|_| show_morph_inline);
    match (visible_strong, visible_morph) {
        (Some(s), Some(m)) => {
            clean_text.push('\u{27E8}');
            clean_text.push_str(s);
            clean_text.push('\u{00B7}');
            clean_text.push_str(m);
            clean_text.push('\u{27E9}');
        }
        (Some(s), None) => {
            clean_text.push('\u{27E8}');
            clean_text.push_str(s);
            clean_text.push('\u{27E9}');
        }
        (None, Some(m)) => {
            clean_text.push('\u{27E8}');
            clean_text.push_str(m);
            clean_text.push('\u{27E9}');
        }
        (None, None) => {}
    }
}

fn attach_strong(pending: &mut Option<PendingWord>, tokens: &mut [VerseToken], content: &str) {
    let stripped = strip_strong_prefix(content);
    if let Some(word) = pending.as_mut() {
        word.has_inline_annotation = true;
        if word.strong.is_none() && !is_bhsek_grammatical_code(&stripped) {
            word.strong = Some(stripped);
        }
    } else if let Some(last) = tokens.last_mut() {
        if last.strong.is_none() && !is_bhsek_grammatical_code(&stripped) {
            last.strong = Some(stripped);
        }
    }
}

fn attach_morph(pending: &mut Option<PendingWord>, tokens: &mut [VerseToken], content: &str) {
    let owned = content.to_string();
    if let Some(word) = pending.as_mut() {
        word.has_inline_annotation = true;
        if word.morph.is_none() {
            word.morph = Some(owned);
        }
    } else if let Some(last) = tokens.last_mut() {
        if last.morph.is_none() {
            last.morph = Some(owned);
        }
    }
}

/// BHSEK+ использует диапазон 9001–9009 не как статьи Strong's, а как
/// служебные обозначения слитных еврейских частиц (предлоги, союз,
/// определённый артикль и т. п.). Их значение уже задано соседним `<m>`;
/// поиск `H9006` в обычном словаре всегда приводит к пустой статье.
fn is_bhsek_grammatical_code(value: &str) -> bool {
    value
        .parse::<u16>()
        .is_ok_and(|number| (9001..=9009).contains(&number))
}

fn strip_strong_prefix(content: &str) -> String {
    let trimmed = content.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    if trimmed.is_empty() {
        content.to_string()
    } else {
        trimmed.to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tag {
    OpenStrong,
    OpenMorph,
    OpenNote,
    OtherOpen,
    OtherClose,
}

struct TagScan {
    tag: Tag,
    content: Range<usize>,
    end: usize,
}

/// Разбор тега в позиции `at` (там `<`). `None` — только когда нет `>` вовсе.
fn parse_tag(bytes: &[u8], at: usize) -> Option<TagScan> {
    debug_assert_eq!(bytes[at], b'<');
    let close_offset = bytes[at + 1..].iter().position(|&b| b == b'>')?;
    let open_end = at + 1 + close_offset + 1;
    let inside = &bytes[at + 1..at + 1 + close_offset];

    if inside.starts_with(b"/") {
        return Some(TagScan {
            tag: Tag::OtherClose,
            content: 0..0,
            end: open_end,
        });
    }

    let needle: &[u8];
    let tag: Tag;
    match inside {
        b"S" => {
            needle = b"</S>";
            tag = Tag::OpenStrong;
        }
        b"m" => {
            needle = b"</m>";
            tag = Tag::OpenMorph;
        }
        b"n" => {
            needle = b"</n>";
            tag = Tag::OpenNote;
        }
        _ => {
            return Some(TagScan {
                tag: Tag::OtherOpen,
                content: 0..0,
                end: open_end,
            });
        }
    }

    let content_start = open_end;
    let needle_len = needle.len();
    let close_offset = bytes[content_start..]
        .windows(needle_len)
        .position(|window| window == needle)?;
    let content_end = content_start + close_offset;
    let after = content_end + needle_len;
    Some(TagScan {
        tag,
        content: content_start..content_end,
        end: after,
    })
}

fn utf8_char_len(first_byte: u8) -> usize {
    if first_byte < 0x80 {
        1
    } else if first_byte < 0xC0 {
        1
    } else if first_byte < 0xE0 {
        2
    } else if first_byte < 0xF0 {
        3
    } else {
        4
    }
}

fn is_word_char(ch: char) -> bool {
    // Combining marks are part of the preceding grapheme, not separators.
    // This matters especially for Hebrew niqqud/cantillation and decomposed
    // Greek accents: splitting them out creates detached bidi fragments and
    // makes Strong's token ranges point at only part of a written word.
    ch.is_alphanumeric()
        || crate::normalize::is_combining_mark(ch)
        || ch == '\u{0027}'
        || matches!(ch, '\u{02BC}' | '\u{2019}')
}

/// Диакритико-независимая форма слова (греч./евр.) — для поиска и hover.
pub mod greek {
    pub use crate::normalize::normalize;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options_full(
        show_verse_numbers: bool,
        show_strong_numbers_inline: bool,
        show_morphology_inline: bool,
    ) -> VerseParseOptions {
        VerseParseOptions {
            show_verse_numbers,
            show_strong_numbers_inline,
            show_morphology_inline,
            show_critical_text: true,
        }
    }

    #[test]
    fn strips_strong_and_morph_tags() {
        let raw = "Ἐν<S>1722</S><m>PREP</m> ἀρχῇ<S>746</S><m>N-DSF</m>";
        let parsed = parse_verse(raw);
        assert_eq!(parsed.clean_text, "Ἐν ἀρχῇ");
        assert_eq!(parsed.tokens.len(), 2);
        assert_eq!(parsed.tokens[0].text, "Ἐν");
        assert_eq!(parsed.tokens[0].strong.as_deref(), Some("1722"));
        assert_eq!(parsed.tokens[0].morph.as_deref(), Some("PREP"));
        assert_eq!(parsed.tokens[1].text, "ἀρχῇ");
        assert_eq!(parsed.tokens[1].strong.as_deref(), Some("746"));
        assert_eq!(parsed.tokens[1].morph.as_deref(), Some("N-DSF"));
        assert_eq!(
            &parsed.clean_text[parsed.tokens[0].range.clone()],
            parsed.tokens[0].text
        );
        assert_eq!(
            &parsed.clean_text[parsed.tokens[1].range.clone()],
            parsed.tokens[1].text
        );
    }

    #[test]
    fn handles_plain_text_without_tags() {
        let raw = "In the beginning";
        let parsed = parse_verse(raw);
        assert_eq!(parsed.clean_text, "In the beginning");
        assert_eq!(parsed.tokens.len(), 3);
        assert_eq!(parsed.tokens[0].text, "In");
        assert_eq!(parsed.tokens[1].text, "the");
        assert_eq!(parsed.tokens[2].text, "beginning");
    }

    #[test]
    fn drops_unknown_tags() {
        let raw = "<i>Word</i><S>1</S>";
        let parsed = parse_verse(raw);
        assert_eq!(parsed.clean_text, "Word");
        assert_eq!(parsed.tokens.len(), 1);
        assert_eq!(parsed.tokens[0].text, "Word");
        assert_eq!(parsed.tokens[0].strong.as_deref(), Some("1"));
    }

    #[test]
    fn strong_with_letter_prefix_is_stripped() {
        let raw = "Word<S>G1234</S>";
        let parsed = parse_verse(raw);
        assert_eq!(parsed.tokens[0].strong.as_deref(), Some("1234"));
    }

    #[test]
    fn parse_chapter_uses_verse_numbers_when_enabled() {
        let verses = vec![
            (1u32, "Word<S>1</S> two<S>2</S>".to_string()),
            (2u32, "More<S>3</S>".to_string()),
        ];
        let parsed = parse_chapter(&verses, &options_full(true, false, false));
        assert_eq!(parsed.clean_text, "1  Word two\n2  More");
        assert_eq!(parsed.tokens.len(), 3);
        let first = &parsed.tokens[0];
        assert_eq!(&parsed.clean_text[first.range.clone()], "Word");
        let last = parsed.tokens.last().unwrap();
        assert_eq!(&parsed.clean_text[last.range.clone()], "More");
    }

    #[test]
    fn parse_chapter_skips_verse_numbers_when_disabled() {
        let verses = vec![(1u32, "Word<S>1</S>".to_string())];
        let parsed = parse_chapter(&verses, &options_full(false, false, false));
        assert_eq!(parsed.clean_text, "Word");
        assert_eq!(parsed.tokens.len(), 1);
        assert_eq!(parsed.tokens[0].range, 0..4);
    }

    #[test]
    fn parse_chapter_appends_inline_strong_when_enabled() {
        let verses = vec![(1u32, "Word<S>1234</S> two<S>5</S>".to_string())];
        let parsed = parse_chapter(&verses, &options_full(false, true, false));
        assert_eq!(
            parsed.clean_text,
            "Word\u{27E8}1234\u{27E9} two\u{27E8}5\u{27E9}"
        );
        assert_eq!(parsed.tokens.len(), 2);
        assert_eq!(&parsed.clean_text[parsed.tokens[0].range.clone()], "Word");
        assert_eq!(&parsed.clean_text[parsed.tokens[1].range.clone()], "two");
    }

    #[test]
    fn parse_chapter_appends_inline_morph_when_enabled() {
        let verses = vec![(1u32, "Word<S>1</S><m>N-NSM</m>".to_string())];
        let parsed = parse_chapter(&verses, &options_full(false, false, true));
        assert_eq!(parsed.clean_text, "Word\u{27E8}N-NSM\u{27E9}");
        assert_eq!(parsed.tokens.len(), 1);
        assert_eq!(&parsed.clean_text[parsed.tokens[0].range.clone()], "Word");
    }

    #[test]
    fn parse_chapter_appends_strong_and_morph_combined() {
        let verses = vec![(1u32, "Word<S>1</S><m>N-NSM</m>".to_string())];
        let parsed = parse_chapter(&verses, &options_full(false, true, true));
        assert_eq!(parsed.clean_text, "Word\u{27E8}1\u{00B7}N-NSM\u{27E9}");
        assert_eq!(&parsed.clean_text[parsed.tokens[0].range.clone()], "Word");
    }

    #[test]
    fn hebrew_niqqud_and_cantillation_stay_inside_word_tokens() {
        let source = "בְּרֵאשִׁית בָּרָא אֱלֹהִים";
        let parsed = parse_verse_full(source, false, false, false);

        assert_eq!(parsed.clean_text, source);
        let words: Vec<&str> = parsed
            .tokens
            .iter()
            .map(|token| &parsed.clean_text[token.range.clone()])
            .collect();
        assert_eq!(words, ["בְּרֵאשִׁית", "בָּרָא", "אֱלֹהִים"]);
        assert_eq!(parsed.tokens.len(), 3);
    }

    #[test]
    fn bhsek_splits_article_from_lexeme_and_ignores_pseudo_strong() {
        let parsed = parse_verse("ה<S>9006</S><m>art</m>שמים<S>8064</S><m>n.mp.a</m>");

        assert_eq!(parsed.clean_text, "השמים");
        assert_eq!(parsed.tokens.len(), 2);
        assert_eq!(parsed.tokens[0].text, "ה");
        assert_eq!(parsed.tokens[0].range, 0..2);
        assert_eq!(parsed.tokens[0].strong, None);
        assert_eq!(parsed.tokens[0].morph.as_deref(), Some("art"));
        assert_eq!(parsed.tokens[1].text, "שמים");
        assert_eq!(parsed.tokens[1].range, 2..10);
        assert_eq!(parsed.tokens[1].strong.as_deref(), Some("8064"));
        assert_eq!(parsed.tokens[1].morph.as_deref(), Some("n.mp.a"));
        assert_eq!(parsed.tokens[0].word_index_in_verse, 0);
        assert_eq!(parsed.tokens[1].word_index_in_verse, 0);
    }

    #[test]
    fn bhsek_keeps_multiple_joined_prefixes_as_independent_segments() {
        let parsed =
            parse_verse("ו<S>9005</S><m>conj</m>ב<S>9001</S><m>prep</m>ארץ<S>776</S><m>n.fs</m>");

        assert_eq!(parsed.clean_text, "ובארץ");
        assert_eq!(
            parsed
                .tokens
                .iter()
                .map(|token| (
                    token.text.as_str(),
                    token.strong.as_deref(),
                    token.morph.as_deref()
                ))
                .collect::<Vec<_>>(),
            vec![
                ("ו", None, Some("conj")),
                ("ב", None, Some("prep")),
                ("ארץ", Some("776"), Some("n.fs")),
            ]
        );
        assert_eq!(parsed.tokens[0].range.end, parsed.tokens[1].range.start);
        assert_eq!(parsed.tokens[1].range.end, parsed.tokens[2].range.start);
        assert!(
            parsed
                .tokens
                .iter()
                .all(|token| token.word_index_in_verse == 0)
        );
    }

    #[test]
    fn bhsek_joined_segments_do_not_shift_following_word_index() {
        let parsed =
            parse_verse("ה<S>9006</S><m>art</m>ארץ<S>776</S><m>n.fs</m> טוב<S>2896</S><m>a.ms</m>");

        assert_eq!(parsed.tokens.len(), 3);
        assert_eq!(parsed.tokens[0].word_index_in_verse, 0);
        assert_eq!(parsed.tokens[1].word_index_in_verse, 0);
        assert_eq!(parsed.tokens[2].word_index_in_verse, 1);
    }

    #[test]
    fn bhsek_inline_annotations_stay_between_joined_segments() {
        let parsed = parse_verse_full(
            "ה<S>9006</S><m>art</m>ארץ<S>776</S><m>n.fs</m>",
            true,
            true,
            false,
        );

        assert_eq!(parsed.clean_text, "ה⟨art⟩ארץ⟨776·n.fs⟩");
        assert_eq!(&parsed.clean_text[parsed.tokens[0].range.clone()], "ה");
        assert_eq!(&parsed.clean_text[parsed.tokens[1].range.clone()], "ארץ");
    }

    #[test]
    fn decomposed_greek_accents_stay_inside_word_tokens() {
        let source = "λογ\u{03CC}ς α\u{0301}";
        let parsed = parse_verse_full(source, false, false, false);
        let words: Vec<&str> = parsed
            .tokens
            .iter()
            .map(|token| &parsed.clean_text[token.range.clone()])
            .collect();
        assert_eq!(words, ["λογ\u{03CC}ς", "α\u{0301}"]);
    }

    #[test]
    fn critical_text_visible_by_default() {
        let parsed = parse_verse_full("Lord{God}<S>1</S>", false, false, true);
        assert_eq!(parsed.clean_text, "LordGod");
    }

    #[test]
    fn critical_text_hidden_drops_brace_contents() {
        let parsed = parse_verse_full("Lord{God}<S>1</S>", false, false, false);
        assert_eq!(parsed.clean_text, "Lord");
        assert!(parsed.had_hidden_critical);
        assert_eq!(parsed.tokens.len(), 1);
        assert_eq!(parsed.tokens[0].text, "Lord");
    }

    #[test]
    fn critical_text_hidden_handles_internal_punctuation() {
        let parsed = parse_verse_full("Alpha {Apparatus phrase} Omega", false, false, false);
        assert_eq!(parsed.clean_text, "Alpha  Omega");
        assert!(parsed.had_hidden_critical);
    }

    #[test]
    fn critical_text_hidden_drops_corner_bracket_contents() {
        let parsed = parse_verse_full("τῶν 「δαιμονίων」 ἄρχοντι", false, false, false);
        assert_eq!(parsed.clean_text, "τῶν  ἄρχοντι");
        assert!(parsed.had_hidden_critical);
    }

    #[test]
    fn critical_text_visible_keeps_corner_bracket_contents() {
        let parsed = parse_verse_full("τῶν 「δαιμονίων」 ἄρχοντι", false, false, true);
        assert_eq!(parsed.clean_text, "τῶν δαιμονίων ἄρχοντι");
        assert!(!parsed.had_hidden_critical);
    }

    #[test]
    fn critical_text_hidden_collapses_note_label_with_brace_block() {
        let raw = "καὶ <n>NA27/UBS4 variant:</n> { καὶ ἀκολουθοῦντες | οἱ δὲ ἀκολουθοῦντες } λοιπά";
        let parsed = parse_verse_full(raw, false, false, false);
        assert_eq!(parsed.clean_text, "καὶ   λοιπά");
        assert!(parsed.had_hidden_critical);
    }

    #[test]
    fn critical_text_visible_keeps_note_label_inline() {
        let raw = "καὶ <n>variant:</n> { Alpha | Beta } end";
        let parsed = parse_verse_full(raw, false, false, true);
        assert_eq!(parsed.clean_text, "καὶ variant:  Alpha | Beta  end");
        assert!(!parsed.had_hidden_critical);
    }

    #[test]
    fn crit_marker_appended_at_verse_end_after_xref() {
        let verses = vec![(1u32, "Lord{God} reigns".to_string())];
        let mut options = options_full(true, false, false);
        options.show_critical_text = false;
        let parsed = parse_chapter_with_markers(&verses, &options, |_| VerseMarkers {
            has_xrefs: true,
            ..Default::default()
        });
        assert_eq!(parsed.clean_text, "1  Lord reigns  (xref)  [CRIT]");
    }

    #[test]
    fn crit_marker_absent_when_critical_text_visible() {
        let verses = vec![(1u32, "Lord{God} reigns".to_string())];
        let parsed = parse_chapter(&verses, &options_full(true, false, false));
        assert_eq!(parsed.clean_text, "1  LordGod reigns");
        assert!(!parsed.clean_text.contains("[CRIT]"));
    }
}
