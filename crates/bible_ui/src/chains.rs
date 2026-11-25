//! Панель «Цепочки» (architecture.md §6.8, этап 12).
//!
//! Два режима, как в форке (`chains_panel.rs`):
//!   * Список — все цепочки с числом узлов, фильтр, «+ Новая цепочка».
//!   * Открытая цепочка — узлы в топопорядке с отступом по глубине,
//!     пометки «слияние»/«ветвь», действия над узлом.
//!
//! Узлы можно вставлять до, после или между связанными узлами.
//!
//! Все данные — через сценарии shell::chains; после каждой мутации
//! панель шлёт ChainsChanged, чтобы «Контекст» и чтение перечитали
//! свои полосы и маркеры.

use gpui_kit::base::{SelectableText, h_flex, v_flex};
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, popover::Popover};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use bible_domain::ids::{BookNumber, ModuleId, VerseRef};
use bible_domain::ports::VerseSource as _;
use bible_domain::reference::{self, Reference};

use crate::navigation::{self, OpenBook};
use crate::services::{app_services, app_services_arc};
use crate::verse_preview;
use std::collections::HashMap;

/// Где окажется узел после подтверждения формы.
#[derive(Debug, Clone)]
enum Placement {
    /// Новый корень (без родителей).
    Root,
    /// Следствие узла (ребро parent -> новый).
    Conclusion {
        parent: String,
    },
    Before {
        target: String,
    },
    Between {
        source: String,
        target: String,
    },
    /// Правка существующего узла.
    Edit {
        node: String,
    },
}

/// Раскрытая форма ввода узла (refs + утверждение).
struct PendingForm {
    placement: Placement,
    refs_input: Entity<InputState>,
    assertion_input: Entity<TextareaState>,
}

/// Открытая цепочка и её локальное UI-состояние.
struct OpenChainState {
    view: shell::chains::ChainView,
    renaming: bool,
    form: Option<PendingForm>,
    texts: HashMap<String, Vec<Option<ChainVerse>>>,
    text_load: Option<Task<()>>,
    reading_module: Option<ModuleId>,
}

struct ChainVerse {
    module: ModuleId,
    text: String,
}

fn read_reference(
    services: &shell::AppServices,
    reference: &Reference,
    modules: &[ModuleId],
) -> Option<ChainVerse> {
    let book = BookNumber(reference.book_number);
    let chapter = reference.chapter;
    modules.iter().find_map(|module| {
        let text = if let Some(verse) = reference.verse {
            let address = VerseRef {
                book,
                chapter,
                verse,
                end_verse: reference.end_verse,
            };
            let raw = services.registry.verse_range(module, &address).ok()?;
            verse_preview::plain_text(&raw)
        } else {
            services
                .registry
                .chapter(module, book, chapter)
                .ok()?
                .into_iter()
                .map(|verse| verse_preview::plain_text(&verse.text))
                .collect::<Vec<_>>()
                .join("\n")
        };
        (!text.is_empty()).then_some(ChainVerse {
            module: module.clone(),
            text,
        })
    })
}

