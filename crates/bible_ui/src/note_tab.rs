//! Центральная вкладка заметки (этап 15.2, polish-plan.md): выбранная в
//! менюшке заметка открывается вкладкой в центре, как в форке.
//!
//! Механика перенесена из панели «Заметки» (notes.rs), минус список
//! хранилища (он остался в менюшке): редактор (component::input::Editor,
//! markdown) / предпросмотр (TextView::markdown), debounce-запись 500 мс,
//! внешние изменения файла (watcher notes_core) — чистый буфер
//! перезагружается молча, «грязный» получает баннер конфликта
//! «[перезагрузить] [оставить мои]» (решение №6 архдокумента).
//!
//! События индекса приходят с потока watcher'а — дескрипторы GPUI там
//! трогать нельзя (!Send), поэтому callback складывает их в очередь
//! (Arc<Mutex>), а вкладка разбирает очередь поллингом раз в 300 мс.
//!
//! Персистентность (§7.2): payload `NoteTabPayload { relpath }` в
//! layout.json, восстановление — `new_restored` через PanelRegistry.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent, PanelInfo, PanelState};
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use shell::notes::{IndexEvent, LinkHit, LinkSourceRef, NotesHandle, NotesSubscription};

use crate::services::app_services;

/// Режим вкладки (переключатель в шапке).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoteMode {
    Edit,
    Preview,
}

/// Состояние вкладки в payload layout.json (§7.2): путь внутри хранилища
/// заметок — этого достаточно, чтобы перечитать файл при восстановлении.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NoteTabPayload {
    /// Версия схемы payload — мягкое чтение старых записей.
    pub v: u32,
    pub relpath: String,
}

pub struct NoteTab {
    focus_handle: FocusHandle,
    relpath: String,
    title: String,
    editor: Option<Entity<EditorState>>,
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
    /// Хранилище заметок отключено (notes_dir не задан) или файла нет.
    error: Option<String>,
    /// Очередь событий индекса с потока watcher'а; разбирается поллингом.
    pending_events: Arc<Mutex<Vec<IndexEvent>>>,
    /// Индекс, на который оформлена подписка; по ptr_eq замечаем замену.
    subscribed_index: Option<NotesHandle>,
    _save_task: Option<Task<()>>,
    _index_token: Option<NotesSubscription>,
    _editor_sub: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl NoteTab {
    pub const NAME: &'static str = "note";

    /// Открыть заметку хранилища (клик в менюшке, ссылка в статье).
    pub fn open(window: &mut Window, cx: &mut Context<Self>, relpath: String) -> Self {
        let mut this = Self::new(window, cx);
        this.relpath = relpath;
        this.load(window, cx);
        this
    }

    /// Вкладка из payload layout.json (§7.2).
    pub fn new_restored(
        window: &mut Window,
        cx: &mut Context<Self>,
        payload: NoteTabPayload,
    ) -> Self {
        Self::open(window, cx, payload.relpath)
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        // Хранилище заметок переинициализировано из настроек — вкладка
        // из прежнего хранилища закрывается (буфер отбрасывается).
        subscriptions.push(cx.subscribe(
            &crate::navigation::bus(cx),
            |this, _, _: &crate::navigation::SettingsChanged, cx| {
                this.on_settings_changed(cx);
            },
        ));

        // События индекса (внешние правки файлов) приходят с потока
        // watcher'а или синхронно из save_now. Callback обязан быть
        // Send + Sync, а дескрипторы GPUI таковыми не являются — поэтому
        // callback лишь складывает события в очередь, а разбирает её
        // поллинг на UI-потоке (spawn_in ниже).
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
            relpath: String::new(),
            title: "Заметка".to_string(),
            editor: None,
            mode: NoteMode::Edit,
            dirty: false,
            external_conflict: false,
            ignore_next_update: false,
            saved_body: String::new(),
            saving: false,
            backlinks: Vec::new(),
            error: None,
            pending_events,
            subscribed_index: None,
            _save_task: None,
            _index_token: None,
            _editor_sub: None,
            _subscriptions: subscriptions,
        };
        this.subscribe_index(cx);
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

    /// Настройки изменились: если хранилище заменили (другой Arc индекса)
    /// — переподписаться; вкладка из прежнего хранилища показывает заглушку.
    fn on_settings_changed(&mut self, cx: &mut Context<Self>) {
        let current = Self::notes_index(cx);
        let changed = match (&self.subscribed_index, &current) {
            (None, None) => false,
            (Some(old), Some(new)) => !old.same_storage(new),
            _ => true,
        };
        if changed {
            self.subscribe_index(cx);
            self.pending_events.lock().unwrap().clear();
            self.editor = None;
            self.error =
                Some("Хранилище заметок сменилось — вкладка относится к прежнему.".to_string());
        }
        cx.notify();
    }

    /// Прочитать файл заметки в редактор (открытие и восстановление —
    /// одним путём).
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = Self::notes_index(cx) else {
            self.error = Some("Заметки отключены: укажите notes_dir в настройках.".to_string());
            return;
        };
        match shell::notes::open_note(&index, &self.relpath) {
            Ok(doc) => {
                let editor = cx.new(|cx| EditorState::new(window, cx).language("markdown"));
                editor.update(cx, |editor, cx| {
                    editor.set_value(doc.body.clone(), window, cx);
                });
                // Правки буфера: «грязный» + debounce-запись (500 мс, §6.3.4).
                let editor_sub = cx.subscribe(&editor, |this, _, event: &InputEvent, cx| {
                    if let InputEvent::Change = event {
                        this.dirty = true;
                        this.schedule_save(cx);
                    }
                });
                self.backlinks =
                    shell::notes::note_backlinks(&index, &doc.relpath).unwrap_or_default();
                self.title = doc.title;
                self.saved_body = doc.body;
                self.editor = Some(editor);
                self._editor_sub = Some(editor_sub);
                self.dirty = false;
                self.external_conflict = false;
                self.error = None;
            }
            Err(error) => self.error = Some(format!("{error:#}")),
        }
        cx.notify();
    }

