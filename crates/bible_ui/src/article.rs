//! Рендер статей: ContentBlock → нормализованный HTML для TextView
//! (architecture.md §6.3.3) и обратный разбор внутренних ссылок
//! `bibleown://…` (UI-сторона Link Router, §5.4).
//!
//! TextView (gpui-base) покрывает `a b i u s code mark img br`, заголовки,
//! списки, таблицы, blockquote, p/div — сериализатор выдаёт ровно это
//! подмножество. `<sup>` уже нормализован парсером (small/StrongRef);
//! здесь small-текст отдаём как есть (TextView sup не знает).
//!
//! Внутренние ссылки кодируются схемой `bibleown://`:
//! - `bibleown://bible/{book}/{chapter}/{verse}[/{end}]`
//! - `bibleown://dict/{module}/{topic}`   (topic percent-encoded)
//! - `bibleown://strongs/{topic}`         (topic percent-encoded)
//! - `bibleown://note/{relpath}`          (relpath percent-encoded)

use bible_domain::content::{ContentBlock, ContentInline, TextMarks};
use bible_domain::ids::{BookNumber, ModuleId, VerseRef};
use bible_domain::link::LinkTarget;
use bible_domain::ports::ModuleCatalog as _;
use gpui_kit::base::TextView;
use gpui_kit::{App, SharedString, Styled, px};

use crate::navigation;
use crate::services::app_services;

const SCHEME: &str = "bibleown://";

/// Сериализация дерева содержимого в HTML-подмножество TextView.
pub fn blocks_to_html(blocks: &[ContentBlock]) -> String {
    blocks_to_html_with_lead_word(blocks, false)
}

/// Сериализация статьи для компактной панели «Слово».
///
/// В узкой колонке границы абзацев плохо различимы, особенно когда модуль
/// почти не использует вертикальные отступы. Первое слово каждого абзаца
/// выделяется полужирным без добавления посторонних символов в текст.
pub fn blocks_to_word_panel_html(blocks: &[ContentBlock]) -> String {
    blocks_to_html_with_lead_word(blocks, true)
}

fn blocks_to_html_with_lead_word(blocks: &[ContentBlock], highlight_lead_word: bool) -> String {
    let mut out = String::new();
    for block in blocks {
        match block {
            ContentBlock::Paragraph(inlines) => {
                out.push_str("<p>");
                if highlight_lead_word {
                    paragraph_inlines_to_html(inlines, &mut out);
                } else {
                    inlines_to_html(inlines, &mut out);
                }
                out.push_str("</p>");
            }
            ContentBlock::Heading { level, content } => {
                let level = (*level).clamp(1, 6);
                out.push_str(&format!("<h{level}>"));
                inlines_to_html(content, &mut out);
                out.push_str(&format!("</h{level}>"));
            }
            ContentBlock::List { ordered, items } => {
                out.push_str(if *ordered { "<ol>" } else { "<ul>" });
                for item in items {
                    out.push_str("<li>");
                    inlines_to_html(item, &mut out);
                    out.push_str("</li>");
                }
                out.push_str(if *ordered { "</ol>" } else { "</ul>" });
            }
            ContentBlock::Quote(inlines) => {
                out.push_str("<blockquote>");
                inlines_to_html(inlines, &mut out);
                out.push_str("</blockquote>");
            }
            ContentBlock::Table { head, rows } => {
                out.push_str("<table>");
                if !head.is_empty() {
                    out.push_str("<tr>");
                    for cell in head {
                        out.push_str("<th>");
                        escape_html_into(cell, &mut out);
                        out.push_str("</th>");
                    }
                    out.push_str("</tr>");
                }
                for row in rows {
                    out.push_str("<tr>");
                    for cell in row {
                        out.push_str("<td>");
                        escape_html_into(cell, &mut out);
                        out.push_str("</td>");
                    }
                    out.push_str("</tr>");
                }
                out.push_str("</table>");
            }
            ContentBlock::Rule => out.push_str("<hr>"),
        }
    }
    out
}

