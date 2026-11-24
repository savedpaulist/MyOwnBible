use std::{cell::Cell, ops::Range, path::Path};

use bible_domain::{
    ids::{BookNumber, ModuleId, VerseRef},
    link::LinkTarget,
    ports::{ModuleCatalog as _, ModuleKind},
};
use gpui_kit::base::{TextView, h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{EditorState, Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _};
use gpui_kit::*;
use shell::notes::wikilink::{self, WikilinkTarget};

use crate::{article, services::app_services};

#[derive(Clone, Copy)]
enum Format {
    Wrap(&'static str, &'static str),
    Prefix(&'static str),
}

pub(crate) fn toolbar(editor: &Entity<EditorState>, relpath: &str, cx: &App) -> impl IntoElement {
    let mut bar = h_flex()
        .w_full()
        .items_center()
        .flex_wrap()
        .gap_1()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(cx.theme().border);
    for (id, label, hint, action) in [
        ("bold", "B", "Полужирный", Format::Wrap("**", "**")),
        ("italic", "I", "Курсив", Format::Wrap("*", "*")),
        (
            "underline",
            "U̲",
            "Подчёркивание",
            Format::Wrap("<u>", "</u>"),
        ),
        ("strike", "S̶", "Зачёркивание", Format::Wrap("~~", "~~")),
        (
            "highlight",
            "▧",
            "Выделение цветом",
            Format::Wrap("<mark>", "</mark>"),
        ),
        ("heading", "H₂", "Заголовок", Format::Prefix("## ")),
        ("list", "•", "Список", Format::Prefix("- ")),
        ("quote", "❯", "Цитата", Format::Prefix("> ")),
        ("code", "` `", "Код", Format::Wrap("`", "`")),
    ] {
        let editor = editor.clone();
        bar = bar.child(
            Button::new(id)
                .ghost()
                .xsmall()
                .label(label)
                .tooltip(hint)
                .on_click(move |_, window, cx| {
                    editor.update(cx, |editor, cx| {
                        let text = editor.value();
                        let (range, replacement, selection) =
                            format_edit(&text, editor.selected_range(), action);
                        editor.set_selected_range(range, cx);
                        editor.replace(replacement, window, cx);
                        editor.set_selected_range(selection, cx);
                        editor.focus(window, cx);
                    });
                }),
        );
    }
    let editor = editor.clone();
    let relpath = relpath.to_owned();
    bar.child(
        Button::new("insert-wikilink")
            .ghost()
            .xsmall()
            .icon(gpui_kit::assets::IconName::Link)
            .label(crate::i18n::tr_static("Ссылка"))
            .dropdown_menu(move |mut menu, _, _| {
                for kind in [
                    LinkKind::Bible,
                    LinkKind::Dictionary,
                    LinkKind::Commentary,
                    LinkKind::Note,
                ] {
                    let editor = editor.downgrade();
                    let relpath = relpath.clone();
                    menu = menu.item(PopupMenuItem::new(kind.title()).on_click(
                        move |_, window, cx| {
                            open_link_dialog(editor.clone(), relpath.clone(), kind, window, cx);
                        },
                    ));
                }
                menu
            }),
    )
}

fn format_edit(
    text: &str,
    selected: Range<usize>,
    action: Format,
) -> (Range<usize>, String, Range<usize>) {
    match action {
        Format::Wrap(left, right) => {
            let value = &text[selected.clone()];
            let start = selected.start;
            let end = selected.end;
            if !value.is_empty() && text[..start].ends_with(left) && text[end..].starts_with(right)
            {
                let start = start - left.len();
                (
                    start..end + right.len(),
                    value.to_owned(),
                    start..start + value.len(),
                )
            } else {
                let caret = start + left.len();
                (
                    selected,
                    format!("{left}{value}{right}"),
                    caret..caret + value.len(),
                )
            }
        }
        Format::Prefix(prefix) => {
            let start = text[..selected.start]
                .rfind('\n')
                .map_or(0, |index| index + 1);
            let cursor = if selected.is_empty() {
                selected.end
            } else {
                text[..selected.end].trim_end_matches('\n').len()
            };
            let cursor = cursor.max(start);
            let end = text[cursor..]
                .find('\n')
                .map_or(text.len(), |index| cursor + index);
            let fragment = &text[start..end];
            let remove = fragment.split('\n').all(|line| line.starts_with(prefix));
            let replacement = fragment
                .split('\n')
                .map(|line| {
                    if remove {
                        line[prefix.len()..].to_owned()
                    } else {
                        format!("{prefix}{line}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            let selection = if selected.is_empty() {
                let caret = if remove {
                    selected.start.saturating_sub(prefix.len()).max(start)
                } else {
                    selected.start + prefix.len()
                };
                caret..caret
            } else {
                start..start + replacement.len()
            };
            (start..end, replacement, selection)
        }
    }
}

#[derive(Clone, Copy)]
enum LinkKind {
    Bible,
    Dictionary,
    Commentary,
    Note,
}

impl LinkKind {
    fn title(self) -> &'static str {
        match self {
            Self::Bible => "Библейский стих",
            Self::Dictionary => "Статья словаря",
            Self::Commentary => "Комментарий",
            Self::Note => "Другая заметка",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Bible => "Выберите стих",
            Self::Dictionary => "Модуль/Тема, например Strong/G3056",
            Self::Commentary => "Модуль/Ссылка, например Комментарий/Ин 3:16",
            Self::Note => "Название или путь заметки внутри хранилища",
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Self::Bible => "bible:",
            Self::Dictionary => "dict:",
            Self::Commentary => "commentary:",
            Self::Note => "",
        }
    }
}

fn open_link_dialog(
    editor: WeakEntity<EditorState>,
    relpath: String,
    kind: LinkKind,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(edit) = editor.upgrade() else { return };
    let selected = edit.read(cx).selected_text().to_string();
    let range = edit.read(cx).selected_range();
    if matches!(kind, LinkKind::Bible) {
        crate::verse_picker::open(
            "Вставить ссылку на стих",
            None,
            move |picked, window, cx| {
                let reference = picked.reference;
                let label = picked.label(cx);
                let alias = wiki_alias(&selected).unwrap_or(&label);
                let address = format!(
                    "@{}/{}:{}",
                    reference.book.0, reference.chapter, reference.verse
                );
                let link = format!("[[bible:{address}|{alias}]]");
                insert_wikilink(
                    &editor,
                    range.clone(),
                    &selected,
                    if selected.is_empty() || wiki_alias(&selected).is_some() {
                        link
                    } else {
                        format!("{selected}{link}")
                    },
                    window,
                    cx,
                )
            },
            window,
            cx,
        );
        return;
    }
    let form = cx.new(|cx| LinkDialog::new(editor, relpath, kind, selected, range, window, cx));
    let initial_focus = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if initial_focus.replace(false) {
            form.read(cx)
                .input
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx);
        }
        dialog
            .title(format!("Вставить ссылку · {}", kind.title()))
            .width(px(480.))
            .child(form.clone())
    });
}

fn wiki_alias(selection: &str) -> Option<&str> {
    let alias = selection.trim();
    (!alias.is_empty() && !alias.contains(['[', ']', '|', '\n', '\r'])).then_some(alias)
}

fn insert_wikilink(
    editor: &WeakEntity<EditorState>,
    range: std::ops::Range<usize>,
    selected: &str,
    replacement: String,
    window: &mut Window,
    cx: &mut App,
) -> anyhow::Result<()> {
    editor.update(cx, |editor, cx| {
        anyhow::ensure!(
            editor.value().get(range.clone()) == Some(selected),
            "Выделенный текст изменился. Откройте окно ссылки заново."
        );
        editor.set_selected_range(range, cx);
        editor.replace(replacement, window, cx);
        Ok(())
    })?
}

struct LinkDialog {
    editor: WeakEntity<EditorState>,
    relpath: String,
    kind: LinkKind,
    selected: String,
    range: Range<usize>,
    input: Entity<InputState>,
    error: Option<&'static str>,
    _subscription: Subscription,
}

impl LinkDialog {
    fn new(
        editor: WeakEntity<EditorState>,
        relpath: String,
        kind: LinkKind,
        selected: String,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(kind.hint()));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.confirm(window, cx),
                InputEvent::Change => {
                    this.error = None;
                    cx.notify();
                }
                _ => {}
            },
        );
        Self {
            editor,
            relpath,
            kind,
            selected,
            range,
            input,
            error: None,
            _subscription: subscription,
        }
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.input.read(cx).value();
        let address = value.trim();
        if address.is_empty() || address.contains(['[', ']', '|', '\n', '\r']) {
            self.error = Some("Укажите адрес ссылки без квадратных скобок и переноса строки.");
            cx.notify();
            return;
        }
        let target = format!("[[{}{address}]]", self.kind.prefix());
        let parsed = wikilink::scan(&target);
        let notes = matches!(self.kind, LinkKind::Note)
            .then(|| note_targets(cx))
            .unwrap_or_default();
        if parsed
            .first()
            .and_then(|link| destination(&link.target, &self.relpath, &notes, cx))
            .is_none()
        {
            self.error =
                Some("Цель не найдена. Проверьте название заметки, модуль или место Писания.");
            cx.notify();
            return;
        }
        let replacement =
            if let Some(alias) = wiki_alias(&self.selected).filter(|alias| *alias != address) {
                format!("[[{}{address}|{alias}]]", self.kind.prefix())
            } else if self.selected.is_empty() || self.selected.trim() == address {
                target
            } else {
                format!("{}{target}", self.selected)
            };
        let inserted = insert_wikilink(
            &self.editor,
            self.range.clone(),
            &self.selected,
            replacement,
            window,
            cx,
        );
        if inserted.is_ok() {
            window.close_dialog(cx);
            let _ = self
                .editor
                .update(cx, |editor, cx| editor.focus(window, cx));
        } else {
            self.error = Some("Выделенный текст изменился. Откройте окно ссылки заново.");
            cx.notify();
        }
    }
}

