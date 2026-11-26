//! Корневой View окна: TitleBar + sidebar + DockArea (architecture.md §6.1).
//!
//! Раскладка панелей сериализуется в layout.json (architecture.md §7.1):
//! запись с debounce по `DockEvent::LayoutChanged` и при выходе,
//! чтение при создании окна. При несовпадении версии раскладки —
//! мягкий сброс к дефолтной.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::dock::{
    DockArea, DockAreaState, DockEvent, DockLayout, DockPlacement, DockSkin, PanelId, panel_handle,
};
use gpui_kit::component::{ActiveTheme as _, IconName, TitleBar};
use gpui_kit::component::{
    Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use bible_domain::ports::{ModuleCatalog as _, ModuleKind};

use crate::article_tab::ArticleTab;
use crate::chains::ChainsPanel;
use crate::context::ContextPanel;
use crate::note_tab::NoteTab;
use crate::panels::SearchPanel;
use crate::reader::ReaderPanel;
use crate::sidebar::SidebarView;

/// Идентификатор и версия схемы раскладки. Версию поднимаем, когда дефолтная
/// раскладка меняется несовместимо — тогда сохранённая мягко сбрасывается.
const DOCK_ID: &str = "main-dock";
// v3: панель «Слово» (word) заменена на «Контекст» (context).
// v4: в левый док добавлена вкладка «Заметки» (notes).
// v5: левый док убран — его панели («Библиотека», «Заметки») переехали
// в левую менюшку (этап 15, sidebar.rs).
// v6: settings moved from a central dock tab to a dedicated window.
// v7: search moved from the bottom dock into the persistent left sidebar.
const DOCK_VERSION: usize = 7;

pub struct WorkspaceView {
    dock_area: Entity<DockArea>,
    search_panel: Entity<SearchPanel>,
    /// Скин дока: настройки chrome (кнопки сворачивания и закрытия, стиль
    /// панелей). Держим handle, чтобы менять настройки на лету.
    #[allow(dead_code)]
    skin: Rc<DockSkin>,
    last_layout: Option<DockAreaState>,
    /// Подпись бегунка места в Title bar («Бытие 3:16», §6.6). None, пока
    /// чтение не опубликовало место (до открытия книги).
    location_label: Option<String>,
    /// Подпись индикатора фоновой индексации в Status bar («Индексируется
    /// RST…», §6.6). Обновляется опросом — набор index_building живёт в
    /// shell (без gpui), событий оттуда нет.
    indexing_label: Option<String>,
    /// Активная вкладка чтения — цель роутинга ссылок в режиме
    /// reuse_current (§5.4). WeakEntity: закрытая вкладка не протекает,
    /// upgrade() просто вернёт None.
    active_reader: Option<WeakEntity<ReaderPanel>>,
    /// Левая менюшка (этап 15): иконки остаются видимыми, контент
    /// раздела раскрывается/скрывается внутри SidebarView.
    sidebar: Entity<SidebarView>,
    /// Открытые вкладки статей/комментариев (этап 15.2): ключ дедупликации
    /// → вкладка. Повторный клик по той же теме активирует существующую.
    /// Мёртвые WeakEntity вычищаются при ближайшем открытии.
    open_articles: HashMap<String, WeakEntity<ArticleTab>>,
    /// Открытые вкладки заметок (этап 15.2): relpath → вкладка.
    open_notes: HashMap<String, WeakEntity<NoteTab>>,
    _save_layout_task: Option<Task<()>>,
    _indexing_task: Option<Task<()>>,
}

impl WorkspaceView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (dock_area, skin) = crate::workspace_tabs::dock_area(DOCK_ID, DOCK_VERSION, window, cx);
        // Крестик на каждой вкладке для быстрого закрытия (по умолчанию в
        // gpui-kit выключен; закрываемость панели решает её `closable`).
        skin.set_close_button_visible(true, cx);
        // The app owns both dock toggles in the status bar. Do not expose a
        // second collapse button in panel headers, and do not keep the 29 px
        // bottom strip when Search is hidden.
        skin.set_toggle_button_visible(false, cx);
        skin.set_collapsed_tab_bar_visible(false, cx);

        match Self::load_layout(&dock_area, window, cx) {
            Ok(()) => log::info!("layout restored from layout.json"),
            Err(error) => {
                log::info!("layout.json not applied ({error:#}); using default layout");
                Self::reset_default_layout(&dock_area, window, cx);
            }
        }

        // Debounce-запись раскладки: LayoutChanged летит на каждое
        // перетаскивание разделителя, писать файл нужно лишь когда всё стихло.
        cx.subscribe_in(
            &dock_area,
            window,
            |this, dock_area, event: &DockEvent, window, cx| {
                if matches!(event, DockEvent::LayoutChanged) {
                    let bottom_empty = dock_area
                        .read(cx)
                        .layout(DockPlacement::Bottom)
                        .is_some_and(|tree| tree.panels().next().is_none());
                    if bottom_empty {
                        dock_area.update(cx, |area, cx| {
                            area.remove_dock(DockPlacement::Bottom, window, cx);
                        });
                    }
                    this.schedule_layout_save(&dock_area, window, cx);
                }
            },
        )
        .detach();

        // Финальная запись при выходе.
        cx.on_app_quit({
            let dock_area = dock_area.clone();
            move |_, cx| {
                let state = dock_area.read(cx).dump(cx);
                let dock_area = dock_area.clone();
                cx.background_spawn(async move {
                    if let Err(error) = Self::save_state(&state) {
                        log::error!("failed to save layout on quit: {error:#}");
                    }
                    drop(dock_area);
                })
            }
        })
        .detach();

        // Кнопки ←/→ в заголовке перечитывают доступность при смене истории.
        cx.subscribe_in(
            &crate::navigation::history(cx),
            window,
            |_, _, _: &crate::navigation::HistoryChanged, _, cx| cx.notify(),
        )
        .detach();

        // Общий масштаб интерфейса применяется через Window::set_rem_size;
        // изменение настройки должно перерисовать корневое окно целиком.
        cx.subscribe_in(
            &crate::navigation::bus(cx),
            window,
            |_, _, _: &crate::navigation::SettingsChanged, _, cx| cx.notify(),
        )
        .detach();

        // Бегунок места в Title bar: верхний видимый стих чтения (§6.6).
        // Прокрутка меняет payload вкладки — layout.json сохраняем тем же
        // debounce, что и при перетаскивании панелей (§7.1).
        cx.subscribe_in(
            &crate::navigation::bus(cx),
            window,
            |this, _, event: &crate::navigation::LocationChanged, window, cx| {
                let location = &event.0;
                // Короткое имя книги — компактный бегунок места (этап 20).
                let book = if location.book_short.is_empty() {
                    &location.book_title
                } else {
                    &location.book_short
                };
                this.location_label =
                    Some(format!("{} {}:{}", book, location.chapter, location.verse));
                let dock_area = this.dock_area.clone();
                this.schedule_layout_save(&dock_area, window, cx);
                cx.notify();
            },
        )
        .detach();

        // Роутинг библейских ссылок (§5.4): перекрёстные, результаты
        // поиска, палитра перехода — в активную вкладку или в новую,
        // по настройке link_open_mode.
        cx.subscribe_in(
            &crate::navigation::bus(cx),
            window,
            |this, _, request: &crate::navigation::OpenBook, window, cx| {
                this.route_open_book(request, window, cx);
            },
        )
        .detach();

        cx.subscribe_in(
            &crate::navigation::bus(cx),
            window,
            |this, _, request: &crate::navigation::OpenBookInNewTab, window, cx| {
                let reader = Self::spawn_reading_tab(None, window, cx, &this.dock_area);
                reader.update(cx, |reader, cx| reader.open_book(&request.0, window, cx));
            },
        )
        .detach();

        // Активная вкладка чтения сменилась — запоминаем цель роутинга.
        cx.subscribe_in(
            &crate::navigation::bus(cx),
            window,
            |this, _, event: &crate::navigation::ReaderActivated, _, _| {
                this.active_reader = Some(event.0.clone());
            },
        )
        .detach();

        // Запрос поиска из другой панели (секция «Слово»): раскрываем
        // панель поиска и передаём запрос дальше уже адресно ей.
        cx.subscribe_in(
            &crate::navigation::bus(cx),
            window,
            |this, _, event: &crate::navigation::SearchRequested, _window, cx| {
                this.sidebar
                    .update(cx, |sidebar, cx| sidebar.show_search(cx));
                crate::navigation::emit_search_execute(cx, event.query.clone());
            },
        )
        .detach();

        // Открытие модуля/статьи (клик в менюшке, «Открыть в словаре» из
        // секции «Слово», маркер ⁿ в чтении): этап 15.2 — статьи и
        // комментарии открываются вкладками в центре, словарь без темы
        // раскрывает drill-down в менюшке.
        cx.subscribe_in(
            &crate::navigation::bus(cx),
            window,
            |this, _, event: &crate::navigation::OpenModule, window, cx| {
                this.route_open_module(event, window, cx);
            },
        )
        .detach();

        // Открытие заметки вкладкой в центре (клик в менюшке, ссылка
        // bibleown://note/… в статье) — этап 15.2.
        cx.subscribe_in(
            &crate::navigation::bus(cx),
            window,
            |this, _, event: &crate::navigation::OpenNote, window, cx| {
                this.open_note_tab(&event.relpath, window, cx);
            },
        )
        .detach();

        cx.subscribe_in(
            &crate::navigation::bus(cx),
            window,
            |this, _, event: &crate::navigation::OpenCommentary, window, cx| {
                let module = event.module.clone();
                let reference = event.reference;
                this.open_commentary_tab(module, reference, window, cx);
            },
        )
        .detach();

        // Запрос раскрыть раздел в левой панели: иконки остаются на месте,
        // Workspace лишь показывает контентную часть.
        cx.subscribe_in(
            &crate::navigation::bus(cx),
            window,
            |this, _, _: &crate::navigation::OpenSidebarModule, _, cx| {
                this.sidebar
                    .update(cx, |sidebar, cx| sidebar.show_content(cx));
            },
        )
        .detach();

        // Кнопки Status bar «Перекрёстные»/«Комментарий к стиху»
        // (этап 18): раскрываем панель «Verse», секцию открывает сама
        // панель по этому же событию. Свежесозданная панель событие
        // пропустила — повторяем.
        cx.subscribe_in(
            &crate::navigation::bus(cx),
            window,
            |this, _, event: &crate::navigation::OpenVerseSection, window, cx| {
                let created =
                    this.reveal_panel(ContextPanel::NAME, DockPlacement::Right, window, cx);
                if created {
                    crate::navigation::bus(cx).update(cx, |_, cx| cx.emit(*event));
                }
            },
        )
        .detach();

        // Индикатор фоновой индексации в Status bar (§6.6): опрос раз в
        // 500 мс — фоновый индексатор ведёт набор index_building
        // и событий не шлёт (shell без GPUI). notify только при смене
        // подписи, чтобы не дёргать render вхолостую.
        let indexing_task = cx.spawn_in(window, async move |this, window| {
            loop {
                window
                    .background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let updated = this.update_in(window, |this, _, cx| {
                    let label = Self::indexing_label(cx);
                    if label != this.indexing_label {
                        this.indexing_label = label;
                        cx.notify();
                    }
                });
                if updated.is_err() {
                    break;
                }
            }
        });

        let restored_search = {
            let area = dock_area.read(cx);
            [
                DockPlacement::Bottom,
                DockPlacement::Left,
                DockPlacement::Right,
                DockPlacement::Center,
            ]
            .into_iter()
            .filter_map(|placement| area.layout(placement))
            .flat_map(|tree| tree.panels())
            .filter_map(|id| area.panel(id))
            .find_map(|panel| panel.view().downcast::<SearchPanel>().ok())
        };
        let search_panel =
            restored_search.unwrap_or_else(|| cx.new(|cx| SearchPanel::new(window, cx)));
        let sidebar = cx.new(|cx| SidebarView::new(search_panel.clone(), window, cx));
        Self {
            dock_area,
            search_panel,
            skin,
            last_layout: None,
            location_label: None,
            indexing_label: None,
            active_reader: None,
            sidebar,
            open_articles: HashMap::new(),
            open_notes: HashMap::new(),
            _save_layout_task: None,
            _indexing_task: Some(indexing_task),
        }
    }

    /// Текст индикатора индексации по набору строящихся модулей:
    /// «Индексируется RST…», «Индексируется RST, KJV +2…».
    fn indexing_label(cx: &App) -> Option<String> {
        let building = crate::services::app_services(cx).index_building.clone();
        let building = building.lock();
        if building.is_empty() {
            return None;
        }
        let mut names: Vec<&str> = building.iter().map(String::as_str).collect();
        names.sort_unstable();
        let shown = names.len().min(2);
        let mut label = if crate::i18n::is_english(cx) {
            format!("Indexing {}", names[..shown].join(", "))
        } else {
            format!("Индексируется {}", names[..shown].join(", "))
        };
        if names.len() > shown {
            label.push_str(&format!(" +{}", names.len() - shown));
        }
        label.push('…');
        Some(label)
    }

    /// Дефолтная раскладка (architecture.md §6.1): справа «Verse», снизу
    /// «Поиск», в центре «Чтение». Левого дока нет — его роль выполняет
    /// левая менюшка (этап 15, sidebar.rs). Панели «Словарь» тоже нет —
    /// статьи и комментарии открываются вкладками в центре (этап 15.2).
    fn reset_default_layout(dock_area: &Entity<DockArea>, window: &mut Window, cx: &mut App) {
        let reader = cx.new(|cx| ReaderPanel::new(window, cx));
        let context = cx.new(|cx| ContextPanel::new(window, cx));

        dock_area.update(cx, |area, cx| {
            area.set_center(
                DockLayout::tabs().panel_view(panel_handle(reader), cx),
                window,
                cx,
            );
            area.set_dock(
                DockPlacement::Right,
                DockLayout::tabs().panel_view(panel_handle(context), cx),
                window,
                cx,
            );
            area.set_dock_size(DockPlacement::Right, px(320.), window, cx);
            for placement in [DockPlacement::Left, DockPlacement::Right] {
                area.set_dock_collapsible(placement, true, window, cx);
            }
        });
    }

    fn load_layout(
        dock_area: &Entity<DockArea>,
        window: &mut Window,
        cx: &mut App,
    ) -> anyhow::Result<()> {
        let json =
            shell::layout::read_layout().ok_or_else(|| anyhow::anyhow!("layout.json is absent"))?;
        let state: DockAreaState = serde_json::from_str(&json)?;
        if state.version != Some(DOCK_VERSION) {
            anyhow::bail!("layout version {:?} != {DOCK_VERSION}", state.version);
        }
        dock_area.update(cx, |area, cx| {
            area.load(state, window, cx)?;
            for placement in [
                DockPlacement::Left,
                DockPlacement::Bottom,
                DockPlacement::Right,
            ] {
                area.set_dock_collapsible(placement, true, window, cx);
            }
            Ok::<(), anyhow::Error>(())
        })?;
        Ok(())
    }

    fn schedule_layout_save(
        &mut self,
        dock_area: &Entity<DockArea>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dock_area = dock_area.clone();
        self._save_layout_task = Some(cx.spawn_in(window, async move |this, window| {
            window
                .background_executor()
                .timer(Duration::from_millis(800))
                .await;
            _ = this.update_in(window, move |this, _, cx| {
                let state = dock_area.read(cx).dump(cx);
                if this.last_layout.as_ref() == Some(&state) {
                    return;
                }
                if let Err(error) = Self::save_state(&state) {
                    log::error!("failed to save layout: {error:#}");
                }
                this.last_layout = Some(state);
            });
        }));
    }

    fn save_state(state: &DockAreaState) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(state)?;
        shell::layout::write_layout(&json)
    }

    fn toggle_dock(&mut self, placement: DockPlacement, window: &mut Window, cx: &mut App) {
        self.dock_area.update(cx, |area, cx| {
            area.toggle_dock(placement, window, cx);
        });
    }

    /// Переход на соседнюю главу активной вкладки чтения (кнопки Status
    /// bar, этап 18): место — общий снимок current_location, границы —
    /// chapter_count модуля. За краем книги — ничего не делаем (кнопка
    /// и так disabled).

    /// Исполнитель команд палитры (Ctrl+Shift+P, architecture.md §6.5).
    /// Палитра не знает о доках — команды приходят сюда enum'ом.
    pub(crate) fn execute_command(
        &mut self,
        command: crate::command_palette::PaletteCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::command_palette::PaletteCommand as Cmd;
        match command {
            Cmd::VerseJump => crate::jump::open_verse_jump(window, cx),
            Cmd::HistoryBack => crate::navigation::history_go_back(cx),
            Cmd::HistoryForward => crate::navigation::history_go_forward(cx),
            Cmd::OpenSettings => crate::settings::open_settings_window(cx),
            Cmd::OpenChains => {
                crate::navigation::request_open_chain(cx, None);
            }
            Cmd::NewReadingTab => self.new_reading_tab(window, cx),
            Cmd::ToggleDock(placement) => self.toggle_dock(placement, window, cx),
        }
    }

    /// Раскрыть док-панель по имени (запросы из других панелей,
    /// architecture.md §6.4): находим её в любом доке, активируем вкладку
    /// и открываем док, если он свёрнут; панели ещё нет — создаём в
    /// `fallback`-доке. Возвращает true, если панель была создана вызовом
    /// (тогда событие-триггер она пропустила и его надо повторить).
    fn reveal_panel(
        &mut self,
        panel_name: &'static str,
        fallback: DockPlacement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let existing = {
            let area = self.dock_area.read(cx);
            [
                DockPlacement::Bottom,
                DockPlacement::Left,
                DockPlacement::Right,
                DockPlacement::Center,
            ]
            .into_iter()
            .filter_map(|placement| {
                area.layout(placement).and_then(|tree| {
                    tree.panels()
                        .find(|id| {
                            area.panel(*id)
                                .is_some_and(|view| view.panel_name(cx) == panel_name)
                        })
                        .map(|id| (placement, id))
                })
            })
            .next()
        };
        let mut created = false;
        self.dock_area.update(cx, |area, cx| {
            let placement = match existing {
                Some((placement, id)) => {
                    area.select_panel(id, window, cx);
                    placement
                }
                None => {
                    created = true;
                    let id = match panel_name {
                        SearchPanel::NAME => {
                            let panel = self.search_panel.clone();
                            let id = PanelId::from(panel.entity_id());
                            area.add_panel_view(panel_handle(panel), fallback, None, window, cx);
                            id
                        }
                        ChainsPanel::NAME => {
                            let panel = cx.new(|cx| ChainsPanel::new(window, cx));
                            let id = PanelId::from(panel.entity_id());
                            area.add_panel_view(panel_handle(panel), fallback, None, window, cx);
                            id
                        }
                        _ => unreachable!("reveal_panel: unknown panel {panel_name}"),
                    };
                    area.select_panel(id, window, cx);
                    fallback
                }
            };
            // Док может быть свёрнут — тогда панель невидима. Центр
            // сворачиваемым не бывает, toggle по нему — no-op по контракту.
            if placement != DockPlacement::Center && !area.is_dock_open(placement) {
                area.toggle_dock(placement, window, cx);
            }
        });
        created
    }

    /// Роутинг запроса на открытие книги (§5.4): перекрёстные ссылки,
    /// результаты поиска, палитра перехода. Режим — настройка
    /// `bible.link_open_mode`: reuse_current — активная вкладка чтения
    /// (нет активной — создаём), new_tab — всегда новая вкладка.
    fn route_open_book(
        &mut self,
        request: &crate::navigation::OpenBook,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mode = crate::services::app_services(cx)
            .settings
            .read()
            .link_open_mode;
        let target = match mode {
            shell::settings::LinkOpenMode::ReuseCurrent => self
                .active_reader
                .as_ref()
                .and_then(|reader| reader.upgrade()),
            shell::settings::LinkOpenMode::NewTab => None,
        };
        match target {
            Some(reader) => {
                reader.update(cx, |reader, cx| reader.open_book(request, window, cx));
            }
            None => {
                let reader = Self::spawn_reading_tab(None, window, cx, &self.dock_area);
                reader.update(cx, |reader, cx| reader.open_book(request, window, cx));
            }
        }
    }

    /// Новая пустая вкладка чтения (Ctrl+T, палитра, роутер ссылок):
    /// создать, добавить в центр и активировать.
    fn new_reading_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let source = self
            .active_reader
            .as_ref()
            .and_then(|reader| reader.upgrade())
            .and_then(|reader| reader.read(cx).module());
        Self::spawn_reading_tab(source, window, cx, &self.dock_area);
    }

    pub(crate) fn open_new_tab_in(
        &mut self,
        kind: crate::workspace_tabs::NewTab,
        node: gpui_kit::component::dock::NodeId,
        origin: Option<Entity<ReaderPanel>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::workspace_tabs::NewTab;
        use gpui_kit::component::dock::InsertTarget;
        let panel = match kind {
            NewTab::Bible => {
                let source = origin.as_ref().and_then(|reader| reader.read(cx).module());
                panel_handle(cx.new(|cx| ReaderPanel::new_empty(source, window, cx)))
            }
            NewTab::Dictionary => {
                panel_handle(cx.new(|cx| crate::dictionary::DictionaryBrowser::new(window, cx)))
            }
            NewTab::Note => {
                let index = shell::notes::current(crate::services::app_services(cx));
                let Some(index) = index else {
                    crate::settings::open_settings_window(cx);
                    return;
                };
                match shell::notes::create_untitled_note(&index) {
                    Ok(doc) => {
                        let tab = cx.new(|cx| NoteTab::open(window, cx, doc.relpath.clone()));
                        self.open_notes.insert(doc.relpath, tab.downgrade());
                        panel_handle(tab)
                    }
                    Err(error) => {
                        log::error!("could not create a new note: {error:#}");
                        return;
                    }
                }
            }
        };
        let id = panel.panel_id(cx);
        self.dock_area.update(cx, |area, cx| {
            area.add_panel_view(panel, DockPlacement::Center, None, window, cx);
            if area
                .layout(DockPlacement::Center)
                .and_then(|tree| tree.find_panel_node(id))
                != Some(node)
            {
                area.move_panel(
                    id,
                    InsertTarget::Tabs {
                        node,
                        ix: None,
                        activate: true,
                    },
                    window,
                    cx,
                );
            }
            area.select_panel(id, window, cx);
        });
    }

    /// Роутинг запроса на открытие модуля (этап 15.2): статья словаря —
    /// вкладкой в центре; словарь без темы — drill-down в менюшке;
    /// комментарии — вкладкой в центре к текущей главе чтения.
    fn route_open_module(
        &mut self,
        request: &crate::navigation::OpenModule,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let services = crate::services::app_services(cx);
        let Some(meta) = services.registry.module(&request.module) else {
            return;
        };
        match meta.kind {
            ModuleKind::StrongsDictionary | ModuleKind::GeneralDictionary => {
                match &request.topic {
                    Some(topic) => {
                        let key = format!("dict:{}:{}", request.module, topic);
                        let module = request.module.clone();
                        let topic = topic.clone();
                        self.open_article_tab(key, window, cx, move |window, cx| {
                            ArticleTab::new_dict(window, cx, module, topic)
                        });
                    }
                    // Словарь без темы — список тем в менюшке (drill-down).
                    None => crate::navigation::request_open_sidebar(
                        cx,
                        crate::navigation::SidebarSectionKind::Dictionaries,
                        Some(request.module.clone()),
                    ),
                }
            }
            ModuleKind::Commentary => {
                let location = services.current_location.lock().clone();
                let reference = location.map_or_else(
                    || bible_domain::ids::VerseRef::single(bible_domain::ids::BookNumber(0), 0, 0),
                    |location| {
                        bible_domain::ids::VerseRef::single(
                            location.book,
                            location.chapter,
                            location.verse,
                        )
                    },
                );
                self.open_commentary_tab(request.module.clone(), reference, window, cx);
            }
            // Параллельные ссылки и прочие виды: их покрывает секция
            // «Параллельные» панели «Verse» — отдельной вкладки не делаем.
            _ => {}
        }
    }

    /// Открыть (или активировать существующую) вкладку статьи в центре.
    /// Дедупликация по ключу: повторный клик по той же теме/главе
    /// активирует уже открытую вкладку, а не плодит новую.
    fn open_article_tab(
        &mut self,
        key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
        build: impl FnOnce(&mut Window, &mut Context<ArticleTab>) -> ArticleTab,
    ) {
        self.open_articles
            .retain(|_, weak| weak.upgrade().is_some());
        if let Some(tab) = self.open_articles.get(&key).and_then(|weak| weak.upgrade()) {
            let id = PanelId::from(tab.entity_id());
            self.dock_area.update(cx, |area, cx| {
                area.select_panel(id, window, cx);
            });
            return;
        }
        let tab = cx.new(|cx| build(window, cx));
        let id = PanelId::from(tab.entity_id());
        self.open_articles.insert(key, tab.downgrade());
        self.dock_area.update(cx, |area, cx| {
            area.add_panel_view(panel_handle(tab), DockPlacement::Center, None, window, cx);
            area.select_panel(id, window, cx);
        });
    }

    /// One commentary module owns one centre tab. Sidebar references act as
    /// its table of contents and move the already open document instead of
    /// creating a separate tab for every verse or chapter.
    fn open_commentary_tab(
        &mut self,
        module: bible_domain::ids::ModuleId,
        reference: bible_domain::ids::VerseRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = ArticleTab::commentary_key(&module);
        self.open_articles
            .retain(|_, weak| weak.upgrade().is_some());
        if let Some(tab) = self.open_articles.get(&key).and_then(|weak| weak.upgrade()) {
            tab.update(cx, |tab, cx| tab.scroll_commentary_to(reference, cx));
            let id = PanelId::from(tab.entity_id());
            self.dock_area.update(cx, |area, cx| {
                area.select_panel(id, window, cx);
            });
            return;
        }

        let tab = cx.new(|cx| ArticleTab::new_commentary_at(cx, module, reference));
        let id = PanelId::from(tab.entity_id());
        self.open_articles.insert(key, tab.downgrade());
        self.dock_area.update(cx, |area, cx| {
            area.add_panel_view(panel_handle(tab), DockPlacement::Center, None, window, cx);
            area.select_panel(id, window, cx);
        });
    }

    /// Открыть (или активировать существующую) вкладку заметки в центре
    /// (этап 15.2). Дедупликация по relpath, как у статей.
    fn open_note_tab(&mut self, relpath: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_notes.retain(|_, weak| weak.upgrade().is_some());
        if let Some(tab) = self.open_notes.get(relpath).and_then(|weak| weak.upgrade()) {
            let id = PanelId::from(tab.entity_id());
            self.dock_area.update(cx, |area, cx| {
                area.select_panel(id, window, cx);
            });
            return;
        }
        let relpath = relpath.to_string();
        let tab = cx.new(|cx| NoteTab::open(window, cx, relpath.clone()));
        let id = PanelId::from(tab.entity_id());
        self.open_notes.insert(relpath, tab.downgrade());
        self.dock_area.update(cx, |area, cx| {
            area.add_panel_view(panel_handle(tab), DockPlacement::Center, None, window, cx);
            area.select_panel(id, window, cx);
        });
    }

    fn spawn_reading_tab(
        source: Option<bible_domain::ids::ModuleId>,
        window: &mut Window,
        cx: &mut App,
        dock_area: &Entity<DockArea>,
    ) -> Entity<ReaderPanel> {
        let reader = cx.new(|cx| ReaderPanel::new_empty(source, window, cx));
        let id = PanelId::from(reader.entity_id());
        dock_area.update(cx, |area, cx| {
            area.add_panel_view(
                panel_handle(reader.clone()),
                DockPlacement::Center,
                None,
                window,
                cx,
            );
            area.select_panel(id, window, cx);
        });
        reader
    }

    /// Title bar окна: кнопки истории, бегунок места, настройки.
    /// Вынесен из render — см. `icon_button` про кадр стека.
    fn render_title_bar(
        &mut self,
        can_back: bool,
        can_forward: bool,
        location_label: Option<String>,
        cx: &mut Context<Self>,
    ) -> TitleBar {
        let back = icon_button(
            "history-back",
            IconName::ArrowLeft,
            crate::i18n::tr(cx, "Назад (Ctrl+Alt+←)"),
            cx,
        )
        .disabled(!can_back)
        .on_click(|_, _, cx| {
            crate::navigation::history_go_back(cx);
        });
        let forward = icon_button(
            "history-forward",
            IconName::ArrowRight,
            crate::i18n::tr(cx, "Вперёд (Ctrl+Alt+→)"),
            cx,
        )
        .disabled(!can_forward)
        .on_click(|_, _, cx| {
            crate::navigation::history_go_forward(cx);
        });
        // Бегунок места «Бытие 3:16» (§6.6): клик — палитра перехода (как Ctrl+P).
        let location_runner = location_label.map(|label| {
            Button::new("location-runner")
                .ghost()
                .xsmall()
                .ml_2()
                .label(label)
                .tooltip(crate::i18n::tr(cx, "Перейти к месту (Ctrl+P)"))
                .on_click(|_, window, cx| {
                    crate::jump::open_verse_jump(window, cx);
                })
        });
        TitleBar::new()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child("BibleOWN"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    // Кнопки внутри drag-зоны заголовка (Windows):
                    // occlude останавливает hit-test на кнопке, и ОС
                    // не отдаёт область под HTCAPTION — иначе caption-
                    // конвертация теряет mouse down и клики мертвы.
                    .occlude()
                    .child(back)
                    .child(forward)
                    .children(location_runner),
            )
            .child(div().flex_1())
    }
}

