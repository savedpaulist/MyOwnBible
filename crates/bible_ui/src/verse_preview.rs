//! Plain-text previews for Scripture links in rich text.

use bible_domain::content::{ContentBlock, ContentInline};
use bible_domain::ids::{ModuleId, VerseRef};
use bible_domain::link::LinkTarget;
use bible_domain::ports::VerseSource as _;
use bible_domain::ports::{ModuleCatalog as _, ModuleKind};
use gpui_kit::base::{TextView, h_flex, v_flex};
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    scroll::ScrollableElement as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{article, bidi, services::app_services};

pub(crate) fn with_preview(view: TextView) -> TextView {
    with_preview_in(view, None)
}

fn with_preview_in(view: TextView, preferred: Option<ModuleId>) -> TextView {
    view.optional_link_tooltip(move |url, _, cx| {
        let LinkTarget::Bible(reference) = article::url_to_link_target(url)? else {
            return None;
        };
        Some(
            cx.new(|cx| VersePreview::new(reference, preferred.clone(), cx))
                .into(),
        )
    })
}

pub(crate) fn bible_modules(cx: &App) -> Vec<ModuleId> {
    app_services(cx)
        .registry
        .modules()
        .into_iter()
        .filter(|meta| meta.kind == ModuleKind::BibleTranslation)
        .map(|meta| meta.id)
        .collect()
}

pub(crate) fn read_clean(
    services: &shell::AppServices,
    reference: &VerseRef,
    preferred: Option<ModuleId>,
) -> Option<(ModuleId, String)> {
    candidate_modules(services, preferred)
        .into_iter()
        .find_map(|module| {
            let raw = services.registry.verse_range(&module, reference).ok()?;
            let text = plain_text(&raw);
            (!text.is_empty()).then_some((module, text))
        })
}

pub(crate) fn candidate_modules(
    services: &shell::AppServices,
    preferred: Option<ModuleId>,
) -> Vec<ModuleId> {
    let current = services
        .current_location
        .lock()
        .as_ref()
        .map(|loc| loc.module.clone());
    let mut modules: Vec<_> = preferred
        .into_iter()
        .chain(current)
        .chain(services.default_translation())
        .collect();
    modules.dedup();
    for meta in services.registry.modules() {
        if meta.kind == ModuleKind::BibleTranslation && !modules.contains(&meta.id) {
            modules.push(meta.id);
        }
    }
    modules
}

pub(crate) fn follow(reference: VerseRef, preferred: Option<ModuleId>, cx: &mut App) {
    let services = app_services(cx);
    let module = candidate_modules(services, preferred)
        .into_iter()
        .find(|module| {
            services
                .registry
                .verse_range(module, &reference)
                .is_ok_and(|text| !text.is_empty())
        });
    if let Some(module) = module {
        crate::navigation::request_open_book(
            cx,
            crate::navigation::OpenBook {
                module,
                book: reference.book,
                chapter: Some(reference.chapter),
                verse: Some(reference.verse),
            },
        );
    }
}

pub(crate) fn text_link(
    id: impl Into<ElementId>,
    label: String,
    reference: VerseRef,
    preferred: Option<ModuleId>,
    cx: &App,
) -> TextView {
    let html = article::blocks_to_html(&[ContentBlock::Paragraph(vec![ContentInline::Link {
        target: LinkTarget::Bible(reference),
        label,
    }])]);
    with_preview_in(TextView::html(id, html), preferred.clone())
        .text_size(px(crate::ui_scale::panel_font_size(cx, 14.)))
        .on_link_click(move |_, _, _, cx| follow(reference, preferred.clone(), cx))
}

pub(crate) fn plain_text(raw: &str) -> String {
    display_text(raw, false)
}

pub(crate) fn display_text(raw: &str, critical: bool) -> String {
    let raw = raw
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("<p/>", "\n")
        .replace("</p>", "\n");
    let parsed = bible_domain::verse::parse_verse_full(&raw, false, false, critical);
    let text = crate::text::unescape_markup_entities(&parsed.clean_text);
    text.trim().to_owned()
}

struct VersePreview {
    reference: VerseRef,
    module: Option<ModuleId>,
    text: Option<String>,
    choosing: bool,
    error: Option<String>,
    load: Option<Task<()>>,
}

