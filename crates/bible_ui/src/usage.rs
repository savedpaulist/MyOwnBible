//! Окно графика употребляемости выбранного слова в текущем переводе.

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::TitleBar;
use gpui_kit::component::hover_card::HoverCard;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::*;

use crate::services::app_services_arc;

pub fn open_word_usage(description: shell::WordDescription, cx: &mut App) {
    cx.defer(move |cx| {
        let mut window_size = size(px(1040.0), px(620.0));
        if let Some(display) = cx.primary_display() {
            let available = display.bounds().size;
            window_size.width = window_size.width.min(available.width * 0.88);
            window_size.height = window_size.height.min(available.height * 0.82);
        }
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                window_size,
                cx,
            ))),
            window_min_size: Some(size(px(640.0), px(420.0))),
            ..TitleBar::window_options()
        };
        if let Err(error) = gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| UsageWindow::new(description, window, cx))
        }) {
            log::error!("failed to open word usage window: {error:#}");
        }
    });
}

struct UsageWindow {
    description: shell::WordDescription,
    result: Option<Result<shell::WordUsageViewModel, String>>,
    _task: Task<()>,
}

impl UsageWindow {
    fn new(
        description: shell::WordDescription,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let services = app_services_arc(cx);
        let request = description.clone();
        let task = cx.spawn_in(window, async move |this, window| {
            let result = window
                .background_executor()
                .spawn(async move {
                    shell::word_usage(&services, &request).map_err(|e| format!("{e:#}"))
                })
                .await;
            let _ = this.update_in(window, |this, _, cx| {
                this.result = Some(result);
                cx.notify();
            });
        });
        Self {
            description,
            result: None,
            _task: task,
        }
    }

    fn chart(&self, model: &shell::WordUsageViewModel, cx: &App) -> AnyElement {
        let maximum = model
            .books
            .iter()
            .map(|book| book.count)
            .max()
            .unwrap_or(0)
            .max(1);
        let chart_height = 330.0;
        let mut bars = h_flex()
            .h(px(chart_height + 54.0))
            .items_end()
            .gap_2()
            .px_2()
            .border_b_1()
            .border_color(cx.theme().border);
        for book in &model.books {
            let fraction = book.count as f32 / maximum as f32;
            let bar_height = (fraction * chart_height).max(if book.count > 0 { 3.0 } else { 0.0 });
            let module = model.module.clone();
            let book_number = book.book;
            let book_title = book.long_name.clone();
            let book_short = book.short_name.clone();
            let hits = book.hits.clone();
            let bar = HoverCard::new(("usage-book-hover", book.book.0 as usize))
                .open_delay(std::time::Duration::from_millis(180))
                .trigger(
                    div()
                        .id(ElementId::Name(format!("usage-bar-{}", book.book.0).into()))
                        .w(px(26.))
                        .h(px(bar_height))
                        .min_h(px(3.0))
                        .rounded_t_md()
                        .bg(cx.theme().accent)
                        .hover(|style| style.bg(cx.theme().primary)),
                )
                .content(move |_, _, cx| {
                    let mut list = v_flex().w(px(480.)).max_h(px(430.)).gap_1().p_2().child(
                        h_flex()
                            .w_full()
                            .items_baseline()
                            .gap_2()
                            .pb_1()
                            .child(
                                div()
                                    .text_size(px(15.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(book_title.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{} стихов", hits.len())),
                            ),
                    );
                    for hit in &hits {
                        let request = crate::navigation::OpenBook {
                            module: module.clone(),
                            book: book_number,
                            chapter: Some(hit.chapter),
                            verse: Some(hit.verse),
                        };
                        let label = format!("{book_short} {}:{}", hit.chapter, hit.verse);
                        let occurrence_suffix = (hit.occurrences > 1)
                            .then(|| format!(" · ×{}", hit.occurrences))
                            .unwrap_or_default();
                        list = list.child(
                            v_flex()
                                .id(ElementId::Name(
                                    format!(
                                        "usage-hit-{}-{}-{}",
                                        book_number.0, hit.chapter, hit.verse
                                    )
                                    .into(),
                                ))
                                .w_full()
                                .min_w_0()
                                .p_2()
                                .rounded_md()
                                .cursor_pointer()
                                .hover(|style| style.bg(cx.theme().accent.opacity(0.12)))
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(format!("{label}{occurrence_suffix}")),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(cx.theme().muted_foreground)
                                        .child(hit.text.clone()),
                                )
                                .on_click(move |_, _, cx| {
                                    crate::navigation::request_open_book_in_new_tab(
                                        cx,
                                        request.clone(),
                                    );
                                }),
                        );
                    }
                    list.overflow_y_scrollbar()
                });
            bars = bars.child(
                v_flex()
                    .id(ElementId::Name(
                        format!("usage-book-{}", book.book.0).into(),
                    ))
                    .w(px(44.))
                    .h_full()
                    .flex_shrink_0()
                    .justify_end()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().foreground)
                            .child(book.count.to_string()),
                    )
                    .child(bar)
                    .child(
                        div()
                            .h(px(34.))
                            .text_size(px(11.0))
                            .text_center()
                            .text_color(cx.theme().muted_foreground)
                            .child(book.short_name.clone()),
                    ),
            );
        }
        h_flex()
            .w_full()
            .min_h_0()
            .child(
                v_flex()
                    .w(px(46.))
                    .h(px(chart_height + 54.0))
                    .flex_shrink_0()
                    .justify_between()
                    .items_end()
                    .pr_2()
                    .pb(px(38.0))
                    .text_size(px(11.0))
                    .text_color(cx.theme().muted_foreground)
                    .child(maximum.to_string())
                    .child((maximum / 2).to_string())
                    .child("0"),
            )
            .child(div().flex_1().min_w_0().overflow_x_scrollbar().child(bars))
            .into_any_element()
    }
}

impl Render for UsageWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let scale = app_services_arc(cx)
            .settings
            .read()
            .interface_scale
            .clamp(0.8, 1.6);
        window.set_rem_size(px(16.0 * scale));
        let title = format!("Употребляемость: {}", self.description.word);
        let body = match &self.result {
            None => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    gpui_kit::component::progress::ProgressCircle::new("usage-loading")
                        .loading(true),
                )
                .child("Подсчитываю употребления во всём модуле…")
                .into_any_element(),
            Some(Err(error)) => div()
                .p_4()
                .text_color(cx.theme().danger)
                .child(error.clone())
                .into_any_element(),
            Some(Ok(model)) => v_flex()
                .size_full()
                .min_h_0()
                .p_4()
                .gap_3()
                .child(
                    h_flex()
                        .w_full()
                        .items_baseline()
                        .gap_3()
                        .child(
                            div()
                                .text_size(px(20.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format!("{} · {}", model.query, model.module)),
                        )
                        .child(div().text_color(cx.theme().muted_foreground).child(format!(
                            "Всего: {} · поиск {}",
                            model.total,
                            if model.uses_strong {
                                "по Strong’s"
                            } else {
                                "по точному слову"
                            }
                        ))),
                )
                .child(div().flex_1().min_h_0().child(self.chart(model, cx)))
                .into_any_element(),
        };
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
                        .child(title),
                ),
            )
            .child(body)
    }
}
