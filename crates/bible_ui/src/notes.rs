//! Панель «Заметки» (architecture.md §6.3.4, этап 8).
//!
//! Слева — список заметок хранилища + создание новой; справа — открытая
//! заметка в двух режимах: Edit (component::input::Editor, line numbers,
//! многострочный) и Preview (TextView::markdown). Источник истины — файл:
//! сохранение debounce 500 мс после правки; внешнее изменение (watcher
//! notes_core, например правка в Obsidian) — чистый буфер перезагружается
//! молча, «грязный» получает баннер конфликта «[перезагрузить] [оставить
//! мои]» (решение №6 архдокумента: никаких слияний).
//!
//! События индекса приходят с потока watcher'а — дескрипторы GPUI там
//! трогать нельзя (!Send), поэтому callback складывает их в очередь
//! (Arc<Mutex>), а панель разбирает очередь поллингом раз в 300 мс через
//! spawn_in. Собственная запись тоже генерирует событие Updated(relpath) —
//! подавляется флагом ignore_next_update.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent};
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use shell::notes::{IndexEvent, LinkHit, LinkSourceRef, NotesHandle, NotesSubscription};

use crate::services::app_services;

/// Режим открытой заметки (переключатель в шапке).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoteMode {
    Edit,
    Preview,
}

/// Открытая заметка: буфер редактора + признаки состояния.
struct OpenNote {
    relpath: String,
    title: String,
    editor: Entity<EditorState>,
    mode: NoteMode,
    /// Есть несохранённые правки.
    dirty: bool,
    /// Файл изменился на диске, пока буфер «грязный» — показан баннер.
    external_conflict: bool,
    /// Подавить одно событие Updated(relpath) — след нашей записи.
    ignore_next_update: bool,
    saved_body: String,
    saving: bool,
    backlinks: Vec<LinkHit>,
    /// Подписка на правки буфера; живёт ровно столько, сколько заметка.
    _editor_sub: Subscription,
}

pub struct NotesPanel {
    focus_handle: FocusHandle,
    /// Список хранилища (relpath + заголовок); обновляется по событиям.
    list: Vec<shell::notes::NoteSummary>,
    new_note_input: Entity<InputState>,
    open: Option<OpenNote>,
    error: Option<String>,
    /// Очередь событий индекса с потока watcher'а; разбирается поллингом.
    /// Живёт весь срок панели: переподписка на другой индекс (смена
    /// notes_dir в настройках) использует ту же очередь.
    pending_events: Arc<Mutex<Vec<IndexEvent>>>,
    /// Индекс, на который оформлена подписка; по ptr_eq замечаем замену.
    subscribed_index: Option<NotesHandle>,
    _save_task: Option<Task<()>>,
    _index_token: Option<NotesSubscription>,
    _subscriptions: Vec<Subscription>,
}

