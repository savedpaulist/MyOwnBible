//! Панель чтения: книга целиком (architecture.md §8.1).
//!
//! Список — `gpui::list` с ленивым измерением высот строк. Точный переход
//! к главе/стиху: после смены данных `ListState::reset` сбрасывает измерения,
//! а `scroll_to(ListOffset { item_ix, offset_in_item: px(0.) })` ставит якорь
//! прокрутки на точный индекс строки — стих оказывается у верхней кромки
//! viewport, то есть гарантированно в зоне видимости.
//!
//! Шапка: выбор перевода (Select, короткое имя модуля), кнопка места
//! «Книга глава:стих» (по верхнему видимому стиху, открывает навигатор
//! «книга → глава → стих», порт BibleNavigator из форка) и поле быстрого
//! перехода «3:16». Текст стихов — `SelectableText` (по handle на стих):
//! выделение через границы стихов собирает оконный слой gpui-base,
//! Ctrl+C обрабатывает Root.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bible_domain::ports::VerseSource as _;
use gpui_kit::base::{SelectableText, TextSelection, TextSelectionCopy, h_flex, v_flex};
use gpui_kit::component::IndexPath;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::color_picker::{ColorPickerState, ColorSelect};
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent, PanelInfo, PanelState};
use gpui_kit::component::hover_card::HoverCard;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _};
use gpui_kit::component::{Icon, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use bible_domain::annotations::AnnotationKind;
use bible_domain::ids::{AnchoredRef, BookNumber, ModuleId, VerseRef};
use bible_domain::ports::ModuleCatalog as _;
use bible_domain::tags::Tag;
use bible_domain::verse::CleanVerse;
use shell::BookEntry;
use shell::services::BookViewModel;

use crate::annotations as palette;
use crate::navigation::{self, OpenBook, WordPanelVisibilityChanged};
use crate::services::app_services;
use crate::text::strip_markup;

/// Граница Ветхого/Нового завета в нумерации MyBible (как в форке).
const OLD_TESTAMENT_BOUNDARY: i64 = 470;
const BIBLE_COPY_GROUP_PREFIX: u64 = 0x4249_424C_4500_0000;

fn bible_copy_group(chapter: u16, verse: u16) -> u64 {
    BIBLE_COPY_GROUP_PREFIX | (u64::from(chapter) << 16) | u64::from(verse)
}

fn decode_bible_copy_group(group: u64) -> Option<(u16, u16)> {
    ((group & 0xFFFF_FFFF_FF00_0000) == BIBLE_COPY_GROUP_PREFIX)
        .then_some((((group >> 16) & 0xFFFF) as u16, (group & 0xFFFF) as u16))
}

pub(crate) fn format_bible_selection(copy: TextSelectionCopy, cx: &App) -> String {
    let refs = copy
        .groups
        .iter()
        .map(|group| decode_bible_copy_group(*group))
        .collect::<Option<Vec<_>>>();
    let Some(refs) = refs.filter(|refs| !refs.is_empty()) else {
        return copy.text;
    };
    let Some(location) = app_services(cx).current_location.lock().clone() else {
        return copy.text;
    };
    let settings = app_services(cx).settings.read();
    if !settings.copy_bible_include_reference {
        return copy.text;
    }
    let (first_chapter, first_verse) = refs[0];
    let (last_chapter, last_verse) = refs[refs.len() - 1];
    let range = if (first_chapter, first_verse) == (last_chapter, last_verse) {
        format!("{first_chapter}:{first_verse}")
    } else if first_chapter == last_chapter {
        format!("{first_chapter}:{first_verse}-{last_verse}")
    } else {
        format!("{first_chapter}:{first_verse}–{last_chapter}:{last_verse}")
    };
    let reference = format!("{} {range} · {}", location.book_short, location.module);
    match settings.copy_reference_position {
        shell::settings::CopyReferencePosition::Start => format!("{reference}\n{}", copy.text),
        shell::settings::CopyReferencePosition::End => format!("{}\n{reference}", copy.text),
    }
}

/// Направление строки чтения определяется языком всего модуля, а не
/// отдельными символами стиха. Поэтому еврейская цитата внутри LXX не
/// переставляет номер, а BHSEK+ с legacy-кодом `iw` переставляет.
fn module_language_is_hebrew(language: Option<&str>) -> bool {
    let Some(language) = language else {
        return false;
    };
    let language = language.trim().to_ascii_lowercase();
    let primary = language.split(['-', '_']).next().unwrap_or_default();
    matches!(primary, "he" | "iw" | "heb" | "hbo" | "hebrew")
}

fn bible_copy_text(
    cx: &App,
    text: &str,
    module: Option<&ModuleId>,
    book_short: &str,
    chapter: u16,
    verse_from: u16,
    verse_to: Option<u16>,
) -> String {
    let settings = app_services(cx).settings.read();
    if !settings.copy_bible_include_reference {
        return text.to_string();
    }
    let range = verse_to
        .filter(|end| *end != verse_from)
        .map_or_else(String::new, |end| format!("-{end}"));
    let module = module.map_or_else(String::new, |module| format!(" · {module}"));
    let reference = format!("{book_short} {chapter}:{verse_from}{range}{module}");
    match settings.copy_reference_position {
        shell::settings::CopyReferencePosition::Start => format!("{reference}\n{text}"),
        shell::settings::CopyReferencePosition::End => format!("{text}\n{reference}"),
    }
}

/// Строка списка: заголовок главы или один стих.
#[derive(Debug, Clone)]
enum Row {
    ChapterHeading(u16),
    Verse {
        chapter: u16,
        verse: u16,
        clean: CleanVerse,
        /// К стиху есть комментарий — надстрочный маркер в конце строки.
        has_critical: bool,
    },
}

/// Пункт выпадающего списка переводов.
#[derive(Clone)]
struct TranslationOption {
    id: ModuleId,
    description: Option<String>,
}

impl SelectItem for TranslationOption {
    type Value = ModuleId;

    /// Короткое название модуля (= stem файла): «RSTI», «LSV».
    fn title(&self) -> SharedString {
        self.id.to_string().into()
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }

    /// В выпадающем списке — короткое имя и полное описание из info.
    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut row = h_flex()
            .w_full()
            .gap_2()
            .items_baseline()
            .child(div().flex_shrink_0().child(self.id.to_string()));
        if let Some(description) = &self.description {
            row = row.child(
                div()
                    .min_w_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(description.clone()),
            );
        }
        row
    }
}

/// Стадия навигатора (как в форке: picker.rs, BibleNavigator).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NavStage {
    Book,
    Chapter,
    Verse,
}

/// Состояние всплывающего навигатора «книга → глава → стих».
#[derive(Debug, Clone)]
struct Navigator {
    stage: NavStage,
    books: Rc<Vec<BookEntry>>,
    picked_book: Option<BookEntry>,
    picked_chapter: Option<u16>,
    chapter_count: u16,
    verse_count: u16,
}

/// Состояние модального пикера тегов стиха (этап 21, перенос
/// tag_verse_picker форка): чипы-тоглы, фильтр/создание, цвет нового.
struct TagPicker {
    /// Стих, на который ставятся теги (модуль-независимая ссылка).
    reference: VerseRef,
    tags: Vec<Tag>,
    /// Теги, которые стоят на стихе прямо сейчас.
    selected: HashSet<i64>,
    filter_input: Entity<InputState>,
    /// Цвет создаваемого тега (имя из палитры).
    new_color: Entity<ColorPickerState>,
    error: Option<String>,
}

/// Состояние модального пикера «стих → цепочка» (этап 23, упрощение
/// chains_picker форка): выбор/создание цепочки, refs предзаполнены
/// местом, поле утверждения; placement v1 — в конец цепочки (append
/// ко всем листьям, shell::chains::append_to_chain).
struct ChainPicker {
    /// Стих, который добавляется в цепочку (модуль-независимая ссылка).
    reference: VerseRef,
    chains: Vec<shell::chains::ChainSummary>,
    filter_input: Entity<InputState>,
    refs_input: Entity<InputState>,
    assertion_input: Entity<TextareaState>,
    error: Option<String>,
}

/// Счётчик идентификаторов вкладок чтения: per-вкладка стеки истории и
/// адресная навигация из неё (§5.5). Не персистентен — после перезапуска
/// вкладка получает новый id, а её стек восстанавливается из payload.
static NEXT_TAB_ID: AtomicU64 = AtomicU64::new(1);

/// Состояние вкладки чтения в payload layout.json (§7.2): место (верхний
/// видимый стих) + стек истории ←/→ этой вкладки. При восстановлении
/// открывается обычным путём — тем же кодом, что и клик пользователя.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReaderTabPayload {
    /// Версия схемы payload — мягкое чтение старых записей.
    pub v: u32,
    /// ModuleId (stem файла). Пустая строка — вкладка без открытой книги.
    pub module: String,
    pub book: i64,
    pub chapter: u16,
    pub verse: u16,
    /// Стек ←/→ вкладки (§5.5: история переживает перезапуск вместе с ней).
    #[serde(default)]
    pub history: Option<shell::navigation::HistoryDump>,
}

pub struct ReaderPanel {
    focus_handle: FocusHandle,
    /// Идентификатор вкладки (per-вкладка история, адресные события).
    tab_id: u64,
    /// Вкладка сейчас отображается в своей группе (set_active от дока).
    /// Публикуют место и пишут current_location ТОЛЬКО активные вкладки.
    active: bool,
    module_select: Entity<SelectState<SearchableVec<TranslationOption>>>,
    /// Копия списка переводов (в SelectState геттера нет) — для поиска
    /// индекса модуля при синхронизации выбора.
    translation_options: Vec<TranslationOption>,
    list_state: ListState,
    rows: Rc<Vec<Row>>,
    chapter_rows: Vec<(u16, usize)>,
    module: Option<ModuleId>,
    book: Option<BookNumber>,
    /// Название книги из модуля («Бытие») — заголовок вкладки.
    book_title: String,
    /// Короткое имя книги из модуля («Быт») — бегунок места и
    /// заголовок вкладки (этап 20).
    book_short: String,
    /// Текущее место по верхнему видимому стиху — в шапке «Бытие 3:16».
    position: (u16, u16),
    error: Option<String>,
    navigator: Option<Navigator>,
    /// Панель «Слово» сейчас видна: hover-карточки тогда не показываем
    /// (условие из форка: hover — только когда панель закрыта).
    word_panel_visible: bool,
    word_preview: Option<(Point<Pixels>, shell::services::WordDescription)>,
    /// Модуль комментариев для надстрочных маркеров (из BookViewModel).
    /// Аннотации открытой книги: свои — оформление, чужие — индикатор
    /// (этап 6, architecture.md §3.6).
    annotations: Rc<shell::BookAnnotations>,
    /// Стихи, помеченные цепочками: глава -> набор стихов (маркер ⛓,
    /// этап 12, architecture.md §6.8). Ссылки цепочек модуль-независимы,
    /// поэтому маркер показывается в любом переводе.
    chain_marks: Rc<HashMap<u16, HashSet<u16>>>,
    /// Теги стихов книги: (глава, стих) -> теги (цветные точки в строке,
    /// этап 21). Модуль-независимы, как цепочки.
    tag_marks: Rc<BTreeMap<(u16, u16), Vec<Tag>>>,
    /// Открытый пикер тегов (модалка поверх списка стихов).
    tag_picker: Option<TagPicker>,
    /// Открытый пикер «стих → цепочка» (модалка, этап 23).
    chain_picker: Option<ChainPicker>,
    _subscriptions: Vec<Subscription>,
}

