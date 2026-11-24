use std::{cell::Cell, rc::Rc, time::Duration};

use bible_domain::{
    ids::{BookNumber, ModuleId, VerseRef},
    ports::{ModuleCatalog as _, ModuleKind, VerseSource as _},
    search::Corpus,
};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    scroll::ScrollableElement as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use shell::BookEntry;

use crate::services::{app_services, app_services_arc};

#[derive(Clone)]
pub(crate) struct PickedVerse {
    pub module: ModuleId,
    pub reference: VerseRef,
}

impl PickedVerse {
    pub(crate) fn label(&self, cx: &App) -> String {
        let book_label = app_services(cx)
            .registry
            .lock()
            .books_of(&self.module)
            .into_iter()
            .find(|book| book.book_number == self.reference.book.0)
            .map(|book| book.short_name)
            .unwrap_or_else(|| format!("Кн. {}", self.reference.book.0));
        format!(
            "{book_label} {}:{}",
            self.reference.chapter, self.reference.verse
        )
    }
}

type Confirm = Rc<dyn Fn(PickedVerse, &mut Window, &mut App) -> anyhow::Result<()>>;

pub(crate) fn open(
    title: &'static str,
    preferred: Option<ModuleId>,
    confirm: impl Fn(PickedVerse, &mut Window, &mut App) -> anyhow::Result<()> + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let picker = cx.new(|cx| VersePicker::new(preferred, Rc::new(confirm), window, cx));
    let initial_focus = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if initial_focus.replace(false) {
            picker
                .read(cx)
                .input
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx);
        }
        dialog.title(title).width(px(820.)).child(picker.clone())
    });
}

/// Append a Scripture address to an existing comma-separated field.
pub(crate) fn append_to_field(
    input: Entity<InputState>,
    title: &'static str,
    window: &mut Window,
    cx: &mut App,
) {
    open(
        title,
        None,
        move |picked, window, cx| {
            let reference = bible_domain::reference::Reference {
                book_number: picked.reference.book.0,
                chapter: picked.reference.chapter,
                verse: Some(picked.reference.verse),
                end_verse: None,
            };
            let label = picked.label(cx);
            let address = if crate::chains::parse_refs_input(app_services(cx), &label)
                .is_ok_and(|refs| refs == [reference.clone()])
            {
                label
            } else {
                bible_domain::chains::reference_to_compact(&reference)
            };
            input.update(cx, |input, cx| {
                let current = input.value();
                let current = current.trim().trim_end_matches(',').trim_end();
                let value = if current.is_empty() {
                    address
                } else {
                    format!("{current}, {address}")
                };
                input.set_value(value, window, cx);
            });
            Ok(())
        },
        window,
        cx,
    );
}

#[derive(Clone)]
struct PreviewRow {
    picked: PickedVerse,
    text: String,
}

struct VersePicker {
    input: Entity<InputState>,
    modules: Vec<ModuleId>,
    module: Option<ModuleId>,
    books: Vec<BookEntry>,
    book: Option<BookEntry>,
    chapters: Vec<u16>,
    chapter: Option<u16>,
    verses: Vec<u16>,
    list: ListState,
    preview_list: ListState,
    preview_rows: Rc<Vec<PreviewRow>>,
    preview_task: Option<Task<()>>,
    pending: Option<PreviewRow>,
    selected: Option<PickedVerse>,
    hits: Rc<Vec<shell::search::SearchHit>>,
    searching: bool,
    indexing: bool,
    search_error: Option<String>,
    save_error: Option<String>,
    confirm: Confirm,
    search: Option<Task<()>>,
    _subscription: Subscription,
}

