//! Шина навигации между панелями (architecture.md §5.4, §6).
//!
//! Панели-источники (Библиотека, позже Поиск и ссылки) публикуют запросы
//! на открытие места; панели-исполнители (Чтение, позже Словарь) на них
//! подписаны. Шина — глобальный Entity, потому что панели восстанавливаются
//! из PanelRegistry без прямых ссылок друг на друга.

use bible_domain::ids::{BookNumber, ModuleId};
use gpui_kit::{App, AppContext as _, Entity, EventEmitter, Global, WeakEntity};

// Горячие клавиши (architecture.md §5.5): привязка в run(),
// обработчики — на корневом div окна (workspace.rs).
// NavigateBack/Forward — история; OpenVerseJump — палитра перехода (§6.5);
// OpenCommandPalette — командная палитра (§6.5); NewReadingTab — новая
// вкладка чтения (§6.3, Ctrl+T).
gpui_kit::actions!(
    bibleown,
    [
        NavigateBack,
        NavigateForward,
        OpenVerseJump,
        OpenCommandPalette,
        NewReadingTab
    ]
);

/// Запрос открыть книгу (целиком) и показать место chapter[:verse].
#[derive(Debug, Clone)]
pub struct OpenBook {
    pub module: ModuleId,
    pub book: BookNumber,
    /// None — начало книги.
    pub chapter: Option<u16>,
    /// None — начало главы.
    pub verse: Option<u16>,
}

/// Запрос открыть справочный модуль (словарь, комментарии, ссылки) —
/// исполняет панель «Словарь».
#[derive(Debug, Clone)]
pub struct OpenModule {
    pub module: ModuleId,
    /// Тема статьи, которую нужно сразу показать (для словарей).
    pub topic: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OpenCommentary {
    pub module: ModuleId,
    pub reference: bible_domain::ids::VerseRef,
}

/// Клик по слову в тексте: карточка собрана в shell, показывает
/// панель «Слово» (architecture.md §6.4).
#[derive(Debug, Clone)]
pub struct WordSelected {
    pub description: shell::WordDescription,
}

pub struct WordCleared;

/// Клик по строке стиха: стих становится «активным» — панель «Контекст»
/// перечитывает переводы/ссылки/комментарии для него (как курсор в форке).
#[derive(Debug, Clone)]
pub struct VerseActivated {
    pub module: ModuleId,
    pub book: BookNumber,
    pub book_title: String,
    /// Короткое имя книги — для бегунка места (этап 20).
    pub book_short: String,
    pub chapter: u16,
    pub verse: u16,
}

impl VerseActivated {
    pub(crate) fn location(&self) -> shell::CurrentLocation {
        shell::CurrentLocation {
            module: self.module.clone(),
            book: self.book,
            book_title: self.book_title.clone(),
            book_short: self.book_short.clone(),
            chapter: self.chapter,
            verse: self.verse,
        }
    }
}

/// Запрос открыть ColorPicker оформления конкретного стиха из контекстного
/// меню чтения. Workspace гарантирует, что правая панель «Verse» видима.
#[derive(Debug, Clone)]
pub struct OpenVerseStylePicker {
    pub verse: VerseActivated,
    pub word: Option<Box<shell::WordDescription>>,
    pub word_scope: bool,
}

/// Видимость панели «Слово» изменилась (стала активной вкладкой,
/// скрылась, закрылась). От неё зависит, показывать ли hover-карточки
/// слов в чтении: hover работает, только когда панель закрыта.
#[derive(Debug, Clone, Copy)]
pub struct WordPanelVisibilityChanged(pub bool);

/// Верхний видимый стих чтения сменился (прокрутка, открытие книги):
/// бегунок места в Title bar обновляет подпись «Бытие 3:16» (§6.6).
/// Отличается от `VerseActivated` (клик по строке) — здесь источник
/// истины один: верхний видимый стих, как подпись в шапке чтения.
#[derive(Debug, Clone)]
pub struct LocationChanged(pub shell::CurrentLocation);

/// Секции панели «Verse» для кнопок Status bar (этап 18). Порядок
/// фиксирован порядком TITLES/contents в context.rs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerseSection {
    Word,
    Translations,
    CrossRefs,
    Commentaries,
    Notes,
}

/// Запрос раскрыть панель «Verse» и открыть в ней секцию (кнопки
/// Status bar — «Перекрёстные», «Комментарий к стиху», этап 18).
#[derive(Debug, Clone, Copy)]
pub struct OpenVerseSection(pub VerseSection);

