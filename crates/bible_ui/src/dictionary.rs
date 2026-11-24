use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use bible_domain::{
    ids::ModuleId,
    ports::{ModuleCatalog as _, ModuleKind, ModuleMeta},
};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{navigation, services::app_services_arc};

#[derive(Clone)]
struct Topic {
    module: ModuleId,
    title: String,
}

struct Search {
    cancelled: Arc<AtomicBool>,
    _task: Task<()>,
}

impl Drop for Search {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

pub(crate) struct DictionaryBrowser {
    focus: FocusHandle,
    input: Entity<InputState>,
    module: Option<ModuleId>,
    modules: Vec<ModuleMeta>,
    include_body: bool,
    busy: bool,
    error: Option<String>,
    results: Rc<Vec<Topic>>,
    list: ListState,
    search: Option<Search>,
    _subscriptions: Vec<Subscription>,
}

impl DictionaryBrowser {
    pub const NAME: &'static str = "dictionary-browser";

    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(crate::i18n::tr(cx, "Поиск по словарям"))
        });
        let query = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::Change) {
                this.search(window, cx);
            }
        });
        let catalogue = cx.subscribe_in(
            &navigation::bus(cx),
            window,
            |this, _, _: &navigation::SettingsChanged, window, cx| {
                let modules = Self::modules(cx);
                if modules
                    .iter()
                    .map(|m| (&m.id, &m.path, &m.description))
                    .eq(this
                        .modules
                        .iter()
                        .map(|m| (&m.id, &m.path, &m.description)))
                {
                    cx.notify();
                    return;
                }
                this.modules = modules;
                if this
                    .module
                    .as_ref()
                    .is_some_and(|id| !this.modules.iter().any(|m| m.id == *id))
                {
                    this.module = None;
                }
                this.search(window, cx);
            },
        );
        Self {
            focus: cx.focus_handle(),
            input,
            module: None,
            modules: Self::modules(cx),
            include_body: false,
            busy: false,
            error: None,
            results: Rc::default(),
            list: ListState::new(0, ListAlignment::Top, px(240.)),
            search: None,
            _subscriptions: vec![query, catalogue],
        }
    }

    fn modules(cx: &App) -> Vec<ModuleMeta> {
        app_services_arc(cx)
            .registry
            .modules()
            .into_iter()
            .filter(|m| {
                matches!(
                    m.kind,
                    ModuleKind::GeneralDictionary | ModuleKind::StrongsDictionary
                )
            })
            .collect()
    }

    pub(crate) fn select_module(
        &mut self,
        module: Option<ModuleId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.module = module;
        self.search(window, cx);
    }

    fn search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search = None;
        self.error = None;
        self.results = Rc::default();
        self.list.reset(0);
        let query = self.input.read(cx).value().trim().to_owned();
        self.busy = self.module.is_some() || !query.is_empty();
        cx.notify();
        if !self.busy {
            return;
        }
        let include_body = self.include_body;
        let modules: Vec<_> = self
            .modules
            .iter()
            .filter(|meta| self.module.as_ref().is_none_or(|id| meta.id == *id))
            .cloned()
            .collect();
        let services = app_services_arc(cx);
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let task = cx.spawn_in(window, async move |this, window| {
            window
                .background_executor()
                .timer(Duration::from_millis(140))
                .await;
            let (results, errors) = window
                .background_executor()
                .spawn(async move {
                    let mut results = Vec::new();
                    let mut errors = Vec::new();
                    for module in modules {
                        if flag.load(Ordering::Relaxed) {
                            break;
                        }
                        match services.registry.search_dictionary_topics(
                            &module,
                            &query,
                            include_body,
                            || flag.load(Ordering::Relaxed),
                        ) {
                            Ok(topics) => results.extend(topics.into_iter().map(|title| Topic {
                                module: module.id.clone(),
                                title,
                            })),
                            Err(error) => {
                                log::warn!("dictionary search in {}: {error:#}", module.id);
                                errors.push(module.id.to_string());
                            }
                        }
                    }
                    (results, errors)
                })
                .await;
            let _ = this.update_in(window, |this, _, cx| {
                this.busy = false;
                this.list.reset(results.len());
                this.results = Rc::new(results);
                this.error = (!errors.is_empty())
                    .then(|| format!("Не удалось прочитать: {}", errors.join(", ")));
                cx.notify();
            });
        });
        self.search = Some(Search {
            cancelled,
            _task: task,
        });
    }
}