impl ReaderPanel {
    pub const NAME: &'static str = "reader";

    /// Вкладка чтения из дефолтной раскладки / старого layout.json без
    /// payload: наследует место из session.json (миграция с MVP, §7.2).
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self::build(None, window, cx);
        // Если модуль исчез из каталога — restore вернёт None, панель
        // откроется пустой, как при самом первом запуске.
        if let Some((module, book, chapter, verse)) = app_services(cx).restore_reading_position() {
            this.open_book(
                &OpenBook {
                    module,
                    book,
                    chapter: Some(chapter),
                    verse: Some(verse),
                },
                window,
                cx,
            );
        }
        this
    }

    /// Пустая вкладка (Ctrl+T, новая ссылка в режиме new_tab): выбор книги
    /// навигатором, ничего не восстанавливаем.
    pub fn new_empty(
        source: Option<ModuleId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let services = app_services(cx);
        let preferred = if services.settings.read().new_tabs_use_current_module {
            source.or_else(|| {
                services
                    .current_location
                    .lock()
                    .as_ref()
                    .map(|loc| loc.module.clone())
            })
        } else {
            None
        };
        Self::build(preferred, window, cx)
    }

    pub(crate) fn module(&self) -> Option<ModuleId> {
        self.module.clone()
    }

    /// Вкладка из payload layout.json (§7.2): открываем сохранённое место
    /// обычным путём открытия и возвращаем её стек истории ←/→.
    pub fn new_restored(
        window: &mut Window,
        cx: &mut Context<Self>,
        payload: ReaderTabPayload,
    ) -> Self {
        let mut this = Self::build(None, window, cx);
        if let Some(history) = payload.history {
            navigation::history_register_stack(cx, this.tab_id, history);
        }
        // Пустая вкладка (module == "") переживает перезапуск пустой.
        if !payload.module.is_empty() && payload.book > 0 {
            this.open_book(
                &OpenBook {
                    module: ModuleId(payload.module),
                    book: BookNumber(payload.book),
                    chapter: Some(payload.chapter),
                    verse: Some(payload.verse),
                },
                window,
                cx,
            );
        }
        this
    }

    fn build(preferred: Option<ModuleId>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Переводы для выпадающего списка; дефолт — из настроек.
        let translations: Vec<TranslationOption> = app_services(cx)
            .registry
            .modules()
            .into_iter()
            .filter(|module| module.kind == bible_domain::ports::ModuleKind::BibleTranslation)
            .map(|module| TranslationOption {
                id: module.id,
                description: module.description.clone(),
            })
            .collect();
        let default_index = preferred
            .and_then(|module| translations.iter().position(|option| option.id == module))
            .or_else(|| {
                app_services(cx)
                    .default_translation()
                    .and_then(|module| translations.iter().position(|option| option.id == module))
            });
        let module = default_index
            .and_then(|index| translations.get(index))
            .map(|option| option.id.clone());

        let module_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(translations.clone()),
                default_index.map(IndexPath::new),
                window,
                cx,
            )
        });

        let mut subscriptions = Vec::new();

        // Выбор перевода в шапке.
        subscriptions.push(cx.subscribe_in(
            &module_select,
            window,
            |this, _, event: &SelectEvent<SearchableVec<TranslationOption>>, window, cx| {
                if let SelectEvent::Confirm(Some(module)) = event {
                    this.switch_module(module.clone(), window, cx);
                }
            },
        ));

        // Запросы навигации (перекрёстные ссылки, поиск, палитра) НЕ
        // слушаем: с появлением вкладок их маршрутизирует WorkspaceView —
        // в активную вкладку или в новую, по настройке link_open_mode (§5.4).

        // Видимость панели «Слово» управляет hover-карточками.
        subscriptions.push(cx.subscribe_in(
            &navigation::bus(cx),
            window,
            |this, _, event: &WordPanelVisibilityChanged, _, cx| {
                if this.word_panel_visible != event.0 {
                    this.word_panel_visible = event.0;
                    this.word_preview = None;
                    cx.notify();
                }
            },
        ));

        // Аннотации изменились (контекстное меню стиха, панель «Контекст») —
        // перечитываем оформление открытой книги.
        subscriptions.push(cx.subscribe_in(
            &navigation::bus(cx),
            window,
            |this, _, _: &navigation::AnnotationsChanged, _, cx| {
                this.reload_annotations(cx);
            },
        ));

        // Цепочки изменились — перечитываем маркеры ⛓ открытой книги.
        subscriptions.push(cx.subscribe_in(
            &navigation::bus(cx),
            window,
            |this, _, _: &navigation::ChainsChanged, _, cx| {
                this.reload_chain_marks(cx);
            },
        ));

        // Теги изменились (пикер, список в менюшке) — перечитываем
        // маркеры открытой книги.
        subscriptions.push(cx.subscribe_in(
            &navigation::bus(cx),
            window,
            |this, _, _: &navigation::TagsChanged, _, cx| {
                this.reload_tag_marks(cx);
            },
        ));

        // Запрос пикера тегов (Status bar — без ссылки, контекстное
        // меню стиха — со ссылкой). Откликается только активная вкладка.
        subscriptions.push(cx.subscribe_in(
            &navigation::bus(cx),
            window,
            |this, _, event: &navigation::OpenTagPicker, window, cx| {
                if event.target_tab == Some(this.tab_id) {
                    let reference = event.reference.or_else(|| this.current_reference());
                    if let Some(reference) = reference {
                        this.open_tag_picker(reference, window, cx);
                    }
                }
            },
        ));

        // Запрос пикера «стих → цепочка» (этап 23, та же схема, что и
        // пикер тегов). Откликается только активная вкладка.
        subscriptions.push(cx.subscribe_in(
            &navigation::bus(cx),
            window,
            |this, _, event: &navigation::OpenChainPicker, window, cx| {
                if event.target_tab == Some(this.tab_id) {
                    let reference = event.reference.or_else(|| this.current_reference());
                    if let Some(reference) = reference {
                        this.open_chain_picker(reference, window, cx);
                    }
                }
            },
        ));

        // Настройки изменились: обновляем список переводов (каталог мог
        // быть пересканирован) и переоткрываем книгу с новыми опциями
        // разбора текста (критический аппарат и др., architecture.md §9).
        subscriptions.push(cx.subscribe_in(
            &navigation::bus(cx),
            window,
            |this, _, _: &navigation::SettingsChanged, window, cx| {
                this.on_settings_changed(window, cx);
            },
        ));

        // Переход из истории ←/→ (кнопки в title bar, ctrl-alt-left/right):
        // открываем место без записи в стек (architecture.md §5.5).
        // Событие адресное — откликается только СВОЯ вкладка.
        subscriptions.push(cx.subscribe_in(
            &navigation::history(cx),
            window,
            |this, _, event: &navigation::HistoryNavigate, window, cx| {
                if event.tab == this.tab_id {
                    this.open_book_from_history(&event.entry, window, cx);
                }
            },
        ));

        // Overdraw в px: буфер строк за пределами viewport при измерении.
        let list_state = ListState::new(0, ListAlignment::Top, px(1024.));
        // Верхний видимый элемент списка управляет подписью места в шапке
        // и разделяемым «текущим местом чтения» (architecture.md §5.2).
        list_state.set_scroll_handler({
            let this = cx.entity().downgrade();
            move |event, window, cx| {
                _ = this.update(cx, |this, cx| {
                    if this.word_preview.take().is_some() {
                        cx.notify();
                    }
                    this.sync_position_to(event.visible_range.start, window, cx);
                });
            }
        });

        let this = Self {
            focus_handle: cx.focus_handle(),
            tab_id: NEXT_TAB_ID.fetch_add(1, Ordering::Relaxed),
            active: false,
            module_select,
            translation_options: translations,
            list_state,
            rows: Rc::new(Vec::new()),
            chapter_rows: Vec::new(),
            module,
            book: None,
            book_title: String::new(),
            book_short: String::new(),
            position: (1, 1),
            error: None,
            navigator: None,
            word_panel_visible: false,
            word_preview: None,
            annotations: Rc::new(shell::BookAnnotations::default()),
            chain_marks: Rc::new(HashMap::new()),
            tag_marks: Rc::new(BTreeMap::new()),
            tag_picker: None,
            chain_picker: None,
            _subscriptions: subscriptions,
        };

        this
    }

    /// Настройки/каталог изменились: перечитать список переводов (модули
    /// могли появиться/исчезнуть после перескана) и переоткрыть текущую
    /// книгу на том же месте — применить новые опции разбора текста.
    fn on_settings_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let translations: Vec<TranslationOption> = app_services(cx)
            .registry
            .modules()
            .into_iter()
            .filter(|module| module.kind == bible_domain::ports::ModuleKind::BibleTranslation)
            .map(|module| TranslationOption {
                id: module.id,
                description: module.description.clone(),
            })
            .collect();
        // Текущий модуль мог исчезнуть из каталога — тогда дефолт.
        let selected = self
            .module
            .as_ref()
            .and_then(|module| translations.iter().position(|option| &option.id == module))
            .or_else(|| {
                app_services(cx)
                    .default_translation()
                    .and_then(|default| translations.iter().position(|option| option.id == default))
            });
        self.translation_options = translations.clone();
        self.module_select.update(cx, |select, cx| {
            select.set_items(SearchableVec::new(translations), window, cx);
            select.set_selected_index(selected.map(IndexPath::new), window, cx);
        });
        self.module = selected.map(|index| self.translation_options[index].id.clone());

        if let (Some(module), Some(book)) = (self.module.clone(), self.book) {
            let (chapter, verse) = self.current_position();
            self.open_book(
                &OpenBook {
                    module,
                    book,
                    chapter: Some(chapter),
                    verse: Some(verse),
                },
                window,
                cx,
            );
        } else {
            // Книга не открыта (или переводов не осталось) — достаточно
            // перерисовать шапку с обновлённым списком.
            cx.notify();
        }
    }

    /// Перевод выбран в шапке: если книга открыта — переоткрываем её в новом
    /// модуле, сохраняя текущее место (стих у верхней кромки viewport).
    fn switch_module(&mut self, module: ModuleId, window: &mut Window, cx: &mut Context<Self>) {
        self.module = Some(module.clone());
        if let Some(book) = self.book {
            let (chapter, verse) = self.current_position();
            self.open_book(
                &OpenBook {
                    module,
                    book,
                    chapter: Some(chapter),
                    verse: Some(verse),
                },
                window,
                cx,
            );
        } else {
            cx.notify();
        }
    }

    /// Координаты стиха у верхней кромки списка (для сохранения места).
    fn current_position(&self) -> (u16, u16) {
        let ix = self
            .list_state
            .logical_scroll_top()
            .item_ix
            .min(self.rows.len().saturating_sub(1));
        match self.rows.get(ix) {
            Some(Row::Verse { chapter, verse, .. }) => (*chapter, *verse),
            Some(Row::ChapterHeading(chapter)) => (*chapter, 1),
            None => (1, 1),
        }
    }

    /// Открыть книгу в ЭТОЙ вкладке. Вызывает WorkspaceView-роутер
    /// (ссылки, палитра, история) — прямой подписки на OpenBook у вкладок
    /// нет, иначе все вкладки открывали бы одно и то же место.
    pub(crate) fn open_book(
        &mut self,
        request: &OpenBook,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_book_impl(request, true, window, cx);
    }

    /// Открытие места из истории ←/→: НЕ записывается в стек (курсор уже
    /// сдвинут вызывающим, architecture.md §5.5 — «курсор не гоняется
    /// за самим собой»).
    fn open_book_from_history(
        &mut self,
        entry: &shell::navigation::NavEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let request = OpenBook {
            module: entry.module.clone(),
            book: entry.book,
            chapter: Some(entry.chapter),
            verse: entry.verse,
        };
        self.open_book_impl(&request, false, window, cx);
    }

    fn open_book_impl(
        &mut self,
        request: &OpenBook,
        record: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = shell::open_book(app_services(cx), &request.module, request.book);
        match result {
            Ok(model) => {
                self.apply_model(model, request.chapter, request.verse, window, cx);
                if record {
                    navigation::history_push(
                        cx,
                        self.tab_id,
                        shell::navigation::NavEntry {
                            module: request.module.clone(),
                            book: request.book,
                            chapter: request.chapter.unwrap_or(1),
                            verse: request.verse,
                        },
                    );
                }
            }
            Err(error) => {
                log::error!("open_book failed: {error:#}");
                self.error = Some(format!("{error:#}"));
                cx.notify();
            }
        }
    }

    fn apply_model(
        &mut self,
        model: BookViewModel,
        chapter: Option<u16>,
        verse: Option<u16>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut rows = Vec::with_capacity(model.items.len() + 80);
        let mut current_chapter = 0u16;
        let mut target: Option<usize> = None;
        let mut chapter_rows = Vec::new();

        for item in &model.items {
            if item.chapter != current_chapter {
                current_chapter = item.chapter;
                chapter_rows.push((current_chapter, rows.len()));
                rows.push(Row::ChapterHeading(current_chapter));
            }
            if target.is_none()
                && chapter.is_none_or(|c| c == item.chapter)
                && verse.is_none_or(|v| v == item.verse)
            {
                target = Some(rows.len());
            }
            rows.push(Row::Verse {
                chapter: item.chapter,
                verse: item.verse,
                clean: item.clean.clone(),
                has_critical: item.has_critical,
            });
        }

        self.module = Some(model.module.clone());
        self.book = Some(model.book);
        self.book_title = model.title.clone();
        self.book_short = model.book_short.clone();
        self.position = (chapter.unwrap_or(1), verse.unwrap_or(1));
        self.error = None;
        self.word_preview = None;
        let chapters = chapter_rows.iter().map(|(chapter, _)| *chapter).collect();
        self.chapter_rows = chapter_rows;
        self.rows = Rc::new(rows);
        self.annotations = Rc::new(shell::book_annotations(
            app_services(cx),
            &model.module,
            model.book,
        ));
        self.chain_marks = Rc::new(shell::chains::verses_with_chains_in_book(
            app_services(cx),
            model.book.0,
            &chapters,
        ));
        self.tag_marks = Rc::new(shell::tags::book_tag_marks(app_services(cx), model.book));
        self.sync_module_select(window, cx);
        self.publish_location(cx);

        // Порядок важен: reset сбрасывает измерения, scroll_to ставит якорь
        // на точный индекс — применится при ближайшем layout.
        self.list_state.reset(self.rows.len());
        self.list_state.scroll_to(ListOffset {
            item_ix: target.unwrap_or(0),
            offset_in_item: px(0.),
        });
        cx.notify();
    }

    /// Перечитывает аннотации открытой книги (после изменений из
    /// контекстного меню стиха или панели «Контекст»).
    fn reload_annotations(&mut self, cx: &mut Context<Self>) {
        let (Some(module), Some(book)) = (&self.module, self.book) else {
            return;
        };
        self.annotations = Rc::new(shell::book_annotations(app_services(cx), module, book));
        cx.notify();
    }

    /// Перечитывает маркеры цепочек открытой книги (панель «Цепочки»
    /// изменила данные). Главы — из уже собранных строк.
    fn reload_chain_marks(&mut self, cx: &mut Context<Self>) {
        let Some(book) = self.book else {
            return;
        };
        let chapters: HashSet<u16> = self
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::ChapterHeading(chapter) => Some(*chapter),
                _ => None,
            })
            .collect();
        self.chain_marks = Rc::new(shell::chains::verses_with_chains_in_book(
            app_services(cx),
            book.0,
            &chapters,
        ));
        cx.notify();
    }

    /// Перечитывает маркеры тегов открытой книги (пикер, список тегов
    /// в менюшке изменили данные).
    fn reload_tag_marks(&mut self, cx: &mut Context<Self>) {
        let Some(book) = self.book else {
            return;
        };
        self.tag_marks = Rc::new(shell::tags::book_tag_marks(app_services(cx), book));
        cx.notify();
    }

    /// Каноническая ссылка на стих у верхней кромки списка (для пикера
    /// тегов из Status bar).
    fn current_reference(&self) -> Option<VerseRef> {
        self.book
            .map(|book| VerseRef::single(book, self.position.0, self.position.1))
    }

    // --- Пикер тегов стиха (этап 21) --------------------------------------

    fn open_tag_picker(
        &mut self,
        reference: VerseRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let filter_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(crate::i18n::tr_static("Фильтр или новый тег…"))
        });
        // Изменение фильтра — просто перерисовка (список фильтруется
        // при рендере, как в навигаторе).
        self._subscriptions.push(cx.subscribe_in(
            &filter_input,
            window,
            |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        ));
        window.focus(&filter_input.read(cx).focus_handle(cx), cx);
        let color = palette::base(bible_domain::tags::DEFAULT_COLOR, cx);
        let new_color = cx.new(|cx| ColorPickerState::new(window, cx).default_value(color));
        let services = app_services(cx);
        self.tag_picker = Some(TagPicker {
            reference,
            tags: shell::tags::list_tags(services),
            selected: shell::tags::tags_of_verse(services, &reference)
                .into_iter()
                .map(|tag| tag.id)
                .collect(),
            filter_input,
            new_color,
            error: None,
        });
        cx.notify();
    }

    fn close_tag_picker(&mut self, cx: &mut Context<Self>) {
        self.tag_picker = None;
        cx.notify();
    }

    /// Тогл тега на стихе: хранилище, локальное состояние, шина
    /// (чтение само перечитает маркеры по TagsChanged).
    fn tag_picker_toggle(&mut self, tag_id: i64, cx: &mut Context<Self>) {
        let Some(picker) = &mut self.tag_picker else {
            return;
        };
        match shell::tags::toggle_verse_tag(app_services(cx), tag_id, &picker.reference) {
            Ok(now_selected) => {
                if now_selected {
                    picker.selected.insert(tag_id);
                } else {
                    picker.selected.remove(&tag_id);
                }
                picker.error = None;
            }
            Err(error) => {
                log::error!("toggle verse tag failed: {error:#}");
                picker.error = Some(format!("Не удалось изменить тег: {error:#}"));
            }
        }
        navigation::notify_tags_changed(cx);
        cx.notify();
    }

    /// Создать тег по тексту фильтра и сразу поставить его на стих
    /// (схема create_and_apply форка).
    fn tag_picker_create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = &mut self.tag_picker else {
            return;
        };
        let name = picker.filter_input.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        let new_color = picker
            .new_color
            .read(cx)
            .value()
            .map(palette::to_hex)
            .unwrap_or_else(|| bible_domain::tags::DEFAULT_COLOR.to_owned());
        let reference = picker.reference;
        let services = app_services(cx);
        match shell::tags::create_tag(services, &name, &new_color) {
            Ok(tag) => match shell::tags::toggle_verse_tag(services, tag.id, &reference) {
                Ok(_) => {
                    picker.selected.insert(tag.id);
                    picker.error = None;
                    picker.tags = shell::tags::list_tags(services);
                    picker.filter_input.update(cx, |input, cx| {
                        input.set_value("", window, cx);
                    });
                }
                Err(error) => {
                    log::error!("tag verse failed: {error:#}");
                    picker.error = Some(format!("Не удалось поставить тег: {error:#}"));
                }
            },
            Err(error) => {
                log::error!("create tag failed: {error:#}");
                picker.error = Some(format!("Не удалось создать тег: {error:#}"));
            }
        }
        navigation::notify_tags_changed(cx);
        cx.notify();
    }

    // --- Пикер «стих → цепочка» (этап 23) ----------------------------------

    fn open_chain_picker(
        &mut self,
        reference: VerseRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let filter_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(crate::i18n::tr_static("Фильтр или новая цепочка…"))
        });
        self._subscriptions.push(cx.subscribe_in(
            &filter_input,
            window,
            |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        ));
        // Refs предзаполнены местом стиха — тот же текстовый формат, что
        // разбирает parse_refs_input (короткое имя книги реестра).
        let book = if self.book_short.is_empty() {
            &self.book_title
        } else {
            &self.book_short
        };
        let refs_value = format!("{book} {}:{}", reference.chapter, reference.verse);
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
        window.focus(&filter_input.read(cx).focus_handle(cx), cx);
        self.chain_picker = Some(ChainPicker {
            reference,
            chains: shell::chains::list_chains(app_services(cx)),
            filter_input,
            refs_input,
            assertion_input,
            error: None,
        });
        cx.notify();
    }

    fn close_chain_picker(&mut self, cx: &mut Context<Self>) {
        self.chain_picker = None;
        cx.notify();
    }

    /// Разобрать форму и дописать узел в конец выбранной цепочки.
    fn chain_picker_save(&mut self, chain_id: String, cx: &mut Context<Self>) {
        let Some(picker) = &self.chain_picker else {
            return;
        };
        let refs_text = picker.refs_input.read(cx).value().to_string();
        let assertion = picker.assertion_input.read(cx).value().trim().to_string();
        let services = app_services(cx);
        let refs = match crate::chains::parse_refs_input(services, &refs_text) {
            Ok(refs) => refs,
            Err(bad) => {
                if let Some(picker) = &mut self.chain_picker {
                    picker.error = Some(format!("не разобрал ссылку: {bad}"));
                }
                cx.notify();
                return;
            }
        };
        if refs.is_empty() && assertion.is_empty() {
            if let Some(picker) = &mut self.chain_picker {
                picker.error = Some("Добавьте хотя бы одну ссылку или утверждение.".to_string());
            }
            cx.notify();
            return;
        }
        match shell::chains::append_to_chain(services, &chain_id, refs, assertion) {
            Ok(_) => {
                navigation::notify_chains_changed(cx);
                self.close_chain_picker(cx);
            }
            Err(error) => {
                log::error!("append to chain failed: {error:#}");
                if let Some(picker) = &mut self.chain_picker {
                    picker.error = Some(format!("Не удалось сохранить узел: {error:#}"));
                }
                cx.notify();
            }
        }
    }

    /// Создать цепочку по тексту фильтра и сразу дописать в неё узел.
    fn chain_picker_create_and_save(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = &self.chain_picker else {
            return;
        };
        let title = picker.filter_input.read(cx).value().trim().to_string();
        if title.is_empty() {
            return;
        }
        match shell::chains::create_chain(app_services(cx), &title) {
            Ok(chain) => self.chain_picker_save(chain.id, cx),
            Err(error) => {
                log::error!("create chain failed: {error:#}");
                if let Some(picker) = &mut self.chain_picker {
                    picker.error = Some(format!("Не удалось создать цепочку: {error:#}"));
                }
                cx.notify();
            }
        }
    }

    /// Синхронизирует выпадающий список переводов с активным модулем
    /// (модуль мог быть установлен событием навигации, а не списком).
    fn sync_module_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(module) = &self.module else {
            return;
        };
        let index = self
            .translation_options
            .iter()
            .position(|option| &option.id == module);
        if let Some(index) = index {
            let selected = self.module_select.read(cx).selected_index(cx);
            if selected != Some(IndexPath::new(index)) {
                self.module_select.update(cx, |select, cx| {
                    select.set_selected_index(Some(IndexPath::new(index)), window, cx);
                });
            }
        }
    }

    /// Подпись места в шапке: «Бытие 3:16» (без имени модуля).
    fn location_label(&self) -> String {
        if self.book_title.is_empty() {
            crate::i18n::tr_static("Чтение").to_string()
        } else {
            // Короткое имя книги — компактный бегунок места (этап 20).
            let book = if self.book_short.is_empty() {
                &self.book_title
            } else {
                &self.book_short
            };
            format!("{} {}:{}", book, self.position.0, self.position.1)
        }
    }

    /// Публикует текущее место чтения для панелей словаря/комментариев
    /// (общий снимок в AppServices) и для бегунка места в Title bar
    /// (событие шины, architecture.md §6.6). Публикует только АКТИВНАЯ
    /// вкладка — место одно на приложение, иначе фоновые вкладки при
    /// восстановлении затирали бы место открытой.
    fn publish_location(&self, cx: &mut App) {
        if !self.active {
            return;
        }
        if let (Some(module), Some(book)) = (&self.module, self.book) {
            let location = shell::CurrentLocation {
                module: module.clone(),
                book,
                book_title: self.book_title.clone(),
                book_short: self.book_short.clone(),
                chapter: self.position.0,
                verse: self.position.1,
            };
            *app_services(cx).current_location.lock() = Some(location.clone());
            navigation::notify_location_changed(cx, location);
        }
    }

    /// Верхний видимый элемент списка управляет подписью места и
    /// разделяемым текущим местом чтения. Вызывается из scroll-обработчика
    /// списка — `list_state` здесь трогать нельзя (RefCell уже занят).
    fn sync_position_to(&mut self, top_ix: usize, _window: &mut Window, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let ix = top_ix.min(self.rows.len() - 1);
        let position = match &self.rows[ix] {
            Row::Verse { chapter, verse, .. } => (*chapter, *verse),
            Row::ChapterHeading(chapter) => (*chapter, 1),
        };
        if position == self.position {
            return;
        }
        self.position = position;
        self.publish_location(cx);
        cx.notify();
    }

    // --- Навигатор «книга → глава → стих» ---------------------------------

    fn open_navigator(&mut self, cx: &mut Context<Self>) {
        let Some(module) = &self.module else {
            return;
        };
        let books = app_services(cx).registry.lock().books_of(module);
        if books.is_empty() {
            return;
        }
        self.navigator = Some(Navigator {
            stage: NavStage::Book,
            books: Rc::new(books),
            picked_book: None,
            picked_chapter: None,
            chapter_count: 0,
            verse_count: 0,
        });
        cx.notify();
    }

    fn close_navigator(&mut self, cx: &mut Context<Self>) {
        self.navigator = None;
        cx.notify();
    }

    fn navigator_back(&mut self, cx: &mut Context<Self>) {
        let Some(navigator) = &mut self.navigator else {
            return;
        };
        match navigator.stage {
            NavStage::Book => {
                self.navigator = None;
            }
            NavStage::Chapter => {
                navigator.stage = NavStage::Book;
                navigator.picked_book = None;
                navigator.chapter_count = 0;
            }
            NavStage::Verse => {
                navigator.stage = NavStage::Chapter;
                navigator.picked_chapter = None;
                navigator.verse_count = 0;
            }
        }
        cx.notify();
    }

    fn navigator_pick_book(&mut self, book: BookEntry, cx: &mut Context<Self>) {
        let (Some(module), Some(navigator)) = (&self.module, &mut self.navigator) else {
            return;
        };
        let count = app_services(cx)
            .registry
            .lock()
            .chapter_count_of(module, BookNumber(book.book_number));
        navigator.picked_book = Some(book);
        navigator.chapter_count = count.max(1);
        navigator.stage = NavStage::Chapter;
        cx.notify();
    }

    fn navigator_pick_chapter(&mut self, chapter: u16, cx: &mut Context<Self>) {
        let (Some(module), Some(navigator)) = (&self.module, &mut self.navigator) else {
            return;
        };
        let Some(book) = &navigator.picked_book else {
            return;
        };
        let count = app_services(cx).registry.lock().verse_count_of(
            module,
            BookNumber(book.book_number),
            chapter,
        );
        navigator.picked_chapter = Some(chapter);
        navigator.verse_count = count.max(1);
        navigator.stage = NavStage::Verse;
        cx.notify();
    }

    fn navigator_pick_verse(&mut self, verse: u16, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(module), Some(navigator)) = (&self.module, &self.navigator) else {
            return;
        };
        let (Some(book), Some(chapter)) = (&navigator.picked_book, navigator.picked_chapter) else {
            return;
        };
        let request = OpenBook {
            module: module.clone(),
            book: BookNumber(book.book_number),
            chapter: Some(chapter),
            verse: Some(verse),
        };
        self.navigator = None;
        self.open_book(&request, window, cx);
    }

    // --- Рендер ------------------------------------------------------------

    fn render_row(
        ix: usize,
        tab_id: u64,
        row: &Row,
        module: &Option<ModuleId>,
        book: Option<BookNumber>,
        book_title: &str,
        book_short: &str,
        reading_font: &str,
        reading_size: f32,
        reading_line_spacing: f32,
        reading_column_width: f32,
        module_is_hebrew: bool,
        word_preview_target: Option<&WeakEntity<Self>>,
        annotations: &Rc<shell::BookAnnotations>,
        chain_marks: &Rc<HashMap<u16, HashSet<u16>>>,
        tag_marks: &Rc<BTreeMap<(u16, u16), Vec<Tag>>>,
        cx: &App,
    ) -> AnyElement {
        match row {
            Row::ChapterHeading(chapter) => div()
                .w_full()
                .px_3()
                .pt_3()
                .pb_1()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().foreground)
                .child(format!("Глава {chapter}"))
                .into_any_element(),
            Row::Verse {
                chapter,
                verse,
                clean,
                has_critical,
            } => {
                let marks = annotations.marks_of(*chapter, *verse);
                let (chapter, verse) = (*chapter, *verse);

                let mut row = h_flex()
                    .id(("verse-row", ix))
                    .w_full()
                    .px_3()
                    .py(px(2.0 * reading_line_spacing.clamp(0.6, 1.8)))
                    .gap_2()
                    .items_start()
                    .rounded_sm()
                    // Клик вне слова снимает его выбор и активирует стих.
                    .on_click({
                        let (module, book_title, book_short) = (
                            module.clone(),
                            book_title.to_string(),
                            book_short.to_string(),
                        );
                        move |_, _, cx| {
                            let (Some(module), Some(book)) = (&module, book) else {
                                return;
                            };
                            navigation::request_activate_verse(
                                cx,
                                navigation::VerseActivated {
                                    module: module.clone(),
                                    book,
                                    book_title: book_title.clone(),
                                    book_short: book_short.clone(),
                                    chapter,
                                    verse,
                                },
                            );
                        }
                    })
                    .context_menu({
                        let module = module.clone();
                        let (book_title, book_short) =
                            (book_title.to_string(), book_short.to_string());
                        move |menu, _, _cx| {
                            Self::verse_context_menu(
                                menu,
                                tab_id,
                                &module,
                                book,
                                chapter,
                                verse,
                                &book_title,
                                &book_short,
                                None,
                            )
                        }
                    });

                // Своя аннотация-выделение — фон всей строки (§3.6).
                if let Some(color) = marks.and_then(|marks| marks.highlight.as_deref()) {
                    row = row.bg(palette::highlight_bg(color, cx));
                }

                let mut text_cell = div().flex_1().min_w_0().text_color(cx.theme().foreground);
                // Снимок настроек шрифта передаётся в render_row из render():
                // не блокируем Settings отдельно для каждой строки списка.
                if reading_size > 0.0 {
                    text_cell = text_cell.text_size(px(reading_size));
                } else {
                    text_cell = text_cell.text_sm();
                }
                if !reading_font.is_empty() {
                    text_cell = text_cell.font_family(reading_font.to_string());
                }
                if reading_column_width > 0.0 {
                    text_cell = text_cell.max_w(px(reading_column_width));
                }
                // Подчёркивание стиха — цветная полоса снизу текста
                // (SelectablеText underline не умеет; v1-оформление).
                if let Some(color) = marks.and_then(|marks| marks.underline.as_deref()) {
                    text_cell = text_cell
                        .border_b_2()
                        .border_color(palette::underline_fg(color, cx));
                }
                // Цвет текста всего стиха (меню «Оформление стиха» панели Verse).
                if let Some(color) = marks.and_then(|marks| marks.text_color.as_deref()) {
                    text_cell = text_cell.text_color(palette::word_fg(color, cx));
                }
                text_cell = text_cell.child(Self::render_verse_text(
                    ix,
                    tab_id,
                    chapter,
                    verse,
                    clean,
                    module,
                    book,
                    book_title,
                    book_short,
                    word_preview_target,
                    *has_critical,
                    tag_marks
                        .get(&(chapter, verse))
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                    marks,
                    cx,
                ));

                // Маркеры справа: ✎ — мой комментарий, • — аннотации или
                // комментарии в других модулях (только индикатор, §3.6),
                // ⛓ — стих используется в цепочке (§6.8).
                let has_chain = chain_marks
                    .get(&chapter)
                    .is_some_and(|verses| verses.contains(&verse));
                let mut markers = h_flex().flex_shrink_0().gap_1().text_xs();
                if marks.is_some_and(|marks| marks.own_comment) {
                    markers = markers.child(div().text_color(cx.theme().accent).child("✎"));
                }
                if marks.is_some_and(|marks| marks.foreign) {
                    markers =
                        markers.child(div().text_color(cx.theme().muted_foreground).child("•"));
                }
                if has_chain {
                    // Иконка-ссылка вместо глифа ⛓ (плохо читался, этап 20).
                    markers = markers.child(
                        Icon::new(gpui_kit::assets::IconName::Link)
                            .xsmall()
                            .text_color(cx.theme().accent),
                    );
                }
                let verse_number = div()
                    .w(px(34.))
                    .flex_shrink_0()
                    .when(module_is_hebrew, |number| number.text_left())
                    .when(!module_is_hebrew, |number| number.text_right())
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(verse.to_string());
                let text_cell: AnyElement = if reading_column_width > 0.0 {
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .justify_center()
                        .child(text_cell)
                        .into_any_element()
                } else {
                    text_cell.into_any_element()
                };

                if module_is_hebrew {
                    // Зеркальная раскладка для RTL-модуля: служебные маркеры
                    // слева, текст в центре, номер стиха у правого края.
                    row.child(markers)
                        .child(text_cell)
                        .child(verse_number)
                        .into_any_element()
                } else {
                    row.child(verse_number)
                        .child(text_cell)
                        .child(markers)
                        .into_any_element()
                }
            }
        }
    }

    /// Вложенная палитра контекстного меню. Popover привязан к строке меню,
    /// поэтому раскрывается сбоку от места клика и не перекрывает выбранное
    /// слово/стих. В правую панель этот сценарий больше не маршрутизируется.
    fn local_style_item(label: &'static str, id: String, anchor: AnchoredRef) -> PopupMenuItem {
        PopupMenuItem::element(move |_, cx| {
            let anchor_for_content = anchor.clone();
            let content_id = id.clone();
            Popover::new(SharedString::from(format!("local-style-{id}")))
                .trigger(
                    Button::new(SharedString::from(format!("local-style-trigger-{id}")))
                        .ghost()
                        .xsmall()
                        .w_full()
                        .label(crate::i18n::tr(cx, label))
                        .icon(IconName::ChevronRight),
                )
                .content(move |_, _, cx| {
                    let mut panel = v_flex().w(px(292.)).gap_2().p_2();
                    for (row_label, kind) in [
                        ("Фон", AnnotationKind::Highlight),
                        ("Цвет шрифта", AnnotationKind::WordColor),
                        ("Подчёркивание", AnnotationKind::Underline),
                    ] {
                        let mut swatches = h_flex().gap_1().items_center();
                        for name in ["yellow", "green", "blue", "pink", "orange", "purple"] {
                            let color = palette::base(name, cx);
                            let palette_name = name.to_string();
                            let anchor = anchor_for_content.clone();
                            swatches = swatches.child(
                                div()
                                    .id(ElementId::Name(
                                        format!("local-style-{content_id}-{kind:?}-{name}").into(),
                                    ))
                                    .size(px(22.))
                                    .rounded_md()
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .bg(color)
                                    .cursor_pointer()
                                    .hover(|style| style.border_color(cx.theme().foreground))
                                    .tooltip({
                                        let label = crate::i18n::tr(cx, palette::label(name));
                                        move |window, cx| {
                                            gpui_kit::component::tooltip::Tooltip::new(
                                                label.clone(),
                                            )
                                            .build(window, cx)
                                        }
                                    })
                                    .on_click(move |_, _, cx| {
                                        if let Err(error) = shell::set_annotation(
                                            app_services(cx),
                                            anchor.clone(),
                                            kind,
                                            palette_name.clone(),
                                        ) {
                                            log::error!("set local annotation: {error:#}");
                                        }
                                        navigation::notify_annotations_changed(cx);
                                        cx.stop_propagation();
                                    }),
                            );
                        }
                        let clear_anchor = anchor_for_content.clone();
                        panel = panel.child(
                            h_flex()
                                .w_full()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(94.))
                                        .flex_shrink_0()
                                        .text_size(px(crate::ui_scale::panel_font_size(cx, 12.0)))
                                        .text_color(cx.theme().muted_foreground)
                                        .child(crate::i18n::tr(cx, row_label)),
                                )
                                .child(swatches)
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "local-style-clear-{content_id}-{kind:?}"
                                    )))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Close)
                                    .tooltip(crate::i18n::tr(cx, "Снять"))
                                    .on_click(
                                        move |_, _, cx| {
                                            if let Err(error) = shell::clear_annotation(
                                                app_services(cx),
                                                &clear_anchor,
                                                kind,
                                            ) {
                                                log::error!("clear local annotation: {error:#}");
                                            }
                                            navigation::notify_annotations_changed(cx);
                                            cx.stop_propagation();
                                        },
                                    ),
                                ),
                        );
                    }
                    panel
                })
        })
    }

    /// Контекстное меню строки стиха. Мутации — через shell; о результате
    /// панели узнают событием AnnotationsChanged.
    fn verse_context_menu(
        menu: gpui_kit::component::menu::PopupMenu,
        target_tab: u64,
        module: &Option<ModuleId>,
        book: Option<BookNumber>,
        chapter: u16,
        verse: u16,
        book_title: &str,
        book_short: &str,
        word: Option<bible_domain::verse::VerseToken>,
    ) -> gpui_kit::component::menu::PopupMenu {
        let (Some(module), Some(book)) = (module.clone(), book) else {
            return menu;
        };
        let mut menu = menu;
        if let Some(token) = &word {
            menu = menu.item(Self::local_style_item(
                "Цвет слова",
                format!("word-{}-{}-{}", book.0, chapter, verse),
                AnchoredRef {
                    module: module.clone(),
                    reference: VerseRef::single(book, chapter, verse),
                    word_index: Some(token.word_index_in_verse),
                },
            ));
        }
        menu = menu.item(Self::local_style_item(
            "Цвет стиха",
            format!("verse-{}-{}-{}", book.0, chapter, verse),
            AnchoredRef {
                module: module.clone(),
                reference: VerseRef::single(book, chapter, verse),
                word_index: None,
            },
        ));
        // Теги стиха (этап 21): модальный пикер активной вкладки.
        menu = menu.item(
            PopupMenuItem::new(crate::i18n::tr_static("Тег стиха…")).on_click({
                let reference = VerseRef::single(book, chapter, verse);
                move |_, _, cx| {
                    navigation::request_open_tag_picker_for_tab(cx, reference, target_tab);
                }
            }),
        );

        // Добавление стиха в цепочку (этап 23): модальный пикер.
        menu = menu.item(
            PopupMenuItem::new(crate::i18n::tr_static("Добавить в цепочку…")).on_click({
                let reference = VerseRef::single(book, chapter, verse);
                move |_, _, cx| {
                    navigation::request_open_chain_picker_for_tab(cx, reference, target_tab);
                }
            }),
        );

        // Персональная параллельная ссылка создаётся в секции правой панели.
        // Сначала делаем выбранный стих текущим, чтобы форма получила точный
        // источник даже при split screen.
        menu = menu.item(
            PopupMenuItem::new(crate::i18n::tr_static("Добавить параллельный стих")).on_click({
                let (module, book_title, book_short) = (
                    module.clone(),
                    book_title.to_string(),
                    book_short.to_string(),
                );
                move |_, _, cx| {
                    navigation::history_set_active(cx, target_tab);
                    navigation::request_activate_verse(
                        cx,
                        navigation::VerseActivated {
                            module: module.clone(),
                            book,
                            book_title: book_title.clone(),
                            book_short: book_short.clone(),
                            chapter,
                            verse,
                        },
                    );
                    navigation::request_open_verse_section(cx, navigation::VerseSection::CrossRefs);
                }
            }),
        );

        // Комментарий к стиху (этап 24): стих становится активным для
        // панели «Verse», раскрывается её секция Notes с редактором.
        menu = menu.item(
            PopupMenuItem::new(crate::i18n::tr_static("Комментарий к стиху")).on_click({
                let (module, book_title, book_short) = (
                    module.clone(),
                    book_title.to_string(),
                    book_short.to_string(),
                );
                move |_, _, cx| {
                    navigation::history_set_active(cx, target_tab);
                    navigation::request_activate_verse(
                        cx,
                        navigation::VerseActivated {
                            module: module.clone(),
                            book,
                            book_title: book_title.clone(),
                            book_short: book_short.clone(),
                            chapter,
                            verse,
                        },
                    );
                    navigation::request_open_verse_section(cx, navigation::VerseSection::Notes);
                }
            }),
        );

        menu
    }

    /// Текст стиха: самостоятельный SelectableText для слова и отдельный
    /// участок для пробелов/пунктуации. Подчёркивание не захватывает пробел.
    /// Слова — кликабельные
    /// (hover-подсветка, клик → панель «Слово»; hover-карточка Strong's,
    /// когда панель «Слово» закрыта). Каждый участок — самостоятельный
    /// SelectableText (общий handle на книгу не работает — update_runs
    /// затирает runs при каждом paint); сквозное выделение собирает
    /// оконный слой по document_order (architecture.md §6.3.2).
    fn render_verse_text(
        row_ix: usize,
        tab_id: u64,
        chapter: u16,
        verse: u16,
        clean: &CleanVerse,
        module: &Option<ModuleId>,
        book: Option<BookNumber>,
        book_title: &str,
        book_short: &str,
        word_preview_target: Option<&WeakEntity<Self>>,
        has_critical: bool,
        tags: &[Tag],
        marks: Option<&shell::VerseMarks>,
        cx: &App,
    ) -> AnyElement {
        // Бюджет участков на стих — для уникальных id и document_order.
        const CHUNK_BUDGET: usize = 4096;

        // MyBible stores Hebrew in logical Unicode order. Verse words are
        // separate GPUI children, so only Hebrew-only text gets RTL child flow.
        // Greek/Russian/Latin and mixed-script text remain in normal LTR flow;
        // the Windows DirectWrite renderer handles the glyph order inside each RTL run.
        let is_hebrew = crate::bidi::is_hebrew_paragraph(&clean.clean_text);

        let morphology_colors = {
            let settings = app_services(cx).settings.read();
            (settings.morphology_color_scheme
                == shell::settings::MorphologyColorScheme::PartsOfSpeech)
                .then(|| settings.morphology_colors.clone())
        };
        let morphology_background = marks
            .and_then(|marks| marks.highlight.as_deref())
            .map(|color| {
                cx.theme()
                    .background
                    .blend(palette::highlight_bg(color, cx))
            })
            .unwrap_or_else(|| cx.theme().background);
        let tokens = &clean.tokens;
        let mut spans: Vec<AnyElement> = Vec::with_capacity(tokens.len() * 2 + 1);
        let copy_group = bible_copy_group(chapter, verse);
        let mut cursor = 0usize;
        // Обход через peekable даёт следующий диапазон за O(1), без
        // повторного поиска по оставшейся части токенов для каждого слова.
        let mut token_iter = tokens
            .iter()
            .filter(|token| token.range.end > token.range.start)
            .peekable();

        while let Some(token) = token_iter.next() {
            // Ведущий разделитель стиха (открывающая кавычка и т.п.) —
            // отдельным некликабельным участком.
            if token.range.start > cursor {
                let id = row_ix * CHUNK_BUDGET + spans.len();
                let text = clean.clean_text[cursor..token.range.start].to_string();
                spans.push(
                    SelectableText::new(("verse-chunk", id), text)
                        .flow_selection()
                        .copy_group(copy_group)
                        .document_order(id as u64)
                        .into_any_element(),
                );
            }
            // Слово и хвостовые разделители — разные участки, чтобы
            // border-bottom подчёркивал только буквы, а не следующий пробел.
            let span_end = token_iter
                .peek()
                .map(|next| next.range.start)
                .unwrap_or(clean.clean_text.len());
            let span_text = clean.clean_text[token.range.clone()].to_string();
            let trailing_text = clean.clean_text[token.range.end..span_end].to_string();
            cursor = span_end;

            let id = row_ix * CHUNK_BUDGET + spans.len();
            let has_data = token.strong.is_some() || token.morph.is_some();

            let mut word_span = div()
                .id(("verse-word", id))
                .rounded_sm()
                .child(
                    SelectableText::new(("verse-chunk", id), span_text)
                        .flow_selection()
                        .copy_group(copy_group)
                        .document_order(id as u64),
                )
                .on_click({
                    let (module, book_title, book_short, token) = (
                        module.clone(),
                        book_title.to_string(),
                        book_short.to_string(),
                        token.clone(),
                    );
                    let preview = word_preview_target.cloned().filter(|_| has_data);
                    move |event: &ClickEvent, window, cx| {
                        if TextSelection::has_selection(window, cx) {
                            return;
                        }
                        let (Some(module), Some(book)) = (&module, book) else {
                            return;
                        };
                        let description = shell::describe_word(
                            app_services(cx),
                            module,
                            book,
                            &book_title,
                            chapter,
                            verse,
                            &token,
                        );
                        if let (Some(reader), ClickEvent::Mouse(click)) = (&preview, event) {
                            let position = click.up.position + point(px(4.), px(8.));
                            let _ = reader.update(cx, |this, cx| {
                                this.word_preview = Some((position, description.clone()));
                                cx.notify();
                            });
                        }
                        // Клик по слову — тоже активация стиха (этап 14):
                        // SelectableText внутри перехватывает mouse down,
                        // поэтому событие строки может не дойти — шлём
                        // явно, чтобы секции панели «Verse» перечитали стих.
                        navigation::request_activate_verse(
                            cx,
                            navigation::VerseActivated {
                                module: module.clone(),
                                book,
                                book_title: book_title.clone(),
                                book_short: book_short.clone(),
                                chapter,
                                verse,
                            },
                        );
                        navigation::request_select_word(cx, description);
                        cx.stop_propagation();
                    }
                })
                // Контекстное меню слова (architecture.md §6.7): строится
                // лениво при открытии; пункты аннотаций — этап 6 плана.
                .context_menu({
                    let (module, book_title, token, verse_text) = (
                        module.clone(),
                        book_title.to_string(),
                        token.clone(),
                        clean.clean_text.clone(),
                    );
                    let book_short = book_short.to_string();
                    move |menu, window, cx| {
                        let selected_text = TextSelection::selected_text(window, cx);
                        let mut menu = menu
                            .item(
                                PopupMenuItem::new(crate::i18n::tr_static("Копировать слово"))
                                    .on_click({
                                        let text = token.text.clone();
                                        let module = module.clone();
                                        let book_short = book_short.clone();
                                        move |_, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                bible_copy_text(
                                                    cx,
                                                    &text,
                                                    module.as_ref(),
                                                    &book_short,
                                                    chapter,
                                                    verse,
                                                    None,
                                                ),
                                            ));
                                        }
                                    }),
                            )
                            .item(
                                PopupMenuItem::new(crate::i18n::tr_static("Копировать стих"))
                                    .on_click({
                                        let text = verse_text.clone();
                                        let module = module.clone();
                                        let book_short = book_short.clone();
                                        let selected_text = selected_text.clone();
                                        move |_, _, cx| {
                                            let copied = if selected_text.is_empty() {
                                                bible_copy_text(
                                                    cx,
                                                    &text,
                                                    module.as_ref(),
                                                    &book_short,
                                                    chapter,
                                                    verse,
                                                    None,
                                                )
                                            } else {
                                                selected_text.clone()
                                            };
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                copied,
                                            ));
                                        }
                                    }),
                            )
                            .separator()
                            .item(
                                PopupMenuItem::new(crate::i18n::tr_static(
                                    "Подробнее — панель «Слово»",
                                ))
                                .on_click({
                                    let (module, book_title, token) =
                                        (module.clone(), book_title.clone(), token.clone());
                                    let book_short = book_short.clone();
                                    move |_, _, cx| {
                                        let (Some(module), Some(book)) = (&module, book) else {
                                            return;
                                        };
                                        let description = shell::describe_word(
                                            app_services(cx),
                                            module,
                                            book,
                                            &book_title,
                                            chapter,
                                            verse,
                                            &token,
                                        );
                                        navigation::request_activate_verse(
                                            cx,
                                            navigation::VerseActivated {
                                                module: module.clone(),
                                                book,
                                                chapter,
                                                verse,
                                                book_title: book_title.clone(),
                                                book_short: book_short.clone(),
                                            },
                                        );
                                        navigation::request_open_verse_section(
                                            cx,
                                            navigation::VerseSection::Word,
                                        );
                                        navigation::request_select_word(cx, description);
                                    }
                                }),
                            );
                        // Статья Strong's — запрос в словарь при открытии меню.
                        if let (Some(module), Some(book)) = (module.clone(), book) {
                            let description = shell::describe_word(
                                app_services(cx),
                                &module,
                                book,
                                &book_title,
                                chapter,
                                verse,
                                &token,
                            );
                            let usage_description = description.clone();
                            menu = menu.item(
                                PopupMenuItem::new(crate::i18n::tr_static(
                                    "График употребляемости",
                                ))
                                .on_click(move |_, _, cx| {
                                    crate::usage::open_word_usage(usage_description.clone(), cx);
                                }),
                            );
                            if description.strong_entry.is_some() {
                                if let (Some(strong), Some(dict)) =
                                    (&description.strong, &description.strongs_dict)
                                {
                                    let label = format!("Strong's {strong} — в словаре");
                                    let (dict, topic) = (dict.clone(), strong.clone());
                                    menu = menu.item(PopupMenuItem::new(label).on_click(
                                        move |_, _, cx| {
                                            navigation::request_open_topic(
                                                cx,
                                                dict.clone(),
                                                topic.clone(),
                                            );
                                        },
                                    ));
                                }
                            }
                        }
                        Self::verse_context_menu(
                            menu.separator(),
                            tab_id,
                            &module,
                            book,
                            chapter,
                            verse,
                            &book_title,
                            &book_short,
                            Some(token.clone()),
                        )
                    }
                });

            // Пословные аннотации (этап 6, §3.6): фон слова, цвет текста,
            // подчёркивание. word_index — из парсера стиха.
            let word_has_text_color = marks.is_some_and(|marks| {
                marks.words.iter().any(|(word_ix, kind, _)| {
                    *word_ix == token.word_index_in_verse && *kind == AnnotationKind::WordColor
                })
            });
            let word_morphology_background = marks
                .and_then(|marks| {
                    marks.words.iter().find_map(|(word_ix, kind, color)| {
                        (*word_ix == token.word_index_in_verse
                            && *kind == AnnotationKind::Highlight)
                            .then(|| morphology_background.blend(palette::highlight_bg(color, cx)))
                    })
                })
                .unwrap_or(morphology_background);
            if !word_has_text_color
                && marks.is_none_or(|marks| marks.text_color.is_none())
                && let (Some(colors), Some(morph)) = (&morphology_colors, token.morph.as_deref())
                && let Some(color) =
                    crate::morphology_colors::color(morph, colors, word_morphology_background)
            {
                word_span = word_span.text_color(color);
            }
            if let Some(marks) = marks {
                for (word_ix, kind, color) in &marks.words {
                    if *word_ix != token.word_index_in_verse {
                        continue;
                    }
                    word_span = match kind {
                        AnnotationKind::Highlight => word_span.bg(palette::highlight_bg(color, cx)),
                        AnnotationKind::WordColor => {
                            word_span.text_color(palette::word_fg(color, cx))
                        }
                        AnnotationKind::Underline => word_span
                            .border_b_1()
                            .border_color(palette::underline_fg(color, cx)),
                    };
                }
            }

            spans.push(word_span.into_any_element());

            if !trailing_text.is_empty() {
                let trailing_id = row_ix * CHUNK_BUDGET + spans.len();
                spans.push(
                    SelectableText::new(("verse-chunk", trailing_id), trailing_text)
                        .flow_selection()
                        .copy_group(copy_group)
                        .document_order(trailing_id as u64)
                        .into_any_element(),
                );
            }
        }
        if cursor < clean.clean_text.len() {
            let chunk_ix = spans.len();
            let text = clean.clean_text[cursor..].to_string();
            spans.push(
                SelectableText::new(("verse-chunk", row_ix * CHUNK_BUDGET + chunk_ix), text)
                    .flow_selection()
                    .copy_group(copy_group)
                    .document_order((row_ix * CHUNK_BUDGET + chunk_ix) as u64)
                    .into_any_element(),
            );
        }

        if has_critical && let (Some(module), Some(book)) = (module.clone(), book) {
            let reference = VerseRef::single(book, chapter, verse);
            let label = format!("{book_short} {chapter}:{verse} · Критический аппарат");
            spans.push(
                HoverCard::new(("verse-critical", row_ix))
                    .open_delay(Duration::from_millis(250))
                    .trigger(
                        div().id(("verse-critical-icon", row_ix)).px_1().child(
                            Icon::new(gpui_kit::assets::IconName::FileText)
                                .size(crate::ui_scale::icon_size(cx))
                                .text_color(cx.theme().muted_foreground),
                        ),
                    )
                    .content(move |_, _, cx| {
                        let text = app_services(cx)
                            .registry
                            .verse_range(&module, &reference)
                            .map(|raw| crate::verse_preview::display_text(&raw, true))
                            .unwrap_or_else(|error| format!("Не удалось прочитать стих: {error}"));
                        v_flex()
                            .w(px(560.))
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(label.clone()),
                            )
                            .child(
                                div()
                                    .id("verse-critical-text")
                                    .max_h(px(400.))
                                    .when(crate::bidi::is_hebrew_paragraph(&text), |text| {
                                        text.text_right()
                                    })
                                    .text_size(px(crate::ui_scale::panel_font_size(cx, 16.)))
                                    .child(SelectableText::new("critical-verse", text))
                                    .overflow_y_scrollbar(),
                            )
                    })
                    .into_any_element(),
            );
        }
        if let Some(book) = book
            && !tags.is_empty()
        {
            let tags = tags.to_vec();
            let reference = VerseRef::single(book, chapter, verse);
            spans.push(
                Button::new(("verse-tags", row_ix))
                    .ghost()
                    .xsmall()
                    .child(
                        Icon::new(gpui_kit::assets::IconName::Tag)
                            .size(crate::ui_scale::icon_size(cx)),
                    )
                    .accessibility_label(crate::i18n::tr_static("Теги стиха"))
                    .dropdown_menu(move |mut menu, _, _| {
                        menu = menu.scrollable(true);
                        for tag in &tags {
                            menu = menu.item(PopupMenuItem::element({
                                let (name, color) = (tag.name.clone(), tag.color.clone());
                                move |_, cx| {
                                    h_flex()
                                        .gap_2()
                                        .child(
                                            div()
                                                .size(px(9.))
                                                .rounded_full()
                                                .bg(palette::word_fg(&color, cx)),
                                        )
                                        .child(name.clone())
                                }
                            }));
                        }
                        menu.separator().item(
                            PopupMenuItem::new(crate::i18n::tr_static("Изменить теги…")).on_click(
                                move |_, _, cx| {
                                    navigation::request_open_tag_picker_for_tab(
                                        cx, reference, tab_id,
                                    )
                                },
                            ),
                        )
                    })
                    .into_any_element(),
            );
        }

        let mut text_flow = h_flex().w_full().min_w_0().flex_wrap().items_baseline();
        if is_hebrew {
            text_flow = text_flow.flex_row_reverse().text_right();
        }
        text_flow.children(spans).into_any_element()
    }

    fn render_word_preview(description: &shell::services::WordDescription, cx: &App) -> AnyElement {
        let mut card = v_flex().w(px(320.)).p_3().gap_2();

        // Заголовок как в форке: «лексема транслитерация [чтение] — G25».
        let headline = description
            .lexeme
            .clone()
            .filter(|lexeme| !lexeme.is_empty())
            .unwrap_or_else(|| description.word.clone());
        let mut head = h_flex().w_full().gap_2().items_baseline().child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().foreground)
                .child(headline),
        );
        if let Some(transliteration) = &description.transliteration {
            head = head.child(
                div()
                    .italic()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(transliteration.clone()),
            );
        }
        if let Some(strong) = &description.strong {
            head = head.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().accent)
                    .child(strong.clone()),
            );
        }
        card = card.child(head);

        if let Some(gloss) = &description.morph_gloss {
            card = card.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(gloss.clone()),
            );
        }

        // Краткое определение (колонка short_definition) читается лучше
        // обрезанной разметки; без неё — выдержка полной статьи.
        let body_text = description
            .short_definition
            .clone()
            .filter(|short| !short.is_empty())
            .or_else(|| {
                description
                    .strong_entry
                    .as_ref()
                    .map(|entry| excerpt(&strip_markup(entry), 400))
            });
        match body_text {
            Some(text) => {
                card = card.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_sm()
                        .text_color(cx.theme().foreground)
                        .child(text),
                );
            }
            None if description.strong.is_some() => {
                card = card.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(crate::i18n::tr_static(
                            "Статья не найдена в Strong's-словаре.",
                        )),
                );
            }
            None => {}
        }

        card.into_any_element()
    }

    /// Содержимое hover-карточки маркера комментария: текст комментария
    /// именно к этому стиху. Запрос — при открытии карточки.
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let control = |id| {
            h_flex()
                .id(id)
                .w(px(156.))
                .h_7()
                .px_2()
                .items_center()
                .min_w_0()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().secondary)
        };
        let translation = control("reader-module").child(
            Select::new(&self.module_select)
                .small()
                .appearance(false)
                .w_full(),
        );
        let place = control("open-navigator")
            .gap_1()
            .cursor_pointer()
            .hover(|style| style.bg(crate::ui_scale::hover_background(cx)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .truncate()
                    .child(self.location_label()),
            )
            .child(Icon::new(gpui_kit::component::IconName::ChevronDown).xsmall())
            .on_click(cx.listener(|this, _, _, cx| this.open_navigator(cx)));
        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(translation)
            .child(place)
            .child(
                Button::new("reader-history-back")
                    .xsmall()
                    .icon(gpui_kit::component::IconName::ChevronLeft)
                    .tooltip(crate::i18n::tr_static("Назад"))
                    .disabled(!navigation::history_can_back(cx))
                    .on_click(|_, _, cx| navigation::history_go_back(cx)),
            )
            .child(
                Button::new("reader-history-forward")
                    .xsmall()
                    .icon(gpui_kit::component::IconName::ChevronRight)
                    .tooltip(crate::i18n::tr_static("Вперёд"))
                    .disabled(!navigation::history_can_forward(cx))
                    .on_click(|_, _, cx| navigation::history_go_forward(cx)),
            )
    }

    fn render_navigator(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let stage = self.navigator.as_ref().map(|nav| nav.stage);
        let content = match stage {
            Some(NavStage::Book) => self.render_book_stage(cx),
            Some(NavStage::Chapter) => self.render_chapter_stage(cx),
            Some(NavStage::Verse) => self.render_verse_stage(cx),
            None => div().into_any_element(),
        };

        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(black().opacity(0.4))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.close_navigator(cx);
                }),
            )
            .child(
                v_flex()
                    .w(px(680.))
                    .max_h(px(560.))
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(content),
            )
            .into_any_element()
    }

    /// Модальный пикер тегов стиха (этап 21): чипы-тоглы, фильтр с
    /// созданием, палитра цвета нового тега. Мутации — через shell,
    /// обновление видно сразу (TagsChanged → маркеры перечитываются).
    fn render_tag_picker(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(picker) = &self.tag_picker else {
            return div().into_any_element();
        };
        let reference = picker.reference;
        let book = if self.book_short.is_empty() {
            &self.book_title
        } else {
            &self.book_short
        };
        let filter = picker.filter_input.read(cx).value().trim().to_string();
        let exact_match = picker
            .tags
            .iter()
            .any(|tag| tag.name.eq_ignore_ascii_case(&filter));
        let can_create = !filter.is_empty() && !exact_match;
        let needle = filter.to_lowercase();
        let visible: Vec<Tag> = picker
            .tags
            .iter()
            .filter(|tag| needle.is_empty() || tag.name.to_lowercase().contains(&needle))
            .cloned()
            .collect();

        let swatches = div()
            .w(px(104.))
            .child(ColorSelect::new(&picker.new_color).xsmall().w_full());

        // Чипы тегов: клик — тогл на стихе.
        let mut chips = h_flex().w_full().flex_wrap().gap_1();
        for tag in visible {
            let active = picker.selected.contains(&tag.id);
            let tag_id = tag.id;
            let active_bg = cx.theme().primary;
            let active_fg = cx.theme().primary_foreground;
            let foreground = if active {
                active_fg
            } else {
                cx.theme().foreground
            };
            chips = chips.child(
                h_flex()
                    .id(ElementId::Name(format!("tag-chip-{tag_id}").into()))
                    .px_2()
                    .py(px(2.))
                    .gap_1()
                    .items_center()
                    .rounded_md()
                    .border_1()
                    .border_color(if active { active_bg } else { cx.theme().border })
                    .when(active, |this| {
                        this.bg(active_bg).font_weight(FontWeight::SEMIBOLD)
                    })
                    .cursor_pointer()
                    .hover(|style| {
                        style.bg(if active {
                            active_bg
                        } else {
                            cx.theme().accent.opacity(0.15)
                        })
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tag_picker_toggle(tag_id, cx);
                    }))
                    .children(active.then(|| {
                        Icon::new(gpui_kit::assets::IconName::Check)
                            .xsmall()
                            .text_color(active_fg)
                    }))
                    .child(
                        div()
                            .w(px(10.))
                            .h(px(10.))
                            .rounded_full()
                            .bg(palette::word_fg(&tag.color, cx)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(foreground)
                            .child(tag.name.clone()),
                    ),
            );
        }

        let header = h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .child(format!(
                        "Теги — {book} {}:{}",
                        reference.chapter, reference.verse
                    )),
            )
            .child(
                div()
                    .id("tag-picker-close")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .hover(|style| style.bg(crate::ui_scale::hover_background(cx)))
                    .child("✕")
                    .on_click(cx.listener(|this, _, _, cx| this.close_tag_picker(cx))),
            );

        let mut body = v_flex()
            .w(px(440.))
            .max_h(px(480.))
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(header),
            )
            .child(
                v_flex()
                    .w_full()
                    .p_3()
                    .gap_2()
                    .child(Input::new(&picker.filter_input))
                    .child(swatches),
            );

        if let Some(error) = &picker.error {
            body = body.child(
                div()
                    .px_3()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        if can_create {
            let label = format!("Создать тег «{filter}»");
            body = body.child(
                div().px_3().child(
                    Button::new("tag-picker-create")
                        .small()
                        .label(label)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.tag_picker_create(window, cx);
                        })),
                ),
            );
        }
        body = body.child(
            div()
                .id("tag-picker-chips")
                .w_full()
                .flex_1()
                .min_h_0()
                .p_3()
                .child(chips)
                .overflow_y_scrollbar(),
        );

        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(black().opacity(0.4))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.close_tag_picker(cx);
                }),
            )
            .child(body)
            .into_any_element()
    }

    /// Модалка «стих → цепочка» (этап 23): выбор/создание цепочки, refs,
    /// утверждение. Клик по цепочке сразу сохраняет (placement v1 — в
    /// конец цепочки), как pick_chain → save в форке, но без стадий.
    fn render_chain_picker(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(picker) = &self.chain_picker else {
            return div().into_any_element();
        };
        let reference = picker.reference;
        let book = if self.book_short.is_empty() {
            &self.book_title
        } else {
            &self.book_short
        };
        let filter = picker.filter_input.read(cx).value().trim().to_string();
        let exact_match = picker
            .chains
            .iter()
            .any(|chain| chain.title.eq_ignore_ascii_case(&filter));
        let can_create = !filter.is_empty() && !exact_match;
        let needle = filter.to_lowercase();
        let visible: Vec<shell::chains::ChainSummary> = picker
            .chains
            .iter()
            .filter(|chain| needle.is_empty() || chain.title.to_lowercase().contains(&needle))
            .cloned()
            .collect();

        // Список цепочек: клик — сохранить узел в конец этой цепочки.
        let mut rows = v_flex().w_full().gap_1();
        for chain in visible {
            let chain_id = chain.id.clone();
            rows = rows.child(
                h_flex()
                    .id(ElementId::Name(format!("chain-pick-{chain_id}").into()))
                    .w_full()
                    .px_2()
                    .py_1()
                    .items_center()
                    .justify_between()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(crate::ui_scale::hover_background(cx)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.chain_picker_save(chain_id.clone(), cx);
                    }))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child(chain.title.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("узлов: {}", chain.node_count)),
                    ),
            );
        }

        let header = h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .child(format!(
                        "В цепочку — {book} {}:{}",
                        reference.chapter, reference.verse
                    )),
            )
            .child(
                div()
                    .id("chain-picker-close")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .hover(|style| style.bg(crate::ui_scale::hover_background(cx)))
                    .child("✕")
                    .on_click(cx.listener(|this, _, _, cx| this.close_chain_picker(cx))),
            );

        let mut body = v_flex()
            .w(px(460.))
            .max_h(px(560.))
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(header),
            )
            .child(
                v_flex()
                    .w_full()
                    .p_3()
                    .gap_2()
                    .child(Input::new(&picker.filter_input))
                    .child(Input::new(&picker.refs_input))
                    .child(
                        Button::new("chain-picker-add-verse")
                            .xsmall()
                            .ghost()
                            .label(crate::i18n::tr_static("+ Добавить ссылку на стих"))
                            .on_click({
                                let input = picker.refs_input.clone();
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
                    .child(Textarea::new(&picker.assertion_input).h(px(70.))),
            );

        if let Some(error) = &picker.error {
            body = body.child(
                div()
                    .px_3()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        if can_create {
            let label = format!("Создать цепочку «{filter}» и добавить");
            body = body.child(
                div().px_3().child(
                    Button::new("chain-picker-create")
                        .small()
                        .label(label)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.chain_picker_create_and_save(cx);
                        })),
                ),
            );
        }
        body = body.child(
            div()
                .id("chain-picker-list")
                .w_full()
                .flex_1()
                .min_h_0()
                .p_3()
                .child(rows)
                .overflow_y_scrollbar(),
        );

        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(black().opacity(0.4))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.close_chain_picker(cx);
                }),
            )
            .child(body)
            .into_any_element()
    }

    fn render_nav_header(&self, title: String, show_back: bool, cx: &mut Context<Self>) -> Div {
        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .when(show_back, |this| {
                this.child(
                    div()
                        .id("nav-back")
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .hover(|style| style.bg(crate::ui_scale::hover_background(cx)))
                        .child(crate::i18n::tr_static("← Назад"))
                        .on_click(cx.listener(|this, _, _, cx| this.navigator_back(cx))),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .child(title),
            )
            .child(
                div()
                    .id("nav-close")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .hover(|style| style.bg(crate::ui_scale::hover_background(cx)))
                    .child("✕")
                    .on_click(cx.listener(|this, _, _, cx| this.close_navigator(cx))),
            )
    }

    fn render_book_stage(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(navigator) = &self.navigator else {
            return div().into_any_element();
        };
        let books = navigator.books.clone();
        let old_testament: Vec<BookEntry> = books
            .iter()
            .filter(|book| book.book_number < OLD_TESTAMENT_BOUNDARY)
            .cloned()
            .collect();
        let new_testament: Vec<BookEntry> = books
            .iter()
            .filter(|book| book.book_number >= OLD_TESTAMENT_BOUNDARY)
            .cloned()
            .collect();

        let mut sections = v_flex().w_full().gap_2().p_3();
        for (label, books) in [
            ("Ветхий Завет", old_testament),
            ("Новый Завет", new_testament),
        ] {
            if books.is_empty() {
                continue;
            }
            sections = sections
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                )
                .child(self.render_book_grid(books, cx));
        }

        v_flex()
            .w_full()
            .min_h_0()
            .child(self.render_nav_header(
                crate::i18n::tr_static("Выбор книги").to_string(),
                false,
                cx,
            ))
            .child(
                div()
                    .id("nav-book-grid")
                    .w_full()
                    .min_h_0()
                    .child(sections)
                    .overflow_y_scrollbar(),
            )
            .into_any_element()
    }

    fn render_book_grid(&self, books: Vec<BookEntry>, cx: &mut Context<Self>) -> AnyElement {
        crate::verse_picker::grid(
            10,
            books.into_iter().map(|book| {
                crate::verse_picker::book_cell(&book, cx)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.navigator_pick_book(book.clone(), cx);
                    }))
                    .into_any_element()
            }),
        )
    }

    fn render_chapter_stage(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(navigator) = &self.navigator else {
            return div().into_any_element();
        };
        let title = match &navigator.picked_book {
            Some(book) => format!("{} · выбор главы", book.long_name),
            None => crate::i18n::tr_static("Выбор главы").to_string(),
        };
        let grid = self.render_number_grid(
            1..=navigator.chapter_count,
            navigator.picked_book.as_ref(),
            cx,
            |this, n, _, cx| {
                this.navigator_pick_chapter(n, cx);
            },
        );
        v_flex()
            .w_full()
            .min_h_0()
            .child(self.render_nav_header(title, true, cx))
            .child(
                div()
                    .id("nav-chapter-grid")
                    .w_full()
                    .min_h_0()
                    .p_3()
                    .child(grid)
                    .overflow_y_scrollbar(),
            )
            .into_any_element()
    }

    fn render_verse_stage(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(navigator) = &self.navigator else {
            return div().into_any_element();
        };
        let title = match (&navigator.picked_book, navigator.picked_chapter) {
            (Some(book), Some(chapter)) => {
                format!("{} {chapter} · выбор стиха", book.long_name)
            }
            _ => crate::i18n::tr_static("Выбор стиха").to_string(),
        };
        let grid = self.render_number_grid(
            1..=navigator.verse_count,
            navigator.picked_book.as_ref(),
            cx,
            |this, n, window, cx| {
                this.navigator_pick_verse(n, window, cx);
            },
        );
        v_flex()
            .w_full()
            .min_h_0()
            .child(self.render_nav_header(title, true, cx))
            .child(
                div()
                    .id("nav-verse-grid")
                    .w_full()
                    .min_h_0()
                    .p_3()
                    .child(grid)
                    .overflow_y_scrollbar(),
            )
            .into_any_element()
    }

    /// Числовая сетка 8 колонок для глав и стихов (как в форке).
    fn render_number_grid(
        &self,
        range: std::ops::RangeInclusive<u16>,
        book: Option<&BookEntry>,
        cx: &mut Context<Self>,
        on_select: impl Fn(&mut Self, u16, &mut Window, &mut Context<Self>) + 'static + Clone,
    ) -> AnyElement {
        let tint =
            crate::verse_picker::book_tint(BookNumber(book.map_or(0, |book| book.book_number)), cx);
        crate::verse_picker::grid(
            8,
            range.map(|n| {
                let on_select = on_select.clone();
                crate::verse_picker::grid_cell(n as usize, n.to_string(), tint, px(44.))
                    .on_click(
                        cx.listener(move |this, _, window, cx| on_select(this, n, window, cx)),
                    )
                    .into_any_element()
            }),
        )
    }
}

