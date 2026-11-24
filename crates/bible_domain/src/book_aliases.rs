//! Static fallback table mapping multilingual book names to their MyBible
//! `book_number`. Used by `reference::parse` and the navigator picker so that
//! a user can type `John 3:16` even when the only loaded module is Russian.
//!
//! `book_number` follows the MyBible convention: 10 = Genesis, 470 = Matthew,
//! step-of-10 between books. Names are normalised by `normalize::normalize`
//! at lookup time, so this table holds the human form (mixed case, accents
//! preserved) and the matcher takes care of folding.

/// Each entry: `book_number`, slice of recognised names. Names cover English
/// long/short, Russian long/short, common Greek (LXX/NT) form. Add more as
/// users surface gaps — there is no harm in extras.
pub const BOOK_ALIASES: &[(i64, &[&str])] = &[
    // Pentateuch
    (
        10,
        &["Genesis", "Gen", "Gn", "Бытие", "Быт", "Γένεσις", "Γεν"],
    ),
    (
        20,
        &["Exodus", "Exod", "Ex", "Исход", "Исх", "Ἔξοδος", "Εξοδ"],
    ),
    (
        30,
        &["Leviticus", "Lev", "Lv", "Левит", "Лев", "Λευϊτικόν", "Λευ"],
    ),
    (
        40,
        &["Numbers", "Num", "Nm", "Числа", "Чис", "Ἀριθμοί", "Αρ"],
    ),
    (50, &["Deuteronomy", "Deut", "Dt", "Второзаконие", "Втор"]),
    // Historical
    (60, &["Joshua", "Josh", "Jos", "Иисус Навин", "Нав", "ИНав"]),
    (70, &["Judges", "Judg", "Jdg", "Судьи", "Суд"]),
    (80, &["Ruth", "Rth", "Ru", "Руфь", "Руф"]),
    (90, &["1 Samuel", "1 Sam", "1Sm", "1Sa", "1 Царств", "1Цар"]),
    (
        100,
        &["2 Samuel", "2 Sam", "2Sm", "2Sa", "2 Царств", "2Цар"],
    ),
    (110, &["1 Kings", "1 Kgs", "1Ki", "3 Царств", "3Цар"]),
    (120, &["2 Kings", "2 Kgs", "2Ki", "4 Царств", "4Цар"]),
    (
        130,
        &["1 Chronicles", "1 Chron", "1Ch", "1 Паралипоменон", "1Пар"],
    ),
    (
        140,
        &["2 Chronicles", "2 Chron", "2Ch", "2 Паралипоменон", "2Пар"],
    ),
    (150, &["Ezra", "Ezr", "Ездра", "Езд", "1 Ездры", "1Езд"]),
    (160, &["Nehemiah", "Neh", "Неемия", "Неем"]),
    (190, &["Esther", "Est", "Есфирь", "Есф"]),
    // Wisdom
    (220, &["Job", "Иов", "Job"]),
    (
        230,
        &[
            "Psalms",
            "Psalm",
            "Ps",
            "Псалтирь",
            "Псалмы",
            "Пс",
            "Ψαλμοί",
        ],
    ),
    (240, &["Proverbs", "Prov", "Pr", "Притчи", "Притч"]),
    (250, &["Ecclesiastes", "Eccl", "Ec", "Екклесиаст", "Еккл"]),
    (
        260,
        &["Song of Solomon", "Song", "SS", "Песнь Песней", "Песн"],
    ),
    // Major prophets
    (290, &["Isaiah", "Isa", "Is", "Исаия", "Ис", "Ἠσαΐας"]),
    (300, &["Jeremiah", "Jer", "Иеремия", "Иер"]),
    (310, &["Lamentations", "Lam", "Плач Иеремии", "Плач"]),
    (330, &["Ezekiel", "Ezek", "Eze", "Иезекииль", "Иез"]),
    (340, &["Daniel", "Dan", "Даниил", "Дан"]),
    // Minor prophets
    (350, &["Hosea", "Hos", "Осия", "Ос"]),
    (360, &["Joel", "Joel", "Иоиль", "Иоил"]),
    (370, &["Amos", "Am", "Амос", "Ам"]),
    (380, &["Obadiah", "Obad", "Авдий", "Авд"]),
    (390, &["Jonah", "Jon", "Иона", "Ион"]),
    (400, &["Micah", "Mic", "Михей", "Мих"]),
    (410, &["Nahum", "Nah", "Наум", "Наум"]),
    (420, &["Habakkuk", "Hab", "Аввакум", "Авв"]),
    (430, &["Zephaniah", "Zeph", "Софония", "Соф"]),
    (440, &["Haggai", "Hag", "Аггей", "Агг"]),
    (450, &["Zechariah", "Zech", "Захария", "Зах"]),
    (460, &["Malachi", "Mal", "Малахия", "Мал"]),
    // Gospels
    (
        470,
        &["Matthew", "Matt", "Mt", "Матфей", "Мф", "От Матфея", "Μτ"],
    ),
    (480, &["Mark", "Mk", "Mr", "Марк", "Мк", "От Марка", "Μκ"]),
    (490, &["Luke", "Lk", "Lu", "Лука", "Лк", "От Луки", "Λκ"]),
    (
        500,
        &["John", "Jn", "Jhn", "Иоанн", "Ин", "От Иоанна", "Ιω"],
    ),
    // Acts
    (510, &["Acts", "Ac", "Act", "Деяния", "Деян", "Πράξεις"]),
    // Pauline
    (520, &["Romans", "Rom", "Ro", "Римлянам", "Рим"]),
    (
        530,
        &["1 Corinthians", "1 Cor", "1Co", "1 Коринфянам", "1Кор"],
    ),
    (
        540,
        &["2 Corinthians", "2 Cor", "2Co", "2 Коринфянам", "2Кор"],
    ),
    (550, &["Galatians", "Gal", "Галатам", "Гал"]),
    (560, &["Ephesians", "Eph", "Ep", "Ефесянам", "Еф"]),
    (570, &["Philippians", "Phil", "Pp", "Филиппийцам", "Флп"]),
    (580, &["Colossians", "Col", "Колоссянам", "Кол"]),
    (
        590,
        &[
            "1 Thessalonians",
            "1 Thess",
            "1Th",
            "1 Фессалоникийцам",
            "1Фес",
        ],
    ),
    (
        600,
        &[
            "2 Thessalonians",
            "2 Thess",
            "2Th",
            "2 Фессалоникийцам",
            "2Фес",
        ],
    ),
    (610, &["1 Timothy", "1 Tim", "1Ti", "1 Тимофею", "1Тим"]),
    (620, &["2 Timothy", "2 Tim", "2Ti", "2 Тимофею", "2Тим"]),
    (630, &["Titus", "Tit", "Титу", "Тит"]),
    (640, &["Philemon", "Phlm", "Phm", "Филимону", "Флм"]),
    (650, &["Hebrews", "Heb", "Евреям", "Евр"]),
    // General epistles
    (660, &["James", "Jas", "Jm", "Иаков", "Иак"]),
    (670, &["1 Peter", "1 Pet", "1Pe", "1 Петра", "1Пет"]),
    (680, &["2 Peter", "2 Pet", "2Pe", "2 Петра", "2Пет"]),
    (690, &["1 John", "1 Jn", "1Jo", "1 Иоанна", "1Ин"]),
    (700, &["2 John", "2 Jn", "2Jo", "2 Иоанна", "2Ин"]),
    (710, &["3 John", "3 Jn", "3Jo", "3 Иоанна", "3Ин"]),
    (720, &["Jude", "Jud", "Иуды", "Иуд"]),
    (
        730,
        &[
            "Revelation",
            "Rev",
            "Apoc",
            "Откровение",
            "Откр",
            "Апокалипсис",
        ],
    ),
];

