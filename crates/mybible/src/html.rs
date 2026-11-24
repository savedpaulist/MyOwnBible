//! Парсер MyBible HTML → ContentBlock (architecture.md §4.1.5).
//!
//! Основа — `dictionary.rs` форка (классификация href: `B:`-ссылки на
//! Писание, внутренние ссылки на темы словаря, URL-decoding, снятие
//! вложенных тегов). Отличие: выдача не строка, а дерево
//! `ContentBlock`/`ContentInline` — контракт с Presentation (§3.7).
//!
//! Разметка MyBible простая и местами небрежная (незакрытые теги, `<p/>`),
//! поэтому парсер — свой лёгкий сканер, без html5ever: неизвестные теги
//! пропускаются, их текстовое содержимое сохраняется.
//!
//! Нормализация `<sup>`/`<sub>` (§4.1.5): содержимое вида «G25»/«H123»
//! становится `ContentInline::StrongRef`, остальное — текст с меткой
//! `small` (TextView sup не поддерживает).

use bible_domain::content::{ContentBlock, ContentInline, StrongNumber, TextMarks};
use bible_domain::ids::{BookNumber, ModuleId, VerseRef};
use bible_domain::link::LinkTarget;

/// Разобрать статью словаря/комментария в дерево содержимого.
/// `current_module` — модуль, чья статья разбирается: внутренние ссылки
/// (`<a href="S:ADAM">`, `<a href="Abraham%20%28patriarch%29">`) ведут
/// на темы этого же модуля.
pub fn parse_article(raw: &str, current_module: &ModuleId) -> Vec<ContentBlock> {
    Parser::new(raw, current_module).run()
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
    current_module: &'a ModuleId,
    blocks: Vec<ContentBlock>,
    /// Накопленные inline-элементы текущего абзаца/заголовка/пункта.
    inlines: Vec<ContentInline>,
    /// Текстовый буфер с текущими метками (сливается в Text при смене меток).
    pending: String,
    marks: TextMarks,
    /// Стек меток для вложенных inline-тегов.
    marks_stack: Vec<TextMarks>,
    list: Option<ListState>,
    table: Option<TableState>,
    in_quote: bool,
    /// Цитата накапливает абзацы в один блок через Break.
    quote: Vec<ContentInline>,
}

#[derive(Default)]
struct ListState {
    ordered: bool,
    items: Vec<Vec<ContentInline>>,
}

