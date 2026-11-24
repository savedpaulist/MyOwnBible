//! Центральная вкладка справочного текста (этап 15.2, polish-plan.md):
//! статья словаря или комментарий к главе — отдельной вкладкой в центре,
//! как в форке. Пришла на смену правой панели «Словарь» (упразднена).
//!
//! Содержимое — дерево ContentBlock из shell (`open_dictionary_article` /
//! `open_commentary`), рендер — общий `article::article_text_view`
//! (TextView::html: ссылки кликабельны, у ссылок на Писание tooltip,
//! текст выделяемый).
//!
//! Персистентность (§7.2): payload `ArticleTabPayload` в layout.json,
//! восстановление — `new_restored` через PanelRegistry, контент
//! перечитывается из модуля обычным путём.

use gpui_kit::base::{TextView, h_flex, v_flex};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent, PanelInfo, PanelState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::sync::{Arc, OnceLock};

use bible_domain::ids::{BookNumber, ModuleId, VerseRef};
use bible_domain::ports::ModuleCatalog as _;

use crate::article;
use crate::services::app_services;

/// Что показывает вкладка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArticleKind {
    /// Статья словаря (topic обязателен).
    Dict,
    /// Комментарий модуля к главе (book/chapter обязательны).
    Commentary,
}

impl ArticleKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Dict => "dict",
            Self::Commentary => "commentary",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "dict" => Some(Self::Dict),
            "commentary" => Some(Self::Commentary),
            _ => None,
        }
    }
}

/// Состояние вкладки в payload layout.json (§7.2): чего достаточно, чтобы
/// перечитать содержимое из модуля при восстановлении.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ArticleTabPayload {
    /// Версия схемы payload — мягкое чтение старых записей.
    pub v: u32,
    /// "dict" | "commentary".
    pub kind: String,
    /// ModuleId (stem файла модуля).
    pub module: String,
    /// Тема статьи (kind = dict).
    #[serde(default)]
    pub topic: Option<String>,
    /// Книга и глава комментария (kind = commentary). 0 — место не задано.
    #[serde(default)]
    pub book: i64,
    #[serde(default)]
    pub chapter: u16,
    #[serde(default)]
    pub verse_range: Option<(u16, u16)>,
}

struct CommentaryArticleRow {
    book: BookNumber,
    chapter_from: u16,
    verse_from: u16,
    chapter_to: u16,
    verse_to: u16,
    label: String,
    raw: String,
    html: OnceLock<String>,
}

pub struct ArticleTab {
    focus_handle: FocusHandle,
    kind: ArticleKind,
    module: ModuleId,
    topic: Option<String>,
    book: BookNumber,
    chapter: u16,
    verse_range: Option<(u16, u16)>,
    /// Заголовок вкладки (тема статьи / «2Пар 1»).
    title: String,
    /// Заголовок содержимого (модуль — книга глава).
    heading: String,
    /// Нормализованный HTML для TextView.
    html: String,
    commentary_rows: Arc<Vec<CommentaryArticleRow>>,
    commentary_list: ListState,
    error: Option<String>,
    _settings_subscription: Subscription,
}

impl ArticleTab {
    pub const NAME: &'static str = "article";

    /// Вкладка статьи словаря.
    pub fn new_dict(
        _window: &mut Window,
        cx: &mut Context<Self>,
        module: ModuleId,
        topic: String,
    ) -> Self {
        let mut this = Self::new(cx, ArticleKind::Dict, module);
        this.topic = Some(topic);
        this.reload(cx);
        this
    }

    pub fn new_commentary_at(
        cx: &mut Context<Self>,
        module: ModuleId,
        reference: VerseRef,
    ) -> Self {
        let mut this = Self::new(cx, ArticleKind::Commentary, module);
        this.book = reference.book;
        this.chapter = reference.chapter;
        this.verse_range = (reference.verse > 0).then_some((
            reference.verse,
            reference.end_verse.unwrap_or(reference.verse),
        ));
        this.reload(cx);
        this
    }

    /// Вкладка из payload layout.json (§7.2).
    pub fn new_restored(
        _window: &mut Window,
        cx: &mut Context<Self>,
        payload: ArticleTabPayload,
    ) -> Self {
        let kind = ArticleKind::parse(&payload.kind).unwrap_or(ArticleKind::Dict);
        let mut this = Self::new(cx, kind, ModuleId(payload.module));
        this.topic = payload.topic;
        this.book = BookNumber(payload.book);
        this.chapter = payload.chapter;
        this.verse_range = payload.verse_range;
        this.reload(cx);
        this
    }

