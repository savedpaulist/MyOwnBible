use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    input::{Textarea, TextareaState},
    scroll::ScrollableElement as _,
    switch::Switch,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use shell::questions::{QuestionSummary, QuestionVerse};

use crate::{services::app_services, verse_picker, verse_preview};

struct Draft {
    id: String,
    version: Option<serde_json::Value>,
    created: Option<String>,
    answered: Option<String>,
    answered_at: Option<i64>,
    links: Vec<QuestionVerse>,
    editing_comment: Option<usize>,
    editing_question: bool,
    editing_answer: bool,
}

pub(crate) struct QuestionsPanel {
    available: bool,
    questions: Vec<QuestionSummary>,
    draft: Option<Draft>,
    question: Entity<TextareaState>,
    answer: Entity<TextareaState>,
    comment: Entity<TextareaState>,
    error: Option<String>,
    _sync: Subscription,
}

impl QuestionsPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let services = app_services(cx);
        let available = shell::questions::available(services);
        let (questions, error) = match shell::questions::list(services) {
            Ok(questions) => (questions, None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
        let subscription = cx.subscribe(
            &crate::navigation::bus(cx),
            |this, _, _: &crate::navigation::UserDataSynced, cx| {
                match shell::questions::list(app_services(cx)) {
                    Ok(questions) => this.questions = questions,
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            },
        );
        Self {
            _sync: subscription,
            available,
            questions,
            draft: None,
            error,
            question: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(1, usize::MAX)
                    .placeholder(crate::i18n::tr_static("Введите вопрос"))
            }),
            answer: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(1, usize::MAX)
                    .placeholder(crate::i18n::tr_static("Введите ответ"))
            }),
            comment: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(1, usize::MAX)
                    .placeholder(crate::i18n::tr_static("Комментарий к стиху"))
            }),
        }
    }

    fn open(
        &mut self,
        id: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let existing = id
            .map(|id| {
                shell::questions::get(app_services(cx), id)?
                    .ok_or_else(|| anyhow::anyhow!("Вопрос не найден."))
            })
            .transpose()?;
        let (question, answer, draft) = if let Some(existing) = existing {
            let summary = existing.summary;
            (
                summary.question,
                existing.answer,
                Draft {
                    id: summary.id,
                    version: Some(existing.version),
                    created: Some(summary.created),
                    answered_at: existing.answered_at,
                    answered: summary.answered,
                    links: existing.links,
                    editing_comment: None,
                    editing_question: false,
                    editing_answer: true,
                },
            )
        } else {
            (
                String::new(),
                String::new(),
                Draft {
                    id: bible_domain::chains::new_id(),
                    version: None,
                    created: None,
                    answered: None,
                    answered_at: None,
                    links: Vec::new(),
                    editing_comment: None,
                    editing_question: true,
                    editing_answer: false,
                },
            )
        };
        self.question
            .update(cx, |input, cx| input.set_value(question, window, cx));
        self.answer
            .update(cx, |input, cx| input.set_value(answer, window, cx));
        if draft.editing_question {
            self.question.read(cx).focus_handle(cx).focus(window, cx);
        }
        self.draft = Some(draft);
        self.error = None;
        cx.notify();
        Ok(())
    }

    fn save(&mut self, cx: &mut Context<Self>) -> anyhow::Result<()> {
        self.finish_comment(cx);
        let Some(draft) = &self.draft else {
            return Ok(());
        };
        shell::questions::save_if_current(
            app_services(cx),
            &draft.id,
            &self.question.read(cx).value(),
            &self.answer.read(cx).value(),
            draft.answered_at,
            &draft.links,
            draft.version.as_ref(),
        )?;
        self.questions = shell::questions::list(app_services(cx))?;
        self.draft = None;
        self.error = None;
        cx.notify();
        Ok(())
    }

    fn finish_comment(&mut self, cx: &App) {
        if let Some(draft) = &mut self.draft
            && let Some(index) = draft.editing_comment.take()
            && let Some(link) = draft.links.get_mut(index)
        {
            link.comment = self.comment.read(cx).value().trim().to_owned();
        }
    }

    fn edit_comment(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_comment(cx);
        if let Some(draft) = &mut self.draft {
            let value = draft.links[index].comment.clone();
            draft.editing_comment = Some(index);
            self.comment.update(cx, |input, cx| {
                input.set_value(value, window, cx);
                input.focus_handle(cx).focus(window, cx);
            });
            cx.notify();
        }
    }

    fn add_verse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_comment(cx);
        let Some(draft) = &self.draft else { return };
        let id = draft.id.clone();
        let owner = cx.weak_entity();
        verse_picker::open(
            "Добавить ссылку на стих",
            None,
            move |picked, _, cx| {
                owner.update(cx, |this, cx| {
                    let Some(draft) = this.draft.as_mut().filter(|draft| draft.id == id) else {
                        anyhow::bail!("Этот вопрос уже закрыт.");
                    };
                    draft.links.push(QuestionVerse {
                        module: picked.module,
                        reference: picked.reference,
                        comment: String::new(),
                    });
                    cx.notify();
                    Ok(())
                })?
            },
            window,
            cx,
        );
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let hover = cx.theme().accent.opacity(0.12);
        v_flex()
            .gap_2()
            .child(
                Button::new("new-question")
                    .small()
                    .label(crate::i18n::tr_static("+ Новый вопрос"))
                    .disabled(!self.available)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Err(error) = this.open(None, window, cx) {
                            this.error = Some(error.to_string());
                            cx.notify();
                        }
                    })),
            )
            .children(
                (self.questions.is_empty() && self.error.is_none()).then(|| {
                    div()
                        .text_size(px(crate::ui_scale::left_font_size(cx, 14.)))
                        .text_color(muted)
                        .child(crate::i18n::tr_static("Вопросов пока нет."))
                }),
            )
            .children(self.questions.iter().enumerate().map(|(index, summary)| {
                let id = summary.id.clone();
                v_flex()
                    .id(("study-question", index))
                    .p_2()
                    .rounded_md()
                    .bg(crate::ui_scale::row_background(index, cx))
                    .gap_1()
                    .cursor_pointer()
                    .hover(|style| style.bg(hover))
                    .child(
                        div()
                            .text_size(px(crate::ui_scale::left_font_size(cx, 14.)))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if summary.answered.is_some() {
                                cx.theme().success
                            } else {
                                cx.theme().foreground
                            })
                            .child(summary.question.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(crate::ui_scale::left_font_size(cx, 12.)))
                            .text_color(muted)
                            .child(summary.created.clone()),
                    )
                    .children(summary.answered.as_ref().map(|date| {
                        div()
                            .text_size(px(crate::ui_scale::left_font_size(cx, 12.)))
                            .text_color(cx.theme().success)
                            .child(format!("Ответ получен {date}"))
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Err(error) = this.open(Some(&id), window, cx) {
                            this.error = Some(error.to_string());
                            cx.notify();
                        }
                    }))
            }))
            .into_any_element()
    }

    fn render_field(&self, question: bool, editing: bool, cx: &mut Context<Self>) -> AnyElement {
        let (id, label, input) = if question {
            ("question", "Вопрос", &self.question)
        } else {
            ("answer", "Ответ", &self.answer)
        };
        let input_to_focus = input.clone();
        let value = input.read(cx).value();
        let color = if question
            && self
                .draft
                .as_ref()
                .is_some_and(|draft| draft.answered_at.is_some())
        {
            cx.theme().success
        } else {
            cx.theme().foreground
        };
        v_flex()
            .w_full()
            .flex_shrink_0()
            .gap_1()
            .child(
                h_flex()
                    .justify_between()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(label))
                    .child(
                        Button::new(id)
                            .xsmall()
                            .ghost()
                            .when(editing, |button| {
                                button.label(crate::i18n::tr_static("Готово"))
                            })
                            .when(!editing, |button| {
                                button
                                    .icon(IconName::Pencil)
                                    .tooltip(crate::i18n::tr_static("Редактировать"))
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(draft) = &mut this.draft {
                                    if question {
                                        draft.editing_question = !editing;
                                    } else {
                                        draft.editing_answer = !editing;
                                    }
                                }
                                if !editing {
                                    input_to_focus.read(cx).focus_handle(cx).focus(window, cx);
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(if editing {
                Textarea::new(input).flex_shrink_0().into_any_element()
            } else {
                div()
                    .min_w_0()
                    .whitespace_normal()
                    .text_color(color)
                    .text_size(px(crate::ui_scale::left_font_size(cx, 14.)))
                    .child(if value.is_empty() {
                        "—".into()
                    } else {
                        value
                    })
                    .into_any_element()
            })
            .into_any_element()
    }

    fn render_draft(&self, draft: &Draft, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let mut body = v_flex()
            .gap_2()
            .children(draft.created.as_ref().map(|date| {
                div()
                    .text_size(px(crate::ui_scale::left_font_size(cx, 12.)))
                    .text_color(muted)
                    .child(format!("Создан {date}"))
            }))
            .child(self.render_field(true, draft.editing_question, cx))
            .child(self.render_field(false, draft.editing_answer, cx))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_size(px(crate::ui_scale::left_font_size(cx, 14.)))
                    .child(
                        Switch::new("question-answered")
                            .checked(draft.answered_at.is_some())
                            .accessibility_label(crate::i18n::tr_static("Ответ получен"))
                            .on_click(cx.listener(|this, value: &bool, _, cx| {
                                if let Some(draft) = &mut this.draft {
                                    draft.answered_at = value.then(|| {
                                        std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .map_or(0, |elapsed| elapsed.as_secs() as i64)
                                    });
                                    draft.answered = None;
                                }
                                cx.notify();
                            })),
                    )
                    .child(crate::i18n::tr_static("Ответ получен")),
            )
            .children(draft.answered.as_ref().map(|date| {
                div()
                    .text_size(px(crate::ui_scale::left_font_size(cx, 12.)))
                    .text_color(muted)
                    .child(format!("Отмечен {date}"))
            }))
            .child(
                Button::new("add-question-verse")
                    .xsmall()
                    .label(crate::i18n::tr_static("+ Добавить ссылку на стих"))
                    .on_click(cx.listener(|this, _, window, cx| this.add_verse(window, cx))),
            );
        let book_labels = if draft.links.is_empty() {
            std::collections::HashMap::new()
        } else {
            let registry = app_services(cx).registry.lock();
            registry
                .book_entries()
                .iter()
                .map(|book| (book.book_number, book.short_name.clone()))
                .collect()
        };
        for (index, link) in draft.links.iter().enumerate() {
            let reference = link.reference;
            let label = book_labels
                .get(&reference.book.0)
                .cloned()
                .unwrap_or_else(|| format!("Кн. {}", reference.book.0));
            let label = format!("{label} {}:{}", reference.chapter, reference.verse);
            let row = h_flex()
                .gap_1()
                .items_center()
                .child(div().flex_1().min_w_0().child(verse_preview::text_link(
                    ("question-verse", index),
                    label,
                    reference,
                    Some(link.module.clone()),
                    cx,
                )))
                .child(
                    Button::new(("question-verse-comment", index))
                        .xsmall()
                        .ghost()
                        .icon(IconName::Pencil)
                        .tooltip(crate::i18n::tr_static("Комментарий к стиху"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.edit_comment(index, window, cx)
                        })),
                )
                .child(
                    Button::new(("question-verse-remove", index))
                        .xsmall()
                        .ghost()
                        .icon(IconName::X)
                        .tooltip(crate::i18n::tr_static("Убрать ссылку"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.finish_comment(cx);
                            if let Some(draft) = &mut this.draft {
                                draft.links.remove(index);
                            }
                            cx.notify();
                        })),
                );
            let mut item = v_flex()
                .px_2()
                .py_1()
                .border_l_1()
                .border_color(cx.theme().border)
                .child(row);
            if draft.editing_comment == Some(index) {
                item = item.child(Textarea::new(&self.comment)).child(
                    Button::new(("question-verse-comment-done", index))
                        .xsmall()
                        .label(crate::i18n::tr_static("Готово"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.finish_comment(cx);
                            cx.notify();
                        })),
                );
            } else if !link.comment.is_empty() {
                item = item.child(
                    div()
                        .text_size(px(crate::ui_scale::left_font_size(cx, 14.)))
                        .text_color(muted)
                        .child(link.comment.clone()),
                );
            }
            body = body.child(item);
        }
        body.child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("save-study-question")
                        .small()
                        .primary()
                        .label(crate::i18n::tr_static("Сохранить"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Err(error) = this.save(cx) {
                                this.error = Some(error.to_string());
                                cx.notify();
                            }
                        })),
                )
                .child(
                    Button::new("cancel-study-question")
                        .small()
                        .ghost()
                        .label(crate::i18n::tr_static("Отмена"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.draft = None;
                            this.error = None;
                            cx.notify();
                        })),
                ),
        )
        .into_any_element()
    }
}

impl Render for QuestionsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("questions-panel")
            .size_full()
            .min_h_0()
            .min_w_0()
            .gap_2()
            .p_3()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(crate::i18n::tr_static("Вопросы")),
            )
            .children(self.error.as_ref().map(|error| {
                div()
                    .text_size(px(crate::ui_scale::left_font_size(cx, 14.)))
                    .text_color(cx.theme().danger)
                    .child(error.clone())
            }))
            .child(match &self.draft {
                Some(draft) => self.render_draft(draft, cx),
                None => self.render_list(cx),
            })
            .overflow_y_scrollbar()
    }
}
