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

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent, PanelInfo, PanelState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

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

    /// Вкладка комментария модуля к главе.
    pub fn new_commentary(
        _window: &mut Window,
        cx: &mut Context<Self>,
        module: ModuleId,
        book: BookNumber,
        chapter: u16,
    ) -> Self {
        Self::new_commentary_at(
            cx,
            module,
            VerseRef {
                book,
                chapter,
                verse: 0,
                end_verse: None,
            },
        )
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
            ArticleKind::Commentary => Self::commentary_key(
                &self.module,
                VerseRef {
                    book: self.book,
                    chapter: self.chapter,
                    verse: self.verse_range.map_or(0, |range| range.0),
                    end_verse: self.verse_range.map(|range| range.1),
                },
            ),
        }
    }

    pub fn commentary_key(module: &ModuleId, reference: VerseRef) -> String {
        let key = format!("comm:{module}:{}:{}", reference.book.0, reference.chapter);
        if reference.verse == 0 {
            key
        } else {
            format!(
                "{key}:{}:{}",
                reference.verse,
                reference.end_verse.unwrap_or(reference.verse)
            )
        }
    }

    /// Имена книги (длинное/короткое): сначала из таблицы книг самого
    /// модуля; если там её нет (у части комментариев книги не описаны) —
    /// из модуля текущего чтения, затем из перевода по умолчанию;
    /// в конце — «Кн. N».
    fn book_names(&self, cx: &App) -> (String, String) {
        let services = app_services(cx);
        let mut sources: Vec<ModuleId> = vec![self.module.clone()];
        if let Some(location) = services.current_location.lock().as_ref() {
            sources.push(location.module.clone());
        }
        if let Some(default) = services.default_translation() {
            sources.push(default);
        }
        let mut registry = services.registry.lock();
        for module in sources {
            if let Some(entry) = registry
                .books_of(&module)
                .into_iter()
                .find(|entry| entry.book_number == self.book.0)
            {
                return (entry.long_name, entry.short_name);
            }
        }
        (
            format!("Кн. {}", self.book.0),
            format!("Кн.{}", self.book.0),
        )
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
                let (long_name, short_name) = self.book_names(cx);
                self.title = format!("{short_name} {}", self.chapter);
                let location = shell::services::CurrentLocation {
                    module: self.module.clone(),
                    book: self.book,
                    book_title: long_name,
                    book_short: short_name,
                    chapter: self.chapter,
                    verse: 1,
                };
                let view = shell::article::open_commentary_range(
                    services,
                    &self.module,
                    &label,
                    &location,
                    self.verse_range,
                );
                self.heading = view.title;
                self.html = article::blocks_to_html(&view.blocks);
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
            .child(
                div().flex_1().min_h_0().w_full().child(
                    article::article_text_view(
                        ElementId::Name(format!("article-tab-{}", self.dedup_key()).into()),
                        self.html.clone(),
                        cx,
                    )
                    .text_size(font_size),
                ),
            )
            .when(!panel_font_family.is_empty(), |root| {
                root.font_family(panel_font_family)
            })
    }
}
