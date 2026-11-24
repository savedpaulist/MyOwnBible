//! Панели приложения (architecture.md §6).
//!
//! Все панели полноценные, в отдельных модулях; здесь — только регистрация
//! в PanelRegistry, чтобы раскладка и вкладки восстанавливались между
//! запусками (panel_name — ключ восстановления, менять нельзя).

use gpui_kit::component::dock::{panel_handle, register_panel};
use gpui_kit::{App, AppContext as _};

use crate::article_tab::{ArticleTab, ArticleTabPayload};
use crate::chains::ChainsPanel;
use crate::context::ContextPanel;
use crate::library::LibraryPanel;
use crate::note_tab::{NoteTab, NoteTabPayload};
use crate::notes::NotesPanel;
use crate::reader::ReaderPanel;
pub use crate::search::SearchPanel;

/// Регистрация всех типов панелей для восстановления раскладки.
/// Вызывается один раз при старте приложения, до открытия окон.
pub fn register_all(cx: &mut App) {
    register_panel(
        cx,
        crate::dictionary::DictionaryBrowser::NAME,
        |_, window, cx| {
            panel_handle(cx.new(|cx| crate::dictionary::DictionaryBrowser::new(window, cx)))
        },
    );
    register_panel(cx, LibraryPanel::NAME, |_, _, cx| {
        panel_handle(cx.new(LibraryPanel::new))
    });
    register_panel(cx, ReaderPanel::NAME, |context, window, cx| {
        // Восстановление вкладки чтения (§7.2): payload (место + стек
        // истории) из layout.json; payload отсутствует или битый —
        // наследуем место из session.json (миграция с MVP).
        let payload = match context.info() {
            gpui_kit::component::dock::PanelInfo::Panel(value) => {
                serde_json::from_value::<crate::reader::ReaderTabPayload>(value.clone()).ok()
            }
            _ => None,
        };
        match payload {
            Some(payload) if payload.v >= 1 => {
                panel_handle(cx.new(|cx| ReaderPanel::new_restored(window, cx, payload)))
            }
            _ => panel_handle(cx.new(|cx| ReaderPanel::new(window, cx))),
        }
    });
    // Вкладки статей/комментариев (этап 15.2): payload — вид + адрес
    // содержимого, контент перечитывается из модуля при восстановлении.
    register_panel(cx, ArticleTab::NAME, |context, window, cx| {
        let payload = match context.info() {
            gpui_kit::component::dock::PanelInfo::Panel(value) => {
                serde_json::from_value::<ArticleTabPayload>(value.clone()).ok()
            }
            _ => None,
        };
        match payload {
            Some(payload) if payload.v >= 1 => {
                panel_handle(cx.new(|cx| ArticleTab::new_restored(window, cx, payload)))
            }
            _ => panel_handle(cx.new(|cx| {
                ArticleTab::new_restored(
                    window,
                    cx,
                    ArticleTabPayload {
                        v: 1,
                        kind: "dict".to_string(),
                        module: String::new(),
                        topic: None,
                        book: 0,
                        chapter: 0,
                        verse_range: None,
                    },
                )
            })),
        }
    });
    // Вкладки заметок (этап 15.2): payload — relpath в хранилище.
    register_panel(cx, NoteTab::NAME, |context, window, cx| {
        let payload = match context.info() {
            gpui_kit::component::dock::PanelInfo::Panel(value) => {
                serde_json::from_value::<NoteTabPayload>(value.clone()).ok()
            }
            _ => None,
        };
        match payload {
            Some(payload) if payload.v >= 1 => {
                panel_handle(cx.new(|cx| NoteTab::new_restored(window, cx, payload)))
            }
            _ => panel_handle(cx.new(|cx| {
                NoteTab::new_restored(
                    window,
                    cx,
                    NoteTabPayload {
                        v: 1,
                        relpath: String::new(),
                    },
                )
            })),
        }
    });
    register_panel(cx, ContextPanel::NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| ContextPanel::new(window, cx)))
    });
    register_panel(cx, ChainsPanel::NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| ChainsPanel::new(window, cx)))
    });
    register_panel(cx, SearchPanel::NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| SearchPanel::new(window, cx)))
    });
    register_panel(cx, NotesPanel::NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| NotesPanel::new(window, cx)))
    });
}