    fn new(cx: &mut Context<Self>, kind: ArticleKind, module: ModuleId) -> Self {
        let settings_bus = crate::navigation::bus(cx);
        let settings_subscription = cx.subscribe(
            &settings_bus,
            |_, _, _: &crate::navigation::SettingsChanged, cx| cx.notify(),
        );
        Self {
            focus_handle: cx.focus_handle(),
            kind,
            module,
            topic: None,
            book: BookNumber(0),
            chapter: 0,
            verse_range: None,
            title: String::new(),
            heading: String::new(),
            html: String::new(),
            commentary_rows: Arc::new(Vec::new()),
            commentary_list: ListState::new(0, ListAlignment::Top, px(120.)),
            error: None,
            _settings_subscription: settings_subscription,
        }
    }

    /// Ключ дедупликации вкладок (workspace держит открытые в мапе):
    /// повторный клик по той же теме активирует существующую вкладку.
    pub fn dedup_key(&self) -> String {
        match self.kind {
            ArticleKind::Dict => format!(
                "dict:{}:{}",
                self.module,
                self.topic.as_deref().unwrap_or("")
            ),
            ArticleKind::Commentary => Self::commentary_key(&self.module),
        }
    }

    pub fn commentary_key(module: &ModuleId) -> String {
        format!("comm:{module}")
    }

    fn commentary_row_index(&self, reference: VerseRef) -> Option<usize> {
        let contains = |row: &CommentaryArticleRow| {
            if row.book != reference.book
                || reference.chapter < row.chapter_from
                || reference.chapter > row.chapter_to
            {
                return false;
            }
            if reference.verse == 0 {
                return true;
            }
            let after_start =
                reference.chapter > row.chapter_from || reference.verse >= row.verse_from;
            let before_end = reference.chapter < row.chapter_to || reference.verse <= row.verse_to;
            after_start && before_end
        };
        self.commentary_rows.iter().position(contains).or_else(|| {
            self.commentary_rows.iter().position(|row| {
                row.book.0 > reference.book.0
                    || (row.book == reference.book
                        && (row.chapter_from > reference.chapter
                            || (row.chapter_from == reference.chapter
                                && row.verse_from >= reference.verse)))
            })
        })
    }

    fn scroll_commentary_list(&self, reference: VerseRef) {
        if let Some(item_ix) = self.commentary_row_index(reference) {
            self.commentary_list.scroll_to(ListOffset {
                item_ix,
                offset_in_item: px(0.),
            });
        }
    }

    pub fn scroll_commentary_to(&mut self, reference: VerseRef, cx: &mut Context<Self>) {
        self.book = reference.book;
        self.chapter = reference.chapter;
        self.verse_range = (reference.verse > 0).then_some((
            reference.verse,
            reference.end_verse.unwrap_or(reference.verse),
        ));
        self.scroll_commentary_list(reference);
        cx.notify();
    }

    /// Перечитать содержимое из модуля (открытие и восстановление —
    /// одним путём, как место чтения в ReaderPanel).
    fn reload(&mut self, cx: &mut App) {
        let services = app_services(cx);
        let Some(meta) = services.registry.module(&self.module) else {
            self.error = Some(format!("Модуль «{}» не найден.", self.module));
            self.title = "Статья".to_string();
            return;
        };
        let label = crate::text::strip_markup(
            &meta
                .description
                .clone()
                .unwrap_or_else(|| meta.id.to_string()),
        );
        match self.kind {
            ArticleKind::Dict => {
                let topic = self.topic.clone().unwrap_or_default();
                match shell::open_dictionary_article(services, &self.module, &topic) {
                    Some(view) => {
                        self.title = view.topic;
                        self.heading = label;
                        self.html = article::blocks_to_html(&view.blocks);
                        self.error = None;
                    }
                    None => {
                        self.title = topic.clone();
                        self.heading = label;
                        self.html = "<p>Статья не найдена.</p>".to_string();
                        self.error = None;
                    }
                }
            }
            ArticleKind::Commentary => {
                let (entries, book_labels) = {
                    let mut registry = services.registry.lock();
                    let entries = registry.commentary_entries(&self.module);
                    let labels = registry
                        .book_entries()
                        .iter()
                        .map(|entry| (entry.book_number, entry.short_name.clone()))
                        .collect::<std::collections::HashMap<_, _>>();
                    (entries, labels)
                };
                let rows = entries
                    .into_iter()
                    .map(|entry| {
                        let book = book_labels
                            .get(&entry.book.0)
                            .cloned()
                            .unwrap_or_else(|| format!("Кн.{}", entry.book.0));
                        let row_label = if entry.chapter_to != entry.chapter_from {
                            format!(
                                "{book} {}:{}–{}:{}",
                                entry.chapter_from,
                                entry.verse_from,
                                entry.chapter_to,
                                entry.verse_to
                            )
                        } else if entry.verse_to != entry.verse_from {
                            format!(
                                "{book} {}:{}–{}",
                                entry.chapter_from, entry.verse_from, entry.verse_to
                            )
                        } else {
                            format!("{book} {}:{}", entry.chapter_from, entry.verse_from)
                        };
                        CommentaryArticleRow {
                            book: entry.book,
                            chapter_from: entry.chapter_from,
                            verse_from: entry.verse_from,
                            chapter_to: entry.chapter_to,
                            verse_to: entry.verse_to,
                            label: row_label,
                            raw: entry.text,
                            html: OnceLock::new(),
                        }
                    })
                    .collect::<Vec<_>>();
                self.commentary_list.reset(rows.len());
                self.commentary_rows = Arc::new(rows);
                self.title = label.clone();
                self.heading = label;
                self.scroll_commentary_list(VerseRef {
                    book: self.book,
                    chapter: self.chapter,
                    verse: self.verse_range.map_or(0, |range| range.0),
                    end_verse: self.verse_range.map(|range| range.1),
                });
                self.error = None;
            }
        }
    }
}

