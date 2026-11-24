//! Модель поискового запроса (architecture.md §3.5).
//!
//! **v1 — сознательное упрощение форкового DSL** (пометка в
//! architecture.md): вместо AST с lemma/morph/domain/freq-условиями —
//! перевод пользовательского ввода напрямую в FTS5 MATCH. Булева логика
//! (неявное AND, OR, NOT/-минус), фразы в кавычках, префикс `*`,
//! скобки — всё это FTS5 исполняет сам по стиховому индексу
//! (mybible::search_index). Strong's-номера доступны как обычные слова
//! («G25»), потому что индекс их хранит токенами.

use crate::ids::OLD_TESTAMENT_BOUNDARY;

/// Корпус поиска — фильтр канона (UI: «везде / ВЗ / НЗ»).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Corpus {
    #[default]
    All,
    OldTestament,
    NewTestament,
}

impl Corpus {
    /// Диапазон book_number включительно; None — весь канон.
    pub fn book_range(self) -> Option<(i64, i64)> {
        match self {
            Self::All => None,
            Self::OldTestament => Some((0, OLD_TESTAMENT_BOUNDARY - 1)),
            Self::NewTestament => Some((OLD_TESTAMENT_BOUNDARY, i64::MAX)),
        }
    }
}

/// Разобранный запрос: готовое FTS5-выражение + слова для подсветки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    /// Аргумент FTS5 MATCH.
    pub match_expr: String,
    /// Слова запроса (lowercase, без `*`/кавычек) — для подсветки
    /// совпадений в тексте результата.
    pub highlight_terms: Vec<String>,
}

/// Разбирает пользовательский ввод в FTS5-выражение.
/// None — ввод пуст или не содержит ни одного слова.
pub fn parse_query(input: &str) -> Option<SearchQuery> {
    let tokens = lex(input);
    let has_words = tokens
        .iter()
        .any(|token| matches!(token, Tok::Word(_) | Tok::Phrase(_)));
    if !has_words {
        return None;
    }
    let tokens = balance_parens(tokens);
    let (match_expr, highlight_terms) = emit(&tokens);
    if match_expr.is_empty() {
        return None;
    }
    Some(SearchQuery {
        match_expr,
        highlight_terms,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Word(String),
    Phrase(String),
    Or,
    Not,
    LParen,
    RParen,
}

/// Лексер: кавычки ("" — экранированная), скобки, OR/NOT/AND (заглавные),
/// -минус перед словом. Незакрытая кавычка — слова идут как обычные.
fn lex(input: &str) -> Vec<Tok> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&ch) = chars.peek() {
        match ch {
            ch if ch.is_whitespace() => {
                chars.next();
            }
            '"' => {
                chars.next();
                let mut phrase = String::new();
                let mut closed = false;
                while let Some(ch) = chars.next() {
                    if ch == '"' {
                        if chars.peek() == Some(&'"') {
                            chars.next();
                            phrase.push('"');
                        } else {
                            closed = true;
                            break;
                        }
                    } else {
                        phrase.push(ch);
                    }
                }
                if closed {
                    if !phrase.trim().is_empty() {
                        tokens.push(Tok::Phrase(phrase));
                    }
                } else {
                    tokens.extend(
                        phrase
                            .split_whitespace()
                            .map(|word| Tok::Word(word.to_string())),
                    );
                }
            }
            '(' => {
                chars.next();
                tokens.push(Tok::LParen);
            }
            ')' => {
                chars.next();
                tokens.push(Tok::RParen);
            }
            _ => {
                let mut word = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch.is_whitespace() || matches!(ch, '"' | '(' | ')') {
                        break;
                    }
                    word.push(ch);
                    chars.next();
                }
                match word.as_str() {
                    // FTS5 читает ключевые слова только заглавными.
                    "OR" => tokens.push(Tok::Or),
                    "NOT" => tokens.push(Tok::Not),
                    // AND неявный — опускаем.
                    "AND" => {}
                    word_str if word_str.starts_with('-') && word_str.len() > 1 => {
                        tokens.push(Tok::Not);
                        tokens.push(Tok::Word(word_str[1..].to_string()));
                    }
                    _ => tokens.push(Tok::Word(word)),
                }
            }
        }
    }
    tokens
}

/// Отбрасывает скобки без пары (FTS5 непарные скобки не простит).
fn balance_parens(tokens: Vec<Tok>) -> Vec<Tok> {
    let mut depth = 0i32;
    let mut kept: Vec<Tok> = Vec::with_capacity(tokens.len());
    // Открывающие держим во временном буфере: в вывод попадают, только
    // если для них нашлась закрывающая.
    let mut pending_open = 0usize;
    for token in tokens {
        match token {
            Tok::LParen => {
                pending_open += 1;
                depth += 1;
                kept.push(token);
            }
            Tok::RParen => {
                if depth > 0 {
                    depth -= 1;
                    pending_open -= 1;
                    kept.push(token);
                }
            }
            other => kept.push(other),
        }
    }
    if pending_open > 0 {
        kept.retain(|token| !matches!(token, Tok::LParen));
    }
    kept
}

