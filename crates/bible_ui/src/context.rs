//! Панель «Verse» (architecture.md §6.4, переработка `word_panel.rs`
//! форка): одна правая панель с секциями — «Слово», «Другие переводы»,
//! «Параллельные ссылки», «Комментарии», «Заметки». Открыта одна секция
//! за раз: её заголовок-триггер закреплён над прокручиваемым содержимым
//! (этап 14), остальные триггеры видны всегда. По умолчанию открыта
//! секция «Слово», как в форке.
//!
//! Секция «Слово» следует за кликом по слову (событие `WordSelected`),
//! остальные секции — за активным стихом (событие `VerseActivated`:
//! клик по строке стиха или по слову в чтении). Данные собираются целиком
//! в shell (`describe_word`, `verse_context`) — панель только рисует.

use std::collections::BTreeSet;

use gpui_kit::base::{SelectableText, TextView, h_flex, v_flex};
use gpui_kit::component::IndexPath;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::collapsible::Collapsible;
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use bible_domain::content::{ContentBlock, ContentInline, TextMarks};
use bible_domain::ids::{AnchoredRef, ModuleId, VerseRef};
use bible_domain::link::LinkTarget;
use bible_domain::ports::{ModuleCatalog as _, ModuleKind};

use crate::article;
use crate::navigation::{self, VerseActivated, WordSelected};
use crate::services::app_services;

/// Секция «Слово» — открыта по умолчанию (как в форке); остальные
/// индексы фиксированы порядком TITLES/contents в render().
const SECTION_WORD: usize = 0;
const SECTION_TRANSLATIONS: usize = 1;
const SECTION_CROSSREFS: usize = 2;
const SECTION_COMMENTARIES: usize = 3;
const SECTION_NOTES: usize = 4;

/// Пункт пикера Strong's-словаря: «Авто» или конкретный модуль
/// (настройка `preferred_strongs_dictionary`, architecture.md §6.4).
#[derive(Clone)]
struct DictOption {
    id: Option<ModuleId>,
    description: Option<String>,
}

impl SelectItem for DictOption {
    type Value = Option<ModuleId>;

    fn title(&self) -> SharedString {
        match &self.id {
            Some(id) => id.to_string().into(),
            None => "Авто".into(),
        }
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut row = h_flex().w_full().gap_2().items_baseline();
        row = row.child(div().flex_shrink_0().child(self.title()));
        if let Some(description) = &self.description {
            row = row.child(
                div()
                    .min_w_0()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                    .text_color(cx.theme().muted_foreground)
                    .child(description.clone()),
            );
        }
        row
    }
}

pub struct ContextPanel {
    focus_handle: FocusHandle,
    word: Option<shell::WordDescription>,
    context: Option<shell::VerseContextViewModel>,
    /// Активный стих целиком — из него строится якорь комментария
    /// секции «Заметки».
    location: Option<shell::CurrentLocation>,
    /// Редактор моего комментария к активному стиху (секция «Заметки»).
    comment_input: Entity<TextareaState>,
    /// Цепочки, использующие активный стих (полоса над секциями, §6.8).
    chain_hits: Vec<shell::chains::ChainHit>,
    /// Пикер Strong's-словаря секции «Слово» (закрепление в настройках).
    cross_ref_error: Option<String>,
    dict_select: Entity<SelectState<SearchableVec<DictOption>>>,
    /// Открытая секция (одна за раз, этап 14): заголовок открытой
    /// закреплён над прокруткой, остальные триггеры всегда видимы.
    /// None — все свёрнуты.
    open_section: Option<usize>,
    /// Раскрытые переводы в секции «Другие переводы» (по id модуля,
    /// этап 14). Переживает смену активного стиха.
    open_translations: BTreeSet<String>,
    /// Раскрытые источники в секции «Комментарии» (ключ — модуль + индекс
    /// записи, переживает смену стиха).
    open_commentaries: BTreeSet<String>,
    _subscriptions: Vec<Subscription>,
}