/// Иконочная кнопка chrome (Title bar / edge rail): ghost, xsmall, иконка,
/// tooltip. Выделена в функцию не только ради краткости: `Button` весит
/// ~9,4 КБ, а builder-цепочка в debug-сборке держит все промежуточные
/// значения в кадре вызывающего до конца statement'а. В монолитном render
/// десяток таких кнопок раздувал кадр до ~0,9 МБ, и приложение падало с
/// переполнением стека. Шаги внутри функции освобождают её кадр при возврате.
fn icon_button(
    id: &'static str,
    icon: impl Into<gpui_kit::component::Icon>,
    tooltip: impl Into<SharedString>,
    cx: &App,
) -> Button {
    let tooltip = tooltip.into();
    Button::new(id)
        .ghost()
        .xsmall()
        .tooltip(tooltip.clone())
        .accessibility_label(tooltip)
        .child(icon.into().size(crate::ui_scale::icon_size(cx)))
}

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // GPUI rem size is the common UI zoom: it scales rem-based spacing,
        // controls and default typography in the entire window.
        let (interface_scale, ui_font_size, ui_font_family) = {
            let settings = crate::services::app_services(cx).settings.read();
            let interface_scale = if settings.interface_scale.is_finite() {
                settings.interface_scale.clamp(0.8, 1.6)
            } else {
                1.0
            };
            let ui_font_size = if settings.ui_font_size.is_finite() {
                settings.ui_font_size.clamp(10.0, 32.0)
            } else {
                16.0
            };
            (
                interface_scale,
                ui_font_size,
                settings.ui_font_family.clone(),
            )
        };
        // Изменение базового кегля применяется на уровне каждого окна;
        // interface_scale дополнительно масштабирует метрики и отступы.
        window.set_rem_size(px(ui_font_size * interface_scale));
        let can_back = crate::navigation::history_can_back(cx);
        let can_forward = crate::navigation::history_can_forward(cx);
        let workspace = cx.weak_entity();
        // Подпись бегунок места: при старте событие LocationChanged могло
        // уйти до подписки (книга открывается при создании панели чтения) —
        // тогда берём снимок из общего состояния напрямую.
        let location_label = self.location_label.clone().or_else(|| {
            crate::services::app_services(cx)
                .current_location
                .lock()
                .as_ref()
                .map(|location| {
                    let book = if location.book_short.is_empty() {
                        &location.book_title
                    } else {
                        &location.book_short
                    };
                    format!("{} {}:{}", book, location.chapter, location.verse)
                })
        });
        // TitleBar собирается отдельным методом: единое
        // гигантское builder-выражение в debug-сборке раздувало кадр стека
        // render до ~0,9 МБ (временные объекты цепочки живут до конца
        // statement'а) — приложение падало с переполнением стека при
        // добавлении очередной кнопки.
        let title_bar = self.render_title_bar(can_back, can_forward, location_label, cx);
        // The bottom status strip is intentionally gone. The search lives in
        // the left sidebar; the right edge keeps one persistent collapse
        // control so the context panel is always discoverable.
        let right_panel_toggle = icon_button(
            "toggle-right-dock-edge",
            IconName::PanelRight,
            crate::i18n::tr(cx, "Правая панель"),
            cx,
        )
        .on_click(cx.listener(|this, _, window, cx| {
            this.toggle_dock(DockPlacement::Right, window, cx);
        }));
        let workspace_root = div()
            .id("workspace")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .when(!ui_font_family.is_empty(), |root| {
                root.font_family(ui_font_family)
            });
        workspace_root
            .on_action(|_: &crate::navigation::NavigateBack, _, cx| {
                crate::navigation::history_go_back(cx);
            })
            .on_action(|_: &crate::navigation::NavigateForward, _, cx| {
                crate::navigation::history_go_forward(cx);
            })
            .on_action(|_: &crate::navigation::OpenVerseJump, window, cx| {
                crate::jump::open_verse_jump(window, cx);
            })
            .on_action(
                move |_: &crate::navigation::OpenCommandPalette, window, cx| {
                    crate::command_palette::open(window, cx, workspace.clone());
                },
            )
            .on_action(
                cx.listener(|this, _: &crate::navigation::NewReadingTab, window, cx| {
                    this.new_reading_tab(window, cx);
                }),
            )
            .child(title_bar)
            .child(
                // Левая менюшка (этап 15) — вне DockArea: узкая полоса
                // пунктов + контент выбранного пункта фиксированной ширины.
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.sidebar.clone())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.dock_area.clone()),
                    )
                    .child(
                        v_flex()
                            .h_full()
                            .w(px(36.))
                            .flex_shrink_0()
                            .items_center()
                            .pt_1()
                            .border_l_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().sidebar)
                            .child(right_panel_toggle),
                    ),
            )
    }
}