pub struct ChainsPanel {
    focus_handle: FocusHandle,
    list: Vec<shell::chains::ChainSummary>,
    filter_input: Entity<InputState>,
    new_chain_input: Entity<InputState>,
    /// Поле переименования (одно на панель — window нужен для создания,
    /// поэтому живёт здесь, а не в состоянии открытой цепочки).
    rename_input: Entity<InputState>,
    open: Option<OpenChainState>,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl ChainsPanel {
    pub const NAME: &'static str = "chains";

    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(crate::i18n::tr_static("Фильтр цепочек…"))
        });
        let new_chain_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(crate::i18n::tr_static("Название новой цепочки…"))
        });
        let rename_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(crate::i18n::tr_static("Название цепочки…"))
        });

        let subscriptions = vec![
            // Профиль шрифта боковых панелей изменился — перерисовать весь экран.
            cx.subscribe_in(
                &navigation::bus(cx),
                window,
                |_, _, _: &navigation::SettingsChanged, _, cx| cx.notify(),
            ),
            cx.subscribe_in(
                &navigation::bus(cx),
                window,
                |this, _, event: &navigation::LocationChanged, _, cx| {
                    if this.open.as_ref().is_some_and(|open| {
                        open.view.bible_module.is_none()
                            && open.reading_module.as_ref() != Some(&event.0.module)
                    }) {
                        this.load_texts(cx);
                    }
                },
            ),
            // Enter в поле новой цепочки — создать.
            cx.subscribe_in(
                &new_chain_input,
                window,
                |this, input, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        let title = input.read(cx).value().to_string();
                        this.create_chain(title, window, cx);
                    }
                },
            ),
            // Фильтр — просто перерисовка (применяется в render).
            cx.subscribe_in(&filter_input, window, |_, _, event: &InputEvent, _, cx| {
                if let InputEvent::Change = event {
                    cx.notify();
                }
            }),
            // Просьба открыть панель: из полосы «Контекста» (с id) или из
            // палитры (без id — показать список).
            cx.subscribe_in(
                &navigation::bus(cx),
                window,
                |this, _, event: &navigation::OpenChain, _, cx| match &event.chain_id {
                    Some(id) => this.open_chain(id, cx),
                    None => this.reload_list(cx),
                },
            ),
            // Любая мутация цепочек (в т.ч. наша собственная — дешевле
            // перечитать, чем различать источник).
            cx.subscribe_in(
                &navigation::bus(cx),
                window,
                |this, _, _: &navigation::ChainsChanged, _, cx| {
                    this.reload_list(cx);
                    if let Some(open) = &this.open {
                        let id = open.view.id.clone();
                        this.reload_open(&id, cx);
                    }
                },
            ),
        ];

        let mut this = Self {
            focus_handle: cx.focus_handle(),
            list: Vec::new(),
            filter_input,
            new_chain_input,
            rename_input,
            open: None,
            error: None,
            _subscriptions: subscriptions,
        };
        this.reload_list(cx);
        this
    }

    fn reload_list(&mut self, cx: &mut Context<Self>) {
        self.list = shell::chains::list_chains(app_services(cx));
        cx.notify();
    }

    /// Перечитать открытую цепочку; если удалили — вернуться к списку.
    fn reload_open(&mut self, id: &str, cx: &mut Context<Self>) {
        match shell::chains::load_chain(app_services(cx), id) {
            Ok(Some(view)) => {
                if let Some(open) = &mut self.open {
                    open.view = view;
                    self.load_texts(cx);
                }
            }
            Ok(None) => self.open = None,
            Err(error) => self.error = Some(format!("{error:#}")),
        }
        cx.notify();
    }

    fn create_chain(&mut self, title: String, window: &mut Window, cx: &mut Context<Self>) {
        let title = if title.trim().is_empty() {
            crate::i18n::tr_static("Новая цепочка").to_string()
        } else {
            title.trim().to_string()
        };
        match shell::chains::create_chain(app_services(cx), &title) {
            Ok(summary) => {
                self.new_chain_input.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                });
                navigation::notify_chains_changed(cx);
                self.open_chain(&summary.id, cx);
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                cx.notify();
            }
        }
    }

    fn open_chain(&mut self, id: &str, cx: &mut Context<Self>) {
        match shell::chains::load_chain(app_services(cx), id) {
            Ok(Some(view)) => {
                self.open = Some(OpenChainState {
                    view,
                    renaming: false,
                    form: None,
                    texts: HashMap::new(),
                    text_load: None,
                    reading_module: None,
                });
                self.load_texts(cx);
            }
            Ok(None) => {
                self.error = Some("цепочка не найдена (удалена?)".to_string());
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
            }
        }
        cx.notify();
    }

    fn load_texts(&mut self, cx: &mut Context<Self>) {
        let Some(open) = &mut self.open else { return };
        let services = app_services_arc(cx);
        let default_module = open.view.bible_module.clone();
        open.reading_module = services
            .current_location
            .lock()
            .as_ref()
            .map(|loc| loc.module.clone());
        let automatic_modules =
            verse_preview::candidate_modules(&services, open.reading_module.clone());
        let nodes = open
            .view
            .nodes
            .iter()
            .map(|view| {
                (
                    view.node.id.clone(),
                    view.node.refs.clone(),
                    view.bible_module.clone().or_else(|| default_module.clone()),
                )
            })
            .collect::<Vec<_>>();
        open.texts.clear();
        let job = cx.background_executor().spawn(async move {
            nodes
                .into_iter()
                .map(|(id, refs, module)| {
                    let modules = module
                        .as_ref()
                        .map_or(automatic_modules.as_slice(), std::slice::from_ref);
                    let verses = refs
                        .iter()
                        .map(|reference| read_reference(&services, reference, modules))
                        .collect();
                    (id, verses)
                })
                .collect::<HashMap<_, _>>()
        });
        open.text_load = Some(cx.spawn(async move |this, cx| {
            let texts = job.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(open) = &mut this.open {
                    open.texts = texts;
                    open.text_load = None;
                    cx.notify();
                }
            });
        }));
    }

    fn render_module_picker(
        &self,
        node: Option<&shell::chains::ChainNodeView>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(open) = &self.open else {
            return div().into_any_element();
        };
        let chain_id = open.view.id.clone();
        let node_id = node.map(|view| view.node.id.clone());
        let selected = match node {
            Some(view) => view.bible_module.as_ref(),
            None => open.view.bible_module.as_ref(),
        };
        let automatic = if node.is_some() {
            "Как в цепочке"
        } else {
            "Текущий перевод"
        };
        let label = selected.map_or(automatic, |id| id.0.as_str());
        let key = SharedString::from(format!(
            "chain-module-{}",
            node_id.as_deref().unwrap_or("all")
        ));
        let weak = cx.weak_entity();
        Popover::new(key.clone())
            .trigger(
                Button::new(key)
                    .outline()
                    .xsmall()
                    .label(format!("Модуль Библии · {label}")),
            )
            .content(move |_, _, cx| {
                let modules = verse_preview::bible_modules(cx);
                let chain_id = chain_id.clone();
                let node_id = node_id.clone();
                let weak = weak.clone();
                let popover = cx.weak_entity();
                v_flex()
                    .id("chain-module-list")
                    .w(px(210.))
                    .max_h(px(250.))
                    .children(
                        std::iter::once(None)
                            .chain(modules.into_iter().map(Some))
                            .enumerate()
                            .map(move |(index, module)| {
                                let label = module
                                    .as_ref()
                                    .map_or(automatic, |id| id.0.as_str())
                                    .to_owned();
                                let (weak, popover, chain_id, node_id) = (
                                    weak.clone(),
                                    popover.clone(),
                                    chain_id.clone(),
                                    node_id.clone(),
                                );
                                Button::new(("chain-module-option", index))
                                    .ghost()
                                    .xsmall()
                                    .justify_start()
                                    .label(label)
                                    .on_click(move |_, window, cx| {
                                        let _ = weak.update(cx, |this, cx| {
                                            match shell::chains::set_bible_module(
                                                app_services(cx),
                                                &chain_id,
                                                node_id.as_deref(),
                                                module.as_ref(),
                                            ) {
                                                Ok(()) => navigation::notify_chains_changed(cx),
                                                Err(error) => {
                                                    this.error = Some(error.to_string());
                                                    cx.notify();
                                                }
                                            }
                                        });
                                        let _ = popover
                                            .update(cx, |state, cx| state.dismiss(window, cx));
                                    })
                            }),
                    )
                    .overflow_y_scrollbar()
            })
            .into_any_element()
    }

    fn confirm_delete_chain(
        &mut self,
        id: String,
        close_open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let receiver = window.prompt(
            PromptLevel::Critical,
            crate::i18n::tr_static("Удалить цепочку?"),
            Some(crate::i18n::tr_static("Эту операцию нельзя отменить")),
            &[
                crate::i18n::tr_static("Удалить"),
                crate::i18n::tr_static("Отмена"),
            ],
            cx,
        );
        cx.spawn_in(window, async move |this, window| {
            if receiver.await != Ok(0) {
                return;
            }
            let _ = this.update_in(window, |this, _, cx| {
                if let Err(error) = shell::chains::delete_chain(app_services(cx), &id) {
                    this.error = Some(format!("{error:#}"));
                } else {
                    if close_open {
                        this.open = None;
                    }
                    this.reload_list(cx);
                    navigation::notify_chains_changed(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn submit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open.is_none() {
            return;
        }
        let title = self.rename_input.read(cx).value().trim().to_string();
        if title.is_empty() {
            if let Some(open) = &mut self.open {
                open.renaming = false;
            }
            cx.notify();
            return;
        }
        let id = self
            .open
            .as_ref()
            .map(|open| open.view.id.clone())
            .unwrap_or_default();
        if let Err(error) = shell::chains::rename_chain(app_services(cx), &id, &title) {
            self.error = Some(format!("{error:#}"));
        } else {
            navigation::notify_chains_changed(cx);
        }
        if let Some(open) = &mut self.open {
            open.renaming = false;
        }
        let _ = window;
        cx.notify();
    }

    /// Открыть форму добавления/правки узла.
    fn start_form(&mut self, placement: Placement, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        let (refs_value, assertion_value) = match &placement {
            Placement::Edit { node } => {
                let values = self.open.as_ref().and_then(|open| {
                    open.view
                        .nodes
                        .iter()
                        .find(|view| &view.node.id == node)
                        .map(|view| {
                            let refs = view
                                .labels
                                .iter()
                                .map(|label| label.label.clone())
                                .collect::<Vec<_>>()
                                .join(", ");
                            (refs, view.node.assertion.clone())
                        })
                });
                values.unwrap_or_default()
            }
            _ => (String::new(), String::new()),
        };
        let refs_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(crate::i18n::tr_static("Ссылки: Ин 3:16, Рим 5:8…"))
        });
        refs_input.update(cx, |input, cx| {
            input.set_value(refs_value, window, cx);
        });
        let assertion_input = cx.new(|cx| {
            TextareaState::new(window, cx).placeholder(crate::i18n::tr_static("Утверждение…"))
        });
        assertion_input.update(cx, |input, cx| {
            input.set_value(assertion_value, window, cx);
        });
        if let Some(open) = &mut self.open {
            open.form = Some(PendingForm {
                placement,
                refs_input,
                assertion_input,
            });
        }
        cx.notify();
    }

    fn cancel_form(&mut self, cx: &mut Context<Self>) {
        if let Some(open) = &mut self.open {
            open.form = None;
        }
        cx.notify();
    }

    /// Подтвердить форму: разобрать ссылки (через реестр книг), записать.
    fn submit_form(&mut self, cx: &mut Context<Self>) {
        let Some(open) = &self.open else {
            return;
        };
        let Some(form) = &open.form else {
            return;
        };
        let chain_id = open.view.id.clone();
        let refs_text = form.refs_input.read(cx).value().to_string();
        let assertion = form.assertion_input.read(cx).value().trim().to_string();

        let services = app_services(cx);
        let refs = match parse_refs_input(services, &refs_text) {
            Ok(refs) => refs,
            Err(bad) => {
                self.error = Some(format!("не разобрал ссылку: {bad}"));
                cx.notify();
                return;
            }
        };
        let placement = form.placement.clone();
        let result = match &placement {
            Placement::Root => {
                shell::chains::add_node(services, &chain_id, None, refs, assertion).map(|_| ())
            }
            Placement::Conclusion { parent } => {
                shell::chains::add_node(services, &chain_id, Some(parent), refs, assertion)
                    .map(|_| ())
            }
            Placement::Before { target } => {
                shell::chains::insert_node(services, &chain_id, None, Some(target), refs, assertion)
                    .map(|_| ())
            }
            Placement::Between { source, target } => shell::chains::insert_node(
                services,
                &chain_id,
                Some(source),
                Some(target),
                refs,
                assertion,
            )
            .map(|_| ()),
            Placement::Edit { node } => shell::chains::update_node(services, node, refs, assertion),
        };
        match result {
            Ok(()) => {
                self.error = None;
                if let Some(open) = &mut self.open {
                    open.form = None;
                }
                navigation::notify_chains_changed(cx);
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                cx.notify();
            }
        }
    }

    fn delete_node(&mut self, node_id: &str, cx: &mut Context<Self>) {
        if let Err(error) = shell::chains::delete_node(app_services(cx), node_id) {
            self.error = Some(format!("{error:#}"));
            cx.notify();
            return;
        }
        navigation::notify_chains_changed(cx);
    }

    // ------------------------------------------------------------------
    // Рендер
    // ------------------------------------------------------------------

    fn render_index(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (muted, border, danger) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.border, theme.danger)
        };
        let filter = self.filter_input.read(cx).value().trim().to_lowercase();
        let visible: Vec<&shell::chains::ChainSummary> = self
            .list
            .iter()
            .filter(|chain| filter.is_empty() || chain.title.to_lowercase().contains(&filter))
            .collect();

        let mut list_column = v_flex().w_full().gap_px();
        if visible.is_empty() {
            list_column = list_column.child(
                div()
                    .p_3()
                    .text_size(px(crate::ui_scale::left_font_size(cx, 14.0)))
                    .text_color(muted)
                    .child(if self.list.is_empty() {
                        "Цепочек пока нет — создайте первую выше."
                    } else {
                        "По фильтру ничего не найдено."
                    }),
            );
        }
        for chain in visible {
            let id = chain.id.clone();
            let id_for_delete = chain.id.clone();
            let title = if chain.title.trim().is_empty() {
                "(без названия)".to_string()
            } else {
                chain.title.clone()
            };
            list_column = list_column.child(
                h_flex()
                    .w_full()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .id(SharedString::from(format!("chain-row-{id}")))
                            .flex_1()
                            .min_w_0()
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_chain(&id, cx);
                            }))
                            .child(
                                div()
                                    .text_size(px(crate::ui_scale::left_font_size(cx, 14.0)))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(crate::ui_scale::left_font_size(cx, 12.0)))
                                    .text_color(muted)
                                    .child(format!("узлов: {}", chain.node_count)),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!("chain-del-{id_for_delete}")))
                            .xsmall()
                            .ghost()
                            .label("✕")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.confirm_delete_chain(id_for_delete.clone(), false, window, cx);
                            })),
                    ),
            );
        }

        let mut column = v_flex()
            .size_full()
            .min_h_0()
            .child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .p_2()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        Button::new("chain-create")
                            .small()
                            .label(crate::i18n::tr_static("+ Новая цепочка"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                let title = this.new_chain_input.read(cx).value().to_string();
                                this.create_chain(title, window, cx);
                            })),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .child(Input::new(&self.new_chain_input)),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(border)
                    .child(Input::new(&self.filter_input)),
            )
            .child(
                div()
                    .id("chains-index-scroll")
                    .flex_1()
                    .min_h_0()
                    .child(list_column)
                    .overflow_y_scrollbar(),
            );
        if let Some(error) = &self.error {
            column = column.child(
                div()
                    .p_2()
                    .text_size(px(crate::ui_scale::left_font_size(cx, 12.0)))
                    .text_color(danger)
                    .child(error.clone()),
            );
        }
        column.into_any_element()
    }

    fn render_form(&self, form: &PendingForm, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .w_full()
            .gap_1()
            .p_2()
            .my_1()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_md()
            .child(
                div()
                    .text_size(px(crate::ui_scale::left_font_size(cx, 12.0)))
                    .text_color(muted)
                    .child(crate::i18n::tr_static(
                        "Ссылки через запятую — как в палитре перехода (Ин 3:16).",
                    )),
            )
            .child(Input::new(&form.refs_input))
            .child(
                Button::new("chain-add-verse")
                    .xsmall()
                    .ghost()
                    .label(crate::i18n::tr_static("+ Добавить ссылку на стих"))
                    .on_click({
                        let input = form.refs_input.clone();
                        move |_, window, cx| {
                            crate::verse_picker::append_to_field(
                                input.clone(),
                                "Добавить ссылку на стих",
                                window,
                                cx,
                            )
                        }
                    }),
            )
            .child(Textarea::new(&form.assertion_input).h(px(70.)))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("chain-form-submit")
                            .xsmall()
                            .label(crate::i18n::tr_static("Сохранить"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.submit_form(cx);
                            })),
                    )
                    .child(
                        Button::new("chain-form-cancel")
                            .xsmall()
                            .ghost()
                            .label(crate::i18n::tr_static("Отмена"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.cancel_form(cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_node(
        &self,
        view: &shell::chains::ChainNodeView,
        form_under: Option<&PendingForm>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (muted, border, accent_bg) = {
            let theme = cx.theme();
            (
                theme.muted_foreground,
                theme.border,
                theme.accent.opacity(0.12),
            )
        };
        let indent = px(14. * view.depth as f32);

        let preview = self
            .open
            .as_ref()
            .and_then(|open| open.texts.get(&view.node.id));
        let mut chips = v_flex().gap_1();
        for (index, label) in view.labels.iter().enumerate() {
            let reference = label.reference.clone();
            let verse = preview
                .and_then(|verses| verses.get(index))
                .and_then(Option::as_ref);
            let module = verse.map(|verse| verse.module.clone());
            let row_id = SharedString::from(format!("chain-ref-{}-{index}", view.node.id));
            let title = if let Some(module) = &module {
                format!("{} · {module}", label.label)
            } else {
                label.label.clone()
            };
            let link = div()
                .id(row_id.clone())
                .px_1()
                .rounded_sm()
                .bg(accent_bg)
                .cursor_pointer()
                .text_size(px(crate::ui_scale::left_font_size(cx, 12.)))
                .on_click(cx.listener(move |_, _, _, cx| {
                    if let Some(module) = &module {
                        navigation::request_open_book(
                            cx,
                            OpenBook {
                                module: module.clone(),
                                book: BookNumber(reference.book_number),
                                chapter: Some(reference.chapter),
                                verse: reference.verse,
                            },
                        );
                    }
                }))
                .child(title);
            let text = match (verse, preview.is_some()) {
                (Some(verse), _) => verse.text.as_str(),
                (None, true) => "Место отсутствует в выбранном модуле.",
                (None, false) => "Загрузка текста…",
            };
            chips = chips.child(
                v_flex().gap_1().child(link).child(
                    div()
                        .text_size(px(crate::ui_scale::left_font_size(cx, 14.)))
                        .when(crate::bidi::is_hebrew_paragraph(text), |text| {
                            text.text_right()
                        })
                        .child(SelectableText::new(
                            (ElementId::Name(row_id), "text"),
                            text.to_owned(),
                        )),
                ),
            );
        }

        let mut markers = h_flex().gap_2();
        if view.parents > 1 {
            markers = markers.child(
                div()
                    .text_size(px(crate::ui_scale::left_font_size(cx, 12.0)))
                    .text_color(muted)
                    .child(crate::i18n::tr_static("⩒ слияние")),
            );
        }
        if view.children > 1 {
            markers = markers.child(
                div()
                    .text_size(px(crate::ui_scale::left_font_size(cx, 12.0)))
                    .text_color(muted)
                    .child(crate::i18n::tr_static("⩓ ветвь")),
            );
        }

        let node_id = view.node.id.clone();
        let before_id = view.node.id.clone();
        let outgoing = self.open.iter().flat_map(|open| {
            open.view.edges.iter().filter_map(|edge| {
                if edge.source_node_id != view.node.id {
                    return None;
                }
                let target = open
                    .view
                    .nodes
                    .iter()
                    .find(|node| node.node.id == edge.target_node_id)?;
                let label = target
                    .labels
                    .first()
                    .map(|label| label.label.as_str())
                    .or_else(|| {
                        (!target.node.assertion.is_empty())
                            .then_some(target.node.assertion.as_str())
                    })
                    .unwrap_or("узел");
                Some((
                    edge.source_node_id.clone(),
                    edge.target_node_id.clone(),
                    label.chars().take(34).collect::<String>(),
                ))
            })
        });
        let node_id_for_delete = view.node.id.clone();
        let assertion_id = SharedString::from(format!("chain-assertion-{}", view.node.id));
        let assertion = if view.node.assertion.trim().is_empty() {
            "(без утверждения)".to_string()
        } else {
            view.node.assertion.clone()
        };

        let mut column = v_flex()
            .w_full()
            .gap_1()
            .child(chips)
            .child(self.render_module_picker(Some(view), cx))
            .child(
                div()
                    .text_size(px(crate::ui_scale::left_font_size(cx, 14.0)))
                    .child(SelectableText::new(assertion_id, assertion).document_order(1)),
            )
            .child(markers)
            .child(
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .child(
                        Button::new(SharedString::from(format!("chain-add-before-{before_id}")))
                            .xsmall()
                            .ghost()
                            .label(crate::i18n::tr_static("+ До"))
                            .tooltip(crate::i18n::tr_static("Добавить предпосылку перед узлом"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.start_form(
                                    Placement::Before {
                                        target: before_id.clone(),
                                    },
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("chain-add-after-{node_id}")))
                            .xsmall()
                            .ghost()
                            .label(crate::i18n::tr_static("+ После"))
                            .tooltip(crate::i18n::tr_static("Добавить следствие после узла"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.start_form(
                                    Placement::Conclusion {
                                        parent: node_id.clone(),
                                    },
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "chain-edit-{node_id_for_delete}"
                        )))
                        .xsmall()
                        .ghost()
                        .label(crate::i18n::tr_static("Изменить"))
                        .on_click(cx.listener({
                            let node_id = node_id_for_delete.clone();
                            move |this, _, window, cx| {
                                this.start_form(
                                    Placement::Edit {
                                        node: node_id.clone(),
                                    },
                                    window,
                                    cx,
                                );
                            }
                        })),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "chain-node-del-{node_id_for_delete}"
                        )))
                        .xsmall()
                        .ghost()
                        .label("✕")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let id = node_id_for_delete.clone();
                            this.delete_node(&id, cx);
                        })),
                    ),
            );
        for (source, target, label) in outgoing {
            column = column.child(
                Button::new(SharedString::from(format!("chain-split-{source}-{target}")))
                    .xsmall()
                    .ghost()
                    .label(format!("В разрыв → {label}"))
                    .tooltip(crate::i18n::tr_static("Вставить узел в разрыв этой связи"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.start_form(
                            Placement::Between {
                                source: source.clone(),
                                target: target.clone(),
                            },
                            window,
                            cx,
                        );
                    })),
            );
        }
        if let Some(form) = form_under {
            column = column.child(self.render_form(form, cx));
        }

        div()
            .w_full()
            .pl(indent)
            .py_1()
            .child(
                div()
                    .w_full()
                    .p_2()
                    .border_1()
                    .border_color(border)
                    .rounded_md()
                    .child(column),
            )
            .into_any_element()
    }

    fn render_open(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (muted, border, danger) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.border, theme.danger)
        };
        if self.open.is_none() {
            return self.render_index(cx);
        }
        let Some(open) = &self.open else {
            unreachable!("open проверен выше");
        };

        let title_block: AnyElement = if open.renaming {
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&self.rename_input)),
                )
                .child(
                    Button::new("chain-rename-save")
                        .xsmall()
                        .label(crate::i18n::tr_static("Ок"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.submit_rename(window, cx);
                        })),
                )
                .into_any_element()
        } else {
            let title = if open.view.title.trim().is_empty() {
                "(без названия)".to_string()
            } else {
                open.view.title.clone()
            };
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(crate::ui_scale::left_font_size(cx, 14.0)))
                .overflow_hidden()
                .whitespace_nowrap()
                .child(title)
                .into_any_element()
        };

        let header = h_flex()
            .w_full()
            .items_center()
            .gap_1()
            .p_2()
            .border_b_1()
            .border_color(border)
            .child(
                Button::new("chain-back")
                    .xsmall()
                    .ghost()
                    .label("←")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.open = None;
                        this.reload_list(cx);
                    })),
            )
            .child(title_block)
            .child(
                Button::new("chain-rename")
                    .xsmall()
                    .ghost()
                    .label("✎")
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(open) = &this.open {
                            let title = open.view.title.clone();
                            this.rename_input.update(cx, |input, cx| {
                                input.set_value(title, window, cx);
                            });
                        }
                        if let Some(open) = &mut this.open {
                            open.renaming = true;
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("chain-delete")
                    .xsmall()
                    .ghost()
                    .label("🗑")
                    .on_click(cx.listener(|this, _, window, cx| {
                        let Some(open) = &this.open else { return };
                        this.confirm_delete_chain(open.view.id.clone(), true, window, cx);
                    })),
            );

        // Форма нового корня живёт над списком; формы узлов — под своим узлом.
        let root_form = open
            .form
            .as_ref()
            .filter(|form| matches!(form.placement, Placement::Root));

        let mut nodes_column = v_flex()
            .w_full()
            .p_2()
            .gap_1()
            .child(self.render_module_picker(None, cx));
        nodes_column = nodes_column.child(
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .child(
                    Button::new("chain-add-root")
                        .xsmall()
                        .label(crate::i18n::tr_static("+ Узел"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.start_form(Placement::Root, window, cx);
                        })),
                )
                .child(
                    div()
                        .text_size(px(crate::ui_scale::left_font_size(cx, 12.0)))
                        .text_color(muted)
                        .child(format!("узлов: {}", open.view.nodes.len())),
                ),
        );
        if let Some(form) = root_form {
            nodes_column = nodes_column.child(self.render_form(form, cx));
        }
        if open.view.nodes.is_empty() && root_form.is_none() {
            nodes_column = nodes_column.child(
                div()
                    .py_3()
                    .text_size(px(crate::ui_scale::left_font_size(cx, 14.0)))
                    .text_color(muted)
                    .child(crate::i18n::tr_static(
                        "Пустая цепочка. Добавьте первый узел — ссылку на стих и утверждение.",
                    )),
            );
        }
        for view in &open.view.nodes {
            let form_under = open.form.as_ref().filter(|form| match &form.placement {
                Placement::Conclusion { parent } => parent == &view.node.id,
                Placement::Before { target } => target == &view.node.id,
                Placement::Between { source, .. } => source == &view.node.id,
                Placement::Edit { node } => node == &view.node.id,
                Placement::Root => false,
            });
            nodes_column = nodes_column.child(self.render_node(view, form_under, cx));
        }

        let mut column = v_flex().size_full().min_h_0().child(header).child(
            div()
                .id("chains-open-scroll")
                .flex_1()
                .min_h_0()
                .child(nodes_column)
                .overflow_y_scrollbar(),
        );
        if let Some(error) = &self.error {
            column = column.child(
                div()
                    .p_2()
                    .text_size(px(crate::ui_scale::left_font_size(cx, 12.0)))
                    .text_color(danger)
                    .child(error.clone()),
            );
        }
        column.into_any_element()
    }
}