impl VersePreview {
    fn new(reference: VerseRef, preferred: Option<ModuleId>, cx: &mut Context<Self>) -> Self {
        let module = app_services(cx)
            .settings
            .read()
            .verse_preview_module
            .clone()
            .map(ModuleId);
        let mut view = Self {
            reference,
            module: None,
            text: None,
            choosing: false,
            error: None,
            load: None,
        };
        view.load(module.clone().or(preferred), module.is_some(), cx);
        view
    }

    fn load(&mut self, preferred: Option<ModuleId>, exact: bool, cx: &mut Context<Self>) {
        let services = crate::services::app_services_arc(cx);
        let reference = self.reference;
        self.text = None;
        self.module = preferred.clone();
        let job = cx.background_executor().spawn(async move {
            if exact && let Some(module) = preferred {
                let text = services
                    .registry
                    .verse_range(&module, &reference)
                    .ok()
                    .map(|raw| plain_text(&raw))
                    .filter(|text| !text.is_empty());
                return (
                    Some(module),
                    Some(text.unwrap_or_else(|| "В этом модуле стих отсутствует.".into())),
                );
            }
            match read_clean(&services, &reference, preferred) {
                Some((module, text)) => (Some(module), Some(text)),
                None => (None, Some("Стих не найден.".into())),
            }
        });
        self.load = Some(cx.spawn(async move |this, cx| {
            let (module, text) = job.await;
            let _ = this.update(cx, |this, cx| {
                this.module = module;
                this.text = text;
                cx.notify();
            });
        }));
    }
}

impl Render for VersePreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text = self.text.as_deref().unwrap_or("Загрузка…");
        v_flex()
            .id("verse-link-preview")
            .w(px(360.))
            .gap_2()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .text_size(px(crate::ui_scale::panel_font_size(cx, 14.)))
            .occlude()
            .child(
                h_flex().child(
                    Button::new("preview-module")
                        .xsmall()
                        .outline()
                        .label(
                            self.module
                                .as_ref()
                                .map_or("Модуль", |module| module.0.as_str())
                                .to_owned(),
                        )
                        .accessibility_label(crate::i18n::tr_static(
                            "Модуль Библии для предпросмотра",
                        ))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.choosing = !this.choosing;
                            cx.notify();
                        })),
                ),
            )
            .children(self.choosing.then(|| {
                v_flex()
                    .id("preview-modules")
                    .max_h(px(126.))
                    .children(
                        bible_modules(cx)
                            .into_iter()
                            .enumerate()
                            .map(|(index, module)| {
                                Button::new(("preview-module", index))
                                    .xsmall()
                                    .ghost()
                                    .justify_start()
                                    .label(module.0.clone())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let id = module.0.clone();
                                        this.error = shell::settings::update(
                                            app_services(cx),
                                            move |settings| {
                                                settings.verse_preview_module = Some(id);
                                            },
                                        )
                                        .err()
                                        .map(|error| error.to_string());
                                        this.choosing = false;
                                        this.load(Some(module.clone()), true, cx);
                                        cx.notify();
                                    }))
                            }),
                    )
                    .overflow_y_scrollbar()
            }))
            .children(
                self.error
                    .clone()
                    .map(|error| div().text_color(cx.theme().danger).child(error)),
            )
            .child(
                div()
                    .id("preview-text")
                    .max_h(px(170.))
                    .when(bidi::is_hebrew_paragraph(text), |card| card.text_right())
                    .child(text.to_owned())
                    .overflow_y_scrollbar(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::plain_text;

    #[test]
    fn preview_contains_only_verse_text() {
        assert_eq!(
            plain_text(
                "<i>В начале</i> было<S>G1510</S><m>V-IAI-3S</m> Слово.<n>сноска</n>{вариант}<br/>И Слово было у Бога."
            ),
            "В начале было Слово.\nИ Слово было у Бога.",
        );
        assert_eq!(
            plain_text("בְּרֵאשִׁית<S>H7225</S><m>N</m> &amp; Ἐν ἀρχῇ"),
            "בְּרֵאשִׁית & Ἐν ἀρχῇ",
        );
    }
}
