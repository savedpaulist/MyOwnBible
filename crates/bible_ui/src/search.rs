//! Панель поиска (architecture.md §6.5, этап 7).
//!
//! Поле запроса и фильтры источников и корпуса → поиск по готовым
//! FTS5-индексам на фоновом потоке. Результаты — виртуальный список
//! «Ин 3:16 · RST» + текст с подсветкой слов запроса; клик открывает
//! место в панели чтения (navigation::OpenBook).

use std::collections::BTreeSet;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::IndexPath;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::{Icon, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use bible_domain::ids::ModuleId;
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

/// Пункт списка корпуса: везде / ВЗ / НЗ.
#[derive(Clone)]
struct CorpusOption {
    corpus: Corpus,
    title: &'static str,
}

impl SelectItem for CorpusOption {
    type Value = Corpus;

    fn title(&self) -> SharedString {
        self.title.into()
    }

    fn value(&self) -> &Self::Value {
        &self.corpus
    }
}

pub struct SearchPanel {
    focus_handle: FocusHandle,
    query_input: Entity<InputState>,
    corpus_select: Entity<SelectState<SearchableVec<CorpusOption>>>,
    excluded_sources: BTreeSet<ModuleId>,
    sources_open: bool,
    source_categories: BTreeSet<&'static str>,
    result_list: ListState,
    result_list_dirty: bool,
    corpus: Corpus,
    results: Option<shell::SearchResultsViewModel>,
    searching: bool,
    error: Option<String>,
    _search_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
    expanded_categories: BTreeSet<String>,
    expanded_sources: BTreeSet<String>,
}

impl SearchPanel {
    pub const NAME: &'static str = "search";

    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(crate::i18n::tr_static(
                "возлюбил* мир, \"благодать и истина\", G26…",
            ))
        });

        let corpus_options = vec![
            CorpusOption {
                corpus: Corpus::All,
                title: crate::i18n::tr_static("Вся Библия"),
            },
            CorpusOption {
                corpus: Corpus::OldTestament,
                title: crate::i18n::tr_static("Ветхий Завет"),
            },
            CorpusOption {
                corpus: Corpus::NewTestament,
                title: crate::i18n::tr_static("Новый Завет"),
            },
        ];
        let corpus_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(corpus_options),
                Some(IndexPath::new(0)),
                window,
                cx,
            )
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
        subscriptions.push(cx.subscribe_in(
            &corpus_select,
            window,
            |this, _, event: &SelectEvent<SearchableVec<CorpusOption>>, window, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    this.corpus = *value;
                    this.run_search(window, cx);
                    cx.notify();
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
            corpus_select,
            excluded_sources: BTreeSet::new(),
            sources_open: false,
            source_categories: BTreeSet::new(),
            result_list: ListState::new(0, ListAlignment::Top, px(300.)),
            result_list_dirty: false,
            corpus: Corpus::All,
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
        let input = self.query_input.read(cx).value().to_string();
        if input.trim().is_empty() {
            return;
        }
        let services = app_services_arc(cx);
        let excluded = self.excluded_sources.clone();
        let corpus = self.corpus;
        self.searching = true;
        self.error = None;
        cx.notify();

        self._search_task = Some(cx.spawn_in(window, async move |this, window| {
            let mut first_result = true;
            loop {
                let search_services = services.clone();
                let input = input.clone();
                let excluded = excluded.clone();
                let result = window
                    .background_executor()
                    .spawn(async move {
                        shell::search::search_sources(&search_services, &input, corpus, |id| {
                            !excluded.contains(id)
                        })
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
            .hover(|style| style.bg(cx.theme().accent.opacity(0.10)))
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

    fn render_source_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let modules = app_services_arc(cx).registry.modules();
        let all: BTreeSet<_> = modules
            .iter()
            .filter(|meta| source_category(meta.kind).is_some())
            .map(|meta| meta.id.clone())
            .collect();
        let mut picker = v_flex().gap_1().child(
            h_flex()
                .gap_2()
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
        for category in ["Библия", "Комментарии", "Словари"] {
            let open = self.source_categories.contains(category);
            picker = picker.child(
                Button::new(category)
                    .xsmall()
                    .ghost()
                    .label(category)
                    .icon(if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.source_categories.insert(category) {
                            this.source_categories.remove(category);
                        }
                        cx.notify();
                    })),
            );
            if open {
                for meta in modules
                    .iter()
                    .filter(|meta| source_category(meta.kind) == Some(category))
                {
                    let id = meta.id.clone();
                    let label = meta
                        .description
                        .as_ref()
                        .map_or_else(|| id.to_string(), |d| format!("{id} — {d}"));
                    picker = picker.child(
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
            }
        }
        picker
            .id("search-source-picker")
            .max_h(px(140.))
            .overflow_y_scrollbar()
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
                    .group_trigger(key.into(), title.into(), count, true, cx)
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
                            .hover(|style| style.bg(cx.theme().accent.opacity(0.10)))
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
            return div()
                .p_4()
                .text_color(danger)
                .child(format!("Ошибка поиска: {error:#}"))
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
                .child(format!("Найдено: {}", results.hit_count)),
        );
        if !results.building.is_empty() {
            header = header.child(
                div()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                    .text_color(accent)
                    .child(format!("Индексируется: {}", results.building.join(", "))),
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
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(
                                Button::new("search-sources")
                                    .xsmall()
                                    .label(crate::i18n::tr(cx, "Источники"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.sources_open = !this.sources_open;
                                        cx.notify();
                                    })),
                            )
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
                                        this.query_input.update(cx, |input, cx| {
                                            input.set_value("", window, cx)
                                        });
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Select::new(&self.corpus_select).small()),
                            ),
                    )
                    .when(self.sources_open, |header| {
                        header.child(self.render_source_picker(cx))
                    }),
            )
            .child(div().flex_1().min_h_0().child(self.render_results(cx)))
            .when(!crate::ui_scale::panel_font_family(cx).is_empty(), |root| {
                root.font_family(crate::ui_scale::panel_font_family(cx))
            })
    }
}
