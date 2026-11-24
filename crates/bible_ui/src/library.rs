//! Панель «Библиотека»: справочные модули — словари, комментарии,
//! параллельные ссылки (architecture.md §6.3).
//!
//! Переводы здесь не показываются: перевод выбирается в шапке панели
//! чтения. Открытие статей словарей и комментариев появится на этапах 4–5;
//! сейчас панель — каталог, сгруппированный по типу модуля.

use gpui_kit::base::v_flex;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use bible_domain::ports::{ModuleCatalog, ModuleKind, ModuleMeta};

use crate::navigation;
use crate::services::app_services;

pub struct LibraryPanel {
    focus_handle: FocusHandle,
    _subscription: Subscription,
}

impl LibraryPanel {
    pub const NAME: &'static str = "library";

    pub fn new(cx: &mut Context<Self>) -> Self {
        // Каталог пересканирован из настроек — список перечитается в render.
        let subscription = cx.subscribe(
            &navigation::bus(cx),
            |_, _, _: &navigation::SettingsChanged, cx| {
                cx.notify();
            },
        );
        Self {
            focus_handle: cx.focus_handle(),
            _subscription: subscription,
        }
    }

    fn render_section(
        &self,
        title: &str,
        modules: &[ModuleMeta],
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if modules.is_empty() {
            return None;
        }
        let mut section = v_flex().w_full().gap(px(1.)).child(
            div()
                .px_2()
                .pt_3()
                .pb_1()
                .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child(title.to_string()),
        );
        for module in modules {
            let label = module
                .description
                .clone()
                .unwrap_or_else(|| module.id.to_string());
            let module_id = module.id.clone();
            section = section.child(
                div()
                    .id(ElementId::Name(format!("module-{}", module.id).into()))
                    .w_full()
                    .min_w_0()
                    .px_2()
                    .py(px(2.))
                    .rounded_md()
                    .cursor_pointer()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                    .text_color(cx.theme().foreground)
                    .hover(|style| style.bg(cx.theme().accent.opacity(0.12)))
                    .child(label)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        navigation::request_open_module(cx, module_id.clone());
                    })),
            );
        }
        Some(section.into_any_element())
    }
}

impl EventEmitter<PanelEvent> for LibraryPanel {}

impl Focusable for LibraryPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for LibraryPanel {
    fn panel_name(&self) -> &'static str {
        Self::NAME
    }
}

impl Panel for LibraryPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        crate::i18n::tr_static("Библиотека")
    }
}

impl Render for LibraryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let modules = app_services(cx).registry.modules();
        let by_kind = |kinds: &[ModuleKind]| -> Vec<ModuleMeta> {
            modules
                .iter()
                .filter(|module| kinds.contains(&module.kind))
                .cloned()
                .collect()
        };

        let dictionaries = by_kind(&[ModuleKind::StrongsDictionary, ModuleKind::GeneralDictionary]);
        let commentaries = by_kind(&[ModuleKind::Commentary]);
        let cross_references = by_kind(&[ModuleKind::CrossReferences]);

        let mut list = v_flex().w_full().min_w_0().p_1();
        let mut any = false;
        for (title, group) in [
            (crate::i18n::tr_static("Словари"), dictionaries),
            (crate::i18n::tr_static("Комментарии"), commentaries),
            (
                crate::i18n::tr_static("Параллельные ссылки"),
                cross_references,
            ),
        ] {
            if let Some(section) = self.render_section(title, &group, cx) {
                list = list.child(section);
                any = true;
            }
        }

        if !any {
            let modules_dir = app_services(cx).settings.read().modules_dir.clone();
            list = list.child(
                div()
                    .p_3()
                    .text_size(px(crate::ui_scale::panel_font_size(cx, 14.0)))
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "Модули не найдены. Проверьте путь в настройках: {}",
                        modules_dir.display()
                    )),
            );
        }

        div()
            .id("library-scroll")
            .size_full()
            .when(!crate::ui_scale::panel_font_family(cx).is_empty(), |root| {
                root.font_family(crate::ui_scale::panel_font_family(cx))
            })
            .child(list)
            .overflow_y_scrollbar()
    }
}
