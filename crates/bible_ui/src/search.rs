//! Панель поиска (architecture.md §6.5, этап 7).
//!
//! Поле запроса и фильтры источников и корпуса → поиск по готовым
//! FTS5-индексам на фоновом потоке. Результаты — виртуальный список
//! «Ин 3:16 · RST» + текст с подсветкой слов запроса; клик открывает
//! место в панели чтения (navigation::OpenBook).

use std::collections::BTreeSet;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Icon, IconName, TitleBar};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use bible_domain::ids::{BookNumber, ModuleId};
use bible_domain::ports::{ModuleCatalog as _, ModuleKind};
use bible_domain::search::Corpus;

use crate::navigation::{self, OpenBook};
use crate::services::app_services_arc;

#[derive(Clone, Copy)]
enum ResultKind {
    Bible,
    Dictionary,
    Commentary,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchMode {
    Exact,
    Partial,
    Similar,
}

impl SearchMode {
    fn label(self, cx: &App) -> SharedString {
        crate::i18n::tr(
            cx,
            match self {
                Self::Exact => "Точное",
                Self::Partial => "Частичное",
                Self::Similar => "Похожее",
            },
        )
    }

    fn query(self, input: &str) -> String {
        let input = input.trim();
        match self {
            Self::Exact => format!("\"{}\"", input.replace('"', "\"\"")),
            Self::Partial | Self::Similar => {
                let separator = if self == Self::Similar { " OR " } else { " " };
                input
                    .split_whitespace()
                    .map(|word| {
                        word.trim_matches(|ch: char| {
                            !ch.is_alphanumeric() && ch != '_' && ch != '-'
                        })
                    })
                    .filter(|word| !word.is_empty())
                    .map(|word| format!("{}*", word.trim_end_matches('*')))
                    .collect::<Vec<_>>()
                    .join(separator)
            }
        }
    }
}

#[derive(Clone, Copy)]
enum ResultRow {
    Category(ResultKind),
    Source(ResultKind, usize),
    Hit(ResultKind, usize, usize),
}

enum ResultAction {
    Verse(OpenBook),
    Topic(ModuleId, String),
}

#[derive(Default)]
struct ResultRows {
    starts: Vec<(usize, ResultRow)>,
    len: usize,
}

impl ResultRows {
    fn push(&mut self, row: ResultRow, count: usize) {
        if count != 0 {
            self.starts.push((self.len, row));
            self.len += count;
        }
    }

    fn at(&self, index: usize) -> ResultRow {
        let entry = self.starts.partition_point(|(start, _)| *start <= index) - 1;
        let (start, row) = self.starts[entry];
        match row {
            ResultRow::Hit(kind, group, _) => ResultRow::Hit(kind, group, index - start),
            _ => row,
        }
    }
}

fn hit_count<T>(groups: &[shell::search::SearchModuleGroup<T>]) -> usize {
    groups.iter().map(|group| group.hits.len()).sum()
}

fn group_info<T>(group: &shell::search::SearchModuleGroup<T>) -> (&ModuleId, Option<&str>, usize) {
    (
        &group.module,
        group.description.as_deref(),
        group.hits.len(),
    )
}

pub struct SearchPanel {
    focus_handle: FocusHandle,
    query_input: Entity<InputState>,
    excluded_sources: BTreeSet<ModuleId>,
    source_categories: BTreeSet<&'static str>,
    excluded_books: BTreeSet<BookNumber>,
    result_list: ListState,
    result_list_dirty: bool,
    corpus: Corpus,
    search_mode: SearchMode,
    respect_diacritics: bool,
    results: Option<shell::SearchResultsViewModel>,
    searching: bool,
    error: Option<String>,
    _search_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
    expanded_categories: BTreeSet<String>,
    expanded_sources: BTreeSet<String>,
}

struct SearchSourcesWindow {
    panel: Entity<SearchPanel>,
}

impl Render for SearchSourcesWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let scale = app_services_arc(cx)
            .settings
            .read()
            .interface_scale
            .clamp(0.8, 1.6);
        window.set_rem_size(px(16.0 * scale));
        let picker = self
            .panel
            .update(cx, |panel, cx| panel.render_source_picker(cx));
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(
                TitleBar::new().child(
                    h_flex()
                        .h_full()
                        .px_3()
                        .items_center()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(crate::i18n::tr(cx, "Источники и книги поиска")),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .p_3()
                    .child(picker)
                    .overflow_y_scrollbar(),
            )
    }
}

