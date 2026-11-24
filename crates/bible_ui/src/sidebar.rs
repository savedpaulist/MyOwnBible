//! Левая панель с раскрываемой полосой разделов.

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bible_domain::ids::{BookNumber, ModuleId, VerseRef};
use bible_domain::ports::{ModuleCatalog as _, ModuleKind};
use bible_domain::tags::Tag;
use gpui_kit::component::color_picker::{ColorPickerEvent, ColorPickerState, ColorSelect};
use shell::notes::{IndexEvent, NotesHandle, NotesSubscription};

use crate::annotations as palette;
use crate::dictionary::DictionaryBrowser;
use crate::navigation;
use crate::services::app_services;

/// Пункт менюшки.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Section {
    Dictionaries,
    Notes,
    Comments,
    Tags,
    Chains,
    Questions,
}

#[derive(Clone)]
struct SidebarCommentaryRow {
    reference: VerseRef,
    label: String,
}

impl Section {
    fn from_settings(value: &str) -> Self {
        match value {
            "notes" => Self::Notes,
            "comments" => Self::Comments,
            "tags" => Self::Tags,
            "chains" => Self::Chains,
            "questions" => Self::Questions,
            _ => Self::Dictionaries,
        }
    }

    fn as_settings(self) -> &'static str {
        match self {
            Self::Dictionaries => "dictionaries",
            Self::Notes => "notes",
            Self::Comments => "comments",
            Self::Tags => "tags",
            Self::Chains => "chains",
            Self::Questions => "questions",
        }
    }

    fn label(self, cx: &App) -> SharedString {
        crate::i18n::tr(
            cx,
            match self {
                Self::Dictionaries => "Словари",
                Self::Notes => "Заметки",
                Self::Comments => "Комментарии",
                Self::Tags => "Теги",
                Self::Chains => "Цепочки",
                Self::Questions => "Вопросы",
            },
        )
    }

    fn icon(self) -> gpui_kit::assets::IconName {
        match self {
            Self::Dictionaries => gpui_kit::assets::IconName::BookA,
            Self::Notes => gpui_kit::assets::IconName::NotebookPen,
            Self::Comments => gpui_kit::assets::IconName::MessageSquare,
            Self::Tags => gpui_kit::assets::IconName::Tags,
            Self::Chains => gpui_kit::assets::IconName::Link,
            Self::Questions => gpui_kit::assets::IconName::CircleQuestionMark,
        }
    }
}

pub struct SidebarView {
    section: Section,
    collapsed: bool,
    /// Контент выбранного раздела; при скрытии остаётся только полоса иконок.
    content_visible: bool,
    dictionaries: Entity<DictionaryBrowser>,
    chains: Entity<crate::chains::ChainsPanel>,
    questions: Entity<crate::questions::QuestionsPanel>,
    /// Заметки (этап 15.2): список хранилища; клик открывает заметку
    /// вкладкой в центре (note_tab.rs). Создание — полем внизу списка.
    notes_list: Vec<shell::notes::NoteSummary>,
    new_note_input: Entity<InputState>,
    new_tag_input: Entity<InputState>,
    commentary_module: Option<ModuleId>,
    commentary_rows: Arc<Vec<SidebarCommentaryRow>>,
    commentary_list: ListState,
    /// Очередь событий индекса заметок с потока watcher'а; поллинг 300 мс.
    pending_events: Arc<Mutex<Vec<IndexEvent>>>,
    subscribed_index: Option<NotesHandle>,
    _index_token: Option<NotesSubscription>,
    /// Теги (этап 21): список со счётчиками, drill-down в стихи тега.
    tags: Vec<Tag>,
    tag_counts: HashMap<i64, i64>,
    open_tag: Option<Tag>,
    tag_verses: Vec<VerseRef>,
    tag_rename_input: Entity<InputState>,
    tag_color: Entity<ColorPickerState>,
    /// Список тегов уже загружался (ленивая первая загрузка при
    /// выборе раздела; дальше — по TagsChanged).
    tags_loaded: bool,
    _subscriptions: Vec<Subscription>,
}