impl NotesPanel {
    pub const NAME: &'static str = "notes";

    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let new_note_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(crate::i18n::tr_static("Папка/Название новой заметки"))
        });

        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(
            &new_note_input,
            window,
            |this, input, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let name = input.read(cx).value().to_string();
                    this.create_note(name, window, cx);
                }
            },
        ));

        // Хранилище заметок переинициализировано из настроек —
        // переподписаться на новый индекс.
        subscriptions.push(cx.subscribe(
            &crate::navigation::bus(cx),
            |this, _, _: &crate::navigation::SettingsChanged, cx| {
                this.on_settings_changed(cx);
            },
        ));

        // События индекса (внешние правки файлов, переиндексация) приходят
        // с потока watcher'а или синхронно из save_note. Callback обязан
        // быть Send + Sync, а дескрипторы GPUI (WeakEntity, AsyncWindowContext)
        // таковыми не являются — поэтому callback лишь складывает события
        // в очередь, а разбирает её поллинг на UI-потоке (spawn_in ниже).
        let pending_events: Arc<Mutex<Vec<IndexEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let pending_for_loop = Arc::clone(&pending_events);

        // Поллинг очереди событий индекса: раз в 300 мс забираем пачку и
        // обрабатываем на UI-потоке. Цикл завершается, когда панель умерла
        // (update_in возвращает Err).
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
                    .update_in(window, |this, window, cx| {
                        for event in events {
                            this.on_index_event(event, window, cx);
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();

        let mut this = Self {
            focus_handle: cx.focus_handle(),
            list: Vec::new(),
            new_note_input,
            open: None,
            error: None,
            pending_events,
            subscribed_index: None,
            _save_task: None,
            _index_token: None,
            _subscriptions: subscriptions,
        };
        this.subscribe_index(cx);
        this.reload_list(cx);
        this
    }

    fn notes_index(cx: &App) -> Option<NotesHandle> {
        shell::notes::current(app_services(cx))
    }

    /// Подписаться на текущий индекс заметок (старт и смена хранилища).
    fn subscribe_index(&mut self, cx: &mut Context<Self>) {
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
    /// индекса) — переподписаться и закрыть открытую заметку (она из
    /// прежнего хранилища; несохранённый буфер отбрасывается).
    fn on_settings_changed(&mut self, cx: &mut Context<Self>) {
        let current = Self::notes_index(cx);
        let changed = match (&self.subscribed_index, &current) {
            (None, None) => false,
            (Some(old), Some(new)) => !old.same_storage(new),
            _ => true,
        };
        if changed {
            self.subscribe_index(cx);
            self.open = None;
            // События прежнего индекса в очереди неактуальны.
            self.pending_events.lock().unwrap().clear();
        }
        self.reload_list(cx);
    }

    fn reload_list(&mut self, cx: &mut Context<Self>) {
        let Some(index) = Self::notes_index(cx) else {
            self.list = Vec::new();
            return;
        };
        match shell::notes::list_notes(&index) {
            Ok(list) => self.list = list,
            Err(error) => self.error = Some(format!("{error:#}")),
        }
        cx.notify();
    }

    /// Сохранить открытую заметку, если «грязная» (перед переключением).
    fn save_if_dirty(&mut self, cx: &mut Context<Self>) {
        if self.open.as_ref().is_some_and(|open| open.dirty) {
            self.save_now(false, cx);
        }
    }

    /// Немедленная запись открытой заметки на диск (фон, UI не ждёт).
    fn save_now(&mut self, force: bool, cx: &mut Context<Self>) {
        let Some(open) = &mut self.open else { return };
        if open.saving || (!force && (!open.dirty || open.external_conflict)) {
            return;
        }
        let Some(index) = Self::notes_index(cx) else {
            return;
        };
        let relpath = open.relpath.clone();
        let editor = open.editor.clone();
        let body = editor.read(cx).value().to_string();
        let expected = open.saved_body.clone();
        open.saving = true;
        open.ignore_next_update = true;
        cx.spawn(async move |this, cx| {
            let saved_body = body.clone();
            let result = cx.background_executor().spawn(async move {
                if force {
                    shell::notes::save_note(&index, &relpath, &body).map(|_| true)
                } else {
                    shell::notes::save_note_if_current(&index, &relpath, &expected, &body)
                }
            }).await;
            _ = this.update(cx, |this, cx| {
                let Some(open) = this.open.as_mut().filter(|open| open.editor == editor) else {
                    if !matches!(result, Ok(true)) {
                        this.error = Some("Не удалось сохранить предыдущую заметку. Откройте её, чтобы проверить изменения.".to_owned());
                        cx.notify();
                    }
                    return;
                };
                open.saving = false;
                match result {
                    Ok(true) => {
                        open.saved_body = saved_body;
                        open.dirty = editor.read(cx).value().as_ref() != open.saved_body.as_str();
                        if open.dirty {
                            this.schedule_save(cx);
                        }
                    }
                    Ok(false) => {
                        open.ignore_next_update = false;
                        open.external_conflict = true;
                    }
                    Err(error) => {
                        open.ignore_next_update = false;
                        this.error = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    /// Debounce-сохранение через 500 мс после последней правки (§6.3.4).
    /// Окно не нужно: save_now пишет фоном.
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self._save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            _ = this.update(cx, |this, cx| this.save_now(false, cx));
        }));
    }

    /// Открыть заметку из списка.
    fn open_note(&mut self, relpath: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .open
            .as_ref()
            .is_some_and(|open| open.relpath == relpath)
        {
            return;
        }
        self.save_if_dirty(cx);
        let Some(index) = Self::notes_index(cx) else {
            return;
        };
        match shell::notes::open_note(&index, relpath) {
            Ok(doc) => {
                let editor = cx.new(|cx| EditorState::new(window, cx).language("markdown"));
                editor.update(cx, |editor, cx| {
                    editor.set_value(doc.body.clone(), window, cx);
                });
                // Правки буфера: «грязный» + debounce-запись (500 мс, §6.3.4).
                let editor_sub = cx.subscribe(&editor, |this, _, event: &InputEvent, cx| {
                    if let InputEvent::Change = event {
                        if let Some(open) = &mut this.open {
                            open.dirty = true;
                        }
                        this.schedule_save(cx);
                    }
                });
                let backlinks =
                    shell::notes::note_backlinks(&index, &doc.relpath).unwrap_or_default();
                self.open = Some(OpenNote {
                    relpath: doc.relpath,
                    title: doc.title,
                    editor,
                    mode: NoteMode::Edit,
                    dirty: false,
                    external_conflict: false,
                    ignore_next_update: false,
                    saved_body: doc.body,
                    saving: false,
                    backlinks,
                    _editor_sub: editor_sub,
                });
                self.error = None;
            }
            Err(error) => self.error = Some(format!("{error:#}")),
        }
        cx.notify();
    }

    /// Создать заметку и открыть её.
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
                self.reload_list(cx);
                self.open_note(&doc.relpath, window, cx);
            }
            Err(error) => self.error = Some(format!("{error:#}")),
        }
        cx.notify();
    }

    /// Перезагрузить открытую заметку с диска (внешнее изменение, чистый
    /// буфер, либо кнопка «перезагрузить» на баннере конфликта).
    fn reload_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = &self.open else {
            return;
        };
        let relpath = open.relpath.clone();
        self.open = None; // форсируем полное переоткрытие
        self.open_note(&relpath, window, cx);
    }

    /// События индекса: обновление списка + реакция открытой заметки.
    fn on_index_event(&mut self, event: IndexEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.reload_list(cx);
        let (relpaths, is_delete) = match &event {
            IndexEvent::Updated { relpaths } => (relpaths, false),
            IndexEvent::Deleted { relpaths } => (relpaths, true),
        };
        if let Some(open) = &mut self.open {
            if relpaths.contains(&open.relpath) {
                if open.ignore_next_update {
                    open.ignore_next_update = false;
                } else if is_delete || open.dirty {
                    // Конфликт: файл изменился на диске поверх наших правок
                    // (или вовсе удалён) — баннер, никаких слияний (решение №6).
                    open.external_conflict = true;
                } else if let Some(index) = Self::notes_index(cx) {
                    // Чистый буфер — молча перечитать файл. set_value не
                    // эмитит Change, поэтому dirty не взводится.
                    if let Ok(doc) = shell::notes::open_note(&index, &open.relpath) {
                        open.title = doc.title;
                        open.saved_body = doc.body.clone();
                        let editor = open.editor.clone();
                        editor.update(cx, |editor, cx| {
                            editor.set_value(doc.body, window, cx);
                        });
                    }
                }
            }
        }
        // Обратные ссылки могли измениться от любой правки хранилища.
        if let Some(open) = &mut self.open {
            if let Some(index) = Self::notes_index(cx) {
                open.backlinks =
                    shell::notes::note_backlinks(&index, &open.relpath).unwrap_or_default();
            }
        }
        cx.notify();
    }

    fn render_note_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let mut list = v_flex().w_full().gap_1();
        if self.list.is_empty() {
            list = list.child(
                div()
                    .p_2()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                    .text_color(theme.muted_foreground)
                    .child(crate::i18n::tr_static(
                        "Заметок пока нет — создайте первую.",
                    )),
            );
        }
        for (index, summary) in self.list.iter().enumerate() {
            let active = self
                .open
                .as_ref()
                .is_some_and(|open| open.relpath == summary.relpath);
            let relpath = summary.relpath.clone();
            list = list.child(
                div()
                    .id(ElementId::Name(format!("note-{}", summary.relpath).into()))
                    .w_full()
                    .min_w_0()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(crate::ui_scale::row_background(index, cx))
                    .cursor_pointer()
                    .when(active, |style| style.bg(theme.accent.opacity(0.18)))
                    .when(!active, |style| {
                        style.hover(|style| style.bg(theme.accent.opacity(0.12)))
                    })
                    .child(
                        div()
                            .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                            .text_color(theme.foreground)
                            .child(summary.title.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                            .text_color(theme.muted_foreground)
                            .child(summary.relpath.clone()),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_note(&relpath, window, cx);
                    })),
            );
        }
        v_flex()
            .id("notes-list-scroll")
            .size_full()
            .min_h_0()
            .p_2()
            .child(list)
            .overflow_y_scrollbar()
            .into_any_element()
    }

    fn render_backlinks(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(open) = &self.open else {
            return div().into_any_element();
        };
        let mut list = v_flex().w_full().gap_1();
        if open.backlinks.is_empty() {
            list = list.child(
                div()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                    .text_color(theme.muted_foreground)
                    .child(crate::i18n::tr_static("Обратных ссылок нет.")),
            );
        }
        for hit in &open.backlinks {
            let LinkSourceRef::Note { relpath, title, .. } = &hit.source else {
                continue;
            };
            let relpath = relpath.clone();
            let title = title.clone();
            let snippet = hit.snippet.clone();
            list = list.child(
                div()
                    .id(ElementId::Name(
                        format!("backlink-{}-{}", relpath, hit.byte_start).into(),
                    ))
                    .w_full()
                    .min_w_0()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.accent.opacity(0.12)))
                    .child(
                        div()
                            .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                            .font_weight(FontWeight::MEDIUM)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                            .text_color(theme.muted_foreground)
                            .child(snippet),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_note(&relpath, window, cx);
                    })),
            );
        }
        v_flex()
            .w_full()
            .border_t_1()
            .border_color(theme.border)
            .p_2()
            .child(
                div()
                    .pb_1()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child(crate::i18n::tr_static("Обратные ссылки")),
            )
            .child(list)
            .into_any_element()
    }

    fn render_open_note(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(open) = &self.open else {
            return div()
                .size_full()
                .p_4()
                .text_color(theme.muted_foreground)
                .child(crate::i18n::tr_static(
                    "Выберите заметку слева или создайте новую.",
                ))
                .into_any_element();
        };

        // Шапка: имя + переключатель режима + состояние сохранения.
        let status = if open.dirty {
            crate::i18n::tr_static("не сохранено…")
        } else {
            crate::i18n::tr_static("сохранено")
        };
        let header = h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .p_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(open.title.clone()),
            )
            .child(
                Button::new("note-mode")
                    .xsmall()
                    .label(match open.mode {
                        NoteMode::Edit => crate::i18n::tr_static("Просмотр"),
                        NoteMode::Preview => crate::i18n::tr_static("Правка"),
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(open) = &mut this.open {
                            open.mode = match open.mode {
                                NoteMode::Edit => NoteMode::Preview,
                                NoteMode::Preview => NoteMode::Edit,
                            };
                        }
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                    .text_color(theme.muted_foreground)
                    .child(status),
            );

        // Баннер конфликта: файл изменён на диске поверх наших правок.
        let conflict = open.external_conflict.then(|| {
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .p_2()
                .bg(theme.danger.opacity(0.15))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                        .child(crate::i18n::tr_static(
                            "Файл изменён на диске (внешняя правка).",
                        )),
                )
                .child(
                    Button::new("note-reload")
                        .xsmall()
                        .label(crate::i18n::tr_static("Перезагрузить"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.reload_open(window, cx);
                        })),
                )
                .child(
                    Button::new("note-keep-mine")
                        .xsmall()
                        .label(crate::i18n::tr_static("Оставить мои"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(open) = &mut this.open {
                                open.external_conflict = false;
                            }
                            this.save_now(true, cx);
                        })),
                )
        });

        let body: AnyElement = match open.mode {
            NoteMode::Edit => v_flex()
                .size_full()
                .child(crate::note_markup::toolbar(&open.editor, &open.relpath, cx))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .child(Editor::new(&open.editor).h(relative(1.)).bordered(false)),
                )
                .into_any_element(),
            NoteMode::Preview => {
                let markdown = open.editor.read(cx).value().to_string();
                crate::note_markup::preview("note-preview", &markdown, &open.relpath, cx)
                    .into_any_element()
            }
        };

        v_flex()
            .size_full()
            .min_h_0()
            .child(header)
            .when_some(conflict, |this, banner| this.child(banner))
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_backlinks(cx))
            .into_any_element()
    }
}