fn open_sources_window(panel: Entity<SearchPanel>, cx: &mut App) {
    if let Some(existing) = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<SearchSourcesWindow>())
    {
        let _ = existing.update(cx, |_, window, _| window.activate_window());
        return;
    }
    cx.defer(move |cx| {
        let window_size = size(px(620.0), px(720.0));
        let bounds = Bounds::centered(None, window_size, cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(420.0), px(420.0))),
            ..TitleBar::window_options()
        };
        if let Err(error) = gpui_kit::open_window(options, cx, |_, cx| {
            cx.new(|_| SearchSourcesWindow { panel })
        }) {
            log::error!("failed to open search sources window: {error:#}");
        }
    });
}

impl SearchPanel {
    pub const NAME: &'static str = "search";

    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(crate::i18n::tr_static(
                "возлюбил* мир, \"благодать и истина\", G26…",
            ))
        });

        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(
            &query_input,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.run_search(window, cx);
                }
            },
        ));
        // Запрос из другой панели («Искать слово»/«Искать Strong's» в
        // секции «Слово», §6.4): подставляем запрос и сразу ищем. Панель
        // уже раскрыта WorkspaceView (reveal_search_panel).
        subscriptions.push(cx.subscribe_in(
            &navigation::bus(cx),
            window,
            |this, _, event: &navigation::SearchExecute, window, cx| {
                let query = event.query.clone();
                this.query_input.update(cx, |input, cx| {
                    input.set_value(query, window, cx);
                });
                this.run_search(window, cx);
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &navigation::bus(cx),
            window,
            |_, _, _: &navigation::SettingsChanged, _, cx| cx.notify(),
        ));

        Self {
            focus_handle: cx.focus_handle(),
            query_input,
            excluded_sources: BTreeSet::new(),
            source_categories: BTreeSet::new(),
            excluded_books: BTreeSet::new(),
            result_list: ListState::new(0, ListAlignment::Top, px(300.)),
            result_list_dirty: false,
            corpus: Corpus::All,
            search_mode: SearchMode::Partial,
            respect_diacritics: false,
            results: None,
            searching: false,
            error: None,
            _search_task: None,
            _subscriptions: subscriptions,
            expanded_categories: BTreeSet::new(),
            expanded_sources: BTreeSet::new(),
        }
    }

    /// Запуск поиска: сам сценарий уходит на фоновый поток (построение
    /// индекса по новому модулю — секунды), панель показывает «Ищу…».
    fn run_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw_input = self.query_input.read(cx).value().to_string();
        if raw_input.trim().is_empty() {
            return;
        }
        let input = self.search_mode.query(&raw_input);
        let services = app_services_arc(cx);
        let excluded = self.excluded_sources.clone();
        let excluded_books = self.excluded_books.clone();
        let corpus = self.corpus;
        let respect_diacritics = self.respect_diacritics;
        self.searching = true;
        self.error = None;
        cx.notify();

        self._search_task = Some(cx.spawn_in(window, async move |this, window| {
            let mut first_result = true;
            loop {
                let search_services = services.clone();
                let input = input.clone();
                let raw_input = raw_input.clone();
                let excluded = excluded.clone();
                let excluded_books = excluded_books.clone();
                let result = window
                    .background_executor()
                    .spawn(async move {
                        let mut results = shell::search::search_sources(
                            &search_services,
                            &input,
                            corpus,
                            |id| !excluded.contains(id),
                        )?;
                        if !excluded_books.is_empty() {
                            for group in &mut results.verse_groups {
                                group.hits.retain(|hit| !excluded_books.contains(&hit.book));
                            }
                            for group in &mut results.commentary_groups {
                                group.hits.retain(|hit| !excluded_books.contains(&hit.book));
                            }
                            results.verse_groups.retain(|group| !group.hits.is_empty());
                            results
                                .commentary_groups
                                .retain(|group| !group.hits.is_empty());
                            results.hits = results
                                .verse_groups
                                .iter()
                                .flat_map(|group| group.hits.iter().cloned())
                                .collect();
                            results.hit_count = results.hits.len()
                                + results
                                    .dictionary_groups
                                    .iter()
                                    .map(|group| group.hits.len())
                                    .sum::<usize>()
                                + results
                                    .commentary_groups
                                    .iter()
                                    .map(|group| group.hits.len())
                                    .sum::<usize>();
                        }
                        if respect_diacritics {
                            let needle = raw_input.trim().to_lowercase();
                            let contains = |text: &str| text.to_lowercase().contains(&needle);
                            for group in &mut results.verse_groups {
                                group.hits.retain(|hit| contains(&hit.text));
                            }
                            for group in &mut results.dictionary_groups {
                                group
                                    .hits
                                    .retain(|hit| contains(&hit.snippet) || contains(&hit.topic));
                            }
                            for group in &mut results.commentary_groups {
                                group.hits.retain(|hit| contains(&hit.snippet));
                            }
                            results.verse_groups.retain(|group| !group.hits.is_empty());
                            results
                                .dictionary_groups
                                .retain(|group| !group.hits.is_empty());
                            results
                                .commentary_groups
                                .retain(|group| !group.hits.is_empty());
                            results.hits = results
                                .verse_groups
                                .iter()
                                .flat_map(|group| group.hits.iter().cloned())
                                .collect();
                            results.hit_count = results.hits.len()
                                + results
                                    .dictionary_groups
                                    .iter()
                                    .map(|group| group.hits.len())
                                    .sum::<usize>()
                                + results
                                    .commentary_groups
                                    .iter()
                                    .map(|group| group.hits.len())
                                    .sum::<usize>();
                        }
                        Ok::<_, anyhow::Error>(results)
                    })
                    .await;
                let waiting = match &result {
                    Ok(results) => results.building.clone(),
                    Err(_) => Vec::new(),
                };
                if this
                    .update_in(window, |this, _, cx| {
                        this.searching = false;
                        match result {
                            Ok(results) => {
                                if first_result {
                                    this.result_list.reset(0);
                                }
                                this.result_list_dirty = true;
                                this.results = Some(results);
                                this.error = None;
                            }
                            Err(error) => {
                                log::error!("search failed: {error:#}");
                                this.error = Some(format!("{error:#}"));
                                this.results = None;
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                    || waiting.is_empty()
                {
                    break;
                }
                first_result = false;
                while {
                    window
                        .background_executor()
                        .timer(std::time::Duration::from_millis(250))
                        .await;
                    let pending = services.index_building.lock();
                    waiting.iter().all(|id| pending.contains(id))
                } {}
            }
        }));
    }

    fn highlighted_text(text: &str, terms: &[String], cx: &App) -> StyledText {
        let mut offset = 0;
        let highlights = text.split_whitespace().filter_map(|word| {
            let start = offset + text[offset..].find(word)?;
            offset = start + word.len();
            let normalized: String = word
                .chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect();
            terms
                .iter()
                .any(|term| normalized.starts_with(term))
                .then_some((
                    start..offset,
                    HighlightStyle {
                        color: Some(cx.theme().accent),
                        font_weight: Some(FontWeight::BOLD),
                        ..Default::default()
                    },
                ))
        });
        StyledText::new(text.to_owned()).with_highlights(highlights)
    }

    fn toggle_expanded(&mut self, key: String, is_category: bool, cx: &mut Context<Self>) {
        let set = if is_category {
            &mut self.expanded_categories
        } else {
            &mut self.expanded_sources
        };
        if !set.insert(key.clone()) {
            set.remove(&key);
        }
        self.result_list_dirty = true;
        cx.notify();
    }

    fn group_trigger(
        &self,
        key: String,
        title: String,
        count: usize,
        category: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let open = if category {
            self.expanded_categories.contains(&key)
        } else {
            self.expanded_sources.contains(&key)
        };
        let event_key = key.clone();
        h_flex()
            .id(ElementId::Name(format!("search-group-{key}").into()))
            .w_full()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .when(category, |style| style.bg(cx.theme().secondary))
            .hover(|style| style.bg(crate::ui_scale::hover_background(cx)))
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall(),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(crate::ui_scale::panel_font_size(
                        cx,
                        if category { 14.0 } else { 13.0 },
                    )))
                    .when(category, |style| style.font_weight(FontWeight::SEMIBOLD))
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 11.0)))
                    .text_color(cx.theme().muted_foreground)
                    .child(count.to_string()),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_expanded(event_key.clone(), category, cx);
            }))
    }

    fn render_source_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let modules = app_services_arc(cx).registry.modules();
        let all: BTreeSet<_> = modules
            .iter()
            .filter(|meta| source_category(meta.kind).is_some())
            .map(|meta| meta.id.clone())
            .collect();
        let mut picker = v_flex().w_full().gap_4().child(
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .text_size(px(crate::ui_scale::panel_font_size(cx, 18.0)))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(crate::i18n::tr(cx, "Источники")),
                )
                .child(
                    Button::new("sources-all")
                        .xsmall()
                        .label(crate::i18n::tr(cx, "Выбрать все"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.excluded_sources.clear();
                            this.run_search(window, cx);
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("sources-none")
                        .xsmall()
                        .label(crate::i18n::tr(cx, "Снять все"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.excluded_sources = all.clone();
                            this.run_search(window, cx);
                            cx.notify();
                        })),
                ),
        );
        let mut source_tiles = h_flex().w_full().flex_wrap().items_start().gap_3();
        for category in ["Библия", "Комментарии", "Словари"] {
            let open = self.source_categories.contains(category);
            let category_ids: BTreeSet<_> = modules
                .iter()
                .filter(|meta| source_category(meta.kind) == Some(category))
                .map(|meta| meta.id.clone())
                .collect();
            let category_all = category_ids
                .iter()
                .all(|id| !self.excluded_sources.contains(id));
            let mut tile = v_flex()
                .w(px(270.))
                .min_w(px(230.))
                .max_w(px(340.))
                .border_1()
                .border_color(cx.theme().border)
                .rounded_lg()
                .bg(cx.theme().sidebar)
                .overflow_hidden()
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .p_3()
                        .child(
                            h_flex()
                                .id(ElementId::Name(format!("source-tile-{category}").into()))
                                .flex_1()
                                .min_w_0()
                                .gap_2()
                                .items_center()
                                .cursor_pointer()
                                .hover(|style| style.text_color(cx.theme().accent))
                                .child(
                                    Icon::new(if open {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .xsmall(),
                                )
                                .child(
                                    div()
                                        .text_size(px(crate::ui_scale::panel_font_size(cx, 15.0)))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(crate::i18n::tr(cx, category)),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.source_categories.insert(category) {
                                        this.source_categories.remove(category);
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            Checkbox::new(SharedString::from(format!(
                                "search-source-all-{category}"
                            )))
                            .label(crate::i18n::tr(cx, "Все"))
                            .checked(category_all)
                            .on_click(cx.listener(
                                move |this, checked, window, cx| {
                                    for id in &category_ids {
                                        if *checked {
                                            this.excluded_sources.remove(id);
                                        } else {
                                            this.excluded_sources.insert(id.clone());
                                        }
                                    }
                                    this.run_search(window, cx);
                                    cx.notify();
                                },
                            )),
                        ),
                );
            if open {
                let mut list = v_flex()
                    .w_full()
                    .max_h(px(260.))
                    .p_2()
                    .gap_1()
                    .border_t_1()
                    .border_color(cx.theme().border);
                for meta in modules
                    .iter()
                    .filter(|meta| source_category(meta.kind) == Some(category))
                {
                    let id = meta.id.clone();
                    let label = meta
                        .description
                        .as_ref()
                        .map_or_else(|| id.to_string(), |d| format!("{id} — {d}"));
                    list = list.child(
                        Checkbox::new(SharedString::from(format!("search-source-{id}")))
                            .label(label)
                            .checked(!self.excluded_sources.contains(&id))
                            .on_click(cx.listener(move |this, checked, window, cx| {
                                if *checked {
                                    this.excluded_sources.remove(&id);
                                } else {
                                    this.excluded_sources.insert(id.clone());
                                }
                                this.run_search(window, cx);
                                cx.notify();
                            })),
                    );
                }
                tile = tile.child(list.overflow_y_scrollbar());
            }
            source_tiles = source_tiles.child(tile);
        }
        picker = picker.child(source_tiles).child(
            div()
                .mt_2()
                .text_size(px(crate::ui_scale::panel_font_size(cx, 18.0)))
                .font_weight(FontWeight::SEMIBOLD)
                .child(crate::i18n::tr(cx, "Выбранные книги Библии")),
        );
        let books = app_services_arc(cx)
            .default_translation()
            .map(|module| app_services_arc(cx).registry.lock().books_of(&module))
            .unwrap_or_default();
        let mut book_tiles = h_flex().w_full().flex_wrap().items_start().gap_3();
        for (group, from, to) in BOOK_GROUPS {
            let group_books: Vec<_> = books
                .iter()
                .filter(|book| (*from..=*to).contains(&book.book_number))
                .cloned()
                .collect();
            if group_books.is_empty() {
                continue;
            }
            let open = self.source_categories.contains(group);
            let all_selected = group_books
                .iter()
                .all(|book| !self.excluded_books.contains(&BookNumber(book.book_number)));
            let all_books = group_books
                .iter()
                .map(|book| BookNumber(book.book_number))
                .collect::<Vec<_>>();
            let mut tile = v_flex()
                .w(px(270.))
                .min_w(px(230.))
                .max_w(px(340.))
                .border_1()
                .border_color(cx.theme().border)
                .rounded_lg()
                .bg(cx.theme().sidebar)
                .overflow_hidden()
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .p_3()
                        .child(
                            h_flex()
                                .id(ElementId::Name(format!("book-tile-{group}").into()))
                                .flex_1()
                                .min_w_0()
                                .gap_2()
                                .items_center()
                                .cursor_pointer()
                                .hover(|style| style.text_color(cx.theme().accent))
                                .child(
                                    Icon::new(if open {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .xsmall(),
                                )
                                .child(
                                    div()
                                        .text_size(px(crate::ui_scale::panel_font_size(cx, 15.0)))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(crate::i18n::tr(cx, group)),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.source_categories.insert(group) {
                                        this.source_categories.remove(group);
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            Checkbox::new(SharedString::from(format!("search-book-all-{group}")))
                                .label(crate::i18n::tr(cx, "Все"))
                                .checked(all_selected)
                                .on_click(cx.listener(move |this, checked, window, cx| {
                                    for book in &all_books {
                                        if *checked {
                                            this.excluded_books.remove(book);
                                        } else {
                                            this.excluded_books.insert(*book);
                                        }
                                    }
                                    this.run_search(window, cx);
                                    cx.notify();
                                })),
                        ),
                );
            if open {
                let mut list = v_flex()
                    .w_full()
                    .max_h(px(260.))
                    .p_2()
                    .gap_1()
                    .border_t_1()
                    .border_color(cx.theme().border);
                for book in group_books {
                    let number = BookNumber(book.book_number);
                    list = list.child(
                        Checkbox::new(SharedString::from(format!(
                            "search-book-{}",
                            book.book_number
                        )))
                        .label(book.long_name)
                        .checked(!self.excluded_books.contains(&number))
                        .on_click(cx.listener(
                            move |this, checked, window, cx| {
                                if *checked {
                                    this.excluded_books.remove(&number);
                                } else {
                                    this.excluded_books.insert(number);
                                }
                                this.run_search(window, cx);
                                cx.notify();
                            },
                        )),
                    );
                }
                tile = tile.child(list.overflow_y_scrollbar());
            }
            book_tiles = book_tiles.child(tile);
        }
        picker
            .child(book_tiles)
            .id("search-source-picker")
            .into_any_element()
    }

    fn result_row(&self, row: ResultRow, cx: &mut Context<Self>) -> AnyElement {
        let Some(results) = &self.results else {
            return div().into_any_element();
        };
        let (label, text, action) = match row {
            ResultRow::Category(kind) => {
                let (title, key, count) = match kind {
                    ResultKind::Bible => ("Библия", "bible", hit_count(&results.verse_groups)),
                    ResultKind::Dictionary => (
                        "Словари",
                        "dictionary",
                        hit_count(&results.dictionary_groups),
                    ),
                    ResultKind::Commentary => (
                        "Комментарии",
                        "commentary",
                        hit_count(&results.commentary_groups),
                    ),
                };
                return self
                    .group_trigger(
                        key.into(),
                        crate::i18n::tr(cx, title).to_string(),
                        count,
                        true,
                        cx,
                    )
                    .into_any_element();
            }
            ResultRow::Source(kind, group) => {
                let (id, description, count) = match kind {
                    ResultKind::Bible => group_info(&results.verse_groups[group]),
                    ResultKind::Dictionary => group_info(&results.dictionary_groups[group]),
                    ResultKind::Commentary => group_info(&results.commentary_groups[group]),
                };
                let title = description.map_or_else(|| id.to_string(), |d| format!("{id} — {d}"));
                return div()
                    .pl_3()
                    .child(self.group_trigger(id.to_string(), title, count, false, cx))
                    .into_any_element();
            }
            ResultRow::Hit(ResultKind::Bible, group, hit) => {
                let hit = &results.verse_groups[group].hits[hit];
                let action = ResultAction::Verse(OpenBook {
                    module: hit.module.clone(),
                    book: hit.book,
                    chapter: Some(hit.chapter),
                    verse: Some(hit.verse),
                });
                (hit.label.clone(), hit.text.as_str(), Some(action))
            }
            ResultRow::Hit(ResultKind::Dictionary, group, hit) => {
                let hit = &results.dictionary_groups[group].hits[hit];
                let label = hit.lexeme.as_ref().map_or_else(
                    || hit.topic.clone(),
                    |word| format!("{} · {word}", hit.topic),
                );
                let action = ResultAction::Topic(hit.module.clone(), hit.topic.clone());
                (label, hit.snippet.as_str(), Some(action))
            }
            ResultRow::Hit(ResultKind::Commentary, group, hit) => {
                let hit = &results.commentary_groups[group].hits[hit];
                let services = app_services_arc(cx);
                let active = services
                    .current_location
                    .lock()
                    .as_ref()
                    .map(|location| location.module.clone());
                let action = active
                    .or_else(|| services.default_translation())
                    .map(|module| {
                        ResultAction::Verse(OpenBook {
                            module,
                            book: hit.book,
                            chapter: Some(hit.chapter_from),
                            verse: Some(hit.verse_from),
                        })
                    });
                (hit.label.clone(), hit.snippet.as_str(), action)
            }
        };
        div()
            .pl_7()
            .child(
                v_flex()
                    .id("search-hit")
                    .w_full()
                    .min_w_0()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .when(action.is_some(), |row| {
                        row.cursor_pointer()
                            .hover(|style| style.bg(crate::ui_scale::hover_background(cx)))
                    })
                    .child(
                        div()
                            .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                            .font_weight(FontWeight::MEDIUM)
                            .child(label),
                    )
                    .child(
                        div()
                            .text_size(px(crate::ui_scale::panel_font_size(cx, 13.0)))
                            .child(Self::highlighted_text(text, &results.highlight_terms, cx)),
                    )
                    .on_click(move |_, _, cx| match &action {
                        Some(ResultAction::Verse(open)) => {
                            navigation::request_open_book(cx, open.clone())
                        }
                        Some(ResultAction::Topic(module, topic)) => {
                            navigation::request_open_topic(cx, module.clone(), topic.clone())
                        }
                        None => {}
                    }),
            )
            .into_any_element()
    }

    fn append_result_rows<T>(
        &self,
        rows: &mut ResultRows,
        kind: ResultKind,
        key: &str,
        groups: &[shell::search::SearchModuleGroup<T>],
    ) {
        if groups.is_empty() {
            return;
        }
        rows.push(ResultRow::Category(kind), 1);
        if self.expanded_categories.contains(key) {
            for (group, source) in groups.iter().enumerate() {
                rows.push(ResultRow::Source(kind, group), 1);
                if self.expanded_sources.contains(source.module.0.as_str()) {
                    rows.push(ResultRow::Hit(kind, group, 0), source.hits.len());
                }
            }
        }
    }

    fn render_results(&mut self, cx: &mut Context<Self>) -> AnyElement {
        // Копируем цвета (Hsla: Copy), чтобы не держать заимствование `cx`
        // во время вызовов `group_trigger(.., cx)` / `render_*_groups(cx)`.
        let (muted_fg, danger, accent) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.danger, theme.accent)
        };
        if let Some(error) = &self.error {
            let message = if crate::i18n::is_english(cx) {
                format!("Search error: {error:#}")
            } else {
                format!("Ошибка поиска: {error:#}")
            };
            return div()
                .p_4()
                .text_color(danger)
                .child(message)
                .into_any_element();
        }
        let Some(results) = &self.results else {
            let mut empty = v_flex().w_full().p_4().gap_3();
            if self.searching {
                empty = empty.child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            gpui_kit::component::progress::ProgressCircle::new(
                                "search-progress-empty",
                            )
                            .xsmall()
                            .loading(true)
                            .color(cx.theme().accent),
                        )
                        .child(
                            div()
                                .text_color(muted_fg)
                                .child(crate::i18n::tr(cx, "Выполняется поиск по модулям…")),
                        ),
                );
            }
            return empty
                .child(
                    div().text_color(muted_fg).child(if self.searching {
                        crate::i18n::tr(cx, "Первый поиск может занять больше времени: приложение подготавливает поисковые индексы.")
                    } else {
                        crate::i18n::tr(cx, "Введите слово, фразу в кавычках или Strong’s-номер. Поиск охватывает переводы, словари и комментарии.")
                    }),
                )
                .into_any_element();
        };

        let mut header = h_flex().gap_2().items_center().pb_2();
        if self.searching {
            header = header
                .child(
                    gpui_kit::component::progress::ProgressCircle::new("search-progress-refresh")
                        .xsmall()
                        .loading(true)
                        .color(accent),
                )
                .child(
                    div()
                        .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                        .text_color(accent)
                        .child(crate::i18n::tr(cx, "Обновляю результаты…")),
                );
        }
        header = header.child(
            div()
                .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                .text_color(muted_fg)
                .child(if crate::i18n::is_english(cx) {
                    format!("Found: {}", results.hit_count)
                } else {
                    format!("Найдено: {}", results.hit_count)
                }),
        );
        if !results.building.is_empty() {
            header = header.child(
                div()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                    .text_color(accent)
                    .child(if crate::i18n::is_english(cx) {
                        format!("Indexing: {}", results.building.join(", "))
                    } else {
                        format!("Индексируется: {}", results.building.join(", "))
                    }),
            );
        }

        if results.hit_count == 0 && results.building.is_empty() {
            return v_flex()
                .p_2()
                .child(header)
                .child(
                    div()
                        .p_3()
                        .text_color(muted_fg)
                        .child(crate::i18n::tr(cx, "Совпадений не найдено.")),
                )
                .into_any_element();
        }

        let mut rows = ResultRows::default();
        self.append_result_rows(&mut rows, ResultKind::Bible, "bible", &results.verse_groups);
        self.append_result_rows(
            &mut rows,
            ResultKind::Commentary,
            "commentary",
            &results.commentary_groups,
        );
        self.append_result_rows(
            &mut rows,
            ResultKind::Dictionary,
            "dictionary",
            &results.dictionary_groups,
        );
        if self.result_list_dirty || self.result_list.item_count() != rows.len {
            let top = self.result_list.logical_scroll_top();
            self.result_list.reset(rows.len);
            self.result_list.scroll_to(top);
            self.result_list_dirty = false;
        }
        let panel = cx.entity().downgrade();
        let results_list = list(self.result_list.clone(), move |index, _, cx| {
            div()
                .id(("search-result", index))
                .child(
                    panel
                        .update(cx, |panel, cx| panel.result_row(rows.at(index), cx))
                        .unwrap_or_else(|_| div().into_any_element()),
                )
                .into_any_element()
        })
        .size_full();
        v_flex()
            .size_full()
            .min_h_0()
            .p_2()
            .child(header)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(results_list)
                    .vertical_scrollbar(&self.result_list),
            )
            .into_any_element()
    }
}