impl SidebarView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dictionaries = cx.new(|cx| DictionaryBrowser::new(window, cx));
        let chains = cx.new(|cx| crate::chains::ChainsPanel::new(window, cx));
        let questions = cx.new(|cx| crate::questions::QuestionsPanel::new(window, cx));
        let tag_color = cx.new(|cx| ColorPickerState::new(window, cx));
        let color_subscription =
            cx.subscribe(&tag_color, |this, _, event: &ColorPickerEvent, cx| {
                if let ColorPickerEvent::Change(Some(color)) = event {
                    this.set_open_tag_color(*color, cx);
                }
            });
        let tag_rename_input = cx
            .new(|cx| InputState::new(window, cx).placeholder(crate::i18n::tr_static("Имя тега…")));
        let new_note_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(crate::i18n::tr_static("Папка/Название новой заметки"))
        });
        let new_tag_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(crate::i18n::tr_static("Имя нового тега…"))
        });
        let subscriptions = vec![
            // Enter в поле имени тега — переименование открытого тега.
            cx.subscribe_in(
                &tag_rename_input,
                window,
                |this, tag_rename_input, event: &InputEvent, _, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        let name = tag_rename_input.read(cx).value().to_string();
                        this.rename_open_tag(&name, cx);
                    }
                },
            ),
            // Enter в поле новой заметки — создать и открыть вкладкой.
            cx.subscribe_in(
                &new_note_input,
                window,
                |this, input, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        let name = input.read(cx).value().to_string();
                        this.create_note(name, window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &new_tag_input,
                window,
                |this, input, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.create_tag(input.read(cx).value().to_string(), window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &navigation::bus(cx),
                window,
                |this, _, _: &navigation::OpenChain, _, cx| this.show_section(Section::Chains, cx),
            ),
            // Теги изменились (пикер в чтении, действия здесь) —
            // перечитываем список и открытый тег.
            cx.subscribe_in(
                &navigation::bus(cx),
                window,
                |this, _, _: &navigation::TagsChanged, _, cx| {
                    this.reload_tags(cx);
                },
            ),
            // Запрос раскрыть менюшку на разделе (кнопка «Словари» в
            // Status bar, словарь без темы из других панелей — этап 15.2).
            cx.subscribe_in(
                &navigation::bus(cx),
                window,
                |this, _, event: &navigation::OpenSidebarModule, window, cx| {
                    let section = match event.section {
                        navigation::SidebarSectionKind::Dictionaries => Section::Dictionaries,
                        navigation::SidebarSectionKind::Notes => Section::Notes,
                        navigation::SidebarSectionKind::Tags => Section::Tags,
                    };
                    this.show_section(section, cx);
                    // Словарь указан явно — сразу drill-down в его темы.
                    if let (Section::Dictionaries, Some(module)) = (section, &event.module) {
                        this.dictionaries.update(cx, |dictionary, cx| {
                            dictionary.select_module(Some(module.clone()), window, cx);
                        });
                    }
                },
            ),
            // Хранилище заметок переинициализировано из настроек —
            // переподписаться на новый индекс и перечитать список.
            cx.subscribe_in(
                &navigation::bus(cx),
                window,
                |this, _, _: &navigation::SettingsChanged, _, cx| {
                    this.on_settings_changed(cx);
                },
            ),
        ];

        // События индекса заметок приходят с потока watcher'а — дескрипторы
        // GPUI там трогать нельзя (!Send), поэтому callback складывает их в
        // очередь, а менюшка разбирает её поллингом раз в 300 мс
        // (та же схема, что у панели/вкладки заметок).
        let pending_events: Arc<Mutex<Vec<IndexEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let pending_for_loop = Arc::clone(&pending_events);
        cx.spawn_in(window, async move |this, window| {
            loop {
                window
                    .background_executor()
                    .timer(Duration::from_millis(300))
                    .await;
                let events: Vec<IndexEvent> = pending_for_loop.lock().unwrap().drain(..).collect();
                if events.is_empty() {
                    continue;
                }
                let alive = this
                    .update_in(window, |this, _, cx| {
                        if !events.is_empty() {
                            this.reload_notes(cx);
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();

        let settings = shell::settings::current(app_services(cx));
        let mut this = Self {
            section: Section::from_settings(&settings.sidebar_section),
            collapsed: settings.sidebar_collapsed,
            content_visible: settings.sidebar_visible,
            dictionaries,
            chains,
            questions,
            notes_list: Vec::new(),
            new_note_input,
            new_tag_input,
            commentary_module: None,
            commentary_rows: Arc::new(Vec::new()),
            commentary_list: ListState::new(0, ListAlignment::Top, px(32.)),
            pending_events,
            subscribed_index: None,
            _index_token: None,
            tags: Vec::new(),
            tag_counts: HashMap::new(),
            open_tag: None,
            tag_verses: Vec::new(),
            tag_rename_input,
            tag_color,
            tags_loaded: false,
            _subscriptions: subscriptions
                .into_iter()
                .chain([color_subscription])
                .collect(),
        };
        this.subscribe_notes_index(cx);
        this.reload_notes(cx);
        // Восстановленный раздел «Теги» грузим сразу (ленивая загрузка
        // срабатывает только при смене раздела).
        if this.section == Section::Tags {
            this.reload_tags(cx);
        }
        this
    }

    fn select_section(&mut self, section: Section, cx: &mut Context<Self>) {
        if self.section == section {
            // Повторный клик по той же иконке прячет только контент.
            self.content_visible = !self.content_visible;
        } else {
            self.section = section;
            self.content_visible = true;
        }
        if self.content_visible && section == Section::Tags && !self.tags_loaded {
            self.reload_tags(cx);
        }
        let selected = self.section.as_settings().to_string();
        let visible = self.content_visible;
        if let Err(error) = shell::settings::update(app_services(cx), |settings| {
            settings.sidebar_section = selected;
            settings.sidebar_visible = visible;
        }) {
            log::error!("save sidebar section visibility: {error:#}");
        }
        cx.notify();
    }

    /// Показать выбранный раздел независимо от того, какая иконка нажата.
    /// Используется программной навигацией из других панелей.
    fn show_section(&mut self, section: Section, cx: &mut Context<Self>) {
        self.section = section;
        self.content_visible = true;
        if section == Section::Tags && !self.tags_loaded {
            self.reload_tags(cx);
        }
        let selected = section.as_settings().to_string();
        if let Err(error) = shell::settings::update(app_services(cx), |settings| {
            settings.sidebar_section = selected;
            settings.sidebar_visible = true;
        }) {
            log::error!("save sidebar section: {error:#}");
        }
        cx.notify();
    }

    pub fn show_content(&mut self, cx: &mut Context<Self>) {
        if self.content_visible {
            return;
        }
        self.content_visible = true;
        if let Err(error) =
            shell::settings::update(app_services(cx), |settings| settings.sidebar_visible = true)
        {
            log::error!("save sidebar content visibility: {error:#}");
        }
        cx.notify();
    }

    pub fn toggle_content(&mut self, cx: &mut Context<Self>) {
        self.content_visible = !self.content_visible;
        let visible = self.content_visible;
        if let Err(error) = shell::settings::update(app_services(cx), |settings| {
            settings.sidebar_visible = visible
        }) {
            log::error!("save sidebar content visibility: {error:#}");
        }
        cx.notify();
    }

    fn toggle_collapsed(&mut self, cx: &mut Context<Self>) {
        self.collapsed = !self.collapsed;
        let collapsed = self.collapsed;
        if let Err(error) = shell::settings::update(app_services(cx), |settings| {
            settings.sidebar_collapsed = collapsed;
        }) {
            log::error!("save sidebar collapsed: {error:#}");
        }
        cx.notify();
    }

    // --- Заметки (этап 15.2: список, открытие — вкладкой в центре) -------

    fn notes_index(cx: &App) -> Option<NotesHandle> {
        shell::notes::current(app_services(cx))
    }

    /// Подписаться на текущий индекс заметок (старт и смена хранилища).
    fn subscribe_notes_index(&mut self, cx: &mut Context<Self>) {
        let index = Self::notes_index(cx);
        self._index_token = index.as_ref().map(|index| {
            let pending = Arc::clone(&self.pending_events);
            index.subscribe(move |event| {
                pending.lock().unwrap().push(event.clone());
            })
        });
        self.subscribed_index = index;
    }

    /// Настройки изменились: если хранилище заметок заменили (другой Arc
    /// индекса) — переподписаться и перечитать список.
    fn on_settings_changed(&mut self, cx: &mut Context<Self>) {
        let current = Self::notes_index(cx);
        let changed = match (&self.subscribed_index, &current) {
            (None, None) => false,
            (Some(old), Some(new)) => !old.same_storage(new),
            _ => true,
        };
        if changed {
            self.subscribe_notes_index(cx);
            self.pending_events.lock().unwrap().clear();
            self.reload_notes(cx);
        }
        cx.notify();
    }

    fn reload_notes(&mut self, cx: &mut Context<Self>) {
        let Some(index) = Self::notes_index(cx) else {
            self.notes_list = Vec::new();
            cx.notify();
            return;
        };
        self.notes_list = shell::notes::list_notes(&index).unwrap_or_default();
        cx.notify();
    }

    fn open_commentary_module(&mut self, module: ModuleId, cx: &mut Context<Self>) {
        let entries = app_services(cx).registry.lock().commentary_entries(&module);
        let mut book_labels = std::collections::HashMap::new();
        let rows = entries
            .into_iter()
            .map(|entry| {
                let book = book_labels
                    .entry(entry.book)
                    .or_insert_with(|| self.book_label(entry.book, cx))
                    .clone();
                let label = if entry.chapter_to != entry.chapter_from {
                    format!(
                        "{book} {}:{}–{}:{}",
                        entry.chapter_from, entry.verse_from, entry.chapter_to, entry.verse_to
                    )
                } else if entry.verse_to != entry.verse_from {
                    format!(
                        "{book} {}:{}–{}",
                        entry.chapter_from, entry.verse_from, entry.verse_to
                    )
                } else {
                    format!("{book} {}:{}", entry.chapter_from, entry.verse_from)
                };
                SidebarCommentaryRow {
                    reference: VerseRef {
                        book: entry.book,
                        chapter: entry.chapter_from,
                        verse: entry.verse_from,
                        end_verse: (entry.chapter_to == entry.chapter_from
                            && entry.verse_to != entry.verse_from)
                            .then_some(entry.verse_to),
                    },
                    label,
                }
            })
            .collect::<Vec<_>>();
        let first_reference = rows.first().map(|row| row.reference);
        self.commentary_list.reset(rows.len());
        self.commentary_module = Some(module.clone());
        self.commentary_rows = Arc::new(rows);
        if let Some(reference) = first_reference {
            navigation::request_open_commentary(cx, module, reference);
        }
        cx.notify();
    }

    fn close_commentary_module(&mut self, cx: &mut Context<Self>) {
        self.commentary_module = None;
        self.commentary_rows = Arc::new(Vec::new());
        self.commentary_list.reset(0);
        cx.notify();
    }

    /// Создать заметку и открыть её вкладкой в центре.
    fn create_note(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        let Some(index) = Self::notes_index(cx) else {
            return;
        };
        match shell::notes::create_note(&index, &name) {
            Ok(doc) => {
                self.new_note_input.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                });
                self.reload_notes(cx);
                navigation::request_open_note(cx, doc.relpath);
            }
            Err(error) => log::error!("create note {name}: {error:#}"),
        }
        cx.notify();
    }

    // --- Теги (этап 21) ---------------------------------------------------

    fn create_tag(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        match shell::tags::create_tag(app_services(cx), name, "green") {
            Ok(_) => {
                self.new_tag_input.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                });
                navigation::notify_tags_changed(cx);
            }
            Err(error) => log::error!("create tag {name}: {error:#}"),
        }
    }

    /// Перечитать список тегов и стихи открытого тега (если есть).
    /// Открытый тег мог быть переименован/удалён — синхронизируем его
    /// копию со свежим списком.
    fn reload_tags(&mut self, cx: &mut Context<Self>) {
        let services = app_services(cx);
        self.tags = shell::tags::list_tags(services);
        self.tag_counts = shell::tags::tag_verse_counts(services);
        self.tags_loaded = true;
        if let Some(open) = &self.open_tag {
            match self.tags.iter().find(|tag| tag.id == open.id) {
                Some(fresh) => {
                    let fresh = fresh.clone();
                    self.tag_verses = shell::tags::verses_of_tag(services, fresh.id);
                    self.open_tag = Some(fresh);
                }
                None => {
                    self.open_tag = None;
                    self.tag_verses = Vec::new();
                }
            }
        }
        cx.notify();
    }

    /// Drill-down в тег: стихи + поле переименования с текущим именем.
    fn open_tag_drilldown(&mut self, tag: Tag, window: &mut Window, cx: &mut Context<Self>) {
        self.tag_verses = shell::tags::verses_of_tag(app_services(cx), tag.id);
        let name = tag.name.clone();
        self.tag_rename_input.update(cx, |input, cx| {
            input.set_value(name, window, cx);
        });
        let color = palette::base(&tag.color, cx);
        self.tag_color
            .update(cx, |state, cx| state.set_value(color, window, cx));
        self.open_tag = Some(tag);
        cx.notify();
    }

    fn close_tag(&mut self, cx: &mut Context<Self>) {
        self.open_tag = None;
        self.tag_verses = Vec::new();
        cx.notify();
    }

    fn rename_open_tag(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(tag) = &self.open_tag else {
            return;
        };
        let name = name.trim();
        if name.is_empty() || name == tag.name {
            return;
        }
        if let Err(error) = shell::tags::rename_tag(app_services(cx), tag.id, name) {
            log::error!("rename tag failed: {error:#}");
            return;
        }
        navigation::notify_tags_changed(cx);
    }

    fn set_open_tag_color(&mut self, color: Hsla, cx: &mut Context<Self>) {
        let Some(tag) = &self.open_tag else { return };
        if let Err(error) =
            shell::tags::set_tag_color(app_services(cx), tag.id, &palette::to_hex(color))
        {
            log::error!("set tag color failed: {error:#}");
            return;
        }
        navigation::notify_tags_changed(cx);
    }

    fn delete_open_tag(&mut self, cx: &mut Context<Self>) {
        let Some(tag) = &self.open_tag else {
            return;
        };
        if let Err(error) = shell::tags::delete_tag(app_services(cx), tag.id) {
            log::error!("delete tag failed: {error:#}");
            return;
        }
        navigation::notify_tags_changed(cx);
    }

    /// Подпись книги для строки стиха тега: короткое имя из перевода
    /// по умолчанию; без него — русский алиас из статической таблицы.
    /// Модуль для показа тегированных мест: теги модуль-независимы,
    /// поэтому показываем и открываем их в том переводе, который сейчас
    /// открыт в чтении; если чтение пусто — в переводе по умолчанию.
    fn preferred_bible_module(cx: &App) -> Option<ModuleId> {
        let services = app_services(cx);
        if let Some(location) = services.current_location.lock().as_ref() {
            return Some(location.module.clone());
        }
        services.default_translation()
    }

    fn book_label(&self, book: BookNumber, cx: &App) -> String {
        let services = app_services(cx);
        if let Some(module) = Self::preferred_bible_module(cx) {
            let entry = services
                .registry
                .lock()
                .books_of(&module)
                .into_iter()
                .find(|entry| entry.book_number == book.0);
            if let Some(entry) = entry {
                return entry.short_name;
            }
        }
        bible_domain::book_aliases::BOOK_ALIASES
            .iter()
            .find(|(number, _)| *number == book.0)
            .and_then(|(_, names)| {
                names
                    .iter()
                    .find(|name| name.chars().any(|c| c.is_alphabetic() && !c.is_ascii()))
                    .or_else(|| names.first())
            })
            .map(|name| (*name).to_string())
            .unwrap_or_else(|| format!("Кн.{}", book.0))
    }

    /// Список тегов со счётчиками стихов (drill-down — render_tag_verses).
    fn render_tags(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut list = v_flex()
            .id("sidebar-tags")
            .w_full()
            .min_w_0()
            .flex_1()
            .min_h_0()
            .p_1()
            .gap(px(1.));
        if self.tags.is_empty() {
            list = list.child(
                div()
                    .p_2()
                    .text_size(px(crate::ui_scale::left_font_size(cx, 14.0)))
                    .text_color(cx.theme().muted_foreground)
                    .child(crate::i18n::tr_static(
                        "Тегов пока нет — поставьте первый из меню стиха («Теги стиха…»).",
                    )),
            );
        }
        for tag in &self.tags {
            let tag = tag.clone();
            let count = self.tag_counts.get(&tag.id).copied().unwrap_or(0);
            let tag_for_click = tag.clone();
            list = list.child(
                h_flex()
                    .id(ElementId::Name(format!("sidebar-tag-{}", tag.id).into()))
                    .w_full()
                    .min_w_0()
                    .px_2()
                    .py(px(2.))
                    .gap_2()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(cx.theme().accent.opacity(0.12)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_tag_drilldown(tag_for_click.clone(), window, cx);
                    }))
                    .child(
                        div()
                            .w(px(10.))
                            .h(px(10.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(palette::word_fg(&tag.color, cx)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_size(px(crate::ui_scale::left_font_size(cx, 14.0)))
                            .text_color(cx.theme().foreground)
                            .child(tag.name.clone()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(crate::ui_scale::left_font_size(cx, 12.0)))
                            .text_color(cx.theme().muted_foreground)
                            .child(count.to_string()),
                    ),
            );
        }
        v_flex()
            .w_full()
            .h_full()
            .min_h_0()
            .child(list.overflow_y_scrollbar())
            .child(
                h_flex()
                    .w_full()
                    .gap_1()
                    .p_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.new_tag_input).small()),
                    )
                    .child(
                        gpui_kit::component::button::Button::new("sidebar-tag-create")
                            .xsmall()
                            .label(crate::i18n::tr(cx, "Добавить тег"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                let name = this.new_tag_input.read(cx).value().to_string();
                                this.create_tag(name, window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_comments(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(module) = self.commentary_module.clone() else {
            let modules = app_services(cx)
                .registry
                .modules()
                .into_iter()
                .filter(|module| module.kind == ModuleKind::Commentary)
                .collect::<Vec<_>>();
            if modules.is_empty() {
                return div()
                    .p_3()
                    .text_color(cx.theme().muted_foreground)
                    .child(crate::i18n::tr(cx, "Модули комментариев не найдены."))
                    .into_any_element();
            }
            return v_flex()
                .id("sidebar-commentary-modules")
                .size_full()
                .min_h_0()
                .p_1()
                .gap_1()
                .children(modules.into_iter().enumerate().map(|(index, meta)| {
                    let module = meta.id.clone();
                    let label = meta.description.unwrap_or_else(|| meta.id.to_string());
                    div()
                        .id(("sidebar-commentary-module", index))
                        .w_full()
                        .min_w_0()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(crate::ui_scale::row_background(index, cx))
                        .cursor_pointer()
                        .hover(|style| style.bg(cx.theme().accent.opacity(0.12)))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_commentary_module(module.clone(), cx);
                        }))
                }))
                .overflow_y_scrollbar()
                .into_any_element();
        };

        let module_title = app_services(cx)
            .registry
            .module(&module)
            .and_then(|meta| meta.description)
            .unwrap_or_else(|| module.to_string());
        let rows = self.commentary_rows.clone();
        let module_for_rows = module.clone();
        let results = list(self.commentary_list.clone(), move |index, _, cx| {
            let Some(row) = rows.get(index).cloned() else {
                return div().into_any_element();
            };
            let module = module_for_rows.clone();
            v_flex()
                .id(("sidebar-commentary-row", index))
                .w_full()
                .min_w_0()
                .px_2()
                .py_1()
                .gap_1()
                .rounded_md()
                .bg(crate::ui_scale::row_background(index, cx))
                .cursor_pointer()
                .hover(|style| style.bg(cx.theme().accent.opacity(0.12)))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(row.label.clone()),
                )
                .on_click(move |_, _, cx| {
                    navigation::request_open_commentary(cx, module.clone(), row.reference);
                })
                .into_any_element()
        });
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .id("sidebar-commentary-back")
                            .p_1()
                            .rounded_md()
                            .cursor_pointer()
                            .child(Icon::new(gpui_kit::assets::IconName::ArrowLeft).xsmall())
                            .on_click(
                                cx.listener(|this, _, _, cx| this.close_commentary_module(cx)),
                            ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(module_title),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(results)
                    .vertical_scrollbar(&self.commentary_list),
            )
            .into_any_element()
    }

    /// Drill-down тега: назад, точка-переключатель цвета, имя
    /// (редактируемое, Enter — сохранить), удаление; стихи тега.
    fn render_tag_verses(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut list = v_flex()
            .id("sidebar-tag-verses")
            .w_full()
            .min_w_0()
            .flex_1()
            .min_h_0()
            .p_1()
            .gap(px(1.));
        if self.tag_verses.is_empty() {
            list = list.child(
                div()
                    .p_2()
                    .text_size(px(crate::ui_scale::left_font_size(cx, 14.0)))
                    .text_color(cx.theme().muted_foreground)
                    .child(crate::i18n::tr_static("С этим тегом пока нет стихов.")),
            );
        }
        for reference in &self.tag_verses {
            let reference = *reference;
            let label = format!(
                "{} {}:{}",
                self.book_label(reference.book, cx),
                reference.chapter,
                reference.verse
            );
            list = list.child(
                div()
                    .id(ElementId::Name(
                        format!(
                            "sidebar-tag-verse-{}-{}-{}",
                            reference.book.0, reference.chapter, reference.verse
                        )
                        .into(),
                    ))
                    .w_full()
                    .min_w_0()
                    .px_2()
                    .py(px(2.))
                    .rounded_md()
                    .cursor_pointer()
                    .text_size(px(crate::ui_scale::left_font_size(cx, 14.0)))
                    .text_color(cx.theme().foreground)
                    .hover(|style| style.bg(cx.theme().accent.opacity(0.12)))
                    .child(label)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        // Теги модуль-независимы: открываем место в модуле,
                        // который сейчас в чтении (иначе — перевод по умолчанию).
                        let Some(module) = Self::preferred_bible_module(cx) else {
                            return;
                        };
                        navigation::request_open_book(
                            cx,
                            navigation::OpenBook {
                                module,
                                book: reference.book,
                                chapter: Some(reference.chapter),
                                verse: Some(reference.verse),
                            },
                        );
                    })),
            );
        }

        v_flex()
            .w_full()
            .h_full()
            .min_h_0()
            .child(
                h_flex()
                    .w_full()
                    .p_1()
                    .gap_1()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .id("sidebar-tag-back")
                            .p_1()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|style| style.bg(cx.theme().accent.opacity(0.12)))
                            .child(
                                Icon::new(gpui_kit::assets::IconName::ArrowLeft)
                                    .xsmall()
                                    .text_color(cx.theme().foreground),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.close_tag(cx);
                            })),
                    )
                    .child(
                        div()
                            .w(px(72.))
                            .child(ColorSelect::new(&self.tag_color).xsmall().w_full()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(Input::new(&self.tag_rename_input).small()),
                    )
                    .child(
                        div()
                            .id("sidebar-tag-delete")
                            .p_1()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|style| style.bg(cx.theme().danger.opacity(0.15)))
                            .child(
                                Icon::new(gpui_kit::assets::IconName::Trash)
                                    .xsmall()
                                    .text_color(cx.theme().danger),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.delete_open_tag(cx);
                            })),
                    ),
            )
            .child(list.overflow_y_scrollbar())
            .into_any_element()
    }

    /// Список заметок хранилища (этап 15.2): клик открывает заметку
    /// вкладкой в центре; внизу — создание новой.
    fn render_notes(&self, cx: &mut Context<Self>) -> AnyElement {
        if Self::notes_index(cx).is_none() {
            return v_flex()
                .w_full()
                .h_full()
                .p_3()
                .child(
                    div()
                        .text_size(px(crate::ui_scale::left_font_size(cx, 14.0)))
                        .text_color(cx.theme().muted_foreground)
                        .child(crate::i18n::tr_static(
                            "Заметки отключены: укажите папку заметок в настройках.",
                        )),
                )
                .into_any_element();
        }
        let mut list = v_flex()
            .id("sidebar-notes")
            .w_full()
            .min_w_0()
            .flex_1()
            .min_h_0()
            .p_1()
            .gap(px(1.));
        if self.notes_list.is_empty() {
            list = list.child(
                div()
                    .p_2()
                    .text_size(px(crate::ui_scale::left_font_size(cx, 14.0)))
                    .text_color(cx.theme().muted_foreground)
                    .child(crate::i18n::tr_static(
                        "Заметок пока нет — создайте первую в поле ниже.",
                    )),
            );
        }
        for (index, summary) in self.notes_list.iter().enumerate() {
            let relpath = summary.relpath.clone();
            list = list.child(
                div()
                    .id(ElementId::Name(
                        format!("sidebar-note-{}", summary.relpath).into(),
                    ))
                    .w_full()
                    .min_w_0()
                    .px_2()
                    .py(px(2.))
                    .rounded_md()
                    .bg(crate::ui_scale::row_background(index, cx))
                    .cursor_pointer()
                    .hover(|style| style.bg(cx.theme().accent.opacity(0.12)))
                    .child(
                        div()
                            .text_size(px(crate::ui_scale::left_font_size(cx, 14.0)))
                            .text_color(cx.theme().foreground)
                            .child(summary.title.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(crate::ui_scale::left_font_size(cx, 12.0)))
                            .text_color(cx.theme().muted_foreground)
                            .child(summary.relpath.clone()),
                    )
                    .on_click(move |_, _, cx| {
                        navigation::request_open_note(cx, relpath.clone());
                    }),
            );
        }
        v_flex()
            .w_full()
            .h_full()
            .min_h_0()
            .child(list.overflow_y_scrollbar())
            .child(
                h_flex()
                    .w_full()
                    .gap_1()
                    .p_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.new_note_input).small()),
                    )
                    .child(
                        gpui_kit::component::button::Button::new("sidebar-note-create")
                            .xsmall()
                            .label("+")
                            .on_click(cx.listener(|this, _, window, cx| {
                                let name = this.new_note_input.read(cx).value().to_string();
                                this.create_note(name, window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_content(&mut self, cx: &mut Context<Self>) -> AnyElement {
        match self.section {
            Section::Dictionaries => self.dictionaries.clone().into_any_element(),
            Section::Notes => self.render_notes(cx),
            Section::Comments => self.render_comments(cx),
            Section::Chains => self.chains.clone().into_any_element(),
            Section::Questions => self.questions.clone().into_any_element(),
            Section::Tags => match &self.open_tag {
                Some(_) => self.render_tag_verses(cx),
                None => self.render_tags(cx),
            },
        }
    }
}

impl Render for SidebarView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::assets::IconName;
        let collapsed = self.collapsed;
        let accent = cx.theme().accent;
        let menu = [
            Section::Dictionaries,
            Section::Notes,
            Section::Comments,
            Section::Tags,
            Section::Chains,
            Section::Questions,
        ];
        let icon_size = crate::ui_scale::icon_size(cx);
        let rail_button = |id: &'static str, icon, label: SharedString, show_label: bool| {
            let tooltip = label.clone();
            h_flex()
                .id(id)
                .w_full()
                .h(px(42.))
                .rounded_md()
                .cursor_pointer()
                .hover(|style| style.bg(accent.opacity(0.16)))
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
                })
                .child(
                    h_flex()
                        .w(px(42.))
                        .flex_shrink_0()
                        .justify_center()
                        .child(Icon::new(icon).size(icon_size)),
                )
                .children(show_label.then(|| div().child(label)))
        };
        let rail = v_flex()
            .h_full()
            .flex_shrink_0()
            .p_1()
            .gap_1()
            .w(px(if collapsed { 50. } else { 190. }))
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().border)
            .children(menu.into_iter().map(|item| {
                rail_button(item.as_settings(), item.icon(), item.label(cx), !collapsed)
                    .when(self.content_visible && self.section == item, |button| {
                        button.bg(accent.opacity(0.18))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.select_section(item, cx)))
            }))
            .child(div().flex_1())
            .child(
                rail_button(
                    "open-settings",
                    IconName::Settings,
                    crate::i18n::tr(cx, "Настройки"),
                    !collapsed,
                )
                .on_click(|_, _, cx| crate::settings::open_settings_window(cx)),
            )
            .child(
                rail_button(
                    "toggle-sidebar-labels",
                    if collapsed {
                        IconName::PanelLeftOpen
                    } else {
                        IconName::PanelLeftClose
                    },
                    crate::i18n::tr(
                        cx,
                        if collapsed {
                            "Развернуть меню"
                        } else {
                            "Свернуть меню"
                        },
                    ),
                    false,
                )
                .justify_center()
                .on_click(cx.listener(|this, _, _, cx| this.toggle_collapsed(cx))),
            )
            .child(
                rail_button(
                    "toggle-sidebar-content",
                    IconName::PanelLeft,
                    crate::i18n::tr(cx, "Показать или скрыть содержимое"),
                    false,
                )
                .justify_center()
                .on_click(cx.listener(|this, _, _, cx| this.toggle_content(cx))),
            );
        let section = self.section;
        let content = self.render_content(cx);
        let mut layout = h_flex().h_full().child(rail);
        if self.content_visible {
            layout = layout.child(
                div()
                    .w(px(match self.section {
                        Section::Chains | Section::Questions => 340.,
                        _ => 280.,
                    }))
                    .flex_shrink_0()
                    .h_full()
                    .min_h_0()
                    .border_r_1()
                    .border_color(cx.theme().border)
                    .child(content)
                    .context_menu(move |menu, _, _| match section {
                        Section::Notes => menu.item(
                            PopupMenuItem::new(crate::i18n::tr_static("Создать новую заметку"))
                                .on_click(|_, _, cx| {
                                    let Some(index) = shell::notes::current(app_services(cx))
                                    else {
                                        return;
                                    };
                                    match shell::notes::create_untitled_note(&index) {
                                        Ok(note) => navigation::request_open_note(cx, note.relpath),
                                        Err(error) => log::error!("create note: {error:#}"),
                                    }
                                }),
                        ),
                        Section::Tags => menu.item(
                            PopupMenuItem::new(crate::i18n::tr_static("Создать новый тег"))
                                .on_click(|_, _, cx| {
                                    if let Err(error) = shell::tags::create_tag(
                                        app_services(cx),
                                        crate::i18n::tr_static("Новый тег"),
                                        "green",
                                    ) {
                                        log::error!("create tag: {error:#}");
                                    } else {
                                        navigation::notify_tags_changed(cx);
                                    }
                                }),
                        ),
                        Section::Chains => menu.item(
                            PopupMenuItem::new(crate::i18n::tr_static("Создать новую цепочку"))
                                .on_click(|_, _, cx| {
                                    match shell::chains::create_chain(
                                        app_services(cx),
                                        crate::i18n::tr_static("Новая цепочка"),
                                    ) {
                                        Ok(chain) => {
                                            navigation::notify_chains_changed(cx);
                                            navigation::request_open_chain(cx, Some(chain.id));
                                        }
                                        Err(error) => log::error!("create chain: {error:#}"),
                                    }
                                }),
                        ),
                        _ => menu,
                    }),
            );
        }
        let font_family = crate::ui_scale::left_font_family(cx);
        layout
            .text_size(px(crate::ui_scale::left_font_size(cx, 14.)))
            .when(!font_family.is_empty(), |root| {
                root.font_family(font_family)
            })
    }
}