#[derive(Default)]
struct TableState {
    head: Vec<String>,
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: Option<String>,
    cell_is_header: bool,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str, current_module: &'a ModuleId) -> Self {
        Self {
            src,
            pos: 0,
            current_module,
            blocks: Vec::new(),
            inlines: Vec::new(),
            pending: String::new(),
            marks: TextMarks::default(),
            marks_stack: Vec::new(),
            list: None,
            table: None,
            in_quote: false,
            quote: Vec::new(),
        }
    }

    fn run(mut self) -> Vec<ContentBlock> {
        let bytes = self.src.as_bytes();
        while self.pos < bytes.len() {
            match bytes[self.pos] {
                b'<' => {
                    if !self.handle_tag() {
                        // Не тег — буквальный '<'.
                        self.push_char('<');
                        self.pos += 1;
                    }
                }
                b'&' => match decode_entity(bytes, self.pos) {
                    Some((decoded, after)) => {
                        for ch in decoded.chars() {
                            self.push_content_char(ch);
                        }
                        self.pos = after;
                    }
                    None => {
                        self.push_content_char('&');
                        self.pos += 1;
                    }
                },
                _ => {
                    let ch_len = utf8_char_len(bytes[self.pos]);
                    let end = (self.pos + ch_len).min(bytes.len());
                    let ch = self.src[self.pos..end].chars().next().unwrap_or('\u{FFFD}');
                    self.pos = end;
                    self.push_content_char(ch);
                }
            }
        }
        self.flush_paragraph();
        self.finish_list();
        self.finish_table();
        self.finish_quote();
        self.blocks
    }

    // --- Текст ------------------------------------------------------------

    fn in_table_cell(&self) -> bool {
        self.table
            .as_ref()
            .is_some_and(|t| t.current_cell.is_some())
    }

    fn current_cell_text(&self) -> &str {
        self.table
            .as_ref()
            .and_then(|t| t.current_cell.as_deref())
            .unwrap_or("")
    }

    fn current_cell_text_mut(&mut self) -> &mut String {
        self.table
            .as_mut()
            .and_then(|t| t.current_cell.as_mut())
            .expect("checked by caller")
    }

    /// Символ содержимого: внутри ячейки таблицы — в ячейку, иначе в текст.
    fn push_content_char(&mut self, ch: char) {
        if self.in_table_cell() {
            if ch.is_whitespace() {
                if !self.current_cell_text().is_empty() && !self.current_cell_text().ends_with(' ')
                {
                    self.current_cell_text_mut().push(' ');
                }
            } else {
                self.current_cell_text_mut().push(ch);
            }
        } else {
            self.push_text_char(ch);
        }
    }

    /// Символ обычного текста: схлопывание пробелов, буферизация с метками.
    fn push_text_char(&mut self, ch: char) {
        if ch.is_whitespace() {
            if !self.pending.is_empty() && !self.pending.ends_with(' ') {
                self.pending.push(' ');
            }
        } else {
            self.pending.push(ch);
        }
    }

    fn push_char(&mut self, ch: char) {
        self.pending.push(ch);
    }

    /// Слить текстовый буфер в ContentInline::Text с текущими метками.
    fn flush_pending(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.pending);
        let inline = ContentInline::Text {
            text,
            marks: self.marks.clone(),
        };
        self.push_inline(inline);
    }

    fn push_inline(&mut self, inline: ContentInline) {
        if self.in_quote {
            self.quote.push(inline);
        } else {
            self.inlines.push(inline);
        }
    }

    /// Завершить абзац, если в нём есть содержимое.
    fn flush_paragraph(&mut self) {
        self.flush_pending();
        if self.inlines.is_empty() {
            return;
        }
        // Абзац из одних пробелов не считается содержимым.
        let has_content = self.inlines.iter().any(|inline| match inline {
            ContentInline::Text { text, .. } => !text.trim().is_empty(),
            _ => true,
        });
        if !has_content {
            self.inlines.clear();
            return;
        }
        trim_inlines(&mut self.inlines);
        let inlines = std::mem::take(&mut self.inlines);
        self.blocks.push(ContentBlock::Paragraph(inlines));
    }

    // --- Теги -------------------------------------------------------------

    /// Обработать тег в текущей позиции. false — это не тег ('<' в тексте).
    fn handle_tag(&mut self) -> bool {
        let Some(tag) = Tag::parse(self.src, self.pos) else {
            return false;
        };
        self.pos = tag.end;

        if tag.closing {
            self.handle_close_tag(&tag.name);
            return true;
        }

        match tag.name.as_str() {
            "b" | "strong" => self.set_mark(|m| m.bold = true),
            "i" | "em" => self.set_mark(|m| m.italic = true),
            "u" => self.set_mark(|m| m.underline = true),
            "s" | "del" | "strike" => self.set_mark(|m| m.strike = true),
            "sup" | "sub" => self.handle_sup(tag.self_closing),
            "font" => {
                let color = tag.attr("color").map(|s| s.to_string());
                self.flush_pending();
                self.marks_stack.push(self.marks.clone());
                if let Some(color) = color {
                    self.marks.color = Some(color);
                }
            }
            "a" => self.handle_anchor(&tag),
            "br" => {
                self.flush_pending();
                self.push_inline(ContentInline::Break);
            }
            "p" | "div" | "section" | "article" | "center" | "header" | "footer" => {
                self.flush_paragraph();
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "dt" => {
                let level = tag
                    .name
                    .chars()
                    .last()
                    .and_then(|c| c.to_digit(10))
                    .unwrap_or(2) as u8;
                self.handle_heading(&tag.name, level);
            }
            "ul" | "ol" => {
                self.flush_paragraph();
                self.finish_list();
                self.list = Some(ListState {
                    ordered: tag.name == "ol",
                    items: Vec::new(),
                });
            }
            "li" => {
                // Новый пункт: предыдущий накоплен в inlines.
                if self.list.is_some() {
                    self.flush_pending();
                    if !self.inlines.is_empty() {
                        trim_inlines(&mut self.inlines);
                        let item = std::mem::take(&mut self.inlines);
                        if let Some(list) = &mut self.list {
                            list.items.push(item);
                        }
                    }
                } else {
                    // <li> вне списка — просто абзац.
                    self.flush_paragraph();
                }
            }
            "blockquote" => {
                self.flush_paragraph();
                self.in_quote = true;
            }
            "table" => {
                self.flush_paragraph();
                self.finish_table();
                self.table = Some(TableState::default());
            }
            "tr" => {
                if let Some(table) = &mut self.table {
                    if !table.current_row.is_empty() {
                        let row = std::mem::take(&mut table.current_row);
                        table.rows.push(row);
                    }
                }
            }
            "th" | "td" => {
                self.close_table_cell();
                if let Some(table) = &mut self.table {
                    table.cell_is_header = tag.name == "th";
                    table.current_cell = Some(String::new());
                }
            }
            "hr" => {
                self.flush_paragraph();
                self.blocks.push(ContentBlock::Rule);
            }
            "script" | "style" => {
                self.skip_until_close(&tag.name);
            }
            _ => {}
        }
        true
    }

    fn handle_close_tag(&mut self, name: &str) {
        match name {
            "b" | "strong" | "i" | "em" | "u" | "s" | "del" | "strike" | "sup" | "sub" | "font" => {
                self.flush_pending();
                if let Some(previous) = self.marks_stack.pop() {
                    self.marks = previous;
                }
            }
            "p" | "div" | "section" | "article" | "center" | "header" | "footer" => {
                self.flush_paragraph();
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "dt" => {
                // Заголовок без нашего open (незакрытый текст до него) — абзац.
                self.flush_paragraph();
            }
            "ul" | "ol" => self.finish_list(),
            "li" => {
                if self.list.is_some() {
                    self.flush_pending();
                    if !self.inlines.is_empty() {
                        trim_inlines(&mut self.inlines);
                        let item = std::mem::take(&mut self.inlines);
                        if let Some(list) = &mut self.list {
                            list.items.push(item);
                        }
                    }
                }
            }
            "blockquote" => self.finish_quote(),
            "th" | "td" => self.close_table_cell(),
            "tr" => {
                self.close_table_cell();
                if let Some(table) = &mut self.table {
                    if !table.current_row.is_empty() {
                        let row = std::mem::take(&mut table.current_row);
                        table.rows.push(row);
                    }
                }
            }
            "table" => self.finish_table(),
            _ => {}
        }
    }

    fn set_mark(&mut self, update: impl Fn(&mut TextMarks)) {
        self.flush_pending();
        self.marks_stack.push(self.marks.clone());
        update(&mut self.marks);
    }

    /// `<sup>`: Strong's-номер → StrongRef, иначе текст с меткой small.
    fn handle_sup(&mut self, self_closing: bool) {
        if self_closing {
            return;
        }
        // Заглядываем внутрь: если содержимое — чистый номер Strong's,
        // это семантическая ссылка, а не сноска.
        if let Some((inner, after)) = capture_until_close(self.src, self.pos, "sup") {
            let inner_text = strip_tags(&inner);
            let trimmed = inner_text.trim();
            if let Some(number) = parse_strong_number(trimmed) {
                self.flush_pending();
                self.push_inline(ContentInline::StrongRef(number));
                self.pos = after;
                return;
            }
        }
        self.set_mark(|m| m.small = true);
    }

    /// `<a href="…">`: классификация цели (как в форке), label — видимый
    /// текст ссылки без вложенных тегов. Неразрешимая ссылка — просто текст.
    fn handle_anchor(&mut self, tag: &Tag) {
        let Some((inner, after)) = capture_until_close(self.src, self.pos, "a") else {
            return; // незакрытая ссылка — пропускаем сам тег
        };
        let label = collapse_spaces(&strip_tags(&inner));
        let target = tag
            .attr("href")
            .and_then(|href| classify_href(href, self.current_module));
        match target {
            Some(target) if !label.is_empty() => {
                self.flush_pending();
                self.push_inline(ContentInline::Link { target, label });
            }
            // Ссылку без цели/текста не рисуем, но содержимое сохраняем
            // как обычный текст.
            _ if !label.is_empty() => {
                self.flush_pending();
                self.push_inline(ContentInline::Text {
                    text: label,
                    marks: self.marks.clone(),
                });
            }
            _ => {}
        }
        self.pos = after;
    }

    fn handle_heading(&mut self, name: &str, level: u8) {
        self.flush_paragraph();
        let Some((inner, after)) = capture_until_close(self.src, self.pos, name) else {
            return;
        };
        // Заголовок: внутренность разбираем как отдельный поток inline.
        let mut inner_parser = Parser::new(&inner, self.current_module);
        inner_parser.marks = self.marks.clone();
        let inner_blocks = inner_parser.run();
        let mut content: Vec<ContentInline> = Vec::new();
        for block in inner_blocks {
            match block {
                ContentBlock::Paragraph(inlines) | ContentBlock::Quote(inlines) => {
                    content.extend(inlines)
                }
                ContentBlock::Heading { content: inner, .. } => content.extend(inner),
                _ => {}
            }
        }
        trim_inlines(&mut content);
        if !content.is_empty() {
            self.blocks.push(ContentBlock::Heading { level, content });
        }
        self.pos = after;
    }

    fn skip_until_close(&mut self, name: &str) {
        if let Some((_, after)) = capture_until_close(self.src, self.pos, name) {
            self.pos = after;
        }
    }

    // --- Блочные завершители ----------------------------------------------

    fn finish_list(&mut self) {
        // Пункт без закрывающего </li>.
        if self.list.is_some() {
            self.flush_pending();
            if !self.inlines.is_empty() {
                trim_inlines(&mut self.inlines);
                let item = std::mem::take(&mut self.inlines);
                if let Some(list) = &mut self.list {
                    list.items.push(item);
                }
            }
        }
        if let Some(list) = self.list.take() {
            if !list.items.is_empty() {
                self.blocks.push(ContentBlock::List {
                    ordered: list.ordered,
                    items: list.items,
                });
            }
        }
    }

    fn close_table_cell(&mut self) {
        if let Some(table) = &mut self.table {
            if let Some(cell) = table.current_cell.take() {
                let cell = cell.trim().to_string();
                if table.cell_is_header {
                    table.head.push(cell);
                } else {
                    table.current_row.push(cell);
                }
            }
        }
    }

    fn finish_table(&mut self) {
        self.close_table_cell();
        if let Some(mut table) = self.table.take() {
            if !table.current_row.is_empty() {
                table.rows.push(std::mem::take(&mut table.current_row));
            }
            if !table.head.is_empty() || !table.rows.is_empty() {
                self.blocks.push(ContentBlock::Table {
                    head: table.head,
                    rows: table.rows,
                });
            }
        }
    }

    fn finish_quote(&mut self) {
        if !self.in_quote {
            return;
        }
        self.in_quote = false;
        self.flush_pending();
        // Абзац, накопленный вне quote (не должен случиться), возвращаем.
        if !self.inlines.is_empty() {
            let rest = std::mem::take(&mut self.inlines);
            self.quote.extend(rest);
        }
        trim_inlines(&mut self.quote);
        if !self.quote.is_empty() {
            let quote = std::mem::take(&mut self.quote);
            self.blocks.push(ContentBlock::Quote(quote));
        }
    }
}

/// `<sup>G25</sup>` → StrongRef; префикс приводится к верхнему регистру.
fn parse_strong_number(text: &str) -> Option<StrongNumber> {
    let mut chars = text.chars();
    let first = chars.next()?;
    let (prefix, rest) = match first {
        'G' | 'g' => (Some('G'), &text[1..]),
        'H' | 'h' => (Some('H'), &text[1..]),
        _ => return None,
    };
    let number = rest.trim();
    if !number.is_empty() && number.len() <= 5 && number.chars().all(|c| c.is_ascii_digit()) {
        Some(StrongNumber {
            prefix,
            number: number.to_string(),
        })
    } else {
        None
    }
}

/// Классификация `<a href>` (логика форка: B: → Писание, внутренние →
/// тема этого же словаря, внешние схемы → ExternalUrl).
fn classify_href(href: &str, current_module: &ModuleId) -> Option<LinkTarget> {
    let trimmed = href.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    if let Some(reference) = parse_bible_href(trimmed) {
        return Some(LinkTarget::Bible(reference));
    }
    let lower = trimmed.to_ascii_lowercase();
    const EXTERNAL_SCHEMES: [&str; 6] = [
        "http://", "https://", "mailto:", "ftp://", "file://", "data:",
    ];
    if EXTERNAL_SCHEMES.iter().any(|p| lower.starts_with(p)) {
        return Some(LinkTarget::ExternalUrl(trimmed.to_string()));
    }
    if lower.starts_with("javascript:") || lower.starts_with("about:") {
        return None;
    }
    // Внутренняя ссылка на тему этого же словаря: S:-префикс (SWORD) снимаем,
    // %xx декодируем — как в форке.
    let decoded = decode_percent(trimmed);
    let topic = decoded
        .strip_prefix("S:")
        .or_else(|| decoded.strip_prefix("s:"))
        .unwrap_or(&decoded)
        .trim()
        .to_string();
    if topic.is_empty() {
        return None;
    }
    Some(LinkTarget::DictionaryEntry {
        module: current_module.clone(),
        topic,
    })
}

/// `B:540 4:1` / `B:540 4:1-5` / `B:540 4` → VerseRef.
/// Глава без стиха → стих 1 (VerseRef требует конкретный стих).
fn parse_bible_href(href: &str) -> Option<VerseRef> {
    let body = href
        .strip_prefix("B:")
        .or_else(|| href.strip_prefix("b:"))?
        .trim();
    let space_idx = body.find(char::is_whitespace)?;
    let book: i64 = body[..space_idx].trim().parse().ok()?;
    let rest = body[space_idx..].trim();
    let (chapter_str, verses_str) = match rest.split_once(':') {
        Some((chapter, verses)) => (chapter, verses),
        None => (rest, "1"),
    };
    let chapter: u16 = chapter_str.trim().parse().ok()?;
    // Диапазон стихов: "1-5" → 1..5; берём только стихи той же главы
    // ("4:1-5:6" урезается до 4:1-5, как в форке).
    let (verse_str, end_str) = match verses_str.split_once('-') {
        Some((from, to)) => (from, to.split_once(':').map(|(_, v)| v).unwrap_or(to)),
        None => (verses_str, verses_str),
    };
    let verse: u16 = verse_str.trim().parse().ok()?;
    let end_verse: u16 = end_str.trim().parse().unwrap_or(verse);
    Some(VerseRef {
        book: BookNumber(book),
        chapter,
        verse,
        end_verse: (end_verse > verse).then_some(end_verse),
    })
}

/// Разбор тега в позиции `at` (там '<'). None — не тег.
struct Tag {
    name: String,
    closing: bool,
    self_closing: bool,
    attrs: Vec<(String, String)>,
    end: usize,
}

impl Tag {
    fn parse(src: &str, at: usize) -> Option<Tag> {
        let bytes = src.as_bytes();
        debug_assert_eq!(bytes[at], b'<');
        let next = *bytes.get(at + 1)?;
        // "<" за которым не имя тега и не "/" — текст.
        if !next.is_ascii_alphabetic() && next != b'/' {
            return None;
        }
        let close_offset = bytes[at + 1..].iter().position(|&b| b == b'>')?;
        let end = at + 1 + close_offset + 1;
        let inside = &src[at + 1..at + 1 + close_offset];

        let (closing, inside) = match inside.strip_prefix('/') {
            Some(rest) => (true, rest),
            None => (false, inside),
        };
        let self_closing = inside.trim_end().ends_with('/');
        let inside = inside.trim_end_matches('/').trim();

        let name: String = inside
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        if name.is_empty() {
            return None;
        }
        let attrs = parse_attrs(&inside[name.len()..]);
        Some(Tag {
            name,
            closing,
            self_closing,
            attrs,
            end,
        })
    }

    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Атрибуты вида name="value" / name='value' / name=value.
fn parse_attrs(src: &str) -> Vec<(String, String)> {
    let bytes = src.as_bytes();
    let mut attrs = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        // Пропуск до начала имени.
        while pos < bytes.len() && !(bytes[pos].is_ascii_alphabetic() || bytes[pos] == b'_') {
            pos += 1;
        }
        let name_start = pos;
        while pos < bytes.len() && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'-') {
            pos += 1;
        }
        if pos == name_start {
            break;
        }
        let name = src[name_start..pos].to_ascii_lowercase();
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if bytes.get(pos) != Some(&b'=') {
            continue;
        }
        pos += 1;
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        let Some(&quote) = bytes.get(pos) else { break };
        if quote == b'"' || quote == b'\'' {
            pos += 1;
            let value_start = pos;
            while pos < bytes.len() && bytes[pos] != quote {
                pos += 1;
            }
            attrs.push((name, src[value_start..pos].to_string()));
            pos += 1;
        } else {
            let value_start = pos;
            while pos < bytes.len() && !bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            attrs.push((name, src[value_start..pos].to_string()));
        }
    }
    attrs
}