impl EventEmitter<PanelEvent> for SearchPanel {}

fn source_category(kind: ModuleKind) -> Option<&'static str> {
    match kind {
        ModuleKind::BibleTranslation => Some("Библия"),
        ModuleKind::Commentary => Some("Комментарии"),
        ModuleKind::StrongsDictionary | ModuleKind::GeneralDictionary => Some("Словари"),
        _ => None,
    }
}

const BOOK_GROUPS: &[(&str, i64, i64)] = &[
    ("Тора", 10, 50),
    ("Исторические книги", 60, 190),
    ("Поэтические книги", 220, 260),
    ("Большие пророки", 290, 330),
    ("Малые пророки", 340, 460),
    ("Евангелия и Деяния", 470, 510),
    ("Послания Павла", 520, 650),
    ("Соборные послания", 660, 720),
    ("Откровение", 730, 730),
];

impl Focusable for SearchPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for SearchPanel {
    fn panel_name(&self) -> &'static str {
        Self::NAME
    }
}

impl Panel for SearchPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::i18n::tr(cx, "Поиск")
    }
}

impl Render for SearchPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .p_2()
                    .border_b_1()
                    .border_color(theme.border)
                    // Row 1: query and the primary action. Pressing Enter in
                    // the input is wired to the same run_search path in new().
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Input::new(&self.query_input)),
                            )
                            .child(
                                Button::new("search-run")
                                    .primary()
                                    .xsmall()
                                    .label(crate::i18n::tr(cx, "Найти"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.run_search(window, cx);
                                    })),
                            ),
                    )
                    // Row 2: source selection and reset.
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(
                                Button::new("search-sources")
                                    .xsmall()
                                    .label(crate::i18n::tr(cx, "Источники"))
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        open_sources_window(cx.entity(), cx);
                                    })),
                            )
                            .child(div().flex_1())
                            .child(
                                Button::new("search-clear")
                                    .xsmall()
                                    .ghost()
                                    .label(crate::i18n::tr(cx, "Сбросить"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this._search_task = None;
                                        this.results = None;
                                        this.error = None;
                                        this.searching = false;
                                        this.result_list.reset(0);
                                        this.expanded_categories.clear();
                                        this.expanded_sources.clear();
                                        this.excluded_sources.clear();
                                        this.excluded_books.clear();
                                        this.query_input.update(cx, |input, cx| {
                                            input.set_value("", window, cx)
                                        });
                                        cx.notify();
                                    })),
                            ),
                    )
                    // Row 3: one compact search-mode menu and diacritics.
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(
                                Button::new("search-mode")
                                    .xsmall()
                                    .label(self.search_mode.label(cx))
                                    .dropdown_menu({
                                        let panel = cx.weak_entity();
                                        move |mut menu, _, cx| {
                                            for mode in [
                                                SearchMode::Exact,
                                                SearchMode::Partial,
                                                SearchMode::Similar,
                                            ] {
                                                let panel = panel.clone();
                                                menu = menu.item(
                                                    PopupMenuItem::new(mode.label(cx)).on_click(
                                                        move |_, _, cx| {
                                                            _ = panel.update(cx, |this, cx| {
                                                                this.search_mode = mode;
                                                                cx.notify();
                                                            });
                                                        },
                                                    ),
                                                );
                                            }
                                            menu
                                        }
                                    }),
                            )
                            .child(
                                Checkbox::new("search-diacritics")
                                    .label(crate::i18n::tr(cx, "Уч.Диак."))
                                    .tooltip(crate::i18n::tr(cx, "Учитывать диакритику/огласовки"))
                                    .checked(self.respect_diacritics)
                                    .on_click(cx.listener(|this, checked, _window, cx| {
                                        this.respect_diacritics = *checked;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(div().flex_1().min_h_0().child(self.render_results(cx)))
            .when(!crate::ui_scale::panel_font_family(cx).is_empty(), |root| {
                root.font_family(crate::ui_scale::panel_font_family(cx))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::SearchMode;

    #[test]
    fn search_modes_build_stable_fts_queries() {
        assert_eq!(
            SearchMode::Exact.query("благодать и истина"),
            "\"благодать и истина\""
        );
        assert_eq!(SearchMode::Partial.query("благо мир"), "благо* мир*");
        assert_eq!(SearchMode::Similar.query("благо мир"), "благо* OR мир*");
        assert_eq!(SearchMode::Partial.query("\"благо\""), "благо*");
    }
}
