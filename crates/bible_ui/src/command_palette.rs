//! Командная палитра (Ctrl+Shift+P, architecture.md §6.5): все команды
//! приложения в одном `component::command` — поиск с фильтром, навигация
//! стрелками, Enter выполняет.
//!
//! Исполнение команд — `WorkspaceView::execute_command`: у палитры нет
//! доступа к докам, а диспатч действий из диалогового слоя не дошёл бы до
//! корневого div окна (слой диалогов — не его потомок), поэтому палитра
//! зовёт workspace напрямую через WeakEntity. Список команд — единственное
//! место расширения: новая строка в ENTRIES + ветка в execute_command.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::base::v_flex;
use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::dock::DockPlacement;
use gpui_kit::component::{ActiveTheme as _, WindowExt as _};
use gpui_kit::*;

use crate::workspace::WorkspaceView;

/// Команда палитры — исполняет WorkspaceView::execute_command.
#[derive(Debug, Clone, Copy)]
pub enum PaletteCommand {
    VerseJump,
    HistoryBack,
    HistoryForward,
    OpenSettings,
    OpenChains,
    NewReadingTab,
    ToggleDock(DockPlacement),
}

struct PaletteEntry {
    label: &'static str,
    icon: assets::IconName,
    keywords: &'static [&'static str],
    command: PaletteCommand,
}

/// Все команды приложения. keywords — для фильтра палитры (рус/англ).
const ENTRIES: &[PaletteEntry] = &[
    PaletteEntry {
        label: "Перейти к стиху…",
        icon: assets::IconName::BookOpenText,
        keywords: &["go", "verse", "jump", "стих", "переход", "ctrl+p"],
        command: PaletteCommand::VerseJump,
    },
    PaletteEntry {
        label: "Новая вкладка чтения",
        icon: assets::IconName::Plus,
        keywords: &[
            "new",
            "tab",
            "reading",
            "вкладка",
            "чтение",
            "новая",
            "ctrl+t",
        ],
        command: PaletteCommand::NewReadingTab,
    },
    PaletteEntry {
        label: "Назад по истории",
        icon: assets::IconName::ArrowLeft,
        keywords: &["history", "back", "назад", "история"],
        command: PaletteCommand::HistoryBack,
    },
    PaletteEntry {
        label: "Вперёд по истории",
        icon: assets::IconName::ArrowRight,
        keywords: &["history", "forward", "вперёд", "история"],
        command: PaletteCommand::HistoryForward,
    },
    PaletteEntry {
        label: "Настройки…",
        icon: assets::IconName::Settings,
        keywords: &["settings", "preferences", "настройки"],
        command: PaletteCommand::OpenSettings,
    },
    PaletteEntry {
        label: "Цепочки",
        icon: assets::IconName::Link,
        keywords: &["chains", "цепочки", "логика", "вывод"],
        command: PaletteCommand::OpenChains,
    },
    PaletteEntry {
        label: "Левая панель",
        icon: assets::IconName::PanelLeft,
        keywords: &["panel", "dock", "библиотека", "заметки", "панель"],
        command: PaletteCommand::ToggleDock(DockPlacement::Left),
    },
    PaletteEntry {
        label: "Нижняя панель",
        icon: assets::IconName::PanelBottom,
        keywords: &["panel", "dock", "поиск", "панель"],
        command: PaletteCommand::ToggleDock(DockPlacement::Bottom),
    },
    PaletteEntry {
        label: "Правая панель",
        icon: assets::IconName::PanelRight,
        keywords: &["panel", "dock", "словарь", "контекст", "панель"],
        command: PaletteCommand::ToggleDock(DockPlacement::Right),
    },
];

/// Открыть палитру поверх окна. Состояние создаётся до open_dialog (builder
/// может пересобираться); фокус в поле поиска — один раз через defer,
/// паттерн из command_story gpui-kit.
pub fn open(window: &mut Window, cx: &mut App, view: WeakEntity<WorkspaceView>) {
    let state = cx.new(|cx| CommandState::new(window, cx));
    let focus_on_mount = Rc::new(Cell::new(true));

    window.open_dialog(cx, move |dialog, _, _| {
        let state = state.clone();
        let focus_on_mount = focus_on_mount.clone();
        let view = view.clone();

        dialog
            .close_button(false)
            .p_0()
            .width(px(480.))
            .content(move |content, window, cx| {
                if focus_on_mount.replace(false) {
                    let state = state.clone();
                    window.defer(cx, move |window, cx| {
                        state.read(cx).focus_handle(cx).focus(window, cx);
                    });
                }
                content.child(
                    Command::new(&state)
                        .bordered(false)
                        .placeholder(crate::i18n::tr(cx, "Команда…"))
                        .items(ENTRIES.iter().map(|entry| {
                            CommandItem::new()
                                .label(crate::i18n::tr(cx, entry.label))
                                .icon(entry.icon)
                                .keywords(entry.keywords.iter().copied())
                        }))
                        .empty(|_, _, cx| {
                            v_flex()
                                .w_full()
                                .items_center()
                                .py_6()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(crate::i18n::tr(cx, "Ничего не найдено"))
                        })
                        // Индекс — «оригинальный» (до фильтра), поэтому строки
                        // одной безгрупповой секции совпадают с ENTRIES.
                        .on_confirm({
                            let view = view.clone();
                            move |index, window, cx| {
                                let Some(command) =
                                    ENTRIES.get(index.row).map(|entry| entry.command)
                                else {
                                    return;
                                };
                                window.close_dialog(cx);
                                _ = view.update(cx, |this, cx| {
                                    this.execute_command(command, window, cx);
                                });
                            }
                        }),
                )
            })
    });
}