/// Содержимое между текущей позицией и закрывающим `</name>` + позиция
/// после него. Вложенность одноимённых тегов не учитываем (в MyBible
/// `<a>`/`<sup>`/заголовки не вкладываются друг в друга).
fn capture_until_close(src: &str, from: usize, name: &str) -> Option<(String, usize)> {
    let needle = format!("</{name}>");
    let lower = src[from..].to_ascii_lowercase();
    let offset = lower.find(&needle)?;
    let inner = src[from..from + offset].to_string();
    Some((inner, from + offset + needle.len()))
}

/// Снять все теги, оставив текст; сущности декодируются.
fn strip_tags(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut pos = 0;
    while pos < bytes.len() {
        match bytes[pos] {
            b'<' => {
                if let Some(tag) = Tag::parse(raw, pos) {
                    pos = tag.end;
                } else {
                    out.push('<');
                    pos += 1;
                }
            }
            b'&' => match decode_entity(bytes, pos) {
                Some((decoded, after)) => {
                    out.push_str(&decoded);
                    pos = after;
                }
                None => {
                    out.push('&');
                    pos += 1;
                }
            },
            _ => {
                let ch_len = utf8_char_len(bytes[pos]);
                let end = (pos + ch_len).min(bytes.len());
                out.push_str(&raw[pos..end]);
                pos = end;
            }
        }
    }
    out
}