/// Разобрать «Ин 3:16, Рим 5:8-10» в ссылки; Err — кусок, который не
/// разобрался (показываем пользователю как есть). pub(crate) — пикер
/// «стих → цепочка» в reader.rs пользуется тем же разбором.
pub(crate) fn parse_refs_input(
    services: &shell::AppServices,
    input: &str,
) -> std::result::Result<Vec<Reference>, String> {
    let mut refs = Vec::new();
    for piece in input.split([',', ';']) {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        let parsed = bible_domain::chains::parse_compact_reference(piece)
            .or_else(|| reference::parse(piece, |name| services.registry.lock().lookup_book(name)));
        match parsed {
            Some(reference) => refs.push(reference),
            None => return Err(piece.to_string()),
        }
    }
    Ok(refs)
}

impl EventEmitter<PanelEvent> for ChainsPanel {}

impl Focusable for ChainsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for ChainsPanel {
    fn panel_name(&self) -> &'static str {
        Self::NAME
    }
}

impl Panel for ChainsPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        crate::i18n::tr_static("Цепочки")
    }
}

impl Render for ChainsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Ошибка показывается один раз и гаснет при следующем действии —
        // иначе старый текст висел бы вечно.
        let body = if self.open.is_some() {
            self.render_open(cx)
        } else {
            self.render_index(cx)
        };
        div()
            .size_full()
            .when(!crate::ui_scale::left_font_family(cx).is_empty(), |root| {
                root.font_family(crate::ui_scale::left_font_family(cx))
            })
            .child(body)
    }
}
