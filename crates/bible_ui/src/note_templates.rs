//! Editor for persisted note templates. Templates live next to the Markdown
//! vault, so they follow the user's notes instead of a particular machine.

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, TitleBar,
};
use gpui_kit::*;

use shell::notes::{NoteTemplate, NotesHandle};

use crate::services::app_services;

pub struct NoteTemplatesWindow {
    templates: Vec<NoteTemplate>,
    selected: Option<usize>,
    name: Entity<InputState>,
    title: Entity<InputState>,
    body: Entity<TextareaState>,
    folder: Entity<InputState>,
    status: Option<String>,
    notes: NotesHandle,
}

impl NoteTemplatesWindow {
    fn new(notes: NotesHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let templates = shell::notes::load_templates(&notes).unwrap_or_default();
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Название шаблона"));
        let title = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Шаблон названия, например {date} {title}")
        });
        let body = cx
            .new(|cx| TextareaState::new(window, cx).placeholder("Текст новой заметки в Markdown"));
        let folder =
            cx.new(|cx| InputState::new(window, cx).placeholder("Папка, например Дневник/2026"));
        let mut this = Self {
            templates,
            selected: None,
            name,
            title,
            body,
            folder,
            status: None,
            notes,
        };
        if !this.templates.is_empty() {
            this.select(0, window, cx);
        }
        this
    }

    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(template) = self.templates.get(index).cloned() else {
            return;
        };
        self.selected = Some(index);
        self.name
            .update(cx, |input, cx| input.set_value(template.name, window, cx));
        self.title.update(cx, |input, cx| {
            input.set_value(template.title_pattern, window, cx)
        });
        self.body.update(cx, |input, cx| {
            input.set_value(template.body_pattern, window, cx)
        });
        self.folder
            .update(cx, |input, cx| input.set_value(template.folder, window, cx));
        self.status = None;
        cx.notify();
    }

    fn new_template(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = None;
        let template = NoteTemplate::blank();
        self.name
            .update(cx, |input, cx| input.set_value(template.name, window, cx));
        self.title.update(cx, |input, cx| {
            input.set_value(template.title_pattern, window, cx)
        });
        self.body.update(cx, |input, cx| {
            input.set_value(template.body_pattern, window, cx)
        });
        self.folder
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.status = None;
        cx.notify();
    }

    fn append_input(
        input: &Entity<InputState>,
        token: &'static str,
        window: &mut Window,
        cx: &mut App,
    ) {
        input.update(cx, |input, cx| {
            let mut value = input.value().to_string();
            value.push_str(token);
            input.set_value(value, window, cx);
        });
    }

    fn append_textarea(
        input: &Entity<TextareaState>,
        token: &'static str,
        window: &mut Window,
        cx: &mut App,
    ) {
        input.update(cx, |input, cx| {
            let mut value = input.value().to_string();
            value.push_str(token);
            input.set_value(value, window, cx);
        });
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_owned();
        let title_pattern = self.title.read(cx).value().trim().to_owned();
        if name.is_empty() || title_pattern.is_empty() {
            self.status = Some("Укажите имя шаблона и шаблон названия.".to_owned());
            cx.notify();
            return;
        }
        let template = NoteTemplate {
            id: self
                .selected
                .and_then(|index| self.templates.get(index))
                .map(|template| template.id.clone())
                .unwrap_or_else(|| uuid::Uuid::now_v7().to_string()),
            name,
            title_pattern,
            body_pattern: self.body.read(cx).value().to_string(),
            folder: self.folder.read(cx).value().trim().to_owned(),
        };
        if let Some(index) = self.selected {
            self.templates[index] = template;
        } else {
            self.templates.push(template);
            self.selected = Some(self.templates.len() - 1);
        }
        match shell::notes::save_templates(&self.notes, &self.templates) {
            Ok(()) => {
                self.status = Some("Шаблоны сохранены.".to_owned());
                crate::navigation::notify_settings_changed(cx);
            }
            Err(error) => self.status = Some(format!("Не удалось сохранить: {error:#}")),
        }
        cx.notify();
    }

    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected else { return };
        self.templates.remove(index);
        if let Err(error) = shell::notes::save_templates(&self.notes, &self.templates) {
            self.status = Some(format!("Не удалось удалить: {error:#}"));
            cx.notify();
            return;
        }
        crate::navigation::notify_settings_changed(cx);
        if self.templates.is_empty() {
            self.new_template(window, cx);
        } else {
            self.select(index.min(self.templates.len() - 1), window, cx);
        }
    }

    fn title_token_row(
        &self,
        label: &'static str,
        input: Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut row = h_flex().gap_1().items_center().child(
            div()
                .w(px(100.))
                .text_color(cx.theme().muted_foreground)
                .child(label),
        );
        for token in ["{title}", "{date}", "{time}", "{datetime}"] {
            row = row.child(
                Button::new(SharedString::from(format!("{label}-{token}")))
                    .xsmall()
                    .ghost()
                    .label(token)
                    .on_click({
                        let input = input.clone();
                        move |_, window, cx| Self::append_input(&input, token, window, cx)
                    }),
            );
        }
        row.into_any_element()
    }

    fn body_token_row(
        &self,
        label: &'static str,
        input: Entity<TextareaState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut row = h_flex().gap_1().items_center().child(
            div()
                .w(px(100.))
                .text_color(cx.theme().muted_foreground)
                .child(label),
        );
        for token in ["{title}", "{date}", "{time}", "{datetime}"] {
            row = row.child(
                Button::new(SharedString::from(format!("{label}-{token}")))
                    .xsmall()
                    .ghost()
                    .label(token)
                    .on_click({
                        let input = input.clone();
                        move |_, window, cx| Self::append_textarea(&input, token, window, cx)
                    }),
            );
        }
        row.into_any_element()
    }
}

