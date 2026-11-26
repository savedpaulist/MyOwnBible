//! Левая панель с раскрываемой полосой разделов.

use gpui_kit::base::{HandleEdge, h_flex, resize_handle, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tree::{Tree, TreeEvent, TreeItem, TreeState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bible_domain::ids::{BookNumber, ModuleId, VerseRef};
use bible_domain::ports::{ModuleCatalog as _, ModuleKind};
use bible_domain::tags::Tag;
use gpui_kit::component::color_picker::{ColorPickerEvent, ColorPickerState, ColorSelect};
use shell::notes::{IndexEvent, NotesHandle, NotesSubscription};

use crate::annotations as palette;
use crate::dictionary::DictionaryBrowser;
use crate::navigation;
use crate::panels::SearchPanel;
use crate::services::app_services;

/// Пункт менюшки.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Section {
    Search,
    Dictionaries,
    Notes,
    Comments,
    Tags,
    Chains,
    Questions,
}

#[derive(Clone)]
struct ResizeSidebar {
    previous_x: Rc<Cell<Option<Pixels>>>,
}

impl Render for ResizeSidebar {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

#[derive(Clone)]
struct SidebarCommentaryRow {
    book: BookNumber,
    book_label: String,
    reference: VerseRef,
    label: String,
}

fn commentary_tree_items(
    rows: &[SidebarCommentaryRow],
    expanded_books: &HashSet<String>,
) -> Vec<TreeItem> {
    let mut books = BTreeMap::<BookNumber, (String, Vec<TreeItem>)>::new();
    for (index, row) in rows.iter().enumerate() {
        let (_, entries) = books
            .entry(row.book)
            .or_insert_with(|| (row.book_label.clone(), Vec::new()));
        entries.push(TreeItem::new(
            format!("commentary-entry:{index}"),
            row.label.clone(),
        ));
    }

    books
        .into_iter()
        .map(|(book, (label, entries))| {
            let id = format!("commentary-book:{}", book.0);
            TreeItem::new(id.clone(), label)
                .children(entries)
                .expanded(expanded_books.contains(&id))
        })
        .collect()
}

#[derive(Clone)]
struct NoteDrag {
    relpath: String,
    title: String,
}

struct NoteDragPreview(NoteDrag);

impl Render for NoteDragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .px_3()
            .py_2()
            .rounded_md()
            .shadow_lg()
            .bg(cx.theme().background)
            .border_1()
            .border_color(cx.theme().border)
            .child(self.0.title.clone())
    }
}

#[derive(Default)]
struct NoteTreeDirectory {
    directories: BTreeMap<String, NoteTreeDirectory>,
    notes: Vec<shell::notes::NoteSummary>,
}

fn note_tree_items(
    folders: &[shell::notes::NoteFolder],
    notes: &[shell::notes::NoteSummary],
    expanded: &HashSet<String>,
) -> Vec<TreeItem> {
    fn directory_mut<'a>(
        mut directory: &'a mut NoteTreeDirectory,
        components: impl Iterator<Item = &'a str>,
    ) -> &'a mut NoteTreeDirectory {
        for component in components {
            directory = directory
                .directories
                .entry(component.to_owned())
                .or_default();
        }
        directory
    }

    fn build(
        directory: NoteTreeDirectory,
        parent: &str,
        expanded: &HashSet<String>,
    ) -> Vec<TreeItem> {
        let mut items = Vec::new();
        for (name, child) in directory.directories {
            let relpath = if parent.is_empty() {
                name.clone()
            } else {
                format!("{parent}/{name}")
            };
            let id = format!("folder:{relpath}");
            let mut children = build(child, &relpath, expanded);
            if children.is_empty() {
                children
                    .push(TreeItem::new(format!("empty:{relpath}"), "Пустая папка").disabled(true));
            }
            items.push(
                TreeItem::new(id.clone(), name)
                    .children(children)
                    .expanded(expanded.contains(&id)),
            );
        }
        let mut notes = directory.notes;
        notes.sort_by(|a, b| {
            a.title
                .to_lowercase()
                .cmp(&b.title.to_lowercase())
                .then_with(|| a.relpath.cmp(&b.relpath))
        });
        items.extend(
            notes
                .into_iter()
                .map(|note| TreeItem::new(format!("note:{}", note.relpath), note.title)),
        );
        items
    }

    let mut root = NoteTreeDirectory::default();
    for folder in folders {
        directory_mut(&mut root, folder.relpath.split('/'));
    }
    for note in notes {
        let mut components = note.relpath.split('/').collect::<Vec<_>>();
        components.pop();
        directory_mut(&mut root, components.into_iter())
            .notes
            .push(note.clone());
    }
    build(root, "", expanded)
}

impl Section {
    fn from_settings(value: &str) -> Self {
        match value {
            "search" => Self::Search,
            "notes" => Self::Notes,
            "comments" => Self::Comments,
            "tags" => Self::Tags,
            "chains" => Self::Chains,
            "questions" => Self::Questions,
            _ => Self::Dictionaries,
        }
    }

    fn as_settings(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Dictionaries => "dictionaries",
            Self::Notes => "notes",
            Self::Comments => "comments",
            Self::Tags => "tags",
            Self::Chains => "chains",
            Self::Questions => "questions",
        }
    }

    fn label(self, cx: &App) -> SharedString {
        crate::i18n::tr(
            cx,
            match self {
                Self::Search => "Поиск",
                Self::Dictionaries => "Словари",
                Self::Notes => "Заметки",
                Self::Comments => "Комментарии",
                Self::Tags => "Теги",
                Self::Chains => "Цепочки",
                Self::Questions => "Вопросы",
            },
        )
    }

    fn icon(self) -> gpui_kit::assets::IconName {
        match self {
            Self::Search => gpui_kit::assets::IconName::Search,
            Self::Dictionaries => gpui_kit::assets::IconName::BookA,
            Self::Notes => gpui_kit::assets::IconName::NotebookPen,
            Self::Comments => gpui_kit::assets::IconName::MessageSquare,
            Self::Tags => gpui_kit::assets::IconName::Tags,
            Self::Chains => gpui_kit::assets::IconName::Link,
            Self::Questions => gpui_kit::assets::IconName::CircleQuestionMark,
        }
    }
}