impl ContextPanel {
    pub const NAME: &'static str = "context";

    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let comment_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(crate::i18n::tr_static("Мой комментарий к стиху…"))
        });

        // Пикер Strong's-словаря: «Авто» + все Strong's-словари каталога;
        // выбранный — из настроек (preferred_strongs_dictionary).
        let preferred = app_services(cx)
            .settings
            .read()
            .preferred_strongs_dictionary
            .clone();
        let mut dict_options = vec![DictOption {
            id: None,
            description: Some("первый словарь, где есть статья".to_string()),
        }];
        dict_options.extend(
            app_services(cx)
                .registry
                .modules()
                .into_iter()
                .filter(|m| m.kind == ModuleKind::StrongsDictionary)
                .map(|m| DictOption {
                    id: Some(m.id),
                    description: m.description,
                }),
        );
        let selected = dict_options
            .iter()
            .position(|option| option.id.as_ref().map(|id| id.0.as_str()) == preferred.as_deref())
            .unwrap_or(0);
        let dict_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(dict_options),
                Some(IndexPath::new(selected)),
                window,
                cx,
            )
        });

        let subscriptions = vec![
            cx.subscribe_in(
                &navigation::bus(cx),
                window,
                |this, _, event: &WordSelected, _, cx| {
                    this.word = Some(event.description.clone());
                    this.open_section = Some(SECTION_WORD);
                    cx.notify();
                },
            ),
            cx.subscribe_in(
                &navigation::bus(cx),
                window,
                |this, _, _: &navigation::WordCleared, _, cx| {
                    this.word = None;
                    cx.notify();
                },
            ),
            cx.subscribe_in(
                &navigation::bus(cx),
                window,
                |_, _, _: &navigation::SettingsChanged, _, cx| cx.notify(),
            ),
            cx.subscribe_in(
                &dict_select,
                window,
                |this, _, event: &SelectEvent<SearchableVec<DictOption>>, _, cx| {
                    // Закрепление словаря — в настройки; карточку слова
                    // пересобираем тем же сценарием shell (redescribe).
                    if let SelectEvent::Confirm(Some(value)) = event {
                        let services = app_services(cx);
                        let dict = value.as_ref().map(|id| id.0.clone());
                        if let Err(error) = shell::settings::update(services, |s| {
                            s.preferred_strongs_dictionary = dict;
                        }) {
                            log::error!("save preferred strongs dictionary: {error:#}");
                        }
                        if let Some(description) = &this.word {
                            this.word = Some(shell::redescribe(services, description));
                        }
                        cx.notify();
                    }
                },
            ),
            cx.subscribe_in(
                &navigation::bus(cx),
                window,
                |this, _, event: &VerseActivated, window, cx| {
                    this.activate_verse(event, window, cx);
                    cx.notify();
                },
            ),
            cx.subscribe(
                &navigation::bus(cx),
                |this, _, _: &navigation::UserDataSynced, cx| {
                    this.refresh_user_cross_refs(cx);
                    cx.notify();
                },
            ),
            // Правка цепочек — перечитать полосу попаданий.
            cx.subscribe_in(
                &navigation::bus(cx),
                window,
                |this, _, _: &navigation::ChainsChanged, _, cx| {
                    this.load_chain_hits(cx);
                    cx.notify();
                },
            ),
            // Кнопки Status bar: раскрыть запрошенную секцию (этап 18).
            cx.subscribe_in(
                &navigation::bus(cx),
                window,
                |this, _, event: &navigation::OpenVerseSection, _, cx| {
                    this.open_section = Some(match event.0 {
                        navigation::VerseSection::Word => SECTION_WORD,
                        navigation::VerseSection::Translations => SECTION_TRANSLATIONS,
                        navigation::VerseSection::CrossRefs => SECTION_CROSSREFS,
                        navigation::VerseSection::Commentaries => SECTION_COMMENTARIES,
                        navigation::VerseSection::Notes => SECTION_NOTES,
                    });
                    cx.notify();
                },
            ),
        ];

        // Открылись при уже открытой книге — показываем контекст текущего
        // места чтения сразу.
        let location = app_services(cx).current_location.lock().clone();
        let context = location
            .as_ref()
            .map(|location| shell::verse_context(app_services(cx), location));

        let mut this = Self {
            focus_handle: cx.focus_handle(),
            word: None,
            context,
            location,
            comment_input,
            chain_hits: Vec::new(),
            cross_ref_error: None,
            dict_select,
            open_section: Some(SECTION_WORD),
            open_translations: BTreeSet::new(),
            open_commentaries: BTreeSet::new(),
            _subscriptions: subscriptions,
        };
        // Восстановленное место — сразу грузим комментарий к нему.
        let location = this.location.clone();
        if let Some(location) = location {
            this.load_comment(&location, cx);
            this.load_chain_hits(cx);
        }
        this
    }

    /// Синхронизировать выбранный стих со всеми секциями панели.
    fn activate_verse(
        &mut self,
        event: &VerseActivated,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let location = event.location();
        if self.location.as_ref().is_some_and(|previous| {
            previous.module == location.module
                && previous.book == location.book
                && previous.chapter == location.chapter
                && previous.verse == location.verse
        }) {
            return;
        }
        if self.word.as_ref().is_some_and(|word| {
            word.module != location.module
                || word.book != location.book
                || word.chapter != location.chapter
                || word.verse != location.verse
        }) {
            self.word = None;
        }
        self.cross_ref_error = None;
        self.context = Some(shell::verse_context(app_services(cx), &location));
        self.location = Some(location.clone());
        self.load_comment(&location, cx);
        self.load_chain_hits(cx);
    }

    /// Полоса «стих в цепочках» — из активного места (§6.8).
    fn load_chain_hits(&mut self, cx: &mut Context<Self>) {
        self.chain_hits = match &self.location {
            Some(location) => shell::chains::chains_for_verse(
                app_services(cx),
                location.book.0,
                location.chapter,
                location.verse,
            ),
            None => Vec::new(),
        };
    }

    /// Якорь комментария: активный стих в его модуле (§3.6).
    fn comment_anchor(location: &shell::CurrentLocation) -> AnchoredRef {
        AnchoredRef {
            module: location.module.clone(),
            reference: VerseRef::single(location.book, location.chapter, location.verse),
            word_index: None,
        }
    }

    /// Загружает сохранённый комментарий в редактор (пусто — если нет).
    fn load_comment(&mut self, location: &shell::CurrentLocation, cx: &mut Context<Self>) {
        let body = shell::verse_comment(app_services(cx), &Self::comment_anchor(location))
            .map(|comment| comment.body)
            .unwrap_or_default();
        // WeakEntity::update_in сам достаёт окно (AppContext-вариант).
        _ = self
            .comment_input
            .downgrade()
            .update_in(cx, |input, window, cx| {
                input.set_value(body, window, cx);
            });
    }

    /// Сохраняет комментарий; пустой текст удаляет (контракт shell).
    fn save_comment(&mut self, cx: &mut Context<Self>) {
        let Some(location) = &self.location else {
            return;
        };
        let body = self.comment_input.read(cx).value().to_string();
        if let Err(error) =
            shell::save_verse_comment(app_services(cx), Self::comment_anchor(location), &body)
        {
            log::error!("save verse comment failed: {error:#}");
        }
        navigation::notify_annotations_changed(cx);
    }

    // --- Секция «Слово» ----------------------------------------------------

    fn render_word_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(description) = self.word.clone() else {
            return div().into_any_element();
        };
        let description = &description;

        let mut sections = v_flex().w_full().min_w_0().p_2().gap_3();
        let divider = || {
            div()
                .w_full()
                .h(px(1.0))
                .bg(cx.theme().border.opacity(0.65))
        };

        // Заголовок (перенос форка: «лексема транслитерация [чтение] — G25»):
        // лексема из колонки словаря, иначе слово как в тексте.
        let headline = description
            .lexeme
            .clone()
            .filter(|lexeme| !lexeme.is_empty())
            .unwrap_or_else(|| description.word.clone());
        let mut head = h_flex()
            .w_full()
            .min_w_0()
            .flex_wrap()
            .gap_x_2()
            .items_baseline()
            .child(
                div()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 16.0)))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .child(SelectableText::new("word-headline", headline)),
            );
        if let Some(transliteration) = &description.transliteration {
            head = head.child(
                div()
                    .italic()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                    .text_color(cx.theme().muted_foreground)
                    .child(SelectableText::new(
                        "word-transliteration",
                        transliteration.clone(),
                    )),
            );
        }
        if let Some(pronunciation) = &description.pronunciation {
            head = head.child(
                div()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                    .text_color(cx.theme().muted_foreground)
                    .child(SelectableText::new(
                        "word-pronunciation",
                        format!("[{pronunciation}]"),
                    )),
            );
        }
        if let Some(strong) = &description.strong {
            head = head.child(
                div()
                    .px_1()
                    .py(px(1.0))
                    .rounded_sm()
                    .bg(cx.theme().accent.opacity(0.18))
                    .border_1()
                    .border_color(cx.theme().accent.opacity(0.55))
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .child(SelectableText::new("word-strong-number", strong.clone())),
            );
        }
        sections = sections.child(head);

        // Меню оформления вынесено над заголовком секции «Слово».

        // Краткое определение из колонки short_definition (если словарь
        // её даёт) — отдельной строкой до полной статьи.
        if let Some(short) = description
            .short_definition
            .clone()
            .filter(|short| !short.is_empty())
        {
            sections = sections.child(divider()).child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 16.0)))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(cx.theme().success)
                    .child(SelectableText::new("word-short-definition", short)),
            );
        }

        // Морфология: код и расшифровка.
        if let Some(code) = &description.morph_code {
            let gloss = description
                .morph_gloss
                .clone()
                .unwrap_or_else(|| crate::i18n::text(cx, "Расшифровка недоступна."));
            sections = sections.child(divider()).child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().accent)
                            .child(crate::i18n::tr(cx, "Морфология")),
                    )
                    .child(
                        div()
                            .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(cx.theme().foreground)
                            .child(SelectableText::new("word-morph-code", code.clone())),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                            .text_color(cx.theme().foreground)
                            .child(
                                SelectableText::new("word-morph-gloss", gloss).document_order(1),
                            ),
                    ),
            );
        }

        // Статья словаря-источника (разметка → ContentBlock → TextView).
        if let Some(strong) = &description.strong {
            let body: AnyElement = match (&description.strong_entry, &description.strongs_dict) {
                (Some(entry), Some(dict)) => {
                    let blocks = shell::article::parse_module_html(entry, dict);
                    let html = article::blocks_to_word_panel_html(&blocks);
                    article::linked_text_view(
                        TextView::html(
                            ElementId::Name(format!("word-strong-{strong}").into()),
                            html,
                        ),
                        cx,
                    )
                    .w_full()
                    .into_any_element()
                }
                _ => div()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                    .text_color(cx.theme().muted_foreground)
                    .child(crate::i18n::tr(cx, "Статья не найдена в Strong's-словаре."))
                    .into_any_element(),
            };
            // Источник статьи: закреплённый или первый подошедший словарь.
            let source_label = description
                .strongs_dict
                .as_ref()
                .and_then(|id| app_services(cx).registry.module(id))
                .map(|meta| meta.description.unwrap_or_else(|| meta.id.to_string()));
            sections = sections.child(divider()).child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_baseline()
                            .child(
                                div()
                                    .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("Strong's {strong}")),
                            )
                            .children(source_label.map(|label| {
                                div()
                                    .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(label)
                            })),
                    )
                    .child(body),
            );
        }

        if description.strong.is_none() && description.morph_code.is_none() {
            sections = sections.child(divider()).child(
                div()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                    .text_color(cx.theme().muted_foreground)
                    .child(crate::i18n::tr(
                        cx,
                        "Для этого слова нет данных Strong's и морфологии.",
                    )),
            );
        }

        // Действия (перенос accessory-кнопок форка в тело секции):
        // открыть статью в панели «Словарь», поиск слова / Strong's.
        let mut actions = h_flex().w_full().flex_wrap().gap_2().items_center();
        if let (Some(dict), Some(topic)) = (&description.strongs_dict, &description.strong_topic) {
            let (dict, topic) = (dict.clone(), topic.clone());
            actions = actions.child(
                Button::new("word-open-in-dictionary")
                    .outline()
                    .xsmall()
                    .label(crate::i18n::tr(cx, "Открыть в словаре"))
                    .on_click(move |_, _, cx| {
                        navigation::request_open_topic(cx, dict.clone(), topic.clone());
                    }),
            );
        }
        {
            let query = description.word.clone();
            actions = actions.child(
                Button::new("word-search-word")
                    .ghost()
                    .xsmall()
                    .label(crate::i18n::tr(cx, "Искать слово"))
                    .tooltip(crate::i18n::tr(cx, "Найти это слово в переводах"))
                    .on_click(move |_, _, cx| {
                        navigation::request_search(cx, query.clone());
                    }),
            );
        }
        if let Some(strong) = &description.strong {
            let query = strong.clone();
            actions = actions.child(
                Button::new("word-search-strong")
                    .ghost()
                    .xsmall()
                    .label(format!("Искать {strong}"))
                    .tooltip(crate::i18n::tr(cx, "Найти номер Strong's в переводах"))
                    .on_click(move |_, _, cx| {
                        navigation::request_search(cx, query.clone());
                    }),
            );
        }
        let usage_description = description.clone();
        actions = actions.child(
            Button::new("word-usage-chart")
                .ghost()
                .xsmall()
                .label(crate::i18n::tr(cx, "График употребляемости"))
                .tooltip(crate::i18n::tr(
                    cx,
                    "Количество употреблений по книгам текущего модуля",
                ))
                .on_click(move |_, _, cx| {
                    crate::usage::open_word_usage(usage_description.clone(), cx);
                }),
        );
        sections = sections.child(divider()).child(actions);

        // Закрепление Strong's-словаря (перенос strong picker форка):
        // «Авто» — первый словарь приоритета, где нашлась статья.
        sections = sections.child(divider()).child(
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                        .text_color(cx.theme().muted_foreground)
                        .child(crate::i18n::tr(cx, "Словарь Strong's:")),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Select::new(&self.dict_select).xsmall().w_full()),
                ),
        );

        sections.into_any_element()
    }

    // --- Секция «Другие переводы» ------------------------------------------

    /// Каждый перевод — Collapsible с row-триггером (этап 14): по
    /// умолчанию свёрнут, тогда секция обозревается одним экраном.
    /// Раскрытие запоминается по id модуля и переживает смену стиха.
    /// «Открыть перевод» — внутри раскрытого содержимого.
    fn render_translations_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(context) = &self.context else {
            return Self::no_verse_hint(cx);
        };
        if context.translations.is_empty() {
            return Self::empty_hint("Другие переводы не найдены.", cx);
        }
        let weak = cx.entity().downgrade();
        let muted = cx.theme().muted_foreground;
        let foreground = cx.theme().foreground;
        let border = cx.theme().border;
        let mut list = v_flex().w_full().min_w_0();
        for entry in &context.translations {
            let module = entry.module.clone();
            let module_key = module.to_string();
            let open = self.open_translations.contains(&module_key);
            let label = entry.label.clone();
            let text = entry.text.clone();

            let trigger = {
                let weak = weak.clone();
                let module_key = module_key.clone();
                h_flex()
                    .id(ElementId::Name(
                        format!("translation-trigger-{module_key}").into(),
                    ))
                    .w_full()
                    .min_w_0()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .cursor_pointer()
                    .border_b_1()
                    .border_color(border)
                    .hover(|style| style.bg(cx.theme().list_hover))
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .xsmall()
                            .flex_none()
                            .text_color(muted)
                            .rotate(percentage(if open { 0.5 } else { 0. })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(foreground)
                            .child(label),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                            .text_color(muted)
                            .child(module_key.clone()),
                    )
                    .on_click(move |_, _, cx| {
                        _ = weak.update(cx, |this, cx| {
                            if !this.open_translations.remove(&module_key) {
                                this.open_translations.insert(module_key.clone());
                            }
                            cx.notify();
                        });
                    })
            };

            let content = {
                let module = module.clone();
                v_flex()
                    .w_full()
                    .min_w_0()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                            .text_color(foreground)
                            .child(text),
                    )
                    .child(
                        h_flex().w_full().justify_end().child(
                            Button::new(ElementId::Name(
                                format!("open-translation-{module_key}").into(),
                            ))
                            .ghost()
                            .xsmall()
                            .label(crate::i18n::tr(cx, "Открыть перевод"))
                            .on_click(move |_, _, cx| {
                                let location = app_services(cx).current_location.lock().clone();
                                if let Some(location) = location {
                                    navigation::request_open_book(
                                        cx,
                                        navigation::OpenBook {
                                            module: module.clone(),
                                            book: location.book,
                                            chapter: Some(location.chapter),
                                            verse: Some(location.verse),
                                        },
                                    );
                                }
                            }),
                        ),
                    )
            };

            list = list.child(
                Collapsible::new()
                    .open(open)
                    .child(trigger)
                    .content(content),
            );
        }
        list.into_any_element()
    }

    // --- Секции «Параллельные ссылки» и «Комментарии» -----------------------

    fn cross_refs_html(&self) -> Option<String> {
        let context = self.context.as_ref()?;
        if context.cross_refs.is_empty() {
            return None;
        }
        let mut blocks: Vec<ContentBlock> = Vec::new();
        for group in &context.cross_refs {
            blocks.push(ContentBlock::Heading {
                level: 3,
                content: vec![ContentInline::Text {
                    text: group.label.clone(),
                    marks: TextMarks::default(),
                }],
            });
            let items = group
                .targets
                .iter()
                .map(|target| {
                    vec![
                        ContentInline::Link {
                            target: LinkTarget::Bible(target.reference),
                            label: target.label.clone(),
                        },
                        ContentInline::Text {
                            text: format!(" ({})", target.votes),
                            marks: TextMarks {
                                small: true,
                                ..Default::default()
                            },
                        },
                    ]
                })
                .collect();
            blocks.push(ContentBlock::List {
                ordered: false,
                items,
            });
        }
        Some(article::blocks_to_html(&blocks))
    }

    /// «Комментарии»: каждый источник — Collapsible с row-триггером, как в
    /// «Других переводах» (по умолчанию свёрнут). Раскрытие помнится по
    /// модулю и номеру записи и переживает смену стиха.
    fn render_commentary_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(context) = &self.context else {
            return Self::no_verse_hint(cx);
        };
        if context.commentary.is_empty() {
            return Self::empty_hint("Нет комментария к этому стиху.", cx);
        }
        let weak = cx.entity().downgrade();
        let muted = cx.theme().muted_foreground;
        let foreground = cx.theme().foreground;
        let border = cx.theme().border;
        let mut list = v_flex().w_full().min_w_0();
        for (index, entry) in context.commentary.iter().enumerate() {
            let key = format!("{}#{index}", entry.module);
            let open = self.open_commentaries.contains(&key);
            let label = entry.label.clone();
            let anchor = entry.anchor.clone();
            let module_key = entry.module.to_string();

            let trigger = {
                let weak = weak.clone();
                let key = key.clone();
                h_flex()
                    .id(ElementId::Name(format!("commentary-trigger-{key}").into()))
                    .w_full()
                    .min_w_0()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .cursor_pointer()
                    .border_b_1()
                    .border_color(border)
                    .hover(|style| style.bg(cx.theme().list_hover))
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .xsmall()
                            .flex_none()
                            .text_color(muted)
                            .rotate(percentage(if open { 0.5 } else { 0. })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(foreground)
                            .child(label),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                            .text_color(muted)
                            .child(format!("{module_key} · {anchor}")),
                    )
                    .on_click(move |_, _, cx| {
                        _ = weak.update(cx, |this, cx| {
                            if !this.open_commentaries.remove(&key) {
                                this.open_commentaries.insert(key.clone());
                            }
                            cx.notify();
                        });
                    })
            };

            // Тело раскрывается только по клику, поэтому HTML строим лениво.
            let content = if open {
                let html = article::blocks_to_html(&entry.blocks);
                div()
                    .w_full()
                    .min_w_0()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        article::linked_text_view(
                            TextView::html(
                                ElementId::Name(format!("context-commentary-{key}").into()),
                                html,
                            ),
                            cx,
                        )
                        .w_full(),
                    )
                    .into_any_element()
            } else {
                div().into_any_element()
            };

            list = list.child(
                Collapsible::new()
                    .open(open)
                    .child(trigger)
                    .content(content),
            );
        }
        list.into_any_element()
    }

    fn render_cross_refs(&self, cx: &mut Context<Self>) -> AnyElement {
        let (Some(location), Some(context)) = (&self.location, &self.context) else {
            return Self::no_verse_hint(cx);
        };
        let source = VerseRef::single(location.book, location.chapter, location.verse);
        let module = location.module.clone();
        let add = Button::new("add-personal-cross-ref")
            .xsmall()
            .label(crate::i18n::tr(cx, "+ Добавить параллельное место"))
            .on_click(cx.listener(move |_, _, window, cx| {
                let owner = cx.weak_entity();
                crate::verse_picker::open(
                    "Добавить параллельное место",
                    Some(module.clone()),
                    move |picked, _, cx| {
                        shell::verse_context::add_user_cross_ref(
                            app_services(cx),
                            source,
                            picked.reference,
                        )?;
                        let _ = owner.update(cx, |this, cx| this.refresh_user_cross_refs(cx));
                        Ok(())
                    },
                    window,
                    cx,
                );
            }));

        let mut column = v_flex().w_full().min_w_0().gap_2().p_2().child(add);
        if let Some(error) = &self.cross_ref_error {
            column = column.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        if !context.user_cross_refs.is_empty() {
            column = column.child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(crate::i18n::tr(cx, "Мои параллельные места")),
            );
            for (index, target) in context.user_cross_refs.iter().enumerate() {
                let reference = target.reference;
                column = column.child(
                    h_flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(crate::verse_preview::text_link(
                                    ("personal-cross-ref", index),
                                    target.label.clone(),
                                    reference,
                                    None,
                                    cx,
                                )),
                        )
                        .child(
                            Button::new(("remove-personal-cross-ref", index))
                                .xsmall()
                                .ghost()
                                .icon(gpui_kit::assets::IconName::X)
                                .tooltip(crate::i18n::tr(cx, "Удалить параллельное место"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let result = shell::verse_context::remove_user_cross_ref(
                                        app_services(cx),
                                        source,
                                        reference,
                                    );
                                    match result {
                                        Ok(()) => this.refresh_user_cross_refs(cx),
                                        Err(error) => {
                                            this.cross_ref_error = Some(error.to_string());
                                            cx.notify();
                                        }
                                    }
                                })),
                        ),
                );
            }
        }
        if let Some(html) = self.cross_refs_html() {
            column = column.child(
                article::linked_text_view(TextView::html("context-xrefs", html), cx).w_full(),
            );
        } else if context.user_cross_refs.is_empty() {
            column = column.child(Self::empty_hint(
                "Нет параллельных мест для этого стиха.",
                cx,
            ));
        }
        column.into_any_element()
    }

    fn refresh_user_cross_refs(&mut self, cx: &mut Context<Self>) {
        if let (Some(location), Some(context)) = (&self.location, &mut self.context) {
            context.user_cross_refs =
                shell::verse_context::user_cross_refs(app_services(cx), location);
        }
        self.cross_ref_error = None;
        cx.notify();
    }

    fn render_notes_section(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.context.is_none() {
            return Self::no_verse_hint(cx);
        }
        let weak = cx.entity().downgrade();
        v_flex()
            .w_full()
            .min_w_0()
            .p_2()
            .gap_2()
            .child(
                div()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                    .text_color(cx.theme().muted_foreground)
                    .child(crate::i18n::tr(
                        cx,
                        "Комментарий привязан к активному переводу стиха.",
                    )),
            )
            .child(Textarea::new(&self.comment_input).h(px(140.)))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("save-verse-comment")
                            .primary()
                            .xsmall()
                            .label(crate::i18n::tr(cx, "Сохранить"))
                            .on_click(move |_, _, cx| {
                                _ = weak.update(cx, |this, cx| this.save_comment(cx));
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                            .text_color(cx.theme().muted_foreground)
                            .child(crate::i18n::tr(cx, "Пустой текст удаляет комментарий.")),
                    ),
            )
            .into_any_element()
    }

    fn no_verse_hint(cx: &App) -> AnyElement {
        Self::empty_hint("Кликните по стиху в тексте чтения.", cx)
    }

    fn empty_hint(text: &'static str, cx: &App) -> AnyElement {
        div()
            .p_2()
            .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
            .text_color(cx.theme().muted_foreground)
            .child(crate::i18n::tr(cx, text))
            .into_any_element()
    }

    // --- Рендер -------------------------------------------------------------

    /// Заголовок секции (row-триггер, этап 14): закреплён вне прокрутки —
    /// при скролле длинного содержимого остаётся на месте. Открытая
    /// секция подсвечена, шеврон повёрнут (паттерн Accordion gpui-kit).
    fn render_trigger(
        &self,
        index: usize,
        title: SharedString,
        weak: &WeakEntity<Self>,
        cx: &App,
    ) -> AnyElement {
        let open = self.open_section == Some(index);
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let weak = weak.clone();
        h_flex()
            .id(ElementId::Name(format!("context-trigger-{index}").into()))
            .w_full()
            .flex_none()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .cursor_pointer()
            .border_b_1()
            .border_color(border)
            .when(open, |this| this.bg(cx.theme().list_active))
            .hover(|style| style.bg(cx.theme().list_hover))
            .child(
                Icon::new(IconName::ChevronDown)
                    .xsmall()
                    .flex_none()
                    .text_color(muted)
                    .rotate(percentage(if open { 0.5 } else { 0. })),
            )
            .child(
                div()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .child(title),
            )
            .on_click(move |_, _, cx| {
                _ = weak.update(cx, |this, cx| {
                    this.open_section = match this.open_section {
                        Some(current) if current == index => None,
                        _ => Some(index),
                    };
                    cx.notify();
                });
            })
            .into_any_element()
    }

    /// Полоса «стих используется в цепочках» — между шапкой и секциями,
    /// как `render_chains_strip` форка. Клик по чипу открывает цепочку
    /// в панели «Цепочки».
    fn render_chains_strip(&self, cx: &App) -> Option<AnyElement> {
        if self.chain_hits.is_empty() {
            return None;
        }
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let accent = cx.theme().accent;
        let summary = if self.chain_hits.len() == 1 {
            "В 1 цепочке:".to_string()
        } else {
            format!("В {} цепочках:", self.chain_hits.len())
        };
        let chips = self.chain_hits.iter().map(|hit| {
            let id = hit.id.clone();
            div()
                .id(SharedString::from(format!("context-chain-hit-{id}")))
                .px_1()
                .py(px(1.))
                .mr_1()
                .rounded_sm()
                .border_1()
                .border_color(border)
                .bg(accent.opacity(0.12))
                .cursor_pointer()
                .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                .on_click(move |_, _, cx| {
                    navigation::request_open_chain(cx, Some(id.clone()));
                })
                .child(if hit.title.trim().is_empty() {
                    "(без названия)".to_string()
                } else {
                    hit.title.clone()
                })
                .into_any_element()
        });
        Some(
            h_flex()
                .w_full()
                .flex_wrap()
                .items_center()
                .px_3()
                .py_1()
                .gap_1()
                .border_b_1()
                .border_color(border)
                .child(
                    div()
                        .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                        .text_color(muted)
                        .child(summary),
                )
                .children(chips)
                .into_any_element(),
        )
    }

    fn render_header(&self, cx: &App) -> AnyElement {
        let Some(context) = &self.context else {
            return div().into_any_element();
        };
        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .items_baseline()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .child(context.reference_label.clone()),
            )
            .child(
                div()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                    .text_color(cx.theme().muted_foreground)
                    .child(context.module.to_string()),
            )
            .into_any_element()
    }
}