/// Pre-normalised alias index, built once on first use. The legacy
/// implementation re-normalised every alias on every lookup — for ~280
/// aliases that is ~560 string allocations per call. Hashing the normalised
/// names once amortises the cost across all subsequent lookups.
fn alias_index() -> &'static std::collections::HashMap<String, i64> {
    static INDEX: std::sync::OnceLock<std::collections::HashMap<String, i64>> =
        std::sync::OnceLock::new();
    INDEX.get_or_init(|| {
        let mut map: std::collections::HashMap<String, i64> =
            std::collections::HashMap::with_capacity(BOOK_ALIASES.len() * 6);
        for (book_number, names) in BOOK_ALIASES {
            for candidate in *names {
                let key = crate::normalize::normalize(candidate);
                map.entry(key).or_insert(*book_number);
            }
        }
        map
    })
}

/// Look up a book name against the static alias table after diacritic-
/// insensitive normalization. Returns `None` for unrecognised input.
pub fn lookup(name: &str) -> Option<i64> {
    let needle = crate::normalize::normalize(name.trim());
    if needle.is_empty() {
        return None;
    }
    alias_index().get(&needle).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_long_names() {
        assert_eq!(lookup("John"), Some(500));
        assert_eq!(lookup("Genesis"), Some(10));
        assert_eq!(lookup("1 Corinthians"), Some(530));
    }

    #[test]
    fn russian_short_names() {
        assert_eq!(lookup("Ин"), Some(500));
        assert_eq!(lookup("Быт"), Some(10));
        assert_eq!(lookup("1Кор"), Some(530));
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(lookup("JOHN"), Some(500));
        assert_eq!(lookup("john"), Some(500));
    }

    #[test]
    fn diacritic_insensitive() {
        // Normalisation removes Greek accents so `Μτ` matches even with mark.
        assert_eq!(lookup("Μτ"), Some(470));
    }

    #[test]
    fn unknown_returns_none() {
        assert_eq!(lookup("Foo"), None);
        assert_eq!(lookup(""), None);
    }
}
