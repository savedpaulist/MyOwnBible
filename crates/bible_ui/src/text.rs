//! Минимальное снятие HTML-разметки MyBible для сырого показа текстов.
//!
//! Временная мера этапов 3–4: полноценный парсер разметки в `ContentBlock`
//! (architecture.md §3.7) — этап 5 плана, тогда этот модуль уйдёт.

/// Убирает теги, переводит `<p/>`/`<br>` в переводы строк, раскрывает
/// основные HTML-сущности.
pub fn strip_markup(raw: &str) -> String {
    let raw = raw
        .replace("<p/>", "\n\n")
        .replace("<br/>", "\n")
        .replace("<br>", "\n");
    let mut out = String::with_capacity(raw.len());
    let mut in_tag = false;
    for ch in raw.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    unescape_markup_entities(&out)
}

pub(crate) fn unescape_markup_entities(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}
