//! Команды навигации — результат работы Link Router (architecture.md §5.4)
//! и per-вкладка история переходов ←/→ (§5.5).

use bible_domain::ids::{BookNumber, ModuleId};
use bible_domain::link::LinkTarget;

#[derive(Debug, Clone)]
pub enum NavigationCommand {
    OpenChapter {
        module: ModuleId,
        book: BookNumber,
        chapter: u16,
        verse: Option<u16>,
    },
    OpenDictionary {
        module: ModuleId,
        topic: String,
    },
    OpenNote {
        relpath: String,
    },
    OpenExternal(String),
}

/// Распознаёт семантическую цель ссылки в команду навигации.
/// Правила открытия (новая вкладка / reuse) применяет вызывающая сторона
/// по настройке `bible.link_open_mode`.
pub fn route(target: &LinkTarget) -> Option<NavigationCommand> {
    match target {
        LinkTarget::Bible(reference) => Some(NavigationCommand::OpenChapter {
            module: ModuleId(String::new()), // подставляется активным переводом на этапе 2
            book: reference.book,
            chapter: reference.chapter,
            verse: Some(reference.verse),
        }),
        LinkTarget::DictionaryEntry { module, topic } => Some(NavigationCommand::OpenDictionary {
            module: module.clone(),
            topic: topic.clone(),
        }),
        LinkTarget::Note(relpath) => Some(NavigationCommand::OpenNote {
            relpath: relpath.clone(),
        }),
        LinkTarget::ExternalUrl(url) => Some(NavigationCommand::OpenExternal(url.clone())),
        LinkTarget::Commentary { .. } | LinkTarget::CrossReferences { .. } => None, // этап 5
    }
}

// ---------------------------------------------------------------------------
// История навигации ←/→ (architecture.md §5.5, этап 9).
//
// Перенесена идея nav_history.rs форка, но без GPUI: стек чистый, владелец
// (панель чтения / будущие вкладки) хранит по экземпляру. События и кнопки
// живут в bible_ui (шина навигации).
// ---------------------------------------------------------------------------

/// Потолок истории: старшее безболезненно забывается (как в форке).
pub const HISTORY_CAP: usize = 200;

/// Одна запись истории: место чтения, к которому можно вернуться.
/// Serialize — стеки вкладок переживают перезапуск в payload вкладки
/// (layout.json, architecture.md §7.2).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NavEntry {
    pub module: ModuleId,
    pub book: BookNumber,
    pub chapter: u16,
    pub verse: Option<u16>,
}

/// Сериализуемый снимок стека для payload вкладки (§7.2): весь стек
/// целиком + курсор, чтобы после перезапуска ←/→ вели себя как до выхода.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HistoryDump {
    pub entries: Vec<NavEntry>,
    pub cursor: usize,
}

/// Стек назад/вперёд с курсором. Инвариант: курсор указывает на «текущую»
/// запись; push в середине стека отрезает «будущее».
#[derive(Debug, Default)]
pub struct NavHistory {
    entries: Vec<NavEntry>,
    cursor: usize,
}

impl NavHistory {
    /// Записать переход. Повтор текущей записи игнорируется (переоткрытие
    /// той же главы — не новая точка истории).
    pub fn push(&mut self, entry: NavEntry) {
        if self.entries.get(self.cursor) == Some(&entry) {
            return;
        }
        if !self.entries.is_empty() && self.cursor + 1 < self.entries.len() {
            self.entries.truncate(self.cursor + 1);
        }
        self.entries.push(entry);
        self.cursor = self.entries.len() - 1;
        if self.entries.len() > HISTORY_CAP {
            let drop = self.entries.len() - HISTORY_CAP;
            self.entries.drain(0..drop);
            self.cursor = self.cursor.saturating_sub(drop);
        }
    }

    /// Шаг назад. None — история на дне.
    pub fn back(&mut self) -> Option<&NavEntry> {
        if self.cursor == 0 {
            return None;
        }
        self.cursor -= 1;
        self.entries.get(self.cursor)
    }

    /// Шаг вперёд. None — впереди пусто (или отрезано новым push).
    pub fn forward(&mut self) -> Option<&NavEntry> {
        if self.entries.is_empty() || self.cursor + 1 >= self.entries.len() {
            return None;
        }
        self.cursor += 1;
        self.entries.get(self.cursor)
    }