/// Первые `limit` символов текста (по границе char) с многоточием —
/// краткая выдержка статьи для hover-карточки.
fn excerpt(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    let cut = trimmed
        .char_indices()
        .nth(limit)
        .map(|(ix, _)| ix)
        .unwrap_or(trimmed.len());
    format!("{}…", trimmed[..cut].trim_end())
}

impl EventEmitter<PanelEvent> for ReaderPanel {}

impl Focusable for ReaderPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for ReaderPanel {
    fn panel_name(&self) -> &'static str {
        Self::NAME
    }

    /// Док сообщает, что вкладка стала отображаемой (или перестала).
    /// Активная вкладка — владелица «текущего места» приложения: её стек
    /// истории обслуживают кнопки ←/→, её место видят бегунок в заголовке
    /// и контекстные панели, на неё роутятся ссылки (§5.4, §5.5, §6.6).
    fn set_active(&mut self, active: bool, _window: &mut Window, cx: &mut Context<Self>) {
        if self.active == active {
            return;
        }
        self.active = active;
        self.word_preview = None;
        if active {
            navigation::history_set_active(cx, self.tab_id);
            let weak = cx.entity().downgrade();
            navigation::notify_reader_activated(cx, weak);
            // Публикуем своё место: бегунок и панели переключаются на
            // свежеоткрытую вкладку, даже если она ещё не скроллилась.
            self.publish_location(cx);
        }
    }

    /// Вкладка закрыта навсегда: её стек истории умирает вместе с ней.
    fn on_removed(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        navigation::history_drop_tab(cx, self.tab_id);
    }

    /// Персистентное состояние вкладки (§7.2): место (верхний видимый
    /// стих) + её стек истории. Восстановление — ReaderPanel::new_restored
    /// через PanelRegistry, обычным путём открытия.
    fn dump(&self, cx: &App) -> PanelState {
        let payload = ReaderTabPayload {
            v: 1,
            module: self
                .module
                .as_ref()
                .map(|module| module.0.clone())
                .unwrap_or_default(),
            book: self.book.map(|book| book.0).unwrap_or(0),
            chapter: self.position.0,
            verse: self.position.1,
            history: navigation::history_dump_stack(cx, self.tab_id),
        };
        let mut state = PanelState::new(Self::NAME);
        state.info = PanelInfo::panel(serde_json::to_value(payload).unwrap_or_default());
        state
    }
}