fn collapse_spaces(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !out.ends_with(' ') && !out.is_empty() {
                out.push(' ');
            }
        } else {
            out.push(ch);
        }
    }
    out.trim().to_string()
}

/// Убрать пробелы по краям первого/последного inline абзаца.
fn trim_inlines(inlines: &mut Vec<ContentInline>) {
    if let Some(ContentInline::Text { text, .. }) = inlines.first_mut() {
        *text = text.trim_start().to_string();
    }
    if let Some(ContentInline::Text { text, .. }) = inlines.last_mut() {
        *text = text.trim_end().to_string();
    }
    inlines.retain(|inline| !matches!(inline, ContentInline::Text { text, .. } if text.is_empty()));
}

/// HTML-сущности: именованные + числовые (`&#65;`, `&#x41;`).
/// Возвращает декодированную строку и позицию после `;`.
fn decode_entity(bytes: &[u8], at: usize) -> Option<(String, usize)> {
    debug_assert_eq!(bytes[at], b'&');
    let semi = bytes[at..]
        .iter()
        .position(|&b| b == b';')
        .map(|o| at + o)?;
    if semi - at > 10 {
        return None;
    }
    let entity = std::str::from_utf8(&bytes[at..=semi]).ok()?;
    if let Some(decoded) = decode_numeric_entity(entity) {
        return Some((decoded, semi + 1));
    }
    let decoded = match entity {
        "&amp;" => "&",
        "&lt;" => "<",
        "&gt;" => ">",
        "&quot;" => "\"",
        "&apos;" => "'",
        "&nbsp;" => " ",
        "&laquo;" => "«",
        "&raquo;" => "»",
        "&mdash;" => "—",
        "&ndash;" => "–",
        "&hellip;" => "…",
        _ => return None,
    };
    Some((decoded.to_string(), semi + 1))
}