impl Render for LinkDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_2()
            .child(Input::new(&self.input))
            .children(
                self.error
                    .map(|error| div().text_sm().text_color(cx.theme().danger).child(error)),
            )
            .child(
                Button::new("confirm-wikilink")
                    .primary()
                    .label(crate::i18n::tr_static("Вставить"))
                    .on_click(cx.listener(|this, _, window, cx| this.confirm(window, cx))),
            )
    }
}

fn note_targets(cx: &App) -> Vec<shell::notes::NoteSummary> {
    let index = shell::notes::current(app_services(cx));
    index
        .and_then(|index| shell::notes::list_notes(&index).ok())
        .unwrap_or_default()
}

fn note_path(target: &str, current: &str, notes: &[shell::notes::NoteSummary]) -> Option<String> {
    let target = target.trim().trim_end_matches(".md").to_lowercase();
    let local = Path::new(current)
        .parent()
        .and_then(|path| path.to_str())
        .filter(|path| !path.is_empty())
        .map(|parent| format!("{}/{target}", parent.to_lowercase()));
    let matches_path = |note: &&shell::notes::NoteSummary, path: &str| {
        note.relpath.trim_end_matches(".md").to_lowercase() == path
    };
    if let Some(note) = local
        .as_ref()
        .and_then(|path| notes.iter().find(|note| matches_path(note, path)))
        .or_else(|| notes.iter().find(|note| matches_path(note, &target)))
    {
        return Some(note.relpath.clone());
    }
    let mut candidates = notes.iter().filter(|note| {
        note.title.to_lowercase() == target
            || note
                .relpath
                .rsplit('/')
                .next()
                .is_some_and(|part| part.trim_end_matches(".md").to_lowercase() == target)
            || note
                .aliases
                .iter()
                .any(|name| name.to_lowercase() == target)
    });
    match (candidates.next(), candidates.next()) {
        (Some(note), None) => Some(note.relpath.clone()),
        _ => None,
    }
}

