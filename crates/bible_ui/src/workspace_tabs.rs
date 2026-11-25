use std::{cell::Cell, rc::Rc, sync::Arc};

use shell::settings::TabSize;

use gpui_kit::base::{ResizeHandleContext, h_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{
    AnyDrag, BasePanelView, DockArea, DockAreaRenderer, DockContext, DockPlacement, DockSkin,
    DragPanel, DropIndicator, InsertTarget, NodeId, PaneRef, PanelHandle, PanelState,
    TabGroupContext, TabGroupRenderer,
};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Selectable as _, Sizable as _, Size as ControlSize,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{reader::ReaderPanel, services::app_services, workspace::WorkspaceView};

#[derive(Clone, Copy)]
pub(crate) enum NewTab {
    Bible,
    Dictionary,
    Note,
}

pub(crate) fn dock_area(
    id: &'static str,
    version: usize,
    window: &mut Window,
    cx: &mut Context<WorkspaceView>,
) -> (Entity<DockArea>, Rc<DockSkin>) {
    let workspace = cx.weak_entity();
    let mut skin = None;
    let area = cx.new(|cx| {
        let base = DockSkin::new(cx);
        skin = Some(base.clone());
        let tabs = Rc::new(WorkspaceTabBar {
            base: base.tab_group_renderer(),
            area: cx.weak_entity(),
            workspace,
        });
        DockArea::new(id, Some(version), window, cx)
            .with_renderer(Rc::new(WorkspaceDock { base, tabs }))
    });
    (area, skin.expect("dock skin initialized"))
}

struct WorkspaceDock {
    base: Rc<DockSkin>,
    tabs: Rc<WorkspaceTabBar>,
}

impl DockAreaRenderer for WorkspaceDock {
    fn frame(&self, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.base.frame(window, cx)
    }
    fn split_frame(
        &self,
        node: NodeId,
        axis: Axis,
        window: &mut Window,
        cx: &mut App,
    ) -> Stateful<Div> {
        self.base.split_frame(node, axis, window, cx)
    }
    fn center_frame(&self, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.base.center_frame(window, cx)
    }
    fn render_split_handle(
        &self,
        handle: &ResizeHandleContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        self.base.render_split_handle(handle, window, cx)
    }
    fn closed_dock_extent(&self, placement: DockPlacement) -> Pixels {
        self.base.closed_dock_extent(placement)
    }
    fn render_dock(
        &self,
        dock: &DockContext,
        content: AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        self.base.render_dock(dock, content, window, cx)
    }
    fn build_placeholder(
        &self,
        state: &PanelState,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Arc<dyn BasePanelView>> {
        self.base.build_placeholder(state, window, cx)
    }
    fn tab_group_renderer(&self) -> Rc<dyn TabGroupRenderer> {
        self.tabs.clone()
    }
}

struct WorkspaceTabBar {
    base: Rc<dyn TabGroupRenderer>,
    area: WeakEntity<DockArea>,
    workspace: WeakEntity<WorkspaceView>,
}

fn panel_title(panel: &Arc<dyn BasePanelView>, window: &mut Window, cx: &mut App) -> AnyElement {
    match PanelHandle::of(panel) {
        Some(handle) => match handle.tab_name(cx) {
            Some(name) => name.into_any_element(),
            None => handle.title(window, cx),
        },
        None => panel.panel_name(cx).into_any_element(),
    }
}

fn center_tab_group_count(area: &DockArea) -> usize {
    area.layout(DockPlacement::Center)
        .map(|tree| {
            tree.node_ids()
                .into_iter()
                .filter(|node| {
                    tree.find_node(*node)
                        .is_some_and(|node| matches!(node.kind(), PaneRef::Tabs { .. }))
                })
                .count()
        })
        .unwrap_or_default()
}

/// Collapse the centre split into the selected tab group without removing
/// any panels. Moving panels through the dock API preserves their entities,
/// navigation history, editor state, and persisted tab order.
fn merge_center_into(
    area: &WeakEntity<DockArea>,
    target: NodeId,
    window: &mut Window,
    cx: &mut App,
) {
    let _ = area.update(cx, |area, cx| {
        let moving = area
            .layout(DockPlacement::Center)
            .map(|tree| {
                tree.panels()
                    .filter(|panel| tree.find_panel_node(*panel) != Some(target))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        for panel in moving {
            area.move_panel(
                panel,
                InsertTarget::Tabs {
                    node: target,
                    ix: None,
                    activate: false,
                },
                window,
                cx,
            );
        }
    });
}

struct TabDragPreview(Arc<dyn BasePanelView>);

impl Render for TabDragPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .max_w(px(200.))
            .truncate()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().tab_active)
            .child(panel_title(&self.0, window, cx))
    }
}

fn sized_tabs(
    group: &TabGroupContext,
    size: ControlSize,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let scroll = window.use_keyed_state(
        ("workspace-tab-scroll", group.node().as_u64()),
        cx,
        |_, _| (ScrollHandle::new(), Cell::new(None)),
    );
    let (handle, previous) = scroll.read(cx);
    let active = group.active_ix();
    let visible_index = group
        .panels()
        .iter()
        .take(active)
        .filter(|panel| panel.visible(cx))
        .count();
    if previous.replace(Some(active)) != Some(active) {
        handle.scroll_to_item(visible_index);
    }
    let handle = handle.clone();
    let active_panel = group.active_panel().cloned();
    let selected = active_panel.as_ref().map(|panel| panel.panel_id(cx));
    let visible: Vec<_> = group
        .panels()
        .iter()
        .enumerate()
        .filter(|(_, panel)| panel.visible(cx))
        .collect();
    let tabs = visible
        .into_iter()
        .enumerate()
        .map(|(visible_index, (index, panel))| {
            let id = panel.panel_id(cx);
            let title = panel_title(panel, window, cx);
            Tab::new()
                .child(title)
                .selected(selected == Some(id))
                .when(visible_index > 0, |tab| {
                    tab.prefix(div().w(px(1.)).h_4().bg(cx.theme().border.opacity(0.85)))
                })
                .when(group.is_panel_closable(id, cx), |tab| {
                    let group = group.clone();
                    tab.on_mouse_down(MouseButton::Middle, move |_, window, cx| {
                        cx.stop_propagation();
                        group.close(id, window, cx);
                    })
                })
                .when(group.is_panel_closable(id, cx), |tab| {
                    let group = group.clone();
                    tab.suffix(
                        Button::new(("workspace-sized-tab-close", index))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .accessibility_label(crate::i18n::tr(cx, "Закрыть вкладку"))
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                group.close(id, window, cx);
                            }),
                    )
                })
                .on_click({
                    let group = group.clone();
                    move |_, window, cx| group.select_tab(index, window, cx)
                })
                .when_some(
                    group
                        .is_draggable()
                        .then(|| group.drag_panel(index, cx))
                        .flatten(),
                    |tab, drag| {
                        let panel = panel.clone();
                        tab.on_drag(drag, move |drag, offset, _, cx| {
                            cx.stop_propagation();
                            drag.set_drag_offset(offset);
                            cx.new(|_| TabDragPreview(panel.clone()))
                        })
                    },
                )
                .when(group.is_droppable(), |tab| {
                    tab.drag_over::<DragPanel>(|tab, _, _, cx| {
                        tab.bg(cx.theme().tokens.drop_target)
                    })
                    .on_drop({
                        let group = group.clone();
                        move |drag: &DragPanel, window, cx| {
                            cx.stop_propagation();
                            group.drop_panel(drag.clone(), Some(index), true, window, cx);
                        }
                    })
                    .drag_over::<AnyDrag>(|tab, _, _, cx| tab.bg(cx.theme().tokens.drop_target))
                    .on_drop({
                        let group = group.clone();
                        move |drag: &AnyDrag, window, cx| {
                            cx.stop_propagation();
                            group.drop_item(drag.clone(), None, window, cx);
                        }
                    })
                })
        })
        .collect::<Vec<_>>();
    let drop_target = div()
        .id("workspace-tab-end")
        .h_full()
        .flex_1()
        .min_w_12()
        .when(group.is_droppable(), |target| {
            target
                .drag_over::<DragPanel>(|target, _, _, cx| target.bg(cx.theme().tokens.drop_target))
                .on_drop({
                    let group = group.clone();
                    move |drag: &DragPanel, window, cx| {
                        cx.stop_propagation();
                        let index = (drag.source() == group.node())
                            .then(|| group.panels().len().saturating_sub(1));
                        group.drop_panel(drag.clone(), index, false, window, cx);
                    }
                })
                .drag_over::<AnyDrag>(|target, _, _, cx| target.bg(cx.theme().tokens.drop_target))
                .on_drop({
                    let group = group.clone();
                    move |drag: &AnyDrag, window, cx| {
                        cx.stop_propagation();
                        group.drop_item(drag.clone(), None, window, cx);
                    }
                })
        });
    let suffix = active_panel
        .as_ref()
        .and_then(PanelHandle::of)
        .map(|panel| {
            let panel = panel.panel();
            let zoom = panel.zoomable(cx) && panel.zoom_control(cx).is_some();
            let can_close = group.is_closable();
            let group = group.clone();
            h_flex()
                .gap_1()
                .px_2()
                .items_center()
                .children(panel.title_suffix(window, cx))
                .children(panel.toolbar_buttons(window, cx).into_iter().flatten())
                .child(
                    Button::new("workspace-sized-tab-menu")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Ellipsis)
                        .accessibility_label(crate::i18n::tr(cx, "Действия вкладки"))
                        .dropdown_menu(move |mut menu, window, cx| {
                            menu = panel.dropdown_menu(menu, window, cx);
                            if zoom || group.is_zoomed() {
                                let group = group.clone();
                                let label = if group.is_zoomed() {
                                    crate::i18n::tr(cx, "Восстановить размер")
                                } else {
                                    crate::i18n::tr(cx, "Развернуть вкладку")
                                };
                                menu =
                                    menu.item(PopupMenuItem::new(label).on_click(
                                        move |_, window, cx| group.toggle_zoom(window, cx),
                                    ));
                            }
                            if can_close {
                                let id = panel.panel_id(cx);
                                let group = group.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(crate::i18n::tr(cx, "Закрыть вкладку"))
                                        .on_click(move |_, window, cx| group.close(id, window, cx)),
                                );
                            }
                            menu
                        }),
                )
        });
    let bar = TabBar::new("workspace-sized-tabs")
        .with_size(size)
        .track_scroll(&handle)
        .children(tabs)
        .last_empty_space(drop_target)
        .when_some(suffix, |bar, suffix| bar.suffix(suffix));

    // The individual tabs provide exact insertion slots. The full strip is
    // also a drop target so a transfer does not turn into a split merely
    // because the pointer landed in padding, between tabs, or beside the last
    // tab. This is especially important across two existing split groups.
    div()
        .id(("workspace-tab-strip-drop", group.node().as_u64()))
        .w_full()
        .min_w_0()
        .h_full()
        .when(group.is_droppable(), |strip| {
            strip
                .drag_over::<DragPanel>(|strip, _, _, cx| strip.bg(cx.theme().tokens.drop_target))
                .on_drop({
                    let group = group.clone();
                    move |drag: &DragPanel, window, cx| {
                        cx.stop_propagation();
                        group.drop_panel(drag.clone(), None, true, window, cx);
                    }
                })
                .drag_over::<AnyDrag>(|strip, _, _, cx| strip.bg(cx.theme().tokens.drop_target))
                .on_drop({
                    let group = group.clone();
                    move |drag: &AnyDrag, window, cx| {
                        cx.stop_propagation();
                        group.drop_item(drag.clone(), None, window, cx);
                    }
                })
        })
        .child(bar)
        .into_any_element()
}