/// Пользовательские аннотации изменились (выделение/подчёркивание/цвет,
/// комментарий к стиху). Чтение и панель «Контекст» перечитывают данные
/// (architecture.md §5.3: add_annotation/remove_annotation).
#[derive(Debug, Clone, Copy)]
pub struct AnnotationsChanged;

/// Настройки изменились (экран «Настройки», architecture.md §9): флаги
/// текста, перевод по умолчанию, каталог модулей пересканирован или
/// хранилище заметок переинициализировано. Панели перечитывают свои данные:
/// чтение переоткрывает книгу (новые опции разбора), библиотека — список
/// модулей, заметки — переподписываются на новый индекс.
#[derive(Debug, Clone, Copy)]
pub struct SettingsChanged;

pub struct UserDataSynced;

/// Запрос поиска из другой панели (кнопки «Искать слово»/«Искать Strong's»
/// секции «Слово»). WorkspaceView раскрывает панель поиска и публикует
/// `SearchExecute` — разделение нужно, чтобы только что созданная панель
/// гарантированно успела подписаться.
#[derive(Debug, Clone)]
pub struct SearchRequested {
    pub query: String,
}

/// Исполнение поиска: панель «Поиск» подставляет запрос и ищет.
#[derive(Debug, Clone)]
pub struct SearchExecute {
    pub query: String,
}

/// Запрос открыть панель «Цепочки»: из полосы цепочек в «Контексте»
/// (Some(id) — сразу открыть цепочку) или из командной палитры (None —
/// список). WorkspaceView раскрывает панель и повторяет событие, если
/// панель создана только что (как OpenModule для «Словаря», §6.8).
#[derive(Debug, Clone)]
pub struct OpenChain {
    pub chain_id: Option<String>,
}

/// Цепочки изменились (создание/переименование/удаление, правка узлов).
/// Панель «Цепочки» перечитывает список/открытую цепочку, «Контекст» —
/// полосу попаданий, чтение — маркеры стихов.
#[derive(Debug, Clone, Copy)]
pub struct ChainsChanged;

/// Теги изменились (создание/переименование/цвет/удаление, постановка
/// или снятие тега со стиха). Чтение перечитывает маркеры, список тегов
/// в менюшке — строки (этап 21, polish-plan.md).
#[derive(Debug, Clone, Copy)]
pub struct TagsChanged;

/// Запрос открыть пикер тегов (этап 21). Откликается активная вкладка
/// чтения. `reference = None` — стих у верхней кромки её списка
/// (кнопка Status bar); Some — конкретный стих (контекстное меню).
#[derive(Debug, Clone, Copy)]
pub struct OpenTagPicker {
    pub reference: Option<bible_domain::ids::VerseRef>,
}

/// Запрос открыть пикер «добавить стих в цепочку» (этап 23). Откликается
/// активная вкладка чтения. `reference = None` — стих у верхней кромки
/// (кнопка Status bar); Some — конкретный стих (контекстное меню).
#[derive(Debug, Clone, Copy)]
pub struct OpenChainPicker {
    pub reference: Option<bible_domain::ids::VerseRef>,
}

/// Запрос открыть заметку отдельной вкладкой в центре (этап 15.2):
/// клик по заметке в менюшке, ссылка `bibleown://note/…` в статье.
/// relpath — путь внутри хранилища заметок (notes_dir).
#[derive(Debug, Clone)]
pub struct OpenNote {
    pub relpath: String,
}

/// Раздел левой менюшки (этап 15). Зеркалит Section в sidebar.rs;
/// живёт здесь, чтобы событие шины не тянуло UI-тип панели.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarSectionKind {
    Dictionaries,
    Notes,
    Tags,
}

/// Запрос раскрыть левую менюшку на разделе (кнопка «Словарь» в Status
/// bar — этап 15.2). `module = Some` — сразу drill-down в этот модуль
/// (клик по словарю в «Библиотеке»-подобных местах без темы).
#[derive(Debug, Clone)]
pub struct OpenSidebarModule {
    pub section: SidebarSectionKind,
    pub module: Option<ModuleId>,
}

// ---------------------------------------------------------------------------
// История навигации ←/→ (architecture.md §5.5, этап 9).
// Стек чистый — живёт в shell::navigation::NavHistory; здесь — глобальное
// хранилище-Entity и события. Кнопки и горячие клавиши (title bar,
// ctrl-alt-left/right) зовут history_go_back/forward; чтение подписано на
// HistoryNavigate и открывает место БЕЗ записи в историю (from_history —
// курсор не гоняется за самим собой, как в форке).
// ---------------------------------------------------------------------------