fn paragraph_inlines_to_html(inlines: &[ContentInline], out: &mut String) {
    let mut lead_word_pending = true;
    for inline in inlines {
        match inline {
            ContentInline::Text { text, marks } if lead_word_pending => {
                let word_start = text.find(|ch: char| !ch.is_whitespace());
                let Some(word_start) = word_start else {
                    inlines_to_html(std::slice::from_ref(inline), out);
                    continue;
                };
                let word_end = text[word_start..]
                    .find(char::is_whitespace)
                    .map(|offset| word_start + offset)
                    .unwrap_or(text.len());

                let open = marks_open(marks);
                let close = marks_close(marks);
                out.push_str(&open);
                escape_html_into(&text[..word_start], out);
                out.push_str("<b>");
                escape_html_into(&text[word_start..word_end], out);
                out.push_str("</b>");
                escape_html_into(&text[word_end..], out);
                out.push_str(&close);
                lead_word_pending = false;
            }
            ContentInline::Link { target, label } if lead_word_pending => {
                if let Some(url) = link_target_to_url(target) {
                    out.push_str(&format!(r#"<a href="{url}"><b>"#));
                    escape_html_into(label, out);
                    out.push_str("</b></a>");
                } else {
                    out.push_str("<b>");
                    escape_html_into(label, out);
                    out.push_str("</b>");
                }
                lead_word_pending = false;
            }
            ContentInline::StrongRef(number) if lead_word_pending => {
                let topic = number.topic();
                out.push_str(&format!(
                    r#"<a href="{SCHEME}strongs/{}"><b>"#,
                    percent_encode(&topic)
                ));
                escape_html_into(&topic, out);
                out.push_str("</b></a>");
                lead_word_pending = false;
            }
            ContentInline::Break => {
                out.push_str("<br>");
            }
            _ => {
                inlines_to_html(std::slice::from_ref(inline), out);
                lead_word_pending = false;
            }
        }
    }
}

fn inlines_to_html(inlines: &[ContentInline], out: &mut String) {
    for inline in inlines {
        match inline {
            ContentInline::Text { text, marks } => {
                let open = marks_open(marks);
                let close = marks_close(marks);
                out.push_str(&open);
                escape_html_into(text, out);
                out.push_str(&close);
            }
            ContentInline::Link { target, label } => {
                if let Some(url) = link_target_to_url(target) {
                    out.push_str(&format!(r#"<a href="{url}">"#));
                    escape_html_into(label, out);
                    out.push_str("</a>");
                } else {
                    escape_html_into(label, out);
                }
            }
            ContentInline::StrongRef(number) => {
                let topic = number.topic();
                out.push_str(&format!(
                    r#"<a href="{SCHEME}strongs/{}">"#,
                    percent_encode(&topic)
                ));
                escape_html_into(&topic, out);
                out.push_str("</a>");
            }
            ContentInline::Break => out.push_str("<br>"),
        }
    }
}

fn marks_open(marks: &TextMarks) -> String {
    let mut out = String::new();
    if marks.bold {
        out.push_str("<b>");
    }
    if marks.italic {
        out.push_str("<i>");
    }
    if marks.underline {
        out.push_str("<u>");
    }
    if marks.strike {
        out.push_str("<s>");
    }
    out
}

fn marks_close(marks: &TextMarks) -> String {
    let mut out = String::new();
    if marks.strike {
        out.push_str("</s>");
    }
    if marks.underline {
        out.push_str("</u>");
    }
    if marks.italic {
        out.push_str("</i>");
    }
    if marks.bold {
        out.push_str("</b>");
    }
    out
}

/// LinkTarget → URL. None для целей, которым ещё нет маршрута
/// (Commentary/CrossReferences — свои представления, не ссылки в тексте).
pub fn link_target_to_url(target: &LinkTarget) -> Option<String> {
    match target {
        LinkTarget::Bible(reference) => {
            let mut url = format!(
                "{SCHEME}bible/{}/{}/{}",
                reference.book.0, reference.chapter, reference.verse
            );
            if let Some(end) = reference.end_verse {
                url.push_str(&format!("/{end}"));
            }
            Some(url)
        }
        LinkTarget::DictionaryEntry { module, topic } => Some(format!(
            "{SCHEME}dict/{}/{}",
            percent_encode(&module.to_string()),
            percent_encode(topic)
        )),
        LinkTarget::Note(relpath) => Some(format!("{SCHEME}note/{}", percent_encode(relpath))),
        LinkTarget::Commentary { module, reference } => {
            let module = percent_encode(&module.0);
            let mut url = format!(
                "{SCHEME}commentary/{module}/{}/{}/{}",
                reference.book.0, reference.chapter, reference.verse
            );
            if let Some(end) = reference.end_verse {
                url.push_str(&format!("/{end}"));
            }
            Some(url)
        }
        LinkTarget::ExternalUrl(url) => Some(url.clone()),
        LinkTarget::CrossReferences { .. } => None,
    }
}

/// Обратный разбор `bibleown://` URL. Внешние URL возвращают ExternalUrl.
pub fn url_to_link_target(url: &str) -> Option<LinkTarget> {
    let rest = url.strip_prefix(SCHEME)?;
    let (kind, tail) = rest.split_once('/')?;
    match kind {
        "bible" | "commentary" => {
            let mut parts = tail.split('/');
            let module = if kind == "commentary" {
                Some(ModuleId(percent_decode(parts.next()?)))
            } else {
                None
            };
            let book: i64 = parts.next()?.parse().ok()?;
            let chapter: u16 = parts.next()?.parse().ok()?;
            let verse: u16 = parts.next()?.parse().ok()?;
            let end_verse: Option<u16> = parts.next().and_then(|p| p.parse().ok());
            let reference = VerseRef {
                book: BookNumber(book),
                chapter,
                verse,
                end_verse,
            };
            Some(match module {
                Some(module) => LinkTarget::Commentary { module, reference },
                None => LinkTarget::Bible(reference),
            })
        }
        "dict" => {
            let (module, topic) = tail.split_once('/')?;
            Some(LinkTarget::DictionaryEntry {
                module: ModuleId(percent_decode(module)),
                topic: percent_decode(topic),
            })
        }
        "note" => Some(LinkTarget::Note(percent_decode(tail))),
        _ => None,
    }
}

/// Разбор `bibleown://strongs/{topic}` — тема Strong's-словаря.
fn url_to_strongs_topic(url: &str) -> Option<String> {
    let rest = url.strip_prefix(SCHEME)?;
    let topic = rest.strip_prefix("strongs/")?;
    Some(percent_decode(topic))
}

/// TextView с типографикой чтения и обработкой ссылок (§6.3.3) — общий
/// рендер статей/комментариев для вкладок (article_tab) и панелей.
/// Ссылки кликабельны (follow_url), у ссылок на Писание — tooltip с
/// текстом стиха, текст выделяемый.
pub fn article_text_view(id: impl Into<gpui_kit::ElementId>, html: String, cx: &App) -> TextView {
    linked_text_view(TextView::html(id, html), cx)
        .scrollable(true)
        .size_full()
        .p_3()
}

/// Общие правила перехода и превью для статей и встроенных фрагментов.
pub(crate) fn linked_text_view(view: TextView, cx: &App) -> TextView {
    crate::verse_preview::with_preview(view)
        .selectable(true)
        .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
        .on_link_click(|url, _event, _window, cx| {
            follow_url(url, cx);
        })
}

/// Клик по ссылке в TextView: внутренние цели — через шину навигации,
/// внешние — системным браузером.
pub fn follow_url(url: &SharedString, cx: &mut App) {
    if let Some(topic) = url_to_strongs_topic(url) {
        // Strong's-ссылка: первый Strong's-словарь каталога.
        let dict = app_services(cx)
            .registry
            .modules()
            .into_iter()
            .find(|m| m.kind == bible_domain::ports::ModuleKind::StrongsDictionary)
            .map(|m| m.id);
        if let Some(dict) = dict {
            navigation::request_open_topic(cx, dict, topic);
        }
        return;
    }
    match url_to_link_target(url) {
        Some(LinkTarget::Bible(reference)) => crate::verse_preview::follow(reference, None, cx),
        Some(LinkTarget::DictionaryEntry { module, topic }) => {
            navigation::request_open_topic(cx, module, topic);
        }
        Some(LinkTarget::Commentary { module, reference }) => {
            navigation::request_open_commentary(cx, module, reference);
        }
        Some(LinkTarget::Note(relpath)) => {
            navigation::request_open_note(cx, relpath);
        }
        Some(LinkTarget::ExternalUrl(_)) | None => {
            cx.open_url(url);
        }
        Some(_) => {}
    }
}

// --- Экранирование и percent-encoding -------------------------------------

fn escape_html_into(text: &str, out: &mut String) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
}

/// Кодируем всё, кроме безопасных ASCII (как query-component).
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn percent_decode(input: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use bible_domain::content::StrongNumber;

    #[test]
    fn paragraph_with_marks() {
        let html = blocks_to_html(&[ContentBlock::Paragraph(vec![
            ContentInline::Text {
                text: "обычный ".to_string(),
                marks: TextMarks::default(),
            },
            ContentInline::Text {
                text: "жирный".to_string(),
                marks: TextMarks {
                    bold: true,
                    ..Default::default()
                },
            },
        ])]);
        assert_eq!(html, "<p>обычный <b>жирный</b></p>");
    }

    #[test]
    fn word_panel_highlights_first_word_of_each_paragraph() {
        let blocks = vec![
            ContentBlock::Paragraph(vec![ContentInline::Text {
                text: "первый".to_string(),
                marks: TextMarks::default(),
            }]),
            ContentBlock::Paragraph(vec![ContentInline::Text {
                text: "второй".to_string(),
                marks: TextMarks::default(),
            }]),
        ];

        assert_eq!(
            blocks_to_word_panel_html(&blocks),
            "<p><b>первый</b></p><p><b>второй</b></p>"
        );
    }

    #[test]
    fn word_panel_preserves_whitespace_and_marks_around_first_word() {
        let html =
            blocks_to_word_panel_html(&[ContentBlock::Paragraph(vec![ContentInline::Text {
                text: "  первое остальное".to_string(),
                marks: TextMarks {
                    italic: true,
                    ..Default::default()
                },
            }])]);

        assert_eq!(html, "<p><i>  <b>первое</b> остальное</i></p>");
    }

    #[test]
    fn bible_link_roundtrip() {
        let target = LinkTarget::Bible(VerseRef {
            book: BookNumber(540),
            chapter: 4,
            verse: 1,
            end_verse: Some(5),
        });
        let url = link_target_to_url(&target).unwrap();
        assert_eq!(url, "bibleown://bible/540/4/1/5");
        assert_eq!(url_to_link_target(&url), Some(target));
    }

    #[test]
    fn dict_link_roundtrip_with_unicode() {
        let target = LinkTarget::DictionaryEntry {
            module: ModuleId::from("CBTEL"),
            topic: "Авраам (патриарх)".to_string(),
        };
        let url = link_target_to_url(&target).unwrap();
        assert_eq!(url_to_link_target(&url), Some(target));
    }

    #[test]
    fn strong_ref_serializes_as_link() {
        let html = blocks_to_html(&[ContentBlock::Paragraph(vec![ContentInline::StrongRef(
            StrongNumber {
                prefix: Some('G'),
                number: "25".to_string(),
            },
        )])]);
        assert_eq!(html, r#"<p><a href="bibleown://strongs/G25">G25</a></p>"#);
    }

    #[test]
    fn html_escaped() {
        let html = blocks_to_html(&[ContentBlock::Paragraph(vec![ContentInline::Text {
            text: "a < b & \"c\"".to_string(),
            marks: TextMarks::default(),
        }])]);
        assert_eq!(html, "<p>a &lt; b &amp; &quot;c&quot;</p>");
    }
}
