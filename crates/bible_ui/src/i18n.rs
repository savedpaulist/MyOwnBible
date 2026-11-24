//! Small, dependency-free UI localization layer.
//!
//! Russian source strings are stable message identifiers. This keeps the
//! presentation crate independent from platform locale APIs and lets every
//! control switch language immediately after `settings.json` is updated.

use gpui_kit::{App, SharedString};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::services::app_services;

static ENGLISH: AtomicBool = AtomicBool::new(false);

pub fn sync(cx: &App) {
    ENGLISH.store(is_english(cx), Ordering::Relaxed);
}

pub fn tr_static(russian: &'static str) -> &'static str {
    if ENGLISH.load(Ordering::Relaxed) {
        english(russian)
    } else {
        russian
    }
}

pub fn is_english(cx: &App) -> bool {
    app_services(cx).settings.read().interface_language
        == shell::settings::InterfaceLanguage::English
}

pub fn tr(cx: &App, russian: &'static str) -> SharedString {
    if !is_english(cx) {
        return russian.into();
    }
    english(russian).into()
}

pub fn text(cx: &App, russian: &str) -> String {
    if !is_english(cx) {
        return russian.to_owned();
    }
    english(russian).to_owned()
}

fn english(russian: &str) -> &str {
    match russian {
        "Настройки" => "Settings",
        "Общие" => "General",
        "Язык интерфейса" => "Interface language",
        "Русский" => "Russian",
        "Английский" => "English",
        "Язык" => "Language",
        "Папки приложения" => "Application folders",
        "Поисковый индекс" => "Search index",
        "Реиндексировать модули" => "Reindex modules",
        "Полностью пересоздаёт индексы переводов, словарей и комментариев." => {
            "Fully rebuilds translation, dictionary, and commentary indexes."
        }
        "Подсказки" => "Help hints",
        "Показывать подсказки функций" => "Show feature hints",
        "Скрывает знаки вопроса и развёрнутые подсказки интерфейса." => {
            "Hides question marks and detailed interface hints."
        }
        "Файл настроек" => "Settings file",
        "По умолчанию" => "Defaults",
        "Авто — первый доступный перевод" => {
            "Auto — first available translation"
        }
        "Текущий модуль" => "Current module",
        "Компактный" => "Compact",
        "Обычный" => "Normal",
        "Крупный" => "Large",
        "Автоматически" => "Automatic",
        "Плотно" => "Dense",
        "Компактно" => "Compact",
        "Обычно" => "Normal",
        "Свободно" => "Relaxed",
        "Очень свободно" => "Very relaxed",
        "Выкл" => "Off",
        "По частям речи" => "By parts of speech",
        "В текущей вкладке" => "In current tab",
        "В новой вкладке" => "In new tab",
        "Внешний вид" => "Appearance",
        "Оформление окна" => "Window appearance",
        "Шрифты" => "Fonts",
        "Системные шрифты" => "System fonts",
        "Интерфейс приложения" => "Application interface",
        "Текст Библии" => "Bible text",
        "Чтение" => "Reading",
        "Копирование" => "Copying",
        "Ссылки на источник" => "Source references",
        "Библия: вставлять ссылку" => "Bible: include reference",
        "Словари: вставлять ссылку на модуль" => {
            "Dictionaries: include module reference"
        }
        "Комментарии: вставлять ссылку на модуль" => {
            "Commentaries: include module reference"
        }
        "Положение ссылки" => "Reference position",
        "В начале" => "At the beginning",
        "В конце" => "At the end",
        "Просмотр" => "Preview",
        "Правка" => "Edit",
        "сохранено" => "saved",
        "не сохранено…" => "unsaved…",
        "Выбор книги" => "Choose book",
        "Выбор главы" => "Choose chapter",
        "Выбор стиха" => "Choose verse",
        "Содержание текста" => "Text content",
        "Переходы" => "Navigation",
        "Библейские ссылки" => "Bible links",
        "Перевод для новых вкладок" => "Translation for new tabs",
        "Цветовая тема" => "Color theme",
        "Масштаб интерфейса" => "Interface scale",
        "Размер вкладок" => "Tab size",
        "Размер иконок" => "Icon size",
        "Ширина текста Библии" => "Bible text width",
        "Интервал между стихами" => "Verse spacing",
        "Размер шрифта интерфейса" => "Interface font size",
        "Размер текста Библии" => "Bible text size",
        "Левое меню" => "Left menu",
        "Размер текста правой панели" => "Right-panel text size",
        "Дополнительный масштаб панелей" => "Additional panel scale",
        "Подсветка морфологии" => "Morphology highlighting",
        "Открывать места Писания" => "Open Scripture references",
        "Библиотека" => "Library",
        "Статья" => "Article",
        "Текст" => "Text",
        "Поиск…" => "Searching…",
        "Статьи не найдены." => "No articles found.",
        "Поиск по словарям" => "Search dictionaries",
        "Искать также в тексте статьи" => "Also search article text",
        "Перейти к стиху" => "Go to verse",
        "Формат: Книга глава[:стих] — например, «Ин 3:16»" => {
            "Format: Book chapter[:verse] — for example, John 3:16"
        }
        "Книга не найдена — проверьте написание" => {
            "Book not found — check the spelling"
        }
        "Перейти к стиху…" => "Go to verse…",
        "Новая вкладка чтения" => "New reading tab",
        "Назад по истории" => "Back in history",
        "Вперёд по истории" => "Forward in history",
        "Настройки…" => "Settings…",
        "Левая панель" => "Left panel",
        "Нижняя панель" => "Bottom panel",
        "Команда…" => "Command…",
        "Ничего не найдено" => "Nothing found",
        "Вся Библия" => "Entire Bible",
        "возлюбил* мир, \"благодать и истина\", G26…" => {
            "loved* world, \"grace and truth\", G26…"
        }
        "Ветхий Завет" => "Old Testament",
        "Новый Завет" => "New Testament",
        "Выбрать все" => "Select all",
        "Снять все" => "Clear all",
        "Выполняется поиск по модулям…" => "Searching modules…",
        "Первый поиск может занять больше времени: приложение подготавливает поисковые индексы." => {
            "The first search may take longer while the application prepares search indexes."
        }
        "Введите слово, фразу в кавычках или Strong’s-номер. Поиск охватывает переводы, словари и комментарии." => {
            "Enter a word, a quoted phrase, or a Strong's number. Search covers Bibles, dictionaries, and commentaries."
        }
        "Обновляю результаты…" => "Updating results…",
        "Совпадений не найдено." => "No matches found.",
        "Найти" => "Search",
        "Источники" => "Sources",
        "Сбросить" => "Reset",
        "Синхронизация" => "Synchronization",
        "Перенос данных" => "Data transfer",
        "Экспортировать…" => "Export…",
        "Импортировать…" => "Import…",
        "Google Drive подключён" => "Google Drive connected",
        "Выполняется…" => "Working…",
        "Google Drive отключён" => "Google Drive disconnected",
        "Подключить Google Drive" => "Connect Google Drive",
        "OAuth-клиент…" => "OAuth client…",
        "Синхронизировать сейчас" => "Sync now",
        "Переподключить…" => "Reconnect…",
        "Выйти из Google-аккаунта" => "Sign out of Google account",
        "Выберите JSON-файл OAuth-клиента Google типа Desktop app." => {
            "Choose a Desktop app Google OAuth client JSON file."
        }
        "Конфликты синхронизации" => "Sync conflicts",
        "Выбрать эту версию" => "Choose this version",
        "Оформление стиха или слова" => "Verse or word styling",
        "Цепочка" => "Chain",
        "Тег" => "Tag",
        "Вопрос" => "Question",
        "Параллельное место" => "Cross-reference",
        "Удаление записи" => "Deleted record",
        "← Книги" => "← Books",
        "← Назад" => "← Back",
        "+ До" => "+ Before",
        "+ Добавить ссылку на стих" => "+ Add verse link",
        "+ Новый вопрос" => "+ New question",
        "+ После" => "+ After",
        "+ Создать" => "+ Create",
        "+ Узел" => "+ Node",
        "⩒ слияние" => "⩒ merge",
        "⩓ ветвь" => "⩓ branch",
        "1:1 · Образец текста Писания" => "1:1 · Scripture text sample",
        "Бытие · Глава 1" => "Genesis · Chapter 1",
        "Введите вопрос" => "Enter a question",
        "Введите ответ" => "Enter an answer",
        "Вопросов пока нет." => "No questions yet.",
        "Вставить" => "Insert",
        "Вставить узел в разрыв этой связи" => {
            "Insert a node into this connection"
        }
        "Выберите заметку слева или создайте новую." => {
            "Select a note on the left or create one."
        }
        "Готово" => "Done",
        "Далее" => "Next",
        "Добавить" => "Add",
        "Добавить предпосылку перед узлом" => {
            "Add a premise before the node"
        }
        "Добавить следствие после узла" => {
            "Add a consequence after the node"
        }
        "Загрузка…" => "Loading…",
        "Заметки из выбранной папки и записи для изучения Библии синхронизируются напрямую с вашим Google Drive." => {
            "Notes and Bible study records sync directly with your Google Drive."
        }
        "Заметки отключены: укажите папку заметок в настройках." => {
            "Notes are disabled: choose a notes folder in Settings."
        }
        "Заметки отключены: укажите notes_dir в settings.json." => {
            "Notes are disabled: set notes_dir in settings.json."
        }
        "Заметок пока нет — создайте первую в поле ниже." => {
            "No notes yet — create the first one below."
        }
        "Заметок пока нет — создайте первую." => {
            "No notes yet — create the first one."
        }
        "Изменить" => "Edit",
        "Имя тега…" => "Tag name…",
        "Ин 3:16, Быт 1:1, John 3…" => "John 3:16, Gen 1:1…",
        "Комментарий к стиху" => "Verse note",
        "Модуль Библии для предпросмотра" => "Bible module for preview",
        "Мой комментарий к стиху…" => "My verse note…",
        "Нажмите на заголовок выше, чтобы выбрать книгу." => {
            "Click the heading above to choose a book."
        }
        "Название новой цепочки…" => "New chain title…",
        "Название цепочки…" => "Chain title…",
        "Обратные ссылки" => "Backlinks",
        "Обратных ссылок нет." => "No backlinks.",
        "Ок" => "OK",
        "Оставить мои" => "Keep mine",
        "Ответ получен" => "Answered",
        "Отмена" => "Cancel",
        "Панель справа" => "Right panel",
        "Папка/Название новой заметки" => "Folder/New note title",
        "Перезагрузить" => "Reload",
        "Поиск по всем переводам Библии" => {
            "Search all Bible translations"
        }
        "Предыдущая глава" => "Previous chapter",
        "Проверка шрифта на греческом, английском, русском и иврите в контексте выбранного профиля. Недостающие глифы могут отображаться системным fallback-шрифтом." => {
            "Preview Greek, English, Russian, and Hebrew in the selected font profile. Missing glyphs may use a system fallback font."
        }
        "Пустая цепочка. Добавьте первый узел — ссылку на стих и утверждение." => {
            "This chain is empty. Add the first node with a verse link and statement."
        }
        "Редактировать" => "Edit",
        "С этим тегом пока нет стихов." => "No verses have this tag yet.",
        "Следующая глава" => "Next chapter",
        "Словарь · Статья «Логос»" => "Dictionary · “Logos” entry",
        "Ссылка" => "Link",
        "Ссылки через запятую — как в палитре перехода (Ин 3:16)." => {
            "Comma-separated references, as in the passage palette (John 3:16)."
        }
        "Ссылки: Ин 3:16, Рим 5:8…" => "References: John 3:16, Rom 5:8…",
        "Тегов пока нет — поставьте первый из меню стиха («Теги стиха…»)." => {
            "No tags yet — add the first one from the verse menu."
        }
        "Убрать ссылку" => "Remove link",
        "Установите хотя бы один перевод Библии." => {
            "Install at least one Bible translation."
        }
        "Утверждение…" => "Statement…",
        "Файл изменён на диске (внешняя правка)." => {
            "The file was modified externally."
        }
        "Фильтр или новая цепочка…" => "Filter or new chain…",
        "Фильтр или новый тег…" => "Filter or new tag…",
        "Фильтр цепочек…" => "Filter chains…",
        "Экспорт создаёт папку с заметками Markdown и файлом bibleown.json. Для экспорта и импорта Google Drive не требуется." => {
            "Export creates a folder with Markdown notes and bibleown.json. Google Drive is not required for export or import."
        }
        "Ширина колонки чтения" => "Reading column width",
        "Межстрочный интервал" => "Line spacing",
        "Семейство шрифта" => "Font family",
        "Размер шрифта" => "Font size",
        "Цветовая схема морфологии" => "Morphology color scheme",
        "Критический аппарат" => "Critical apparatus",
        "Номера стихов" => "Verse numbers",
        "Открывать ссылки" => "Open links",
        "Обзор…" => "Browse…",
        "Выбрать папку в проводнике" => "Choose a folder",
        "Применить" => "Apply",
        "Обновить список" => "Refresh list",
        "Не выбрано — заметки отключены" => {
            "Not selected — notes are disabled"
        }
        "Папка с базами модулей MyBible (*.sqlite3). Выберите её через проводник или вставьте путь вручную." => {
            "Folder containing MyBible module databases (*.sqlite3). Choose it or paste the path manually."
        }
        "Папка Markdown-хранилища (например, Obsidian). Пустое значение отключает заметки." => {
            "Markdown vault folder (for example, Obsidian). An empty value disables notes."
        }
        "Изменение языка применяется сразу во всех окнах приложения." => {
            "The language change is applied immediately in every application window."
        }
        "Перевод, который будет выбран при открытии нового окна чтения." => {
            "Translation selected when a new reading tab is opened."
        }
        "Увеличивает или уменьшает элементы и отступы во всех окнах." => {
            "Scales controls and spacing in every window."
        }
        "Ограничивает ширину текстовой колонки. Полезно для чтения на широких мониторах." => {
            "Limits the text column width for comfortable reading on wide displays."
        }
        "Регулирует вертикальное расстояние между строками стихов." => {
            "Adjusts vertical spacing between verse lines."
        }
        "Основное семейство для меню, кнопок, заголовков и всех рабочих окон. Список берётся из шрифтов, доступных GPUI в системе." => {
            "Main font for menus, buttons, headings, and application windows."
        }
        "Базовый кегль интерфейса. Масштаб окна применяется дополнительно." => {
            "Base interface font size; window scale is applied separately."
        }
        "Независимый шрифт текста Писания. Значение «По умолчанию» наследует шрифт интерфейса." => {
            "Independent Scripture font. Default inherits the interface font."
        }
        "Размер основного текста иврита, греческого и переводов Библии." => {
            "Main font size for Hebrew, Greek, and Bible translations."
        }
        "Шрифт правой панели и областей с общим профилем." => {
            "Font used by the right panel and related areas."
        }
        "Базовый кегль боковых панелей. Относительные подписи сохраняют визуальную иерархию." => {
            "Base side-panel font size; relative labels retain their hierarchy."
        }
        "Тонкая подстройка поверх базового размера шрифта." => {
            "Fine adjustment on top of the base font size."
        }
        "Цвета можно переопределить в разделе morphology_colors файла settings.json." => {
            "Colors can be overridden in the morphology_colors section of settings.json."
        }
        "Показывать текстологические пометки ({…}, 「…」, <n>…) в переводе." => {
            "Show textual-critical marks ({…}, 「…」, <n>…) in translations."
        }
        "Показывать номера стихов в отдельной колонке." => {
            "Show verse numbers in a separate column."
        }
        "Куда открываются перекрёстные ссылки, результаты поиска и переходы из статей." => {
            "Where cross-references, search results, and article links open."
        }
        "Назад (Ctrl+Alt+←)" => "Back (Ctrl+Alt+←)",
        "Вперёд (Ctrl+Alt+→)" => "Forward (Ctrl+Alt+→)",
        "Перейти к месту (Ctrl+P)" => "Go to passage (Ctrl+P)",
        "Поиск" => "Search",
        "Словари" => "Dictionaries",
        "Перекрёстные ссылки к стиху" => "Verse cross-references",
        "Мой комментарий к стиху" => "My verse note",
        "Теги стиха" => "Verse tags",
        "Добавить стих в цепочку" => "Add verse to chain",
        "Правая панель" => "Right panel",
        "СТИХ" => "VERSE",
        "Слово" => "Word",
        "Стих" => "Verse",
        "Тег стиха…" => "Verse tag…",
        "Добавить в цепочку…" => "Add to chain…",
        "Копировать слово" => "Copy word",
        "Копировать стих" => "Copy verse",
        "Подробнее — панель «Слово»" => "More details — Word panel",
        "Изменить теги…" => "Edit tags…",
        "Фон" => "Background",
        "Цвет шрифта" => "Text color",
        "Подчёркивание" => "Underline",
        "Снять" => "Clear",
        "Снять всё" => "Clear all",
        "Морфология" => "Morphology",
        "Открыть в словаре" => "Open in dictionary",
        "Искать слово" => "Search word",
        "Найти это слово в переводах" => "Find this word in translations",
        "Найти номер Strong's в переводах" => {
            "Find this Strong's number in translations"
        }
        "Словарь Strong's:" => "Strong's dictionary:",
        "Открыть перевод" => "Open translation",
        "+ Добавить параллельное место" => "+ Add cross-reference",
        "Мои параллельные места" => "My cross-references",
        "Удалить параллельное место" => "Remove cross-reference",
        "Комментарий привязан к активному переводу стиха." => {
            "The note is linked to the active verse translation."
        }
        "Сохранить" => "Save",
        "Пустой текст удаляет комментарий." => {
            "Empty text deletes the note."
        }
        "Другие переводы" => "Other translations",
        "Параллельные ссылки" => "Cross-references",
        "Комментарии" => "Commentaries",
        "Модули комментариев не найдены." => {
            "No commentary modules found."
        }
        "Заметки" => "Notes",
        "Теги" => "Tags",
        "Добавить тег" => "Add tag",
        "Имя нового тега…" => "New tag name…",
        "Новый тег" => "New tag",
        "Создать новый тег" => "Create new tag",
        "Создать новую заметку" => "Create new note",
        "Создать новую цепочку" => "Create new chain",
        "Новая цепочка" => "New chain",
        "Комментариев пока нет." => "There are no comments yet.",
        "Цепочки" => "Chains",
        "Вопросы" => "Questions",
        "Кликните по слову в тексте чтения" => {
            "Click a word in the reading text."
        }
        "Кликните по стиху в тексте чтения." => {
            "Click a verse in the reading text."
        }
        "Другие переводы не найдены." => "No other translations found.",
        "Нет комментария к этому стиху." => {
            "No commentary for this verse."
        }
        "Нет параллельных мест для этого стиха." => {
            "No cross-references for this verse."
        }
        "Расшифровка недоступна." => "Explanation is unavailable.",
        "Статья не найдена в Strong's-словаре." => {
            "No entry found in the Strong's dictionary."
        }
        "Для этого слова нет данных Strong's и морфологии." => {
            "No Strong's or morphology data is available for this word."
        }
        "Развернуть меню" => "Expand menu",
        "Свернуть меню" => "Collapse menu",
        "Показать или скрыть содержимое" => "Show or hide content",
        "Новая вкладка" => "New tab",
        "Библия" => "Bible",
        "Точное" => "Exact",
        "Частичное" => "Partial",
        "Похожее" => "Similar",
        "Учитывать диакритику/огласовки" => {
            "Respect diacritics/vowel marks"
        }
        "Источники и книги поиска" => "Search sources and books",
        "Выбранные книги Библии" => "Selected Bible books",
        "Все" => "All",
        "Тора" => "Torah",
        "Исторические книги" => "Historical books",
        "Поэтические книги" => "Poetic books",
        "Большие пророки" => "Major prophets",
        "Малые пророки" => "Minor prophets",
        "Евангелия и Деяния" => "Gospels and Acts",
        "Послания Павла" => "Pauline epistles",
        "Соборные послания" => "General epistles",
        "Откровение" => "Revelation",
        "Словарь" => "Dictionary",
        "Заметка" => "Note",
        "Закрыть вкладку" => "Close tab",
        "Действия вкладки" => "Tab actions",
        "Восстановить размер" => "Restore size",
        "Развернуть вкладку" => "Maximize tab",
        _ => russian,
    }
}

#[cfg(test)]
mod tests {
    use super::english;

    #[test]
    fn known_and_unknown_messages_are_stable() {
        assert_eq!(english("Настройки"), "Settings");
        assert_eq!(english("module-id"), "module-id");
    }
}
