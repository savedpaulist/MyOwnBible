//! Presentation: единственный крейт, зависящий от gpui-kit (architecture.md §6).
//!
//! Этап 1: каркас окна «как в Zed» — TitleBar + DockArea + StatusBar,
//! раскладка сохраняется в layout.json и восстанавливается при запуске.

pub mod annotations;
pub mod article;
pub mod article_tab;
pub mod bidi;
pub mod chains;
pub mod command_palette;
pub mod context;
mod dictionary;
pub mod i18n;
pub mod jump;
pub mod library;
pub mod navigation;
mod note_markup;
pub mod note_tab;
pub mod notes;
pub mod panels;
mod questions;
pub mod reader;
pub mod search;
pub mod services;
pub mod settings;
pub mod sidebar;
mod sync;
pub mod text;
pub mod theme;
pub mod ui_scale;
mod verse_picker;
mod verse_preview;
pub mod workspace;
mod workspace_tabs;

use gpui_kit::assets::AllAssets;
use gpui_kit::component::TitleBar;
use gpui_kit::*;
use shell::services::AppServices;

pub fn run() {
    // Логирование в stderr (RUST_LOG, по умолчанию info): log::error! из
    // панелей и shell раньше улетал в никуда — логгера в приложении не было.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    // AllAssets — полный каталог lucide (Assets вшивает только дефолтный
    // набор компонентов): нужен для иконок Status bar (Link2, SquarePen)
    // и маркера цепочек (Link) — этапы 18/20.
    gpui_kit::application().with_assets(AllAssets).run(|cx| {
        gpui_kit::init(cx);
        cx.set_app_identity("com.bibleown.desktop", "BibleOWN");

        // Глобальные горячие клавиши (architecture.md §5.5, §6.5): история
        // ←/→ и палитра перехода к стиху. Обработчики — на корневом div окна.
        cx.bind_keys([
            KeyBinding::new("ctrl-alt-left", navigation::NavigateBack, None),
            KeyBinding::new("ctrl-alt-right", navigation::NavigateForward, None),
            KeyBinding::new("ctrl-p", navigation::OpenVerseJump, None),
            KeyBinding::new("ctrl-shift-p", navigation::OpenCommandPalette, None),
            KeyBinding::new("ctrl-t", navigation::NewReadingTab, None),
        ]);

        // Глобалы приложения: сервисы (модули, настройки) и шина навигации.
        // Должны появиться до создания панелей — они читают их в new().
        cx.set_global(services::Services(std::sync::Arc::new(
            AppServices::bootstrap(),
        )));
        i18n::sync(cx);
        // Тема — после глобала services (init читает сохранённый выбор),
        // до создания окон (они рисуются уже в ней).
        theme::init(cx);
        navigation::init(cx);
        sync::init(cx);

        panels::register_all(cx);

        let mut window_size = size(px(1440.0), px(900.0));
        if let Some(display) = cx.primary_display() {
            let display_size = display.bounds().size;
            window_size.width = window_size.width.min(display_size.width * 0.9);
            window_size.height = window_size.height.min(display_size.height * 0.9);
        }
        let bounds = Bounds::centered(None, window_size, cx);

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(Size {
                width: px(640.),
                height: px(480.),
            }),
            ..TitleBar::window_options()
        };

        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| workspace::WorkspaceView::new(window, cx))
        })
        .expect("failed to open window");

        cx.activate(true);
        let services = services::app_services_arc(cx);
        cx.background_executor()
            .spawn(async move {
                shell::search::warm_indexes(&services);
            })
            .detach();
    });
}