impl EventEmitter<PanelEvent> for ContextPanel {}

impl Focusable for ContextPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for ContextPanel {
    fn panel_name(&self) -> &'static str {
        Self::NAME
    }

    /// Панель стала/перестала быть отображаемой вкладкой своей группы.
    /// Чтение слушает это событие: hover-карточки слов показываются,
    /// только когда панель закрыта (контракт set_active — base dock).
    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
        navigation::set_word_panel_visible(cx, active);
    }

    /// Закрытие/удаление панели — тоже сигнал «не видна» (контракт
    /// base dock: set_active(false) при удалении не приходит).
    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        navigation::set_word_panel_visible(cx, false);
    }
}

impl Panel for ContextPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.context
            .as_ref()
            .map(|context| context.reference_label.clone())
            .unwrap_or_default()
    }
}

impl Render for ContextPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        let open_section = self.open_section;

        // Содержимое секций собирается по индексам (порядок фиксирован).
        let compact: [bool; 5] = [
            self.word.is_none(),
            self.context
                .as_ref()
                .is_none_or(|context| context.translations.is_empty()),
            self.context.is_none(),
            self.context
                .as_ref()
                .is_none_or(|context| context.commentary.is_empty()),
            self.context.is_none(),
        ];
        let contents: [Option<AnyElement>; 5] = [
            (open_section == Some(SECTION_WORD)).then(|| match self.word {
                Some(_) => self.render_word_section(cx),
                None => Self::empty_hint("Кликните по слову в тексте чтения", cx),
            }),
            (open_section == Some(SECTION_TRANSLATIONS))
                .then(|| self.render_translations_section(cx)),
            (open_section == Some(SECTION_CROSSREFS)).then(|| self.render_cross_refs(cx)),
            (open_section == Some(SECTION_COMMENTARIES))
                .then(|| self.render_commentary_section(cx)),
            (open_section == Some(SECTION_NOTES)).then(|| self.render_notes_section(cx)),
        ];
        const TITLES: [&str; 5] = [
            "Слово",
            "Другие переводы",
            "Параллельные ссылки",
            "Комментарии",
            "Заметки",
        ];

        let mut body = v_flex().size_full().min_w_0();
        body = body.child(self.render_header(cx));
        // Оформление слова/стиха открывается локально у контекстного меню
        // чтения и больше не занимает место в правой панели.
        if let Some(strip) = self.render_chains_strip(cx) {
            body = body.child(strip);
        }
        for (index, content) in contents.into_iter().enumerate() {
            body = body.child(self.render_trigger(
                index,
                crate::i18n::tr(cx, TITLES[index]),
                &weak,
                cx,
            ));
            // Прокручивается только содержимое открытой секции — её
            // триггер и триггеры остальных секций всегда на виду (этап 14).
            // flex_shrink вместо flex_1: маленькое содержимое (подсказка
            // «не найдено») не распахивает блок на всю высоту, большое —
            // ужимается до свободного места и скроллится (этап 14.1).
            if let Some(content) = content {
                let wrapper = div()
                    .id(ElementId::Name(format!("context-content-{index}").into()))
                    .w_full()
                    .min_h_0()
                    .child(content);
                let wrapper = if compact[index] {
                    wrapper.flex_none().into_any_element()
                } else {
                    wrapper
                        .flex_shrink(1.0)
                        .overflow_y_scrollbar()
                        .into_any_element()
                };
                body = body.child(wrapper);
            }
        }
        let font_family = crate::ui_scale::panel_font_family(cx);
        body.when(!font_family.is_empty(), |root| {
            root.font_family(font_family)
        })
    }
}