fn destination(
    target: &WikilinkTarget,
    current: &str,
    notes: &[shell::notes::NoteSummary],
    cx: &App,
) -> Option<LinkTarget> {
    let services = app_services(cx);
    let reference = |text: &str, fallback_verse| {
        let registry = services.registry.lock();
        let parsed = bible_domain::reference::parse(text, |book| registry.lookup_book(book))?;
        Some(VerseRef {
            book: BookNumber(parsed.book_number),
            chapter: parsed.chapter,
            verse: parsed.verse.unwrap_or(fallback_verse),
            end_verse: parsed.end_verse,
        })
    };
    match target {
        WikilinkTarget::Bible(text) => reference(text, 1).map(LinkTarget::Bible),
        WikilinkTarget::BibleNumeric {
            book,
            chapter,
            verse,
            end_verse,
        } => Some(LinkTarget::Bible(VerseRef {
            book: BookNumber(*book),
            chapter: *chapter,
            verse: verse.unwrap_or(1),
            end_verse: *end_verse,
        })),
        WikilinkTarget::Dict { module_id, topic } => {
            let module = ModuleId(module_id.clone());
            services.registry.module(&module).filter(|meta| {
                matches!(
                    meta.kind,
                    ModuleKind::GeneralDictionary | ModuleKind::StrongsDictionary
                )
            })?;
            Some(LinkTarget::DictionaryEntry {
                module,
                topic: topic.clone(),
            })
        }
        WikilinkTarget::Commentary {
            module_id,
            reference: text,
        } => {
            let module = ModuleId(module_id.clone());
            services
                .registry
                .module(&module)
                .filter(|meta| meta.kind == ModuleKind::Commentary)?;
            Some(LinkTarget::Commentary {
                module,
                reference: reference(text, 0)?,
            })
        }
        WikilinkTarget::Note(text) => note_path(text, current, notes)
            .map(LinkTarget::Note)
            .or_else(|| reference(text, 1).map(LinkTarget::Bible)),
        _ => None,
    }
}