impl EventEmitter<PanelEvent> for DictionaryBrowser {}
impl Focusable for DictionaryBrowser {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl BasePanel for DictionaryBrowser {
    fn panel_name(&self) -> &'static str {
        Self::NAME
    }
}
impl Panel for DictionaryBrowser {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        crate::i18n::tr_static("Словарь")
    }
}

impl Render for DictionaryBrowser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut header = v_flex()
            .gap_2()
            .p_2()
            .child(Input::new(&self.input).small())
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(crate::ui_scale::dictionary_font_size(cx, 14.)))
                    .child(crate::i18n::tr(cx, "Статья"))
                    .child(
                        Switch::new("dictionary-search-scope")
                            .accessibility_label(crate::i18n::tr(
                                cx,
                                "Искать также в тексте статьи",
                            ))
                            .checked(self.include_body)
                            .on_click(cx.listener(|this, value, window, cx| {
                                this.include_body = *value;
                                this.search(window, cx);
                            })),
                    )
                    .child(crate::i18n::tr(cx, "Текст")),
            );
        if let Some(id) = &self.module {
            header = header.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("all-dictionaries")
                            .ghost()
                            .xsmall()
                            .icon(IconName::ChevronLeft)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.select_module(None, window, cx)
                            })),
                    )
                    .child(
                        div()
                            .text_size(px(crate::ui_scale::dictionary_font_size(cx, 14.)))
                            .child(id.to_string()),
                    ),
            );
        }
        let browse_modules = self.module.is_none() && self.input.read(cx).value().trim().is_empty();
        let body = if browse_modules {
            v_flex()
                .id("dictionary-modules")
                .size_full()
                .p_1()
                .children(self.modules.iter().enumerate().map(|(index, module)| {
                    let id = module.id.clone();
                    let label = module.description.as_deref().unwrap_or(&id.0);
                    div()
                        .id(SharedString::from(id.to_string()))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(crate::ui_scale::row_background(index, cx))
                        .cursor_pointer()
                        .hover(|style| style.bg(cx.theme().accent.opacity(0.15)))
                        .child(crate::text::strip_markup(label))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.select_module(Some(id.clone()), window, cx);
                        }))
                }))
                .overflow_y_scrollbar()
                .into_any_element()
        } else if self.busy {
            div()
                .p_2()
                .child(crate::i18n::tr(cx, "Поиск…"))
                .into_any_element()
        } else if self.results.is_empty() {
            div()
                .p_2()
                .child(crate::i18n::tr(cx, "Статьи не найдены."))
                .into_any_element()
        } else {
            let results = self.results.clone();
            let show_module = self.module.is_none();
            let rows = list(self.list.clone(), move |index, _, cx| {
                let topic = results[index].clone();
                let accent = cx.theme().accent.opacity(0.15);
                let mut row = v_flex()
                    .id(("dictionary-topic", index))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(crate::ui_scale::row_background(index, cx))
                    .text_size(px(crate::ui_scale::dictionary_font_size(cx, 14.)))
                    .cursor_pointer()
                    .hover(|style| style.bg(accent))
                    .child(topic.title.clone());
                if show_module {
                    row = row.child(
                        div()
                            .text_size(px(crate::ui_scale::dictionary_font_size(cx, 12.)))
                            .text_color(cx.theme().muted_foreground)
                            .child(topic.module.to_string()),
                    );
                }
                row.on_click(move |_, _, cx| {
                    navigation::request_open_topic(cx, topic.module.clone(), topic.title.clone())
                })
                .into_any_element()
            })
            .size_full();
            div()
                .relative()
                .size_full()
                .child(rows)
                .vertical_scrollbar(&self.list)
                .into_any_element()
        };
        let family = crate::ui_scale::dictionary_font_family(cx);
        v_flex()
            .size_full()
            .min_h_0()
            .text_size(px(crate::ui_scale::dictionary_font_size(cx, 14.)))
            .when(!family.is_empty(), |body| body.font_family(family))
            .child(header)
            .children(
                self.error
                    .as_ref()
                    .map(|error| div().px_2().text_sm().child(error.clone())),
            )
            .child(div().flex_1().min_h_0().child(body))
    }
}