    /// Немедленная запись буфера на диск (фон, UI не ждёт).
    fn save_now(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.saving || (!force && (!self.dirty || self.external_conflict)) {
            return;
        }
        let Some(editor) = &self.editor else { return };
        let Some(index) = Self::notes_index(cx) else {
            return;
        };
        let editor = editor.clone();
        let relpath = self.relpath.clone();
        let body = editor.read(cx).value().to_string();
        let expected = self.saved_body.clone();
        self.saving = true;
        self.ignore_next_update = true;
        cx.spawn(async move |this, cx| {
            let saved_body = body.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    if force {
                        shell::notes::save_note(&index, &relpath, &body).map(|_| true)
                    } else {
                        shell::notes::save_note_if_current(&index, &relpath, &expected, &body)
                    }
                })
                .await;
            _ = this.update(cx, |this, cx| {
                this.saving = false;
                if this.editor.as_ref() != Some(&editor) {
                    return;
                }
                match result {
                    Ok(true) => {
                        this.saved_body = saved_body;
                        this.dirty = editor.read(cx).value().as_ref() != this.saved_body.as_str();
                        if this.dirty {
                            this.schedule_save(cx);
                        }
                    }
                    Ok(false) => {
                        this.ignore_next_update = false;
                        this.external_conflict = true;
                    }
                    Err(error) => {
                        this.ignore_next_update = false;
                        this.error = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Debounce-сохранение через 500 мс после последней правки (§6.3.4).
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self._save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            _ = this.update(cx, |this, cx| this.save_now(false, cx));
        }));
    }

    /// Перезагрузить заметку с диска (кнопка «перезагрузить» на баннере).
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor = None; // форсируем полное переоткрытие
        self.load(window, cx);
    }

    /// События индекса: реакция на внешние правки ЭТОЙ заметки.
    fn on_index_event(&mut self, event: IndexEvent, window: &mut Window, cx: &mut Context<Self>) {
        let (relpaths, is_delete) = match &event {
            IndexEvent::Updated { relpaths } => (relpaths, false),
            IndexEvent::Deleted { relpaths } => (relpaths, true),
        };
        if !relpaths.contains(&self.relpath) {
            return;
        }
        if self.ignore_next_update {
            self.ignore_next_update = false;
        } else if is_delete || self.dirty {
            // Конфликт: файл изменился на диске поверх наших правок (или
            // вовсе удалён) — баннер, никаких слияний (решение №6).
            self.external_conflict = true;
        } else if let (Some(index), Some(editor)) = (Self::notes_index(cx), self.editor.clone()) {
            // Чистый буфер — молча перечитать файл. set_value не эмитит
            // Change, поэтому dirty не взводится.
            if let Ok(doc) = shell::notes::open_note(&index, &self.relpath) {
                self.title = doc.title;
                self.saved_body = doc.body.clone();
                editor.update(cx, |editor, cx| {
                    editor.set_value(doc.body, window, cx);
                });
            }
        }
        // Обратные ссылки могли измениться от любой правки хранилища.
        if let Some(index) = Self::notes_index(cx) {
            self.backlinks =
                shell::notes::note_backlinks(&index, &self.relpath).unwrap_or_default();
        }
        cx.notify();
    }