pub(crate) fn preview(id: &'static str, raw: &str, relpath: &str, cx: &App) -> TextView {
    let links = wikilink::scan(raw);
    let notes = if links
        .iter()
        .any(|link| matches!(link.target, WikilinkTarget::Note(_)))
    {
        note_targets(cx)
    } else {
        Vec::new()
    };
    let markdown = render_links(raw, links, |target| {
        destination(target, relpath, &notes, cx)
    });
    article::linked_text_view(TextView::markdown(id, markdown), cx)
        .scrollable(true)
        .size_full()
        .p_3()
}

fn render_links(
    raw: &str,
    links: Vec<wikilink::Wikilink>,
    resolve: impl Fn(&WikilinkTarget) -> Option<LinkTarget>,
) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut cursor = 0;
    for link in links {
        let Some(url) =
            resolve(&link.target).and_then(|target| article::link_target_to_url(&target))
        else {
            continue;
        };
        let start = link.raw_range.start;
        let end = link.raw_range.end;
        let label = link
            .alias
            .as_deref()
            .filter(|alias| !alias.is_empty())
            .unwrap_or(raw[start + 2..end - 2].trim());
        let text_end = if start > cursor && raw.as_bytes()[start - 1] == b'!' {
            start - 1
        } else {
            start
        };
        out.push_str(&raw[cursor..text_end]);
        out.push('[');
        for ch in label.chars() {
            if matches!(ch, '\\' | '[' | ']') {
                out.push('\\');
            }
            out.push(ch);
        }
        out.push_str("](<");
        out.push_str(&url);
        out.push_str(">)");
        cursor = end;
    }
    out.push_str(&raw[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::{BookNumber, Format, LinkTarget, VerseRef, format_edit, render_links, wikilink};

    #[test]
    fn formats_utf8_selection_and_toggles_existing_markup() {
        let action = Format::Wrap("**", "**");
        let (range, text, selection) = format_edit("мир", 0..6, action);
        assert_eq!((range, text.as_str(), selection), (0..6, "**мир**", 2..8));
        let (range, text, selection) = format_edit("**мир**", 2..8, action);
        assert_eq!((range, text.as_str(), selection), (0..10, "мир", 0..6));
    }

    #[test]
    fn turns_wiki_links_into_internal_links_only_outside_fenced_code() {
        let raw = "[[bible:John 3:16|verse]]\n```\n[[bible:John 3:16]]\n```";
        let output = render_links(raw, wikilink::scan(raw), |_| {
            Some(LinkTarget::Bible(VerseRef {
                book: BookNumber(500),
                chapter: 3,
                verse: 16,
                end_verse: None,
            }))
        });
        assert_eq!(
            output,
            "[verse](<bibleown://bible/500/3/16>)\n```\n[[bible:John 3:16]]\n```"
        );
    }
}