pub struct SidebarView {
    section: Section,
    collapsed: bool,
    /// Контент выбранного раздела; при скрытии остаётся только полоса иконок.
    content_visible: bool,
    content_width: Pixels,
    search: Entity<SearchPanel>,
    dictionaries: Entity<DictionaryBrowser>,
    chains: Entity<crate::chains::ChainsPanel>,
    questions: Entity<crate::questions::QuestionsPanel>,
    /// Заметки (этап 15.2): список хранилища; клик открывает заметку
    /// вкладкой в центре (note_tab.rs). Создание — действиями над списком.
    notes_list: Vec<shell::notes::NoteSummary>,
    note_folders: Vec<shell::notes::NoteFolder>,
    notes_tree: Entity<TreeState>,
    expanded_note_folders: HashSet<String>,
    new_note_input: Entity<InputState>,
    new_folder_input: Entity<InputState>,
    new_tag_input: Entity<InputState>,
    commentary_module: Option<ModuleId>,
    commentary_rows: Arc<Vec<SidebarCommentaryRow>>,
    commentary_tree: Entity<TreeState>,
    expanded_commentary_books: HashSet<String>,
    /// Очередь событий индекса заметок с потока watcher'а; поллинг 300 мс.
    pending_events: Arc<Mutex<Vec<IndexEvent>>>,
    subscribed_index: Option<NotesHandle>,
    _index_token: Option<NotesSubscription>,
    /// Теги (этап 21): список со счётчиками, drill-down в стихи тега.
    tags: Vec<Tag>,
    tag_counts: HashMap<i64, i64>,
    open_tag: Option<Tag>,
    tag_verses: Vec<VerseRef>,
    tag_rename_input: Entity<InputState>,
    tag_color: Entity<ColorPickerState>,
    /// Список тегов уже загружался (ленивая первая загрузка при
    /// выборе раздела; дальше — по TagsChanged).
    tags_loaded: bool,
    _subscriptions: Vec<Subscription>,
}