impl TabGroupRenderer for WorkspaceTabBar {
    fn frame(&self, group: &TabGroupContext, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.base.frame(group, window, cx)
    }
    fn content_frame(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Stateful<Div> {
        self.base.content_frame(group, window, cx)
    }
    fn render_tab_bar(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let node = group.node();
        let central = self.area.upgrade().is_some_and(|area| {
            area.read(cx)
                .layout(DockPlacement::Center)
                .is_some_and(|tree| tree.find_node(node).is_some())
        });
        if !central {
            return self.base.render_tab_bar(group, window, cx);
        }
        let size = match app_services(cx).settings.read().tab_size {
            TabSize::Compact => ControlSize::Small,
            TabSize::Normal => ControlSize::Medium,
            TabSize::Large => ControlSize::Large,
        };
        let bar = sized_tabs(group, size, window, cx);
        // Обычная вкладка GPUI-kit прозрачна и наследует фон TabBar.
        // Выраженная примесь foreground отделяет неактивные вкладки от
        // остального интерфейса на светлых и тёмных темах, не меняя
        // отдельный цвет активной вкладки (`tab_active`).
        let inactive_tab_background = cx
            .theme()
            .tokens
            .tab_bar
            .blend(cx.theme().foreground.opacity(0.16));

        let has_split = self
            .area
            .upgrade()
            .is_some_and(|area| center_tab_group_count(area.read(cx)) > 1);

        let workspace = self.workspace.clone();
        let origin = group
            .active_panel()
            .and_then(|panel| panel.view().downcast::<ReaderPanel>().ok())
            .map(|reader| reader.downgrade());
        let plus = Button::new(("workspace-new-tab", node.as_u64()))
            .xsmall()
            .ghost()
            .icon(IconName::Plus)
            .tooltip(crate::i18n::tr(cx, "Новая вкладка"))
            .dropdown_menu(move |mut menu, _, cx| {
                for (label, kind) in [
                    (crate::i18n::tr(cx, "Библия"), NewTab::Bible),
                    (crate::i18n::tr(cx, "Словарь"), NewTab::Dictionary),
                    (crate::i18n::tr(cx, "Заметка"), NewTab::Note),
                ] {
                    let workspace = workspace.clone();
                    let origin = origin.clone();
                    menu = menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        let _ = workspace.update(cx, |workspace, cx| {
                            workspace.open_new_tab_in(
                                kind,
                                node,
                                origin.as_ref().and_then(WeakEntity::upgrade),
                                window,
                                cx,
                            )
                        });
                    }));
                }
                menu
            });
        let close_split = has_split.then(|| {
            let area = self.area.clone();
            Button::new(("workspace-close-split", node.as_u64()))
                .xsmall()
                .ghost()
                .icon(IconName::PanelLeftClose)
                .tooltip(crate::i18n::tr(cx, "Закрыть разделение"))
                .on_click(move |_, window, cx| merge_center_into(&area, node, window, cx))
        });
        h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .bg(inactive_tab_background)
            .child(div().flex_1().min_w_0().child(bar))
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .px_1()
                    .children(close_split)
                    .child(plus),
            )
            .into_any_element()
    }
    fn render_active_panel(
        &self,
        panel: AnyView,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        self.base.render_active_panel(panel, group, window, cx)
    }
    fn render_drop_indicator(
        &self,
        indicator: DropIndicator,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        self.base.render_drop_indicator(indicator, window, cx)
    }
    fn render_empty(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        self.base.render_empty(group, window, cx)
    }
}