impl VersePicker {
    fn new(
        preferred: Option<ModuleId>,
        confirm: Confirm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(crate::i18n::tr_static("Поиск по всем переводам Библии"))
        });
        let subscription = cx.subscribe_in(&input, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::Change) {
                this.search(window, cx);
            }
        });
        let services = app_services(cx);
        let preferred = preferred
            .or_else(|| {
                services
                    .current_location
                    .lock()
                    .as_ref()
                    .map(|loc| loc.module.clone())
            })
            .or_else(|| services.default_translation());
        let modules = crate::verse_preview::bible_modules(cx);
        let module = preferred
            .filter(|id| modules.contains(id))
            .or_else(|| modules.first().cloned());
        let books = module
            .as_ref()
            .map(|id| services.registry.lock().books_of(id))
            .unwrap_or_default();
        Self {
            input,
            modules,
            module,
            books,
            book: None,
            chapters: Vec::new(),
            chapter: None,
            verses: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(350.)),
            preview_list: ListState::new(0, ListAlignment::Top, px(340.)),
            preview_rows: Rc::default(),
            preview_task: None,
            pending: None,
            selected: None,
            hits: Rc::default(),
            searching: false,
            indexing: false,
            search_error: None,
            save_error: None,
            confirm,
            search: None,
            _subscription: subscription,
        }
    }

    fn search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search = None;
        self.hits = Rc::default();
        self.list.reset(0);
        self.save_error = None;
        self.search_error = None;
        self.indexing = false;
        let query = self.input.read(cx).value().trim().to_owned();
        self.searching = !query.is_empty();
        cx.notify();
        if query.is_empty() {
            return;
        }
        let services = app_services_arc(cx);
        self.search = Some(cx.spawn_in(window, async move |this, window| {
            let executor = window.background_executor().clone();
            executor.timer(Duration::from_millis(220)).await;
            loop {
                let (search_services, query) = (services.clone(), query.clone());
                let result = executor
                    .spawn(async move {
                        let modules = search_services
                            .registry
                            .modules()
                            .into_iter()
                            .filter(|meta| meta.kind == ModuleKind::BibleTranslation)
                            .map(|meta| meta.id)
                            .collect::<std::collections::HashSet<_>>();
                        shell::search::search_sources(&search_services, &query, Corpus::All, |id| {
                            modules.contains(id)
                        })
                    })
                    .await;
                let pending = result
                    .as_ref()
                    .map(|results| results.building.clone())
                    .unwrap_or_default();
                if this
                    .update_in(window, |this, _, cx| {
                        this.searching = false;
                        this.indexing = !pending.is_empty();
                        match result {
                            Ok(results) => {
                                this.list.reset(results.hits.len());
                                this.hits = Rc::new(results.hits);
                            }
                            Err(error) => this.search_error = Some(error.to_string()),
                        }
                        cx.notify();
                    })
                    .is_err()
                    || pending.is_empty()
                {
                    break;
                }
                while pending
                    .iter()
                    .all(|id| services.index_building.lock().contains(id))
                {
                    executor.timer(Duration::from_millis(250)).await;
                }
            }
        }));
    }

    fn pick_module(&mut self, module: ModuleId, cx: &mut Context<Self>) {
        if self.module.as_ref() == Some(&module) {
            return;
        }
        let book = self.book.as_ref().map(|book| book.book_number);
        let chapter = self.chapter;
        let verse = self.selected.as_ref().map(|picked| picked.reference.verse);
        self.books = app_services(cx).registry.lock().books_of(&module);
        self.module = Some(module);
        if let Some(entry) = self
            .books
            .iter()
            .find(|entry| Some(entry.book_number) == book)
            .cloned()
        {
            self.pick_book(entry, cx);
            if let Some(chapter) = chapter.filter(|chapter| self.chapters.contains(chapter)) {
                self.pick_chapter(chapter, cx);
                if let Some(verse) = verse.filter(|verse| self.verses.contains(verse)) {
                    self.select_numbered_verse(verse, cx);
                }
            }
        } else {
            self.reset_book();
        }
        cx.notify();
    }

    fn reset_book(&mut self) {
        self.book = None;
        self.chapter = None;
        self.selected = None;
        self.preview_task = None;
        self.preview_rows = Rc::default();
        self.preview_list.reset(0);
    }

    fn pick_book(&mut self, book: BookEntry, cx: &mut Context<Self>) {
        let Some(module) = self.module.clone() else {
            return;
        };
        let number = BookNumber(book.book_number);
        let count = app_services(cx)
            .registry
            .lock()
            .chapter_count_of(&module, number);
        self.reset_book();
        self.chapters = (1..=count).collect();
        self.book = Some(book);
        if count > 0 {
            self.pick_chapter(1, cx);
        }
        let services = app_services_arc(cx);
        let job = cx.background_executor().spawn(async move {
            let raw = services
                .registry
                .lock()
                .verses_of_book(&module, number)
                .unwrap_or_default();
            raw.into_iter()
                .map(|(chapter, verse, text)| PreviewRow {
                    picked: PickedVerse {
                        module: module.clone(),
                        reference: VerseRef::single(number, chapter, verse),
                    },
                    text: crate::verse_preview::plain_text(&text),
                })
                .collect::<Vec<_>>()
        });
        self.preview_task = Some(cx.spawn(async move |this, cx| {
            let rows = job.await;
            let _ = this.update(cx, |this, cx| {
                this.preview_list.reset(rows.len());
                this.preview_rows = Rc::new(rows);
                this.preview_task = None;
                let verse = this.selected.as_ref().map(|picked| picked.reference.verse);
                this.scroll_to(this.chapter.unwrap_or(1), verse);
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn pick_chapter(&mut self, chapter: u16, cx: &mut Context<Self>) {
        let (Some(book), Some(module)) = (&self.book, &self.module) else {
            return;
        };
        self.verses = app_services(cx).registry.lock().verse_numbers_of(
            module,
            BookNumber(book.book_number),
            chapter,
        );
        self.chapter = Some(chapter);
        self.selected = None;
        self.scroll_to(chapter, None);
        cx.notify();
    }

    fn select_numbered_verse(&mut self, verse: u16, cx: &mut Context<Self>) {
        let (Some(book), Some(module), Some(chapter)) = (&self.book, &self.module, self.chapter)
        else {
            return;
        };
        self.select_verse(
            PickedVerse {
                module: module.clone(),
                reference: VerseRef::single(BookNumber(book.book_number), chapter, verse),
            },
            cx,
        );
    }

    fn scroll_to(&self, chapter: u16, verse: Option<u16>) {
        if let Some(index) = self.preview_rows.iter().position(|row| {
            let reference = row.picked.reference;
            reference.chapter == chapter && verse.is_none_or(|verse| reference.verse == verse)
        }) {
            self.preview_list.scroll_to(ListOffset {
                item_ix: index,
                offset_in_item: px(0.),
            });
        }
    }

    fn select_verse(&mut self, picked: PickedVerse, cx: &mut Context<Self>) {
        let reference = picked.reference;
        if self.chapter != Some(reference.chapter) {
            self.pick_chapter(reference.chapter, cx);
        }
        self.scroll_to(reference.chapter, Some(reference.verse));
        self.selected = Some(picked);
        cx.notify();
    }

    fn choose(&mut self, picked: PickedVerse, cx: &mut Context<Self>) {
        self.scroll_to(picked.reference.chapter, Some(picked.reference.verse));
        let text = self
            .preview_rows
            .iter()
            .find(|row| {
                row.picked.module == picked.module && row.picked.reference == picked.reference
            })
            .map(|row| row.text.clone())
            .or_else(|| {
                app_services(cx)
                    .registry
                    .verse_range(&picked.module, &picked.reference)
                    .ok()
                    .map(|raw| crate::verse_preview::plain_text(&raw))
            })
            .unwrap_or_default();
        self.pending = Some(PreviewRow { picked, text });
        self.save_error = None;
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = &self.pending else { return };
        self.commit(row.picked.clone(), window, cx);
    }

    fn commit(&mut self, picked: PickedVerse, window: &mut Window, cx: &mut Context<Self>) {
        match (self.confirm)(picked, window, cx) {
            Ok(()) => window.close_dialog(cx),
            Err(error) => {
                self.save_error = Some(error.to_string());
                cx.notify();
            }
        }
    }

    fn render_preview(&self, cx: &mut Context<Self>) -> AnyElement {
        let weak = cx.weak_entity();
        let rows = self.preview_rows.clone();
        let selected = self.selected.as_ref().map(|picked| picked.reference);
        if rows.is_empty() {
            return div()
                .p_2()
                .child(if self.preview_task.is_some() {
                    "Загрузка стихов…"
                } else {
                    "Стихи отсутствуют."
                })
                .into_any_element();
        }
        div()
            .relative()
            .size_full()
            .child(
                list(self.preview_list.clone(), move |index, _, cx| {
                    let row = &rows[index];
                    let chapter = row.picked.reference.chapter;
                    let new_chapter =
                        index == 0 || rows[index - 1].picked.reference.chapter != chapter;
                    let picked = row.picked.clone();
                    let weak = weak.clone();
                    let rtl = crate::bidi::is_hebrew_paragraph(&row.text);
                    v_flex()
                        .id(("browse-verse-row", index))
                        .p_2()
                        .gap_1()
                        .bg(if selected == Some(picked.reference) {
                            cx.theme().accent.opacity(0.22)
                        } else {
                            crate::ui_scale::row_background(index, cx)
                        })
                        .cursor_pointer()
                        .hover(|style| style.bg(cx.theme().accent.opacity(0.15)))
                        .children(new_chapter.then(|| {
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format!("Глава {chapter}"))
                        }))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "{chapter}:{} · {}",
                                    picked.reference.verse, picked.module
                                )),
                        )
                        .child(
                            div()
                                .when(rtl, |text| text.text_right())
                                .child(row.text.clone()),
                        )
                        .on_click(move |_, _, cx| {
                            let _ =
                                weak.update(cx, |this, cx| this.select_verse(picked.clone(), cx));
                        })
                        .into_any_element()
                })
                .size_full(),
            )
            .vertical_scrollbar(&self.preview_list)
            .into_any_element()
    }

    fn render_confirmation(&self, row: &PreviewRow, cx: &mut Context<Self>) -> AnyElement {
        let label = row.picked.label(cx);
        v_flex()
            .size_full()
            .gap_2()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("{label} · {}", row.picked.module)),
            )
            .child(
                div()
                    .id("confirm-verse-text")
                    .flex_1()
                    .min_h_0()
                    .p_2()
                    .when(crate::bidi::is_hebrew_paragraph(&row.text), |text| {
                        text.text_right()
                    })
                    .child(row.text.clone())
                    .overflow_y_scrollbar(),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("verse-confirm-back")
                            .ghost()
                            .label(crate::i18n::tr_static("← Назад"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.pending = None;
                                this.save_error = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("verse-confirm-add")
                            .primary()
                            .label(crate::i18n::tr_static("Добавить"))
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_dropdown<T: Clone + PartialEq + ToString + 'static>(
        id: &'static str,
        label: &'static str,
        options: Vec<T>,
        selected: Option<T>,
        cx: &mut Context<Self>,
        choose: fn(&mut Self, T, &mut Context<Self>),
    ) -> AnyElement {
        let caption = selected
            .as_ref()
            .map_or_else(|| label.to_owned(), |item| item.to_string());
        let weak = cx.weak_entity();
        v_flex()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(
                Button::new(id)
                    .small()
                    .outline()
                    .w_full()
                    .label(caption)
                    .disabled(options.is_empty())
                    .dropdown_menu(move |mut menu, _, _| {
                        menu = menu.scrollable(true);
                        for item in &options {
                            let checked = selected.as_ref() == Some(item);
                            let item = item.clone();
                            let weak = weak.clone();
                            menu = menu.item(
                                PopupMenuItem::new(item.to_string())
                                    .checked(checked)
                                    .on_click(move |_, _, cx| {
                                        let _ = weak
                                            .update(cx, |this, cx| choose(this, item.clone(), cx));
                                    }),
                            );
                        }
                        menu
                    }),
            )
            .into_any_element()
    }

    fn render_browse(&self, cx: &mut Context<Self>) -> AnyElement {
        use bible_domain::ids::OLD_TESTAMENT_BOUNDARY;
        let modules = Self::render_dropdown(
            "verse-picker-module",
            "Модуль Библии",
            self.modules.clone(),
            self.module.clone(),
            cx,
            Self::pick_module,
        );
        let mut body = v_flex().gap_2().p_1().child(modules);
        if let Some(book) = &self.book {
            body = body.child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("verse-picker-back")
                            .xsmall()
                            .ghost()
                            .label(crate::i18n::tr_static("← Книги"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.reset_book();
                                cx.notify();
                            })),
                    )
                    .child(book.long_name.clone()),
            );
            body = body
                .child(Self::render_dropdown(
                    "verse-picker-chapter",
                    "Глава",
                    self.chapters.clone(),
                    self.chapter,
                    cx,
                    Self::pick_chapter,
                ))
                .child(Self::render_dropdown(
                    "verse-picker-verse",
                    "Стих",
                    self.verses.clone(),
                    self.selected.as_ref().map(|picked| picked.reference.verse),
                    cx,
                    Self::select_numbered_verse,
                ));
        } else {
            for (title, new_testament) in [("Ветхий Завет", false), ("Новый Завет", true)]
            {
                let cells: Vec<_> = self
                    .books
                    .iter()
                    .filter(|book| (book.book_number >= OLD_TESTAMENT_BOUNDARY) == new_testament)
                    .map(|book| {
                        let choice = book.clone();
                        book_cell(book, cx)
                            .on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.pick_book(choice.clone(), cx)
                                }),
                            )
                            .into_any_element()
                    })
                    .collect();
                if !cells.is_empty() {
                    body = body
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(title),
                        )
                        .child(grid(10, cells));
                }
            }
            if self.books.is_empty() {
                body = body.child(crate::i18n::tr_static(
                    "Установите хотя бы один перевод Библии.",
                ));
            }
        }
        let controls = div()
            .id("verse-picker-books")
            .h_full()
            .min_h_0()
            .child(body)
            .overflow_y_scrollbar();
        if self.book.is_some() {
            h_flex()
                .size_full()
                .gap_2()
                .child(div().w(px(285.)).h_full().flex_shrink_0().child(controls))
                .child(
                    div()
                        .flex_1()
                        .h_full()
                        .min_w_0()
                        .border_l_1()
                        .border_color(cx.theme().border)
                        .child(self.render_preview(cx)),
                )
                .into_any_element()
        } else {
            controls.into_any_element()
        }
    }

    fn render_results(&self, cx: &mut Context<Self>) -> AnyElement {
        let hits = self.hits.clone();
        let weak = cx.weak_entity();
        let muted = cx.theme().muted_foreground;
        let hover = cx.theme().accent.opacity(0.12);
        let status = if self.searching {
            Some("Поиск…")
        } else if self.indexing {
            Some("Ожидаю индексацию переводов…")
        } else if hits.is_empty() && self.search_error.is_none() {
            Some("Стихи не найдены.")
        } else {
            None
        };
        v_flex()
            .size_full()
            .min_h_0()
            .children(status.map(|status| div().p_2().text_sm().text_color(muted).child(status)))
            .children(
                self.search_error
                    .clone()
                    .map(|error| div().p_2().text_color(cx.theme().danger).child(error)),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(
                        list(self.list.clone(), move |index, _, _| {
                            let Some(hit) = hits.get(index) else {
                                return div().into_any_element();
                            };
                            let picked = PickedVerse {
                                module: hit.module.clone(),
                                reference: VerseRef::single(hit.book, hit.chapter, hit.verse),
                            };
                            let weak = weak.clone();
                            let text = crate::text::unescape_markup_entities(&hit.text);
                            v_flex()
                                .id(("verse-picker-hit", index))
                                .p_2()
                                .gap_1()
                                .rounded_md()
                                .cursor_pointer()
                                .hover(|style| style.bg(hover))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(format!("{} · {}", hit.label, hit.module)),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .when(crate::bidi::is_hebrew_paragraph(&text), |el| {
                                            el.text_right()
                                        })
                                        .child(text),
                                )
                                .on_click(move |_, window, cx| {
                                    let _ = weak.update(cx, |picker, cx| {
                                        picker.commit(picked.clone(), window, cx)
                                    });
                                })
                                .into_any_element()
                        })
                        .size_full(),
                    )
                    .vertical_scrollbar(&self.list),
            )
            .into_any_element()
    }
}