impl SidebarView {
    pub fn new(search: Entity<SearchPanel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dictionaries = cx.new(|cx| DictionaryBrowser::new(window, cx));
        let chains = cx.new(|cx| crate::chains::ChainsPanel::new(window, cx));
        let questions = cx.new(|cx| crate::questions::QuestionsPanel::new(window, cx));
        let notes_tree = cx.new(|cx| TreeState::new(cx));
        let commentary_tree = cx.new(|cx| TreeState::new(cx));
        let tag_color = cx.new(|cx| ColorPickerState::new(window, cx));
        let color_subscription =
            cx.subscribe(&tag_color, |this, _, event: &ColorPickerEvent, cx| {
                if let ColorPickerEvent::Change(Some(color)) = event {
                    this.set_open_tag_color(*color, cx);
                }
            });
        let tag_rename_input = cx
            .new(|cx| InputState::new(window, cx).placeholder(crate::i18n::tr_static("Имя тега…")));
        let new_note_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(crate::i18n::tr_static("Папка/Название новой заметки"))
        });
        let new_folder_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(crate::i18n::tr_static("Название новой папки"))
        });
        let new_tag_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(crate::i18n::tr_static("Имя нового тега…"))
        });
        let subscriptions = vec![
            cx.subscribe(&notes_tree, |this, _, event: &TreeEvent, cx| {
                let (id, expanded) = match event {
                    TreeEvent::Expanded(id) => (id.to_string(), true),
                    TreeEvent::Collapsed(id) => (id.to_string(), false),
                };
                if id.starts_with("folder:") {
                    if expanded {
                        this.expanded_note_folders.insert(id);
                    } else {
                        this.expanded_note_folders.remove(&id);
                    }
                }
                cx.notify();
            }),
            cx.subscribe(&commentary_tree, |this, _, event: &TreeEvent, cx| {
                let (id, expanded) = match event {
                    TreeEvent::Expanded(id) => (id.to_string(), true),
                    TreeEvent::Collapsed(id) => (id.to_string(), false),
                };
                if id.starts_with("commentary-book:") {
                    if expanded {
                        this.expanded_commentary_books.insert(id);
                    } else {
                        this.expanded_commentary_books.remove(&id);
                    }
                }
                cx.notify();
            }),
            // Enter в поле имени тега — переименование открытого тега.
            cx.subscribe_in(
                &tag_rename_input,
                window,
                |this, tag_rename_input, event: &InputEvent, _, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        let name = tag_rename_input.read(cx).value().to_string();
                        this.rename_open_tag(&name, cx);
                    }
                },
            ),
            // Enter в поле новой заметки — создать и открыть вкладкой.
            cx.subscribe_in(
                &new_note_input,
                window,
                |this, input, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        let name = input.read(cx).value().to_string();
                        this.create_note(name, window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &new_folder_input,
                window,
                |this, input, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.create_note_folder(input.read(cx).value().to_string(), window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &new_tag_input,
                window,
                |this, input, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.create_tag(input.read(cx).value().to_string(), window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &navigation::bus(cx),
                window,
                |this, _, _: &navigation::OpenChain, _, cx| this.show_section(Section::Chains, cx),
            ),
            // Теги изменились (пикер в чтении, действия здесь) —
            // перечитываем список и открытый тег.
            cx.subscribe_in(
                &navigation::bus(cx),
                window,
                |this, _, _: &navigation::TagsChanged, _, cx| {
                    this.reload_tags(cx);
                },
            ),
            // Запрос раскрыть менюшку на разделе (кнопка «Словари» в
            // Status bar, словарь без темы из других панелей — этап 15.2).
            cx.subscribe_in(
                &navigation::bus(cx),
                window,
                |this, _, event: &navigation::OpenSidebarModule, window, cx| {
                    let section = match event.section {
                        navigation::SidebarSectionKind::Dictionaries => Section::Dictionaries,
                        navigation::SidebarSectionKind::Notes => Section::Notes,
                        navigation::SidebarSectionKind::Tags => Section::Tags,
                    };
                    this.show_section(section, cx);
                    // Словарь указан явно — сразу drill-down в его темы.
                    if let (Section::Dictionaries, Some(module)) = (section, &event.module) {
                        this.dictionaries.update(cx, |dictionary, cx| {
                            dictionary.select_module(Some(module.clone()), window, cx);
                        });
                    }
                },
            ),
            // Хранилище заметок переинициализировано из настроек —
            // переподписаться на новый индекс и перечитать список.
            cx.subscribe_in(
                &navigation::bus(cx),
                window,
                |this, _, _: &navigation::SettingsChanged, _, cx| {
                    this.on_settings_changed(cx);
                },
            ),
        ];

        // События индекса заметок приходят с потока watcher'а — дескрипторы
        // GPUI там трогать нельзя (!Send), поэтому callback складывает их в
        // очередь, а менюшка разбирает её поллингом раз в 300 мс
        // (та же схема, что у панели/вкладки заметок).
        let pending_events: Arc<Mutex<Vec<IndexEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let pending_for_loop = Arc::clone(&pending_events);
        cx.spawn_in(window, async move |this, window| {
            loop {
                window
                    .background_executor()
                    .timer(Duration::from_millis(300))
                    .await;
                let events: Vec<IndexEvent> = pending_for_loop.lock().unwrap().drain(..).collect();
                if events.is_empty() {
                    continue;
                }
                let alive = this
                    .update_in(window, |this, _, cx| {
                        if !events.is_empty() {
                            this.reload_notes(cx);
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();

        let settings = shell::settings::current(app_services(cx));
        let mut this = Self {
            section: Section::from_settings(&settings.sidebar_section),
            collapsed: settings.sidebar_collapsed,
            content_visible: settings.sidebar_visible,
            content_width: px(settings.sidebar_content_width.max(120.)),
            search,
            dictionaries,
            chains,
            questions,
            notes_list: Vec::new(),
            note_folders: Vec::new(),
            notes_tree,
            expanded_note_folders: HashSet::new(),
            new_note_input,
            new_folder_input,
            new_tag_input,
            commentary_module: None,
            commentary_rows: Arc::new(Vec::new()),
            commentary_tree,
            expanded_commentary_books: HashSet::new(),
            pending_events,
            subscribed_index: None,
            _index_token: None,
            tags: Vec::new(),
            tag_counts: HashMap::new(),
            open_tag: None,
            tag_verses: Vec::new(),
            tag_rename_input,
            tag_color,
            tags_loaded: false,
            _subscriptions: subscriptions
                .into_iter()
                .chain([color_subscription])
                .collect(),
        };
        this.subscribe_notes_index(cx);
        this.reload_notes(cx);
        // Восстановленный раздел «Теги» грузим сразу (ленивая загрузка
        // срабатывает только при смене раздела).
        if this.section == Section::Tags {
            this.reload_tags(cx);
        }
        this
    }

    pub fn show_search(&mut self, cx: &mut Context<Self>) {
        self.show_section(Section::Search, cx);
    }

    fn select_section(&mut self, section: Section, cx: &mut Context<Self>) {
        if self.section == section {
            // Повторный клик по той же иконке прячет только контент.
            self.content_visible = !self.content_visible;
        } else {
            self.section = section;
            self.content_visible = true;
        }
        if self.content_visible && section == Section::Tags && !self.tags_loaded {
            self.reload_tags(cx);
        }
        let selected = self.section.as_settings().to_string();
        let visible = self.content_visible;
        if let Err(error) = shell::settings::update(app_services(cx), |settings| {
            settings.sidebar_section = selected;
            settings.sidebar_visible = visible;
        }) {
            log::error!("save sidebar section visibility: {error:#}");
        }
        cx.notify();
    }

    /// Показать выбранный раздел независимо от того, какая иконка нажата.
    /// Используется программной навигацией из других панелей.
    fn show_section(&mut self, section: Section, cx: &mut Context<Self>) {
        self.section = section;
        self.content_visible = true;
        if section == Section::Tags && !self.tags_loaded {
            self.reload_tags(cx);
        }
        let selected = section.as_settings().to_string();
        if let Err(error) = shell::settings::update(app_services(cx), |settings| {
            settings.sidebar_section = selected;
            settings.sidebar_visible = true;
        }) {
            log::error!("save sidebar section: {error:#}");
        }
        cx.notify();
    }

    pub fn show_content(&mut self, cx: &mut Context<Self>) {
        if self.content_visible {
            return;
        }
        self.content_visible = true;
        if let Err(error) =
            shell::settings::update(app_services(cx), |settings| settings.sidebar_visible = true)
        {
            log::error!("save sidebar content visibility: {error:#}");
        }
        cx.notify();
    }

    pub fn toggle_content(&mut self, cx: &mut Context<Self>) {
        self.content_visible = !self.content_visible;
        let visible = self.content_visible;
        if let Err(error) = shell::settings::update(app_services(cx), |settings| {
            settings.sidebar_visible = visible
        }) {
            log::error!("save sidebar content visibility: {error:#}");
        }
        cx.notify();
    }

    fn toggle_collapsed(&mut self, cx: &mut Context<Self>) {
        self.collapsed = !self.collapsed;
        let collapsed = self.collapsed;
        if let Err(error) = shell::settings::update(app_services(cx), |settings| {
            settings.sidebar_collapsed = collapsed;
        }) {
            log::error!("save sidebar collapsed: {error:#}");
        }
        cx.notify();
    }

    // --- Заметки (этап 15.2: список, открытие — вкладкой в центре) -------

    fn notes_index(cx: &App) -> Option<NotesHandle> {
        shell::notes::current(app_services(cx))
    }

    /// Подписаться на текущий индекс заметок (старт и смена хранилища).
    fn subscribe_notes_index(&mut self, cx: &mut Context<Self>) {
        let index = Self::notes_index(cx);
        self._index_token = index.as_ref().map(|index| {
            let pending = Arc::clone(&self.pending_events);
            index.subscribe(move |event| {
                pending.lock().unwrap().push(event.clone());
            })
        });
        self.subscribed_index = index;
    }

    /// Настройки изменились: если хранилище заметок заменили (другой Arc
    /// индекса) — переподписаться и перечитать список.
    fn on_settings_changed(&mut self, cx: &mut Context<Self>) {
        let current = Self::notes_index(cx);
        let changed = match (&self.subscribed_index, &current) {
            (None, None) => false,
            (Some(old), Some(new)) => !old.same_storage(new),
            _ => true,
        };
        if changed {
            self.subscribe_notes_index(cx);
            self.pending_events.lock().unwrap().clear();
            self.reload_notes(cx);
        }
        cx.notify();
    }

    fn reload_notes(&mut self, cx: &mut Context<Self>) {
        let Some(index) = Self::notes_index(cx) else {
            self.notes_list = Vec::new();
            self.note_folders = Vec::new();
            self.notes_tree
                .update(cx, |tree, cx| tree.set_items(Vec::new(), cx));
            cx.notify();
            return;
        };
        self.notes_list = shell::notes::list_notes(&index).unwrap_or_default();
        self.note_folders = shell::notes::list_folders(&index).unwrap_or_default();
        let tree_items = note_tree_items(
            &self.note_folders,
            &self.notes_list,
            &self.expanded_note_folders,
        );
        self.notes_tree
            .update(cx, |tree, cx| tree.set_items(tree_items, cx));
        cx.notify();
    }

    fn open_commentary_module(&mut self, module: ModuleId, cx: &mut Context<Self>) {
        let entries = app_services(cx).registry.lock().commentary_entries(&module);
        let mut book_labels = std::collections::HashMap::new();
        let rows = entries
            .into_iter()
            .map(|entry| {
                let book = book_labels
                    .entry(entry.book)
                    .or_insert_with(|| self.book_label(entry.book, cx))
                    .clone();
                let label = if entry.chapter_to != entry.chapter_from {
                    format!(
                        "{}:{}–{}:{}",
                        entry.chapter_from, entry.verse_from, entry.chapter_to, entry.verse_to
                    )
                } else if entry.verse_to != entry.verse_from {
                    format!(
                        "{}:{}–{}",
                        entry.chapter_from, entry.verse_from, entry.verse_to
                    )
                } else {
                    format!("{}:{}", entry.chapter_from, entry.verse_from)
                };
                SidebarCommentaryRow {
                    book: entry.book,
                    book_label: book,
                    reference: VerseRef {
                        book: entry.book,
                        chapter: entry.chapter_from,
                        verse: entry.verse_from,
                        end_verse: (entry.chapter_to == entry.chapter_from
                            && entry.verse_to != entry.verse_from)
                            .then_some(entry.verse_to),
                    },
                    label,
                }
            })
            .collect::<Vec<_>>();
        let first_reference = rows.first().map(|row| row.reference);
        self.expanded_commentary_books.clear();
        if let Some(first) = rows.first() {
            self.expanded_commentary_books
                .insert(format!("commentary-book:{}", first.book.0));
        }
        let tree_items = commentary_tree_items(&rows, &self.expanded_commentary_books);
        self.commentary_tree
            .update(cx, |tree, cx| tree.set_items(tree_items, cx));
        self.commentary_module = Some(module.clone());
        self.commentary_rows = Arc::new(rows);
        if let Some(reference) = first_reference {
            navigation::request_open_commentary(cx, module, reference);
        }
        cx.notify();
    }

    fn close_commentary_module(&mut self, cx: &mut Context<Self>) {
        self.commentary_module = None;
        self.commentary_rows = Arc::new(Vec::new());
        self.expanded_commentary_books.clear();
        self.commentary_tree
            .update(cx, |tree, cx| tree.set_items(Vec::new(), cx));
        cx.notify();
    }

    /// Создать заметку и открыть её вкладкой в центре.
    fn create_note(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let name = name.trim().to_string();
        let Some(index) = Self::notes_index(cx) else {
            return;
        };
        let result = if name.is_empty() {
            shell::notes::create_untitled_note(&index)
        } else {
            shell::notes::create_note(&index, &name)
        };
        match result {
            Ok(doc) => {
                self.new_note_input.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                });
                self.reload_notes(cx);
                navigation::request_open_note(cx, doc.relpath);
            }
            Err(error) => log::error!("create note {name}: {error:#}"),
        }
        cx.notify();
    }

    fn create_note_folder(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let Some(index) = Self::notes_index(cx) else {
            return;
        };
        match shell::notes::create_folder(&index, name) {
            Ok(_) => {
                self.new_folder_input.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                });
                self.reload_notes(cx);
            }
            Err(error) => log::error!("create note folder {name}: {error:#}"),
        }
    }

    // --- Теги (этап 21) ---------------------------------------------------

    fn create_tag(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let entered = name.trim();
        let name = if entered.is_empty() {
            crate::i18n::tr_static("Новый тег").as_ref()
        } else {
            entered
        };
        match shell::tags::create_tag(app_services(cx), name, "green") {
            Ok(_) => {
                self.new_tag_input.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                });
                navigation::notify_tags_changed(cx);
            }
            Err(error) => log::error!("create tag {name}: {error:#}"),
        }
    }

    /// Перечитать список тегов и стихи открытого тега (если есть).
    /// Открытый тег мог быть переименован/удалён — синхронизируем его
    /// копию со свежим списком.
    fn reload_tags(&mut self, cx: &mut Context<Self>) {
        let services = app_services(cx);
        self.tags = shell::tags::list_tags(services);
        self.tag_counts = shell::tags::tag_verse_counts(services);
        self.tags_loaded = true;
        if let Some(open) = &self.open_tag {
            match self.tags.iter().find(|tag| tag.id == open.id) {
                Some(fresh) => {
                    let fresh = fresh.clone();
                    self.tag_verses = shell::tags::verses_of_tag(services, fresh.id);
                    self.open_tag = Some(fresh);
                }
                None => {
                    self.open_tag = None;
                    self.tag_verses = Vec::new();
                }
            }
        }
        cx.notify();
    }

    /// Drill-down в тег: стихи + поле переименования с текущим именем.
    fn open_tag_drilldown(&mut self, tag: Tag, window: &mut Window, cx: &mut Context<Self>) {
        self.tag_verses = shell::tags::verses_of_tag(app_services(cx), tag.id);
        let name = tag.name.clone();
        self.tag_rename_input.update(cx, |input, cx| {
            input.set_value(name, window, cx);
        });
        let color = palette::base(&tag.color, cx);
        self.tag_color
            .update(cx, |state, cx| state.set_value(color, window, cx));
        self.open_tag = Some(tag);
        cx.notify();
    }

    fn close_tag(&mut self, cx: &mut Context<Self>) {
        self.open_tag = None;
        self.tag_verses = Vec::new();
        cx.notify();
    }

    fn rename_open_tag(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(tag) = &self.open_tag else {
            return;
        };
        let name = name.trim();
        if name.is_empty() || name == tag.name {
            return;
        }
        if let Err(error) = shell::tags::rename_tag(app_services(cx), tag.id, name) {
            log::error!("rename tag failed: {error:#}");
            return;
        }
        navigation::notify_tags_changed(cx);
    }

    fn set_open_tag_color(&mut self, color: Hsla, cx: &mut Context<Self>) {
        let Some(tag) = &self.open_tag else { return };
        if let Err(error) =
            shell::tags::set_tag_color(app_services(cx), tag.id, &palette::to_hex(color))
        {
            log::error!("set tag color failed: {error:#}");
            return;
        }
        navigation::notify_tags_changed(cx);
    }

    fn confirm_delete_open_tag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tag) = &self.open_tag else {
            return;
        };
        let tag_id = tag.id;
        let receiver = window.prompt(
            PromptLevel::Critical,
            crate::i18n::tr_static("Удалить тег?"),
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
                if let Err(error) = shell::tags::delete_tag(app_services(cx), tag_id) {
                    log::error!("delete tag failed: {error:#}");
                    return;
                }
                this.open_tag = None;
                navigation::notify_tags_changed(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Подпись книги для строки стиха тега: короткое имя из перевода
    /// по умолчанию; без него — русский алиас из статической таблицы.
    /// Модуль для показа тегированных мест: теги модуль-независимы,
    /// поэтому показываем и открываем их в том переводе, который сейчас
    /// открыт в чтении; если чтение пусто — в переводе по умолчанию.
    fn preferred_bible_module(cx: &App) -> Option<ModuleId> {
        let services = app_services(cx);
        if let Some(location) = services.current_location.lock().as_ref() {
            return Some(location.module.clone());
        }
        services.default_translation()
    }

    fn book_label(&self, book: BookNumber, cx: &App) -> String {
        let services = app_services(cx);
        if let Some(module) = Self::preferred_bible_module(cx) {
            let entry = services
                .registry
                .lock()
                .books_of(&module)
                .into_iter()
                .find(|entry| entry.book_number == book.0);
            if let Some(entry) = entry {
                return entry.short_name;
            }
        }
        bible_domain::book_aliases::BOOK_ALIASES
            .iter()
            .find(|(number, _)| *number == book.0)
            .and_then(|(_, names)| {
                names
                    .iter()
                    .find(|name| name.chars().any(|c| c.is_alphabetic() && !c.is_ascii()))
                    .or_else(|| names.first())
            })
            .map(|name| (*name).to_string())
            .unwrap_or_else(|| format!("Кн.{}", book.0))
    }

    /// Список тегов со счётчиками стихов (drill-down — render_tag_verses).
    fn render_tags(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut list = v_flex()
            .id("sidebar-tags")
            .w_full()
            .min_w_0()
            .flex_1()
            .min_h_0()
            .p_1()
            .gap(px(1.));
        if self.tags.is_empty() {
            list = list.child(
                div()
                    .p_2()
                    .text_size(px(crate::ui_scale::left_font_size(cx, 14.0)))
                    .text_color(cx.theme().muted_foreground)
                    .child(crate::i18n::tr_static(
                        "Тегов пока нет — поставьте первый из меню стиха («Теги стиха…»).",
                    )),
            );
        }
        for tag in &self.tags {
            let tag = tag.clone();
            let count = self.tag_counts.get(&tag.id).copied().unwrap_or(0);
            let tag_for_click = tag.clone();
            list = list.child(
                h_flex()
                    .id(ElementId::Name(format!("sidebar-tag-{}", tag.id).into()))
                    .w_full()
                    .min_w_0()
                    .px_2()
                    .py(px(2.))
                    .gap_2()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(crate::ui_scale::hover_background(cx)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_tag_drilldown(tag_for_click.clone(), window, cx);
                    }))
                    .child(
                        div()
                            .w(px(10.))
                            .h(px(10.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(palette::word_fg(&tag.color, cx)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_size(px(crate::ui_scale::left_font_size(cx, 14.0)))
                            .text_color(cx.theme().foreground)
                            .child(tag.name.clone()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(crate::ui_scale::left_font_size(cx, 12.0)))
                            .text_color(cx.theme().muted_foreground)
                            .child(count.to_string()),
                    ),
            );
        }
        v_flex()
            .w_full()
            .h_full()
            .min_h_0()
            .child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .p_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("sidebar-tag-create")
                            .small()
                            .label(crate::i18n::tr(cx, "+ Новый тег"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                let name = this.new_tag_input.read(cx).value().to_string();
                                this.create_tag(name, window, cx);
                            })),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .child(Input::new(&self.new_tag_input).small()),
                    ),
            )
            .child(list.overflow_y_scrollbar())
            .into_any_element()
    }

    fn render_comments(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(module) = self.commentary_module.clone() else {
            let modules = app_services(cx)
                .registry
                .modules()
                .into_iter()
                .filter(|module| module.kind == ModuleKind::Commentary)
                .collect::<Vec<_>>();
            if modules.is_empty() {
                return div()
                    .p_3()
                    .text_color(cx.theme().muted_foreground)
                    .child(crate::i18n::tr(cx, "Модули комментариев не найдены."))
                    .into_any_element();
            }
            return v_flex()
                .id("sidebar-commentary-modules")
                .size_full()
                .min_h_0()
                .p_1()
                .gap_1()
                .children(modules.into_iter().enumerate().map(|(index, meta)| {
                    let module = meta.id.clone();
                    let label = meta.description.unwrap_or_else(|| meta.id.to_string());
                    div()
                        .id(("sidebar-commentary-module", index))
                        .w_full()
                        .min_w_0()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(crate::ui_scale::row_background(index, cx))
                        .cursor_pointer()
                        .hover(|style| style.bg(crate::ui_scale::hover_background(cx)))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_commentary_module(module.clone(), cx);
                        }))
                }))
                .overflow_y_scrollbar()
                .into_any_element();
        };

        let module_title = app_services(cx)
            .registry
            .module(&module)
            .and_then(|meta| meta.description)
            .unwrap_or_else(|| module.to_string());
        let rows = self.commentary_rows.clone();
        let module_for_rows = module.clone();
        let tree = Tree::new(&self.commentary_tree, move |index, entry, _, _, cx| {
            let id = entry.item().id.to_string();
            let label = entry.item().label.clone();
            let depth = entry.depth();
            let is_book = id.starts_with("commentary-book:");
            let mut row = h_flex()
                .id(("sidebar-commentary-tree-row", index))
                .w_full()
                .h(px(28.))
                .min_w_0()
                .pl(px(4. + depth as f32 * 16.))
                .pr_1()
                .gap_1()
                .items_center()
                .rounded_md()
                .bg(crate::ui_scale::row_background(index, cx))
                .cursor_pointer()
                .hover(|style| style.bg(crate::ui_scale::hover_background(cx)))
                .child(div().w(px(14.)).flex_shrink_0().children(is_book.then(|| {
                    Icon::new(if entry.is_expanded() {
                        gpui_kit::assets::IconName::ChevronDown
                    } else {
                        gpui_kit::assets::IconName::ChevronRight
                    })
                    .xsmall()
                })))
                .child(
                    Icon::new(if is_book {
                        gpui_kit::assets::IconName::BookOpen
                    } else {
                        gpui_kit::assets::IconName::FileText
                    })
                    .xsmall()
                    .text_color(if is_book {
                        cx.theme().accent
                    } else {
                        cx.theme().muted_foreground
                    }),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_size(px(crate::ui_scale::left_font_size(cx, 13.0)))
                        .font_weight(if is_book {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        })
                        .child(label),
                );
            if let Some(row_index) = id
                .strip_prefix("commentary-entry:")
                .and_then(|value| value.parse::<usize>().ok())
            {
                if let Some(reference) = rows.get(row_index).map(|row| row.reference) {
                    let module = module_for_rows.clone();
                    row = row.on_click(move |_, _, cx| {
                        navigation::request_open_commentary(cx, module.clone(), reference);
                    });
                }
            }
            ListItem::new(("sidebar-commentary-tree-item", index)).child(row)
        });
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .id("sidebar-commentary-back")
                            .p_1()
                            .rounded_md()
                            .cursor_pointer()
                            .child(Icon::new(gpui_kit::assets::IconName::ArrowLeft).xsmall())
                            .on_click(
                                cx.listener(|this, _, _, cx| this.close_commentary_module(cx)),
                            ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(module_title),
                    ),
            )
            .child(div().flex_1().min_h_0().child(tree))
            .into_any_element()
    }

    /// Drill-down тега: назад, точка-переключатель цвета, имя
    /// (редактируемое, Enter — сохранить), удаление; стихи тега.
    fn render_tag_verses(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut list = v_flex()
            .id("sidebar-tag-verses")
            .w_full()
            .min_w_0()
            .flex_1()
            .min_h_0()
            .p_1()
            .gap(px(1.));
        if self.tag_verses.is_empty() {
            list = list.child(
                div()
                    .p_2()
                    .text_size(px(crate::ui_scale::left_font_size(cx, 14.0)))
                    .text_color(cx.theme().muted_foreground)
                    .child(crate::i18n::tr_static("С этим тегом пока нет стихов.")),
            );
        }
        for reference in &self.tag_verses {
            let reference = *reference;
            let label = format!(
                "{} {}:{}",
                self.book_label(reference.book, cx),
                reference.chapter,
                reference.verse
            );
            list = list.child(
                div()
                    .id(ElementId::Name(
                        format!(
                            "sidebar-tag-verse-{}-{}-{}",
                            reference.book.0, reference.chapter, reference.verse
                        )
                        .into(),
                    ))
                    .w_full()
                    .min_w_0()
                    .px_2()
                    .py(px(2.))
                    .rounded_md()
                    .cursor_pointer()
                    .text_size(px(crate::ui_scale::left_font_size(cx, 14.0)))
                    .text_color(cx.theme().foreground)
                    .hover(|style| style.bg(crate::ui_scale::hover_background(cx)))
                    .child(label)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        // Теги модуль-независимы: открываем место в модуле,
                        // который сейчас в чтении (иначе — перевод по умолчанию).
                        let Some(module) = Self::preferred_bible_module(cx) else {
                            return;
                        };
                        navigation::request_open_book(
                            cx,
                            navigation::OpenBook {
                                module,
                                book: reference.book,
                                chapter: Some(reference.chapter),
                                verse: Some(reference.verse),
                            },
                        );
                    })),
            );
        }

        v_flex()
            .w_full()
            .h_full()
            .min_h_0()
            .child(
                h_flex()
                    .w_full()
                    .p_1()
                    .gap_1()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .id("sidebar-tag-back")
                            .p_1()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|style| style.bg(crate::ui_scale::hover_background(cx)))
                            .child(
                                Icon::new(gpui_kit::assets::IconName::ArrowLeft)
                                    .xsmall()
                                    .text_color(cx.theme().foreground),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.close_tag(cx);
                            })),
                    )
                    .child(
                        div()
                            .w(px(72.))
                            .child(ColorSelect::new(&self.tag_color).xsmall().w_full()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(Input::new(&self.tag_rename_input).small()),
                    )
                    .child(
                        div()
                            .id("sidebar-tag-delete")
                            .p_1()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|style| style.bg(cx.theme().danger.opacity(0.15)))
                            .child(
                                Icon::new(gpui_kit::assets::IconName::Trash)
                                    .xsmall()
                                    .text_color(cx.theme().danger),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.confirm_delete_open_tag(window, cx);
                            })),
                    ),
            )
            .child(list.overflow_y_scrollbar())
            .into_any_element()
    }

    /// Список заметок хранилища: действия создания сверху, затем реальные
    /// папки (алфавитно), затем Markdown-файлы.
    fn render_notes(&self, cx: &mut Context<Self>) -> AnyElement {
        if Self::notes_index(cx).is_none() {
            return v_flex()
                .w_full()
                .h_full()
                .p_3()
                .child(
                    div()
                        .text_size(px(crate::ui_scale::left_font_size(cx, 14.0)))
                        .text_color(cx.theme().muted_foreground)
                        .child(crate::i18n::tr_static(
                            "Заметки отключены: укажите папку заметок в настройках.",
                        )),
                )
                .into_any_element();
        }
        let folders = self.note_folders.clone();
        let tree = Tree::new(&self.notes_tree, move |index, entry, _, _, cx| {
            let id = entry.item().id.to_string();
            let label = entry.item().label.clone();
            let depth = entry.depth();
            let is_folder = id.starts_with("folder:");
            let is_note = id.starts_with("note:");
            let is_empty = id.starts_with("empty:");
            let mut row = h_flex()
                .id(("note-tree-row", index))
                .w_full()
                .h(px(26.))
                .min_w_0()
                .pl(px(4. + depth as f32 * 14.))
                .pr_1()
                .gap_1()
                .items_center()
                .cursor_pointer()
                .child(
                    div()
                        .w(px(14.))
                        .flex_shrink_0()
                        .children(is_folder.then(|| {
                            Icon::new(if entry.is_expanded() {
                                gpui_kit::assets::IconName::ChevronDown
                            } else {
                                gpui_kit::assets::IconName::ChevronRight
                            })
                            .xsmall()
                        })),
                )
                .child(
                    Icon::new(if is_folder {
                        gpui_kit::assets::IconName::Folder
                    } else {
                        gpui_kit::assets::IconName::FileText
                    })
                    .xsmall()
                    .text_color(if is_folder {
                        cx.theme().accent
                    } else {
                        cx.theme().muted_foreground
                    }),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_size(px(crate::ui_scale::left_font_size(cx, 13.0)))
                        .text_color(if is_empty {
                            cx.theme().muted_foreground
                        } else {
                            cx.theme().foreground
                        })
                        .child(label.clone()),
                );
            if is_folder {
                let target = id.trim_start_matches("folder:").to_owned();
                row = row
                    .drag_over::<NoteDrag>(|row, _, _, cx| row.bg(cx.theme().tokens.drop_target))
                    .on_drop(move |note: &NoteDrag, _, cx| {
                        let Some(notes) = shell::notes::current(app_services(cx)) else {
                            return;
                        };
                        match shell::notes::move_note(&notes, &note.relpath, &target) {
                            Ok(note) => navigation::request_open_note(cx, note.relpath),
                            Err(error) => log::error!("move note by drag: {error:#}"),
                        }
                    });
            } else if is_note {
                let relpath = id.trim_start_matches("note:").to_owned();
                let drag = NoteDrag {
                    relpath: relpath.clone(),
                    title: label.to_string(),
                };
                row = row
                    .on_drag(drag, |note, _, _, cx| {
                        cx.new(|_| NoteDragPreview(note.clone()))
                    })
                    .on_click(move |_, _, cx| {
                        navigation::request_open_note(cx, relpath.clone());
                    });
            }
            ListItem::new(("note-tree-item", index)).child(row)
        })
        .context_menu(move |_, entry, mut menu, _, _| {
            let id = entry.item().id.to_string();
            if !id.starts_with("note:") {
                return menu;
            }
            let relpath = id.trim_start_matches("note:").to_owned();
            let folders = folders.clone();
            menu = menu.scrollable(true);
            let root_note = relpath.clone();
            menu = menu.item(
                PopupMenuItem::new("Перенести в корень").on_click(move |_, _, cx| {
                    let Some(notes) = shell::notes::current(app_services(cx)) else {
                        return;
                    };
                    if let Err(error) = shell::notes::move_note(&notes, &root_note, "") {
                        log::error!("move note to root: {error:#}");
                    }
                }),
            );
            if !folders.is_empty() {
                menu = menu.separator();
            }
            for folder in &folders {
                let note = relpath.clone();
                let target = folder.relpath.clone();
                menu = menu.item(
                    PopupMenuItem::new(format!("Перенести в {}", folder.relpath)).on_click(
                        move |_, _, cx| {
                            let Some(notes) = shell::notes::current(app_services(cx)) else {
                                return;
                            };
                            if let Err(error) = shell::notes::move_note(&notes, &note, &target) {
                                log::error!("move note from menu: {error:#}");
                            }
                        },
                    ),
                );
            }
            menu = menu.separator();
            let note_to_delete = relpath.clone();
            menu = menu.item(
                PopupMenuItem::new(crate::i18n::tr_static("Удалить заметку…")).on_click(
                    move |_, window, cx| {
                        let receiver = window.prompt(
                            PromptLevel::Critical,
                            crate::i18n::tr_static("Удалить заметку?"),
                            Some(crate::i18n::tr_static("Эту операцию нельзя отменить")),
                            &[
                                crate::i18n::tr_static("Удалить"),
                                crate::i18n::tr_static("Отмена"),
                            ],
                            cx,
                        );
                        let relpath = note_to_delete.clone();
                        cx.spawn(async move |cx| {
                            if receiver.await != Ok(0) {
                                return;
                            }
                            cx.update(|cx| {
                                let Some(notes) = shell::notes::current(app_services(cx)) else {
                                    return;
                                };
                                if let Err(error) = shell::notes::delete_note(&notes, &relpath) {
                                    log::error!("delete note: {error:#}");
                                }
                            });
                        })
                        .detach();
                    },
                ),
            );
            menu
        });
        let templates = Self::notes_index(cx)
            .and_then(|index| shell::notes::load_templates(&index).ok())
            .unwrap_or_default();
        let template_title = self.new_note_input.read(cx).value().to_string();
        let explorer: AnyElement = if self.notes_list.is_empty() && self.note_folders.is_empty() {
            div()
                .flex_1()
                .min_h_0()
                .p_2()
                .text_size(px(crate::ui_scale::left_font_size(cx, 13.0)))
                .text_color(cx.theme().muted_foreground)
                .child(crate::i18n::tr_static(
                    "Заметок пока нет — создайте первую выше.",
                ))
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(tree)
                .into_any_element()
        };
        v_flex()
            .w_full()
            .h_full()
            .min_h_0()
            .child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .p_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("sidebar-note-create")
                            .small()
                            .label(crate::i18n::tr(cx, "+ Новая заметка"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                let name = this.new_note_input.read(cx).value().to_string();
                                this.create_note(name, window, cx);
                            })),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .child(Input::new(&self.new_note_input).small()),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .gap_1()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Input::new(&self.new_folder_input).small()),
                            )
                            .child(
                                Button::new("sidebar-folder-create")
                                    .xsmall()
                                    .label(crate::i18n::tr(cx, "+ Новая папка"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        let name = this.new_folder_input.read(cx).value().to_string();
                                        this.create_note_folder(name, window, cx);
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("sidebar-note-template")
                                    .xsmall()
                                    .label("Создать по шаблону")
                                    .dropdown_menu(move |mut menu, _, _| {
                                        for template in &templates {
                                            let template = template.clone();
                                            let entered_title = if template_title.trim().is_empty() {
                                                crate::i18n::tr_static("Новая заметка").to_string()
                                            } else {
                                                template_title.clone()
                                            };
                                            menu = menu.item(PopupMenuItem::new(template.name.clone()).on_click(
                                                move |_, _, cx| {
                                                    let Some(index) = shell::notes::current(app_services(cx)) else { return };
                                                    match shell::notes::create_note_from_template(&index, &template, &entered_title) {
                                                        Ok(note) => navigation::request_open_note(cx, note.relpath),
                                                        Err(error) => log::error!("create from template: {error:#}"),
                                                    }
                                                },
                                            ));
                                        }
                                        menu
                                    }),
                            )
                            .child(
                                Button::new("sidebar-note-template-settings")
                                    .xsmall()
                                    .ghost()
                                    .label("Шаблоны…")
                                    .on_click(|_, _, cx| crate::note_templates::open(cx)),
                            ),
                    ),
            )
            .child(explorer)
            .into_any_element()
    }

    fn render_content(&mut self, cx: &mut Context<Self>) -> AnyElement {
        match self.section {
            Section::Search => self.search.clone().into_any_element(),
            Section::Dictionaries => self.dictionaries.clone().into_any_element(),
            Section::Notes => self.render_notes(cx),
            Section::Comments => self.render_comments(cx),
            Section::Chains => self.chains.clone().into_any_element(),
            Section::Questions => self.questions.clone().into_any_element(),
            Section::Tags => match &self.open_tag {
                Some(_) => self.render_tag_verses(cx),
                None => self.render_tags(cx),
            },
        }
    }
}