/// Сериализация в FTS5-выражение + сбор слов для подсветки.
fn emit(tokens: &[Tok]) -> (String, Vec<String>) {
    let mut out = String::new();
    let mut terms: Vec<String> = Vec::new();
    for token in tokens {
        match token {
            Tok::Word(word) => {
                let (word, prefix) = match word.strip_suffix('*') {
                    Some(stripped) => (stripped, true),
                    None => (word.as_str(), false),
                };
                if word.is_empty() {
                    continue;
                }
                push_word(&mut out, word, prefix);
                push_term(word, &mut terms);
            }
            Tok::Phrase(phrase) => {
                out.push('"');
                out.push_str(&phrase.replace('"', "\"\""));
                out.push_str("\" ");
                for word in phrase.split_whitespace() {
                    let word = word.trim_matches(|ch: char| !ch.is_alphanumeric());
                    push_term(word, &mut terms);
                }
            }
            Tok::Or => out.push_str("OR "),
            Tok::Not => out.push_str("NOT "),
            Tok::LParen => out.push_str("( "),
            Tok::RParen => out.push_str(") "),
        }
    }
    (out.trim().to_string(), terms)
}

/// Слово в список подсветки (lowercase, без дублей).
fn push_term(word: &str, terms: &mut Vec<String>) {
    let term = word.to_lowercase();
    if !term.is_empty() && !terms.contains(&term) {
        terms.push(term);
    }
}

/// Слово в FTS5: чисто буквенно-цифровое — как есть (с `*` для префикса);
/// со знаками (дефис и т.п.) — в кавычках, чтобы не сломать грамматику.
fn push_word(out: &mut String, word: &str, prefix: bool) {
    if word.chars().all(|ch| ch.is_alphanumeric() || ch == '_') {
        out.push_str(word);
        if prefix {
            out.push('*');
        }
        out.push(' ');
    } else {
        out.push('"');
        out.push_str(&word.replace('"', "\"\""));
        out.push('"');
        if prefix {
            out.push_str(" *");
        }
        out.push(' ');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> SearchQuery {
        parse_query(input).expect("query should parse")
    }

    #[test]
    fn words_and_prefix() {
        let query = parse("возлюбил* мир");
        assert_eq!(query.match_expr, "возлюбил* мир");
        assert_eq!(query.highlight_terms, vec!["возлюбил", "мир"]);
    }

    #[test]
    fn phrase() {
        let query = parse("\"благодать и истина\" мир");
        assert_eq!(query.match_expr, "\"благодать и истина\" мир");
        assert!(query.highlight_terms.contains(&"истина".to_string()));
    }

    #[test]
    fn boolean_operators() {
        assert_eq!(parse("мир OR вечность").match_expr, "мир OR вечность");
        assert_eq!(parse("мир NOT суд").match_expr, "мир NOT суд");
        assert_eq!(parse("мир -суд").match_expr, "мир NOT суд");
        // AND неявный.
        assert_eq!(parse("мир AND суд").match_expr, "мир суд");
    }

    #[test]
    fn parentheses() {
        assert_eq!(
            parse("(мир OR вечность) Бог").match_expr,
            "( мир OR вечность ) Бог"
        );
        // Непарные скобки выбрасываются.
        assert_eq!(parse("(мир").match_expr, "мир");
        assert_eq!(parse("мир)").match_expr, "мир");
    }

    #[test]
    fn hyphenated_word_is_quoted() {
        assert_eq!(parse("сын-человек").match_expr, "\"сын-человек\"");
        assert_eq!(parse("сын-человек*").match_expr, "\"сын-человек\" *");
    }

    #[test]
    fn unclosed_quote_degrades_to_words() {
        assert_eq!(parse("\"благодать истина").match_expr, "благодать истина");
    }

    #[test]
    fn strongs_number_is_plain_word() {
        assert_eq!(parse("любовь G26").match_expr, "любовь G26");
    }

    #[test]
    fn empty_input() {
        assert_eq!(parse_query(""), None);
        assert_eq!(parse_query("   "), None);
        assert_eq!(parse_query("OR NOT"), None);
        assert_eq!(parse_query("*"), None);
    }

    #[test]
    fn corpus_ranges() {
        assert_eq!(Corpus::All.book_range(), None);
        assert_eq!(
            Corpus::OldTestament.book_range(),
            Some((0, OLD_TESTAMENT_BOUNDARY - 1))
        );
        assert_eq!(
            Corpus::NewTestament.book_range(),
            Some((OLD_TESTAMENT_BOUNDARY, i64::MAX))
        );
    }
}