impl Panel for ReaderPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        if self.book_title.is_empty() {
            crate::i18n::tr_static("Чтение").to_string()
        } else if self.book_short.is_empty() {
            self.book_title.clone()
        } else {
            // Короткое имя книги — компактные вкладки (этап 20).
            self.book_short.clone()
        }
    }
}

impl Render for ReaderPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body: AnyElement = if let Some(error) = &self.error {
            div()
                .size_full()
                .p_4()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(error.clone())
                .into_any_element()
        } else if self.rows.is_empty() {
            div()
                .size_full()
                .p_4()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(crate::i18n::tr_static(
                    "Нажмите на заголовок выше, чтобы выбрать книгу.",
                ))
                .into_any_element()
        } else {
            let rows = self.rows.clone();
            let module = self.module.clone();
            let module_is_hebrew = module
                .as_ref()
                .and_then(|module| app_services(cx).registry.module(module))
                .is_some_and(|meta| module_language_is_hebrew(meta.language.as_deref()));
            let book = self.book;
            let book_title = self.book_title.clone();
            let book_short = self.book_short.clone();
            let reader = (!self.word_panel_visible).then(|| cx.weak_entity());
            let tab_id = self.tab_id;
            let annotations = self.annotations.clone();
            let chain_marks = self.chain_marks.clone();
            let tag_marks = self.tag_marks.clone();
            let (reading_font, reading_size, reading_line_spacing, reading_column_width) = {
                let settings = app_services(cx).settings.read();
                let interface_scale = if settings.interface_scale.is_finite() {
                    settings.interface_scale.clamp(0.8, 1.6)
                } else {
                    1.0
                };
                let reading_line_spacing = if settings.reading_line_spacing.is_finite() {
                    settings.reading_line_spacing.clamp(0.6, 1.8)
                } else {
                    1.0
                };
                let reading_column_width = if settings.reading_column_width.is_finite() {
                    settings.reading_column_width.clamp(0.0, 1600.0)
                } else {
                    0.0
                };
                (
                    settings.reading_font_family.clone(),
                    settings.reading_font_size * interface_scale,
                    reading_line_spacing,
                    reading_column_width,
                )
            };
            let verses = list(self.list_state.clone(), move |ix, _, cx| {
                Self::render_row(
                    ix,
                    tab_id,
                    &rows[ix],
                    &module,
                    book,
                    &book_title,
                    &book_short,
                    &reading_font,
                    reading_size,
                    reading_line_spacing,
                    reading_column_width,
                    module_is_hebrew,
                    reader.as_ref(),
                    &annotations,
                    &chain_marks,
                    &tag_marks,
                    cx,
                )
            })
            .with_sizing_behavior(ListSizingBehavior::Auto)
            .size_full();
            div()
                .relative()
                .size_full()
                .child(verses)
                .vertical_scrollbar(&self.list_state)
                .into_any_element()
        };

        v_flex()
            .size_full()
            .relative()
            .child(self.render_header(cx))
            .child(
                div()
                    .id("reader-text-area")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(body)
                    .on_click(|_, _, cx| navigation::clear_selected_word(cx)),
            )
            .children(self.word_preview.as_ref().map(|(position, description)| {
                deferred(
                    anchored().position(*position).snap_to_window().child(
                        div()
                            .id("reader-word-preview")
                            .occlude()
                            .bg(cx.theme().popover)
                            .border_1()
                            .border_color(cx.theme().border)
                            .rounded_md()
                            .shadow_md()
                            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                this.word_preview = None;
                                cx.notify();
                            }))
                            .child(Self::render_word_preview(description, cx)),
                    ),
                )
                .with_priority(2)
            }))
            .children(self.navigator.is_some().then(|| self.render_navigator(cx)))
            .children(
                self.tag_picker
                    .is_some()
                    .then(|| self.render_tag_picker(cx)),
            )
            .children(
                self.chain_picker
                    .is_some()
                    .then(|| self.render_chain_picker(cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::module_language_is_hebrew;

    #[test]
    fn hebrew_layout_follows_module_language_metadata() {
        for language in ["iw", "he", "he-IL", "heb", "hbo", "Hebrew"] {
            assert!(module_language_is_hebrew(Some(language)), "{language}");
        }
        for language in ["el", "grc", "ru", "en", "mixed"] {
            assert!(!module_language_is_hebrew(Some(language)), "{language}");
        }
        assert!(!module_language_is_hebrew(None));
    }
}