    fn render_backlinks(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let mut list = v_flex().w_full().gap_1();
        if self.backlinks.is_empty() {
            list = list.child(
                div()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                    .text_color(theme.muted_foreground)
                    .child(crate::i18n::tr_static("Обратных ссылок нет.")),
            );
        }
        for hit in &self.backlinks {
            let LinkSourceRef::Note { relpath, title, .. } = &hit.source else {
                continue;
            };
            let relpath = relpath.clone();
            let title = title.clone();
            let snippet = hit.snippet.clone();
            list = list.child(
                div()
                    .id(ElementId::Name(
                        format!("note-backlink-{}-{}", relpath, hit.byte_start).into(),
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
                    .on_click(move |_, _, cx| {
                        crate::navigation::request_open_note(cx, relpath.clone());
                    }),
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
}

impl EventEmitter<PanelEvent> for NoteTab {}

impl Focusable for NoteTab {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for NoteTab {
    fn panel_name(&self) -> &'static str {
        Self::NAME
    }

    /// Персистентное состояние вкладки (§7.2): relpath заметки.
    fn dump(&self, _: &App) -> PanelState {
        let payload = NoteTabPayload {
            v: 1,
            relpath: self.relpath.clone(),
        };
        let mut state = PanelState::new(Self::NAME);
        state.info = PanelInfo::panel(serde_json::to_value(payload).unwrap_or_default());
        state
    }

    /// Вкладка закрывается — сохранить «грязный» буфер (та же политика,
    /// что у панели «Заметки» при переключении заметок).
    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.dirty {
            self.save_now(false, cx);
        }
    }
}

impl Panel for NoteTab {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.title.clone()
    }
}

impl Render for NoteTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(error) = &self.error {
            return v_flex().size_full().p_4().child(
                div()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        let Some(editor) = &self.editor else {
            return v_flex().size_full().p_4().child(
                div()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                    .text_color(cx.theme().muted_foreground)
                    .child(crate::i18n::tr_static("Загрузка…")),
            );
        };

        let theme = cx.theme();
        let status = if self.dirty {
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
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                    .text_color(theme.muted_foreground)
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(self.relpath.clone()),
            )
            .child(
                Button::new("note-tab-mode")
                    .xsmall()
                    .label(match self.mode {
                        NoteMode::Edit => crate::i18n::tr_static("Просмотр"),
                        NoteMode::Preview => crate::i18n::tr_static("Правка"),
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.mode = match this.mode {
                            NoteMode::Edit => NoteMode::Preview,
                            NoteMode::Preview => NoteMode::Edit,
                        };
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
        let conflict = self.external_conflict.then(|| {
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
                    Button::new("note-tab-reload")
                        .xsmall()
                        .label(crate::i18n::tr_static("Перезагрузить"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.reload(window, cx);
                        })),
                )
                .child(
                    Button::new("note-tab-keep-mine")
                        .xsmall()
                        .label(crate::i18n::tr_static("Оставить мои"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.external_conflict = false;
                            this.save_now(true, cx);
                        })),
                )
        });

        let body: AnyElement = match self.mode {
            NoteMode::Edit => v_flex()
                .size_full()
                .child(crate::note_markup::toolbar(editor, &self.relpath, cx))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .child(Editor::new(editor).h(relative(1.)).bordered(false)),
                )
                .into_any_element(),
            NoteMode::Preview => {
                let markdown = editor.read(cx).value().to_string();
                crate::note_markup::preview("note-tab-preview", &markdown, &self.relpath, cx)
                    .into_any_element()
            }
        };

        let font_family = crate::ui_scale::panel_font_family(cx);
        v_flex()
            .size_full()
            .min_h_0()
            .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
            .when(!font_family.is_empty(), |root| {
                root.font_family(font_family)
            })
            .child(header)
            .when_some(conflict, |this, banner| this.child(banner))
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_backlinks(cx))
    }
}