impl Render for VersePicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let browse = self.input.read(cx).value().trim().is_empty();
        v_flex()
            .gap_2()
            .children(self.pending.is_none().then(|| Input::new(&self.input)))
            .children(
                self.save_error
                    .clone()
                    .map(|error| div().text_sm().text_color(cx.theme().danger).child(error)),
            )
            .child(
                div()
                    .h(px(340.))
                    .min_h_0()
                    .child(if let Some(row) = &self.pending {
                        self.render_confirmation(row, cx)
                    } else if browse {
                        self.render_browse(cx)
                    } else {
                        self.render_results(cx)
                    }),
            )
            .children(
                (browse && self.pending.is_none())
                    .then(|| self.selected.clone())
                    .flatten()
                    .map(|picked| {
                        h_flex().justify_end().child(
                            Button::new("verse-next")
                                .small()
                                .primary()
                                .label(crate::i18n::tr_static("Далее"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.choose(picked.clone(), cx)
                                })),
                        )
                    }),
            )
    }
}

pub(crate) fn book_cell(book: &BookEntry, cx: &App) -> Stateful<Div> {
    grid_cell(
        book.book_number as usize,
        book.short_name.clone(),
        book_tint(BookNumber(book.book_number), cx),
        px(52.),
    )
}

pub(crate) fn grid_cell(id: usize, label: String, tint: Hsla, height: Pixels) -> Stateful<Div> {
    h_flex()
        .id(("verse-picker-cell", id))
        .flex_1()
        .min_w_0()
        .h(height)
        .items_center()
        .justify_center()
        .rounded_md()
        .cursor_pointer()
        .bg(tint.opacity(0.22))
        .hover(|style| style.bg(tint.opacity(0.38)))
        .child(div().text_xs().truncate().child(label))
}

pub(crate) fn grid(columns: usize, cells: impl IntoIterator<Item = AnyElement>) -> AnyElement {
    let mut body = v_flex().w_full().gap_1();
    let mut row = Vec::with_capacity(columns);
    for cell in cells {
        row.push(cell);
        if row.len() == columns {
            body = body.child(h_flex().w_full().gap_1().children(row.drain(..)));
        }
    }
    if !row.is_empty() {
        row.resize_with(columns, || div().flex_1().into_any_element());
        body = body.child(h_flex().w_full().gap_1().children(row));
    }
    body.into_any_element()
}
pub(crate) fn book_tint(book: BookNumber, cx: &App) -> Hsla {
    match book.0 {
        10..=50 => cx.theme().red,
        60..=190 => cx.theme().yellow,
        220..=260 => cx.theme().green,
        290..=340 => cx.theme().cyan,
        350..=460 => cx.theme().blue,
        470..=510 => cx.theme().magenta,
        520..=650 => cx.theme().warning,
        660..=730 => cx.theme().info,
        _ => cx.theme().muted_foreground,
    }
}