    pub fn can_back(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_forward(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    /// Текущая запись (для диагностики и тестов UI).
    pub fn current(&self) -> Option<&NavEntry> {
        self.entries.get(self.cursor)
    }

    /// Снимок стека для payload вкладки (§7.2).
    pub fn dump(&self) -> HistoryDump {
        HistoryDump {
            entries: self.entries.clone(),
            cursor: self.cursor,
        }
    }

    /// Восстановление из снимка. Битые данные не валят запуск: пустой
    /// снимок — пустая история; курсор за границами прижимается к концу;
    /// потолок HISTORY_CAP соблюдается тем же правилом, что и push.
    pub fn restore(dump: HistoryDump) -> Self {
        let mut history = Self {
            entries: dump.entries,
            cursor: dump.cursor,
        };
        if history.entries.is_empty() {
            history.cursor = 0;
            return history;
        }
        if history.cursor >= history.entries.len() {
            history.cursor = history.entries.len() - 1;
        }
        if history.entries.len() > HISTORY_CAP {
            let drop = history.entries.len() - HISTORY_CAP;
            history.entries.drain(0..drop);
            history.cursor = history.cursor.saturating_sub(drop);
        }
        history
    }
}

// ---------------------------------------------------------------------------
// Палитра «Перейти к стиху» (architecture.md §6.5, этап 9): разбор ввода
// «Ин 3:16» в цель перехода и подсказки имён книг.
// ---------------------------------------------------------------------------

/// Цель перехода из палитры: разобранная ссылка + контекст для превью.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpTarget {
    /// Перевод: активный в чтении, иначе из настроек/первый по алфавиту.
    pub module: ModuleId,
    pub book: BookNumber,
    pub chapter: u16,
    pub verse: Option<u16>,
    /// Длинное имя книги из перевода (превью «Иоанн 3:16»).
    pub book_title: String,
    /// Сколько глав в книге — палитра отклоняет несуществующие главы.
    pub chapter_count: u16,
}

/// Перевод для навигационных сценариев: активный в чтении, иначе
/// из настроек/первый по алфавиту (как preferred_translation форка).
fn active_translation(services: &crate::services::AppServices) -> Option<ModuleId> {
    services
        .current_location
        .lock()
        .as_ref()
        .map(|location| location.module.clone())
        .or_else(|| services.default_translation())
}

/// Разобрать ввод палитры перехода. None — ввод не распознан или книги
/// нет в выбранном переводе (тогда палитра показывает подсказки книг).
pub fn parse_jump(services: &crate::services::AppServices, input: &str) -> Option<JumpTarget> {
    let reference =
        bible_domain::reference::parse(input, |name| services.registry.lock().lookup_book(name))?;
    let module = active_translation(services)?;
    let mut registry = services.registry.lock();
    let book_entry = registry
        .books_of(&module)
        .into_iter()
        .find(|entry| entry.book_number == reference.book_number)?;
    let book = BookNumber(reference.book_number);
    let chapter_count = registry.chapter_count_of(&module, book);
    Some(JumpTarget {
        module,
        book,
        chapter: reference.chapter,
        verse: reference.verse,
        book_title: book_entry.long_name,
        chapter_count,
    })
}

/// Подсказки имён книг для палитры: имена берутся из АКТИВНОГО перевода
/// (как в форке — иначе при установленном английском модуле русские
/// подсказки могли бы проиграть дедупликацию сводного списка); запасной
/// вариант — сводный список всех переводов. Сначала совпадения по префиксу
/// (короткое или длинное имя), затем по подстроке; нормализация та же,
/// что у парсера ссылок. Пустой запрос — пустой список.
pub fn book_suggestions(
    services: &crate::services::AppServices,
    query: &str,
    limit: usize,
) -> Vec<mybible::BookEntry> {
    let key = bible_domain::reference::normalize_book_key(query.trim());
    if key.is_empty() {
        return Vec::new();
    }
    let module = active_translation(services);
    let mut registry = services.registry.lock();
    let entries = match module {
        Some(module) => {
            let books = registry.books_of(&module);
            if books.is_empty() {
                registry.book_entries().to_vec()
            } else {
                books
            }
        }
        None => registry.book_entries().to_vec(),
    };
    drop(registry);
    let mut prefix_matches = Vec::new();
    let mut substring_matches = Vec::new();
    for entry in entries {
        let short = bible_domain::reference::normalize_book_key(&entry.short_name);
        let long = bible_domain::reference::normalize_book_key(&entry.long_name);
        if short.starts_with(&key) || long.starts_with(&key) {
            prefix_matches.push(entry);
        } else if short.contains(&key) || long.contains(&key) {
            substring_matches.push(entry);
        }
    }
    prefix_matches.extend(substring_matches);
    prefix_matches.truncate(limit);
    prefix_matches
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(book: i64, chapter: u16) -> NavEntry {
        NavEntry {
            module: ModuleId("RST".to_string()),
            book: BookNumber(book),
            chapter,
            verse: None,
        }
    }
    #[test]
    fn push_back_forward() {
        let mut history = NavHistory::default();
        assert!(!history.can_back());
        assert!(!history.can_forward());

        history.push(entry(10, 1));
        history.push(entry(10, 2));
        history.push(entry(500, 3));
        assert!(history.can_back());
        assert!(!history.can_forward());

        assert_eq!(history.back(), Some(&entry(10, 2)));
        assert_eq!(history.back(), Some(&entry(10, 1)));
        assert_eq!(history.back(), None);
        assert!(history.can_forward());

        assert_eq!(history.forward(), Some(&entry(10, 2)));
        assert_eq!(history.forward(), Some(&entry(500, 3)));
        assert_eq!(history.forward(), None);
    }

    #[test]
    fn push_dedupes_current_and_truncates_future() {
        let mut history = NavHistory::default();
        history.push(entry(10, 1));
        history.push(entry(10, 2));
        // Повтор текущей записи не плодит дубли.
        history.push(entry(10, 2));
        assert_eq!(history.back(), Some(&entry(10, 1)));
        assert_eq!(history.forward(), Some(&entry(10, 2)));

        // Шаг назад и новый переход — «будущее» отрезается.
        assert_eq!(history.back(), Some(&entry(10, 1)));
        history.push(entry(500, 3));
        assert!(!history.can_forward());
        assert_eq!(history.current(), Some(&entry(500, 3)));
    }

    #[test]
    fn cap_drops_oldest() {
        let mut history = NavHistory::default();
        for index in 0..(HISTORY_CAP as u16 + 10) {
            history.push(entry(10, index));
        }
        assert!(history.can_back());
        // Дошли до дна: самые старые записи забыты, но стек цел.
        while history.back().is_some() {}
        assert_eq!(history.current(), Some(&entry(10, 10)));
        assert!(!history.can_back());
    }

    #[test]
    fn dump_restore_roundtrip_preserves_cursor() {
        let mut history = NavHistory::default();
        history.push(entry(10, 1));
        history.push(entry(10, 2));
        history.push(entry(500, 3));
        assert_eq!(history.back(), Some(&entry(10, 2)));

        let json = serde_json::to_string(&history.dump()).unwrap();
        let restored = NavHistory::restore(serde_json::from_str(&json).unwrap());
        assert_eq!(restored.current(), Some(&entry(10, 2)));
        assert!(restored.can_back());
        assert!(restored.can_forward());
        let mut restored = restored;
        assert_eq!(restored.forward(), Some(&entry(500, 3)));
        assert_eq!(restored.back(), Some(&entry(10, 2)));
        assert_eq!(restored.back(), Some(&entry(10, 1)));
        assert_eq!(restored.back(), None);
    }

    #[test]
    fn restore_heals_broken_dump() {
        // Пустой снимок — пустая история.
        let restored = NavHistory::restore(HistoryDump {
            entries: Vec::new(),
            cursor: 7,
        });
        assert_eq!(restored.current(), None);
        assert!(!restored.can_back());

        // Курсор за границами — прижимается к последней записи.
        let restored = NavHistory::restore(HistoryDump {
            entries: vec![entry(10, 1), entry(10, 2)],
            cursor: 99,
        });
        assert_eq!(restored.current(), Some(&entry(10, 2)));

        // Снимок длиннее потолка — обрезается как при push.
        let restored = NavHistory::restore(HistoryDump {
            entries: (0..(HISTORY_CAP as u16 + 50))
                .map(|i| entry(10, i))
                .collect(),
            cursor: HISTORY_CAP + 49,
        });
        assert_eq!(
            restored.current(),
            Some(&entry(10, HISTORY_CAP as u16 + 49))
        );
        let mut restored = restored;
        let mut steps = 0;
        while restored.back().is_some() {
            steps += 1;
        }
        assert_eq!(steps, HISTORY_CAP - 1);
    }

    // --- Палитра «Перейти к стиху» --------------------------------------

    use std::path::Path;
    use std::sync::Arc;

    use mybible::{ModuleRegistry, SharedRegistry};
    use parking_lot::Mutex;

    /// Перевод-фикстура: одна книга (500) с главами 1..=3.
    fn write_bible(dir: &Path, name: &str, short: &str, long: &str) {
        let path = dir.join(format!("{name}.sqlite3"));
        let connection = sqlez::connection::Connection::open_file(path.to_str().unwrap());
        connection
            .exec("CREATE TABLE info(name TEXT, value TEXT)")
            .unwrap()()
        .unwrap();
        connection
            .exec("INSERT INTO info VALUES('language', 'ru')")
            .unwrap()()
        .unwrap();
        connection
            .exec(
                "CREATE TABLE verses(\
                   book_number INTEGER, chapter INTEGER, verse INTEGER, text TEXT)",
            )
            .unwrap()()
        .unwrap();
        connection
            .exec(
                "INSERT INTO verses VALUES\
                   (500, 1, 1, 'а'), (500, 2, 1, 'б'), (500, 3, 16, 'в')",
            )
            .unwrap()()
        .unwrap();
        connection
            .exec(
                "CREATE TABLE books(\
                   book_number INTEGER, short_name TEXT, long_name TEXT)",
            )
            .unwrap()()
        .unwrap();
        connection
            .exec(&format!(
                "INSERT INTO books VALUES(500, '{short}', '{long}')"
            ))
            .unwrap()()
        .unwrap();
        drop(connection);
    }

    fn services(dir: &Path) -> crate::services::AppServices {
        crate::services::AppServices {
            settings: parking_lot::RwLock::new(app_state::settings::Settings::default()),
            registry: SharedRegistry::new(ModuleRegistry::scan(dir)),
            current_location: Arc::new(Mutex::new(None)),
            state_db: None,
            user_cross_refs: None,
            annotations: None,
            chains: None,
            tags: None,
            search_index: None,
            index_building: Arc::new(Mutex::new(std::collections::HashSet::new())),
            notes: parking_lot::RwLock::new(None),
            _notes_watcher: Mutex::new(None),
        }
    }

    #[test]
    fn parse_jump_resolves_against_translation() {
        let temp = tempfile::tempdir().unwrap();
        write_bible(temp.path(), "RST", "Ин", "Иоанн");
        let services = services(temp.path());

        let target = parse_jump(&services, "Ин 3:16").unwrap();
        assert_eq!(target.module, ModuleId::from("RST"));
        assert_eq!(target.book, BookNumber(500));
        assert_eq!(target.chapter, 3);
        assert_eq!(target.verse, Some(16));
        assert_eq!(target.book_title, "Иоанн");
        assert_eq!(target.chapter_count, 3);

        // Глава без стиха тоже разбирается.
        let target = parse_jump(&services, "Иоанн 2").unwrap();
        assert_eq!(target.chapter, 2);
        assert_eq!(target.verse, None);

        // Английское имя разбирается через таблицу алиасов.
        let target = parse_jump(&services, "John 3:16").unwrap();
        assert_eq!(target.book, BookNumber(500));

        assert!(parse_jump(&services, "Неттакой 1:1").is_none());
        assert!(parse_jump(&services, "").is_none());
    }

    #[test]
    fn parse_jump_prefers_current_location_module() {
        let temp = tempfile::tempdir().unwrap();
        write_bible(temp.path(), "RST", "Ин", "Иоанн");
        write_bible(temp.path(), "ENU", "John", "John");
        let services = services(temp.path());
        // По умолчанию — первый по алфавиту (ENU).
        assert_eq!(services.default_translation(), Some(ModuleId::from("ENU")));
        // Активное место чтения в RST — приоритетнее.
        *services.current_location.lock() = Some(crate::services::CurrentLocation {
            module: ModuleId::from("RST"),
            book: BookNumber(500),
            book_title: "Иоанн".to_string(),
            book_short: "Ин".to_string(),
            chapter: 1,
            verse: 1,
        });
        let target = parse_jump(&services, "Ин 1:1").unwrap();
        assert_eq!(target.module, ModuleId::from("RST"));
    }

    #[test]
    fn book_suggestions_follow_active_translation() {
        let temp = tempfile::tempdir().unwrap();
        write_bible(temp.path(), "RST", "Ин", "Иоанн");
        write_bible(temp.path(), "ENU", "John", "John");
        let services = services(temp.path());

        // Без активного места чтения подсказки — от перевода по умолчанию
        // (первый по алфавиту — ENU).
        let suggestions = book_suggestions(&services, "joh", 6);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].long_name, "John");

        // С активным местом чтения в RST — русские имена, несмотря на
        // присутствие английского модуля в каталоге.
        *services.current_location.lock() = Some(crate::services::CurrentLocation {
            module: ModuleId::from("RST"),
            book: BookNumber(500),
            book_title: "Иоанн".to_string(),
            book_short: "Ин".to_string(),
            chapter: 1,
            verse: 1,
        });
        let suggestions = book_suggestions(&services, "Ио", 6);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].short_name, "Ин");

        // Пустой запрос — пустой список.
        assert!(book_suggestions(&services, "  ", 6).is_empty());
    }
}