/// Курсор истории сдвинулся или появилась запись — кнопки ←/→ перечитывают
/// can_back/can_forward.
#[derive(Debug, Clone, Copy)]
pub struct HistoryChanged;

/// Из истории выбрано место — вкладка чтения открывает его БЕЗ записи в
/// стек (курсор не гоняется за самим собой). Адресно: откликается только
/// вкладка с тем же tab_id, остальные игнорируют.
#[derive(Debug, Clone)]
pub struct HistoryNavigate {
    pub tab: u64,
    pub entry: shell::navigation::NavEntry,
}

/// Активная вкладка чтения сменилась (её set_active(true)): WorkspaceView
/// запоминает её как цель роутинга ссылок (reuse_current) и истории.
#[derive(Debug, Clone)]
pub struct ReaderActivated(pub WeakEntity<crate::reader::ReaderPanel>);

/// Per-вкладка история чтения (architecture.md §5.5): у каждой вкладки
/// свой стек ←/→, как в браузере. Стеки переживают перезапуск в payload
/// вкладки (dump/restore через history_dump_stack/history_register_stack).
/// `active` — вкладка, которой принадлежат кнопки ←/→ в заголовке и
/// горячие клавиши; обновляется из set_active панели чтения.
pub struct HistoryStore {
    stacks: std::collections::HashMap<u64, shell::navigation::NavHistory>,
    active: Option<u64>,
}

impl EventEmitter<HistoryChanged> for HistoryStore {}
impl EventEmitter<HistoryNavigate> for HistoryStore {}

pub struct HistoryBus(pub Entity<HistoryStore>);

impl Global for HistoryBus {}

/// Пустой эмиттер: состояние шине не нужно, только события.
pub struct Navigation;

impl EventEmitter<OpenBook> for Navigation {}
impl EventEmitter<OpenModule> for Navigation {}
impl EventEmitter<OpenCommentary> for Navigation {}
impl EventEmitter<WordCleared> for Navigation {}
impl EventEmitter<WordSelected> for Navigation {}
impl EventEmitter<VerseActivated> for Navigation {}
impl EventEmitter<OpenVerseStylePicker> for Navigation {}
impl EventEmitter<WordPanelVisibilityChanged> for Navigation {}
impl EventEmitter<LocationChanged> for Navigation {}
impl EventEmitter<ReaderActivated> for Navigation {}
impl EventEmitter<AnnotationsChanged> for Navigation {}
impl EventEmitter<SettingsChanged> for Navigation {}
impl EventEmitter<UserDataSynced> for Navigation {}
impl EventEmitter<SearchRequested> for Navigation {}
impl EventEmitter<SearchExecute> for Navigation {}
impl EventEmitter<OpenChain> for Navigation {}
impl EventEmitter<ChainsChanged> for Navigation {}
impl EventEmitter<OpenVerseSection> for Navigation {}
impl EventEmitter<TagsChanged> for Navigation {}
impl EventEmitter<OpenTagPicker> for Navigation {}
impl EventEmitter<OpenChainPicker> for Navigation {}
impl EventEmitter<OpenNote> for Navigation {}
impl EventEmitter<OpenSidebarModule> for Navigation {}

pub struct NavBus(pub Entity<Navigation>);

impl Global for NavBus {}

/// Создаёт шину. Вызывается один раз при старте, до открытия окон.
pub fn init(cx: &mut App) {
    let navigation = cx.new(|_| Navigation);
    cx.set_global(NavBus(navigation));
    let history = cx.new(|_| HistoryStore {
        stacks: std::collections::HashMap::new(),
        active: None,
    });
    cx.set_global(HistoryBus(history));
}

pub fn bus(cx: &App) -> Entity<Navigation> {
    cx.global::<NavBus>().0.clone()
}

pub fn history(cx: &App) -> Entity<HistoryStore> {
    cx.global::<HistoryBus>().0.clone()
}

/// Записать переход в стек СВОЕЙ вкладки (зовёт чтение после успешного
/// открытия книги; переходы из истории сюда не попадают).
pub fn history_push(cx: &mut App, tab: u64, entry: shell::navigation::NavEntry) {
    history(cx).update(cx, |store, cx| {
        store.stacks.entry(tab).or_default().push(entry);
        cx.emit(HistoryChanged);
    });
}

/// Вкладка стала активной (set_active(true) панели чтения): кнопки ←/→
/// и горячие клавиши теперь работают с её стеком.
pub fn history_set_active(cx: &mut App, tab: u64) {
    history(cx).update(cx, |store, cx| {
        if store.active != Some(tab) {
            store.active = Some(tab);
            cx.emit(HistoryChanged);
        }
    });
}