impl EventEmitter<PanelEvent> for NotesPanel {}

impl Focusable for NotesPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for NotesPanel {
    fn panel_name(&self) -> &'static str {
        Self::NAME
    }
}

impl Panel for NotesPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        crate::i18n::tr_static("Заметки")
    }
}

impl Render for NotesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Цвета извлекаем сразу: ссылка на тему держит заимствование cx,
        // а ниже нужны &mut-вызовы (render_note_list / render_open_note).
        let (muted_foreground, border, danger) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.border, theme.danger)
        };
        if Self::notes_index(cx).is_none() {
            let font_family = crate::ui_scale::panel_font_family(cx);
            return v_flex()
                .size_full()
                .p_4()
                .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                .when(!font_family.is_empty(), |root| {
                    root.font_family(font_family)
                })
                .child(
                    div()
                        .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                        .text_color(muted_foreground)
                        .child(crate::i18n::tr_static(
                            "Заметки отключены: укажите notes_dir в settings.json.",
                        )),
                );
        }

        let mut left =
            v_flex()
                .h_full()
                .w(px(240.))
                .flex_shrink_0()
                .border_r_1()
                .border_color(border)
                .child(
                    h_flex()
                        .w_full()
                        .gap_1()
                        .p_2()
                        .border_b_1()
                        .border_color(border)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(&self.new_note_input)),
                        )
                        .child(Button::new("note-create").xsmall().label("+").on_click(
                            cx.listener(|this, _, window, cx| {
                                let name = this.new_note_input.read(cx).value().to_string();
                                this.create_note(name, window, cx);
                            }),
                        )),
                )
                .child(div().flex_1().min_h_0().child(self.render_note_list(cx)));
        if let Some(error) = &self.error {
            left = left.child(
                div()
                    .p_2()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                    .text_color(danger)
                    .child(error.clone()),
            );
        }

        let font_family = crate::ui_scale::panel_font_family(cx);
        h_flex()
            .size_full()
            .min_w_0()
            .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
            .when(!font_family.is_empty(), |root| {
                root.font_family(font_family)
            })
            .child(left)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.render_open_note(cx)),
            )
    }
}