impl EventEmitter<PanelEvent> for ArticleTab {}

impl Focusable for ArticleTab {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for ArticleTab {
    fn panel_name(&self) -> &'static str {
        Self::NAME
    }

    /// Персистентное состояние вкладки (§7.2): вид + адрес содержимого.
    fn dump(&self, _: &App) -> PanelState {
        let payload = ArticleTabPayload {
            v: 1,
            kind: self.kind.as_str().to_string(),
            module: self.module.0.clone(),
            topic: self.topic.clone(),
            book: self.book.0,
            chapter: self.chapter,
            verse_range: self.verse_range,
        };
        let mut state = PanelState::new(Self::NAME);
        state.info = PanelInfo::panel(serde_json::to_value(payload).unwrap_or_default());
        state
    }
}

impl Panel for ArticleTab {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        if self.title.is_empty() {
            "Статья".to_string()
        } else {
            self.title.clone()
        }
    }
}

impl Render for ArticleTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dictionary = matches!(self.kind, ArticleKind::Dict);
        let panel_font_family = if dictionary {
            crate::ui_scale::dictionary_font_family(cx)
        } else {
            crate::ui_scale::panel_font_family(cx)
        };
        let font_size = px(if dictionary {
            crate::ui_scale::dictionary_font_size(cx, 14.)
        } else {
            crate::ui_scale::panel_font_size(cx, 14.)
        });
        if let Some(error) = &self.error {
            return v_flex()
                .size_full()
                .p_4()
                .when(!panel_font_family.is_empty(), |root| {
                    root.font_family(panel_font_family.clone())
                })
                .child(
                    div()
                        .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                        .text_color(cx.theme().danger)
                        .child(error.clone()),
                );
        }
        let content = match self.kind {
            ArticleKind::Dict => article::article_text_view(
                ElementId::Name(format!("article-tab-{}", self.dedup_key()).into()),
                self.html.clone(),
                cx,
            )
            .text_size(font_size)
            .into_any_element(),
            ArticleKind::Commentary => {
                let rows = self.commentary_rows.clone();
                let module = self.module.clone();
                let document = list(self.commentary_list.clone(), move |index, _, cx| {
                    let Some(row) = rows.get(index) else {
                        return div().into_any_element();
                    };
                    let html = row
                        .html
                        .get_or_init(|| {
                            article::blocks_to_html(&shell::article::parse_module_html(
                                &row.raw, &module,
                            ))
                        })
                        .clone();
                    v_flex()
                        .id(("commentary-document-row", index))
                        .w_full()
                        .min_w_0()
                        .border_b_1()
                        .border_color(cx.theme().border.opacity(0.45))
                        .child(
                            div()
                                .px_3()
                                .pt_3()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.theme().accent)
                                .child(row.label.clone()),
                        )
                        .child(
                            article::linked_text_view(
                                TextView::html(("commentary-document-text", index), html),
                                cx,
                            )
                            .w_full()
                            .p_3()
                            .text_size(font_size),
                        )
                        .into_any_element()
                })
                .with_sizing_behavior(ListSizingBehavior::Auto)
                .size_full();
                div()
                    .relative()
                    .size_full()
                    .child(document)
                    .vertical_scrollbar(&self.commentary_list)
                    .into_any_element()
            }
        };

        v_flex()
            .size_full()
            .min_h_0()
            .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(cx.theme().foreground)
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(self.heading.clone()),
                    ),
            )
            .child(div().flex_1().min_h_0().w_full().child(content))
            .when(!panel_font_family.is_empty(), |root| {
                root.font_family(panel_font_family)
            })
    }
}