/// Вкладка закрыта: её стек умирает вместе с ней (§5.5).
pub fn history_drop_tab(cx: &mut App, tab: u64) {
    history(cx).update(cx, |store, cx| {
        store.stacks.remove(&tab);
        if store.active == Some(tab) {
            store.active = None;
        }
        cx.emit(HistoryChanged);
    });
}

/// Снимок стека вкладки для её payload (§7.2: история переживает
/// перезапуск вместе с вкладкой).
pub fn history_dump_stack(cx: &App, tab: u64) -> Option<shell::navigation::HistoryDump> {
    history(cx)
        .read(cx)
        .stacks
        .get(&tab)
        .map(|stack| stack.dump())
}

/// Восстановление стека из payload вкладки при загрузке layout.json.
pub fn history_register_stack(cx: &mut App, tab: u64, dump: shell::navigation::HistoryDump) {
    history(cx).update(cx, |store, _| {
        let stack = shell::navigation::NavHistory::restore(dump);
        if stack.current().is_some() {
            store.stacks.insert(tab, stack);
        }
    });
}

/// can_back активной вкладки (кнопка ← в заголовке).
pub fn history_can_back(cx: &App) -> bool {
    let store = history(cx);
    let store = store.read(cx);
    store
        .active
        .and_then(|tab| store.stacks.get(&tab))
        .is_some_and(|stack| stack.can_back())
}

/// can_forward активной вкладки (кнопка → в заголовке).
pub fn history_can_forward(cx: &App) -> bool {
    let store = history(cx);
    let store = store.read(cx);
    store
        .active
        .and_then(|tab| store.stacks.get(&tab))
        .is_some_and(|stack| stack.can_forward())
}

/// Кнопка ← или ctrl-alt-left: шаг назад в стеке АКТИВНОЙ вкладки,
/// навигация без записи адресно ей.
pub fn history_go_back(cx: &mut App) {
    history(cx).update(cx, |store, cx| {
        let Some(tab) = store.active else { return };
        let Some(stack) = store.stacks.get_mut(&tab) else {
            return;
        };
        if let Some(entry) = stack.back().cloned() {
            cx.emit(HistoryNavigate { tab, entry });
            cx.emit(HistoryChanged);
        }
    });
}

/// Кнопка → или ctrl-alt-right: шаг вперёд в стеке АКТИВНОЙ вкладки.
pub fn history_go_forward(cx: &mut App) {
    history(cx).update(cx, |store, cx| {
        let Some(tab) = store.active else { return };
        let Some(stack) = store.stacks.get_mut(&tab) else {
            return;
        };
        if let Some(entry) = stack.forward().cloned() {
            cx.emit(HistoryNavigate { tab, entry });
            cx.emit(HistoryChanged);
        }
    });
}

/// Вкладка чтения стала активной — WorkspaceView запоминает её как цель
/// роутинга ссылок (режим reuse_current).
pub fn notify_reader_activated(cx: &mut App, reader: WeakEntity<crate::reader::ReaderPanel>) {
    bus(cx).update(cx, |_, cx| cx.emit(ReaderActivated(reader)));
}

/// Опубликовать запрос на открытие книги.
pub fn request_open_book(cx: &mut App, request: OpenBook) {
    bus(cx).update(cx, |_, cx| cx.emit(request));
}

/// Опубликовать запрос на открытие справочного модуля.
pub fn request_open_module(cx: &mut App, module: ModuleId) {
    bus(cx).update(cx, |_, cx| {
        cx.emit(OpenModule {
            module,
            topic: None,
        })
    });
}

pub fn request_open_commentary(
    cx: &mut App,
    module: ModuleId,
    reference: bible_domain::ids::VerseRef,
) {
    bus(cx).update(cx, |_, cx| cx.emit(OpenCommentary { module, reference }));
}

/// Опубликовать запрос на открытие конкретной статьи словаря.
pub fn request_open_topic(cx: &mut App, module: ModuleId, topic: String) {
    bus(cx).update(cx, |_, cx| {
        cx.emit(OpenModule {
            module,
            topic: Some(topic),
        })
    });
}

/// Опубликовать выбор слова (клик в панели чтения).
pub fn request_select_word(cx: &mut App, description: shell::WordDescription) {
    bus(cx).update(cx, |_, cx| cx.emit(WordSelected { description }));
}

pub fn clear_selected_word(cx: &mut App) {
    bus(cx).update(cx, |_, cx| cx.emit(WordCleared));
}