impl Render for SidebarView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::assets::IconName;
        let collapsed = self.collapsed;
        let rail_background = cx.theme().sidebar;
        let rail_foreground = cx.theme().sidebar_foreground;
        let menu = [
            Section::Search,
            Section::Dictionaries,
            Section::Notes,
            Section::Comments,
            Section::Tags,
            Section::Chains,
            Section::Questions,
        ];
        let icon_size = crate::ui_scale::icon_size(cx);
        let rail_button =
            |id: &'static str, icon, label: SharedString, show_label: bool, inverted: bool| {
                let tooltip = label.clone();
                h_flex()
                    .id(id)
                    .w_full()
                    .h(px(42.))
                    .rounded_md()
                    .cursor_pointer()
                    .text_color(rail_foreground)
                    // Hover is intentionally a full colour inversion, not a
                    // subtle translucent wash: it remains visible in low-contrast
                    // grey themes and to peripheral vision. When a section is
                    // open, its button keeps this exact inverted state.
                    .when(inverted, |button| {
                        button.bg(rail_foreground).text_color(rail_background)
                    })
                    .hover(move |style| style.bg(rail_foreground).text_color(rail_background))
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(tooltip.clone())
                            .build(window, cx)
                    })
                    .child(
                        h_flex()
                            .w(px(42.))
                            .flex_shrink_0()
                            .justify_center()
                            .child(Icon::new(icon).size(icon_size)),
                    )
                    .children(show_label.then(|| div().child(label)))
            };
        let rail = v_flex()
            .h_full()
            .flex_shrink_0()
            .p_1()
            .gap_1()
            .w(px(if collapsed { 50. } else { 190. }))
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().border)
            .children(menu.into_iter().map(|item| {
                rail_button(
                    item.as_settings(),
                    item.icon(),
                    item.label(cx),
                    !collapsed,
                    self.content_visible && self.section == item,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.select_section(item, cx)))
            }))
            .child(div().flex_1())
            .child(
                rail_button(
                    "toggle-sidebar-labels",
                    if collapsed {
                        IconName::PanelLeftOpen
                    } else {
                        IconName::PanelLeftClose
                    },
                    crate::i18n::tr(
                        cx,
                        if collapsed {
                            "Развернуть меню"
                        } else {
                            "Свернуть меню"
                        },
                    ),
                    false,
                    false,
                )
                .justify_center()
                .on_click(cx.listener(|this, _, _, cx| this.toggle_collapsed(cx))),
            )
            .child(
                rail_button(
                    "toggle-sidebar-content",
                    IconName::PanelLeft,
                    crate::i18n::tr(cx, "Показать или скрыть содержимое"),
                    false,
                    false,
                )
                .justify_center()
                .on_click(cx.listener(|this, _, _, cx| this.toggle_content(cx))),
            )
            .child(
                rail_button(
                    "open-settings",
                    IconName::Settings,
                    crate::i18n::tr(cx, "Настройки"),
                    !collapsed,
                    false,
                )
                .on_click(|_, _, cx| crate::settings::open_settings_window(cx)),
            );
        let section = self.section;
        let content = self.render_content(cx);
        let mut layout = h_flex()
            .h_full()
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<ResizeSidebar>, _, cx| {
                    let drag = event.drag(cx);
                    let current_x = event.event.position.x;
                    let Some(previous_x) = drag.previous_x.replace(Some(current_x)) else {
                        // The handle's drag-start offset and DragMoveEvent use
                        // different coordinate spaces. Treat the first
                        // absolute move as the origin instead of resizing.
                        return;
                    };
                    let width = (this.content_width + current_x - previous_x).max(px(120.));
                    if this.content_width != width {
                        this.content_width = width;
                        cx.notify();
                    }
                }),
            )
            .on_drop(cx.listener(|this, _: &ResizeSidebar, _, cx| {
                let width = this.content_width / px(1.);
                if let Err(error) = shell::settings::update(app_services(cx), |settings| {
                    settings.sidebar_content_width = width;
                }) {
                    log::error!("save sidebar content width: {error:#}");
                }
            }))
            .child(rail);
        if self.content_visible {
            let resize = ResizeSidebar {
                previous_x: Rc::new(Cell::new(None)),
            };
            layout = layout.child(
                div()
                    .relative()
                    .w(self.content_width)
                    .flex_shrink_0()
                    .h_full()
                    .min_h_0()
                    .border_r_1()
                    .border_color(cx.theme().border)
                    .child(content)
                    .child(
                        resize_handle("sidebar-content-resize", Axis::Horizontal)
                            .inside(HandleEdge::Trailing)
                            .on_drag(resize, |drag, _, _, cx| cx.new(|_| drag.clone())),
                    )
                    .context_menu(move |menu, _, _| match section {
                        Section::Notes => menu.item(
                            PopupMenuItem::new(crate::i18n::tr_static("Создать новую заметку"))
                                .on_click(|_, _, cx| {
                                    let Some(index) = shell::notes::current(app_services(cx))
                                    else {
                                        return;
                                    };
                                    match shell::notes::create_untitled_note(&index) {
                                        Ok(note) => navigation::request_open_note(cx, note.relpath),
                                        Err(error) => log::error!("create note: {error:#}"),
                                    }
                                }),
                        ),
                        Section::Tags => menu.item(
                            PopupMenuItem::new(crate::i18n::tr_static("Создать новый тег"))
                                .on_click(|_, _, cx| {
                                    if let Err(error) = shell::tags::create_tag(
                                        app_services(cx),
                                        crate::i18n::tr_static("Новый тег"),
                                        "green",
                                    ) {
                                        log::error!("create tag: {error:#}");
                                    } else {
                                        navigation::notify_tags_changed(cx);
                                    }
                                }),
                        ),
                        Section::Chains => menu.item(
                            PopupMenuItem::new(crate::i18n::tr_static("Создать новую цепочку"))
                                .on_click(|_, _, cx| {
                                    match shell::chains::create_chain(
                                        app_services(cx),
                                        crate::i18n::tr_static("Новая цепочка"),
                                    ) {
                                        Ok(chain) => {
                                            navigation::notify_chains_changed(cx);
                                            navigation::request_open_chain(cx, Some(chain.id));
                                        }
                                        Err(error) => log::error!("create chain: {error:#}"),
                                    }
                                }),
                        ),
                        _ => menu,
                    }),
            );
        }
        let font_family = crate::ui_scale::left_font_family(cx);
        layout
            .text_size(px(crate::ui_scale::left_font_size(cx, 14.)))
            .when(!font_family.is_empty(), |root| {
                root.font_family(font_family)
            })
    }
}
