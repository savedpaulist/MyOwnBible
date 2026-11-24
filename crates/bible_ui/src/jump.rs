//! Палитра «Перейти к стиху» (Ctrl+P, architecture.md §6.5, этап 9.3):
//! модальный ввод «Ин 3:16» → parse_jump → запрос OpenBook в шине навигации.
//!
//! Логика разбора ввода и подсказок живёт в shell::navigation (там же
//! тесты); здесь — только диалог: Input, превью цели и подсказки имён книг
//! активного перевода. Enter отклоняет несуществующую главу: перехода нет,
//! под вводом красная подпись (как book_search_picker форка, только на
//! component::dialog).

use gpui_kit::base::v_flex;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::*;

use crate::navigation::{self, OpenBook};
use crate::services::app_services;

/// Сколько книг показываем в подсказках под вводом.
const SUGGESTION_LIMIT: usize = 6;

/// Открыть палитру поверх окна (Ctrl+P). Сущности создаются до open_dialog:
/// builder диалога может выполняться повторно при пересборке слоя, а ввод
/// и его состояние должны переживать эти пересборки. Фокус в поле — один
/// раз при открытии (одноразовый Cell, паттерн из документации WindowExt).
pub fn open_verse_jump(window: &mut Window, cx: &mut App) {
    let palette = cx.new(|cx| VerseJumpPalette::new(window, cx));
    let initial_focus = std::cell::Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if initial_focus.replace(false) {
            palette.read(cx).focus_handle(cx).focus(window, cx);
        }
        dialog
            .title(crate::i18n::tr(cx, "Перейти к стиху"))
            .width(px(520.))
            .child(palette.clone())
    });
}

pub struct VerseJumpPalette {
    input: Entity<InputState>,
    /// Разобранная цель перехода (None — ввод не распознан).
    target: Option<shell::navigation::JumpTarget>,
    /// Подсказки имён книг — показываются, пока ссылка не разобрана.
    suggestions: Vec<shell::BookEntry>,
    _subscriptions: Vec<Subscription>,
}

impl VerseJumpPalette {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(crate::i18n::tr_static("Ин 3:16, Быт 1:1, John 3…"))
        });

        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => this.recompute(cx),
                InputEvent::PressEnter { .. } => this.confirm(window, cx),
                _ => {}
            },
        ));

        Self {
            input,
            target: None,
            suggestions: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    /// Пересчитать цель и подсказки по текущему вводу.
    fn recompute(&mut self, cx: &mut Context<Self>) {
        let value = self.input.read(cx).value().to_string();
        let target = shell::navigation::parse_jump(app_services(cx), &value);
        self.suggestions = if target.is_some() {
            Vec::new()
        } else {
            shell::navigation::book_suggestions(app_services(cx), &value, SUGGESTION_LIMIT)
        };
        self.target = target;
        cx.notify();
    }

    /// Глава за пределами книги — Enter молчит, под вводом красная подпись.
    /// Модуль без кэша стихов (chapter_count == 0) не ограничивает переход.
    fn chapter_out_of_range(&self) -> bool {
        self.target
            .as_ref()
            .is_some_and(|target| target.chapter_count > 0 && target.chapter > target.chapter_count)
    }

    /// Enter: валидная цель → запрос на открытие в шине и закрытие диалога.
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.chapter_out_of_range() {
            return;
        }
        let Some(target) = self.target.clone() else {
            return;
        };
        navigation::request_open_book(
            cx,
            OpenBook {
                module: target.module,
                book: target.book,
                chapter: Some(target.chapter),
                verse: target.verse,
            },
        );
        window.close_dialog(cx);
    }

    /// Клик по подсказке: подставить короткое имя книги с пробелом — дальше
    /// пользователь печатает «3:16». set_value не шлёт InputEvent::Change,
    /// поэтому пересчёт и возврат фокуса делаем сами.
    fn pick_suggestion(
        &mut self,
        entry: &shell::BookEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = format!("{} ", entry.short_name.trim());
        self.input
            .update(cx, |input, cx| input.set_value(value, window, cx));
        self.recompute(cx);
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Одна строка подсказки: длинное имя + короткое справа.
    fn render_suggestion(
        &self,
        index: usize,
        entry: &shell::BookEntry,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let entry = entry.clone();
        div()
            .id(("jump-suggestion", index))
            .flex()
            .items_baseline()
            .gap_2()
            .px_2()
            .py_1()
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|style| style.bg(cx.theme().accent.opacity(0.12)))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .child(entry.long_name.clone()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(entry.short_name.clone()),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.pick_suggestion(&entry, window, cx);
            }))
    }
}

impl Focusable for VerseJumpPalette {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Render for VerseJumpPalette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (accent, muted_foreground, danger, foreground) = {
            let theme = cx.theme();
            (
                theme.accent,
                theme.muted_foreground,
                theme.danger,
                theme.foreground,
            )
        };

        let input_empty = self.input.read(cx).value().trim().is_empty();

        // Область под вводом: подсказка формата / превью цели / ошибка
        // главы / подсказки книг — собираем пошагово, как render_topic_row
        // в dictionary.rs (map-замыкание над cx не проходит по лайфтаймам).
        let mut status = v_flex().gap_1().min_h(px(20.));

        if input_empty {
            status = status.child(div().text_xs().text_color(muted_foreground).child(
                crate::i18n::tr(cx, "Формат: Книга глава[:стих] — например, «Ин 3:16»"),
            ));
        }

        if let Some(target) = self.target.clone() {
            if self.chapter_out_of_range() {
                status = status.child(
                    div()
                        .text_xs()
                        .text_color(danger)
                        .child(format!("В этой книге всего глав: {}", target.chapter_count)),
                );
            } else {
                let mut label = format!("{} {}", target.book_title, target.chapter);
                if let Some(verse) = target.verse {
                    label.push_str(&format!(":{verse}"));
                }
                status = status
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(accent)
                            .child(label),
                    )
                    .child(div().text_xs().text_color(muted_foreground).child(format!(
                        "Enter — перейти · {}",
                        target.module.to_string().to_uppercase()
                    )));
            }
        } else if !input_empty && self.suggestions.is_empty() {
            status = status.child(div().text_xs().text_color(muted_foreground).child(
                crate::i18n::tr(cx, "Книга не найдена — проверьте написание"),
            ));
        }

        let suggestions = self.suggestions.clone();
        for (index, entry) in suggestions.iter().enumerate() {
            status = status.child(self.render_suggestion(index, entry, cx));
        }

        v_flex()
            .gap_2()
            .child(Input::new(&self.input))
            .child(status)
            .text_color(foreground)
    }
}