impl Render for NoteTemplatesWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let folders = shell::notes::list_folders(&self.notes).unwrap_or_default();
        let list = v_flex()
            .id("note-templates-list")
            .w(px(220.))
            .h_full()
            .p_2()
            .gap_1()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("template-new")
                    .small()
                    .label("+ Новый шаблон")
                    .on_click(cx.listener(|this, _, window, cx| this.new_template(window, cx))),
            )
            .children(self.templates.iter().enumerate().map(|(index, template)| {
                Button::new(SharedString::from(format!("template-{index}")))
                    .small()
                    .ghost()
                    .selected(self.selected == Some(index))
                    .label(template.name.clone())
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.select(index, window, cx)),
                    )
            }))
            .overflow_y_scrollbar();
        let folder_input = self.folder.clone();
        let form = v_flex()
            .id("note-template-form")
            .flex_1()
            .min_w_0()
            .h_full()
            .p_4()
            .gap_3()
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Имя шаблона"))
            .child(Input::new(&self.name))
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Название новой заметки"))
            .child(Input::new(&self.title))
            .child(self.title_token_row("В название", self.title.clone(), cx))
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Папка по умолчанию"))
            .child(Input::new(&self.folder))
            .child(
                Button::new("template-folder-picker")
                    .small()
                    .ghost()
                    .label("Выбрать папку…")
                    .dropdown_menu(move |mut menu, _, _| {
                        let input = folder_input.clone();
                        menu = menu.item(PopupMenuItem::new("Корень хранилища").on_click(
                            move |_, window, cx| {
                                input.update(cx, |input, cx| input.set_value("", window, cx));
                            },
                        ));
                        for folder in &folders {
                            let input = folder_input.clone();
                            let relpath = folder.relpath.clone();
                            menu = menu.item(PopupMenuItem::new(folder.relpath.clone()).on_click(
                                move |_, window, cx| {
                                    input.update(cx, |input, cx| {
                                        input.set_value(relpath.clone(), window, cx)
                                    });
                                },
                            ));
                        }
                        menu
                    }),
            )
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Текст заметки"))
            .child(Textarea::new(&self.body).h(px(190.)))
            .child(self.body_token_row("В текст", self.body.clone(), cx))
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("Дата: ГГГГ-ММ-ДД; время в имени использует ЧЧ-ММ, чтобы путь оставался допустимым."),
            )
            .children(self.status.clone().map(|status| div().child(status)))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("template-save")
                            .label("Сохранить")
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    )
                    .child(
                        Button::new("template-delete")
                            .danger()
                            .label("Удалить")
                            .disabled(self.selected.is_none())
                            .on_click(cx.listener(|this, _, window, cx| this.delete(window, cx))),
                    ),
            )
            .overflow_y_scrollbar();
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
                        .child("Шаблоны заметок"),
                ),
            )
            .child(h_flex().flex_1().min_h_0().child(list).child(form))
    }
}

pub fn open(cx: &mut App) {
    let Some(notes) = shell::notes::current(app_services(cx)) else {
        return;
    };
    if let Some(existing) = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<NoteTemplatesWindow>())
    {
        let _ = existing.update(cx, |_, window, _| window.activate_window());
        return;
    }
    cx.defer(move |cx| {
        let bounds = Bounds::centered(None, size(px(860.), px(650.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(700.), px(520.))),
            ..TitleBar::window_options()
        };
        if let Err(error) = gpui_kit::open_window(options, cx, move |window, cx| {
            cx.new(|cx| NoteTemplatesWindow::new(notes.clone(), window, cx))
        }) {
            log::error!("open note templates: {error:#}");
        }
    });
}