fn decode_numeric_entity(entity: &str) -> Option<String> {
    let body = entity.strip_prefix("&#")?.strip_suffix(';')?;
    let code = if let Some(hex) = body.strip_prefix('x').or_else(|| body.strip_prefix('X')) {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        body.parse().ok()?
    };
    Some(char::from_u32(code)?.to_string())
}

/// Минимальный %xx-декодер для href (как в форке).
fn decode_percent(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push(((hi << 4) | lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| input.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn module() -> ModuleId {
        ModuleId::from("CBTEL")
    }

    fn parse(raw: &str) -> Vec<ContentBlock> {
        parse_article(raw, &module())
    }

    #[test]
    fn plain_text_becomes_paragraph() {
        let blocks = parse("Простой текст статьи.");
        assert_eq!(blocks.len(), 1);
        let ContentBlock::Paragraph(inlines) = &blocks[0] else {
            panic!("expected paragraph");
        };
        assert_eq!(inlines.len(), 1);
        let ContentInline::Text { text, marks } = &inlines[0] else {
            panic!("expected text");
        };
        assert_eq!(text, "Простой текст статьи.");
        assert_eq!(marks, &TextMarks::default());
    }

    #[test]
    fn paragraphs_split_on_p_tags() {
        let blocks = parse("<p>First</p><p>Second</p>");
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn bold_and_italic_marks() {
        let blocks = parse("<b>жирный</b> и <i>курсив</i>");
        let ContentBlock::Paragraph(inlines) = &blocks[0] else {
            panic!("expected paragraph");
        };
        let ContentInline::Text { text, marks } = &inlines[0] else {
            panic!("expected text");
        };
        assert_eq!(text, "жирный");
        assert!(marks.bold);
        let ContentInline::Text { marks, .. } = &inlines[2] else {
            panic!("expected text");
        };
        assert!(marks.italic);
        assert!(!marks.bold);
    }

    #[test]
    fn bible_href_becomes_link() {
        let blocks = parse(r#"см. <a href="B:540 4:1">Рим 4:1</a> и далее"#);
        let ContentBlock::Paragraph(inlines) = &blocks[0] else {
            panic!("expected paragraph");
        };
        let link = inlines.iter().find_map(|inline| match inline {
            ContentInline::Link { target, label } => Some((target, label)),
            _ => None,
        });
        let (target, label) = link.expect("expected link");
        assert_eq!(label, "Рим 4:1");
        assert_eq!(
            target,
            &LinkTarget::Bible(VerseRef::single(BookNumber(540), 4, 1))
        );
    }

    #[test]
    fn bible_href_range() {
        let reference = parse_bible_href("B:540 4:1-5").unwrap();
        assert_eq!(reference.verse, 1);
        assert_eq!(reference.end_verse, Some(5));
    }

    #[test]
    fn internal_href_becomes_dictionary_link() {
        let blocks = parse(r#"<a href="S:ADAM">Адам</a>"#);
        let ContentBlock::Paragraph(inlines) = &blocks[0] else {
            panic!("expected paragraph");
        };
        let ContentInline::Link { target, label } = &inlines[0] else {
            panic!("expected link");
        };
        assert_eq!(label, "Адам");
        assert_eq!(
            target,
            &LinkTarget::DictionaryEntry {
                module: module(),
                topic: "ADAM".to_string(),
            }
        );
    }

    #[test]
    fn percent_encoded_href_decoded() {
        let target = classify_href("Abraham%20%28patriarch%29", &module()).unwrap();
        assert_eq!(
            target,
            LinkTarget::DictionaryEntry {
                module: module(),
                topic: "Abraham (patriarch)".to_string(),
            }
        );
    }

    #[test]
    fn external_href_stays_external() {
        let target = classify_href("https://example.com/x", &module()).unwrap();
        assert_eq!(
            target,
            LinkTarget::ExternalUrl("https://example.com/x".to_string())
        );
    }

    #[test]
    fn sup_with_strong_number_becomes_strong_ref() {
        let blocks = parse("слово<sup>G25</sup>");
        let ContentBlock::Paragraph(inlines) = &blocks[0] else {
            panic!("expected paragraph");
        };
        assert!(
            inlines
                .iter()
                .any(|inline| matches!(inline, ContentInline::StrongRef(n) if n.topic() == "G25"))
        );
    }

    #[test]
    fn sup_with_footnote_becomes_small_text() {
        let blocks = parse("текст<sup>1</sup> дальше");
        let ContentBlock::Paragraph(inlines) = &blocks[0] else {
            panic!("expected paragraph");
        };
        let small = inlines.iter().any(|inline| match inline {
            ContentInline::Text { text, marks } => marks.small && text.trim() == "1",
            _ => false,
        });
        assert!(small, "expected small text for footnote: {inlines:?}");
    }

    #[test]
    fn heading_becomes_block() {
        let blocks = parse("<h2>Раздел</h2><p>Текст</p>");
        assert_eq!(blocks.len(), 2);
        let ContentBlock::Heading { level, content } = &blocks[0] else {
            panic!("expected heading");
        };
        assert_eq!(*level, 2);
        let ContentInline::Text { text, .. } = &content[0] else {
            panic!("expected text");
        };
        assert_eq!(text, "Раздел");
    }

    #[test]
    fn list_becomes_block() {
        let blocks = parse("<ul><li>первый</li><li>второй</li></ul>");
        assert_eq!(blocks.len(), 1);
        let ContentBlock::List { ordered, items } = &blocks[0] else {
            panic!("expected list");
        };
        assert!(!ordered);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn table_becomes_block() {
        let blocks = parse(
            "<table><tr><th>Кол1</th><th>Кол2</th></tr><tr><td>a</td><td>b</td></tr></table>",
        );
        assert_eq!(blocks.len(), 1);
        let ContentBlock::Table { head, rows } = &blocks[0] else {
            panic!("expected table");
        };
        assert_eq!(head, &vec!["Кол1".to_string(), "Кол2".to_string()]);
        assert_eq!(rows, &vec![vec!["a".to_string(), "b".to_string()]]);
    }

    #[test]
    fn entities_decoded() {
        let blocks = parse("Tom &amp; Jerry &#171;cats&#187;");
        let ContentBlock::Paragraph(inlines) = &blocks[0] else {
            panic!("expected paragraph");
        };
        let ContentInline::Text { text, .. } = &inlines[0] else {
            panic!("expected text");
        };
        assert_eq!(text, "Tom & Jerry «cats»");
    }

    #[test]
    fn blockquote_becomes_quote() {
        let blocks = parse("<blockquote>Цитата <b>важная</b></blockquote>");
        assert_eq!(blocks.len(), 1);
        assert!(matches!(blocks[0], ContentBlock::Quote(_)));
    }

    #[test]
    fn whitespace_collapsed() {
        let blocks = parse("много    пробелов\nи\tтабов");
        let ContentBlock::Paragraph(inlines) = &blocks[0] else {
            panic!("expected paragraph");
        };
        let ContentInline::Text { text, .. } = &inlines[0] else {
            panic!("expected text");
        };
        assert_eq!(text, "много пробелов и табов");
    }

    #[test]
    fn script_content_dropped() {
        let blocks = parse("до<script>var x=1;</script>после");
        let ContentBlock::Paragraph(inlines) = &blocks[0] else {
            panic!("expected paragraph");
        };
        let text: String = inlines
            .iter()
            .filter_map(|inline| match inline {
                ContentInline::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(text.contains("до"), "{text}");
        assert!(text.contains("после"), "{text}");
        assert!(!text.contains("var"), "{text}");
    }

    #[test]
    fn br_becomes_break() {
        let blocks = parse("строка<br>другая");
        let ContentBlock::Paragraph(inlines) = &blocks[0] else {
            panic!("expected paragraph");
        };
        assert!(
            inlines
                .iter()
                .any(|inline| matches!(inline, ContentInline::Break))
        );
    }
}