/// Опубликовать активацию стиха (клик по строке стиха в чтении).
pub fn request_activate_verse(cx: &mut App, event: VerseActivated) {
    *crate::services::app_services(cx).current_location.lock() = Some(event.location());
    bus(cx).update(cx, |_, cx| cx.emit(event));
}

/// Открыть оформление конкретного стиха из контекстного меню чтения.
pub fn request_open_verse_style_picker(cx: &mut App, event: OpenVerseStylePicker) {
    *crate::services::app_services(cx).current_location.lock() = Some(event.verse.location());
    bus(cx).update(cx, |_, cx| cx.emit(event));
}

/// Сообщить о видимости панели «Слово» (вызывает сама панель из
/// `set_active`/`on_removed`).
pub fn set_word_panel_visible(cx: &mut App, visible: bool) {
    bus(cx).update(cx, |_, cx| cx.emit(WordPanelVisibilityChanged(visible)));
}

/// Опубликовать текущее место чтения (вызывает чтение из
/// `publish_location` — при прокрутке и открытии книги).
pub fn notify_location_changed(cx: &mut App, location: shell::CurrentLocation) {
    bus(cx).update(cx, |_, cx| cx.emit(LocationChanged(location)));
}

/// Сообщить, что аннотации изменились (после set/clear/save_comment).
pub fn notify_annotations_changed(cx: &mut App) {
    bus(cx).update(cx, |_, cx| cx.emit(AnnotationsChanged));
}

/// Сообщить, что настройки изменились (экран «Настройки»).
pub fn notify_settings_changed(cx: &mut App) {
    bus(cx).update(cx, |_, cx| cx.emit(SettingsChanged));
}

/// Запросить поиск из другой панели (секция «Слово»): WorkspaceView
/// раскроет панель поиска и передаст запрос дальше (`SearchExecute`).
pub fn request_search(cx: &mut App, query: String) {
    bus(cx).update(cx, |_, cx| cx.emit(SearchRequested { query }));
}

/// Передать запрос панели поиска (зовёт WorkspaceView, когда панель
/// гарантированно существует и видна).
pub fn emit_search_execute(cx: &mut App, query: String) {
    bus(cx).update(cx, |_, cx| cx.emit(SearchExecute { query }));
}

/// Запросить открытие панели «Цепочки» (полоса в «Контексте», палитра).
pub fn request_open_chain(cx: &mut App, chain_id: Option<String>) {
    bus(cx).update(cx, |_, cx| cx.emit(OpenChain { chain_id }));
}

/// Запросить открытие секции панели «Verse» (кнопки Status bar, этап 18).
pub fn request_open_verse_section(cx: &mut App, section: VerseSection) {
    bus(cx).update(cx, |_, cx| cx.emit(OpenVerseSection(section)));
}

/// Сообщить, что цепочки изменились (после любой мутации в панели).
pub fn notify_chains_changed(cx: &mut App) {
    bus(cx).update(cx, |_, cx| cx.emit(ChainsChanged));
}

/// Сообщить, что теги изменились (пикер, список в менюшке).
pub fn notify_tags_changed(cx: &mut App) {
    bus(cx).update(cx, |_, cx| cx.emit(TagsChanged));
}

/// Запросить пикер тегов: Some — для конкретного стиха (контекстное
/// меню), None — для активного стиха чтения (кнопка Status bar).
pub fn request_open_tag_picker(cx: &mut App, reference: Option<bible_domain::ids::VerseRef>) {
    bus(cx).update(cx, |_, cx| cx.emit(OpenTagPicker { reference }));
}

/// Запросить пикер «стих → цепочка» (этап 23): Some — конкретный стих
/// (контекстное меню), None — активный стих чтения (кнопка Status bar).
pub fn request_open_chain_picker(cx: &mut App, reference: Option<bible_domain::ids::VerseRef>) {
    bus(cx).update(cx, |_, cx| cx.emit(OpenChainPicker { reference }));
}

/// Запросить открытие заметки вкладкой в центре (этап 15.2).
pub fn request_open_note(cx: &mut App, relpath: String) {
    bus(cx).update(cx, |_, cx| cx.emit(OpenNote { relpath }));
}

/// Запросить раскрытие левой менюшки на разделе (этап 15.2). WorkspaceView
/// делает менюшку видимой, SidebarView переключает раздел/модуль.
pub fn request_open_sidebar(cx: &mut App, section: SidebarSectionKind, module: Option<ModuleId>) {
    bus(cx).update(cx, |_, cx| cx.emit(OpenSidebarModule { section, module }));
}
