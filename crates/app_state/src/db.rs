//! Собственная БД состояния app_state.db (architecture.md §4.2.2).
//!
//! Своя схема, свои миграции (meta.schema_version), ничего чужого.
//! Миграции применяются в транзакции, откат не поддерживаем — при
//! несовместимом изменении будет новая миграция с переносом данных.
//!
//! Отличие от §4.2.2: первичные ключи таблиц аннотаций — натуральные
//! (module, book, chapter, verse, word_index, kind), а не uuid: upsert
//! по натуральному ключу проще и дубли исключены на уровне схемы.
//! Пометка внесена в architecture.md.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use parking_lot::Mutex;
use sqlez::connection::Connection;

use crate::paths;

/// Актуальная версия схемы. Поднимаем при изменении DDL; миграции —
/// пронумерованные блоки в `SCHEMA`.
const SCHEMA_VERSION: i64 = 6;

/// Миграция v1: аннотации и комментарии к стихам (§4.2.2).
const MIGRATION_V1: &str = "
CREATE TABLE annotations(
    module_id TEXT NOT NULL,
    book INTEGER NOT NULL,
    chapter INTEGER NOT NULL,
    verse INTEGER NOT NULL,
    word_index INTEGER NOT NULL DEFAULT -1,  -- -1 = весь стих
    kind TEXT NOT NULL,                      -- 'highlight' | 'underline' | 'word_color'
    color TEXT NOT NULL,                     -- имя из палитры bible_domain::annotations::PALETTE
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(module_id, book, chapter, verse, word_index, kind)
) STRICT;
CREATE INDEX idx_annotations_ref ON annotations(book, chapter);

CREATE TABLE verse_comments(
    module_id TEXT NOT NULL,
    book INTEGER NOT NULL,
    chapter INTEGER NOT NULL,
    verse INTEGER NOT NULL,
    word_index INTEGER NOT NULL DEFAULT -1,
    note_relpath TEXT,
    body TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(module_id, book, chapter, verse, word_index)
) STRICT;
CREATE INDEX idx_verse_comments_ref ON verse_comments(book, chapter);
";

/// Миграция v2: логические цепочки (§6.8, перенос схемы форка; без
/// inter_edges/activity/FTS — они придут со следующими заходами).
/// Ссылки узла — JSON-массив компактных строк `bible_domain::chains`.
const MIGRATION_V2: &str = "
CREATE TABLE chains(
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE chain_nodes(
    id TEXT PRIMARY KEY,
    chain_id TEXT NOT NULL REFERENCES chains(id) ON DELETE CASCADE,
    refs_json TEXT NOT NULL DEFAULT '[]',
    assertion TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    sort_key INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX idx_chain_nodes_chain ON chain_nodes(chain_id);

CREATE TABLE chain_edges(
    chain_id TEXT NOT NULL REFERENCES chains(id) ON DELETE CASCADE,
    source_node_id TEXT NOT NULL REFERENCES chain_nodes(id) ON DELETE CASCADE,
    target_node_id TEXT NOT NULL REFERENCES chain_nodes(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(source_node_id, target_node_id)
) STRICT;
CREATE INDEX idx_chain_edges_chain ON chain_edges(chain_id);
CREATE INDEX idx_chain_edges_source ON chain_edges(source_node_id);
CREATE INDEX idx_chain_edges_target ON chain_edges(target_node_id);
";

/// Миграция v3: теги стихов (этап 21 polish-plan.md, перенос
/// tags/verse_tags форка). Привязка модуль-независимая — как у цепочек;
/// цвет — имя из палитры bible_domain::annotations::PALETTE.
const MIGRATION_V3: &str = "
CREATE TABLE tags(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    color TEXT NOT NULL
) STRICT;

CREATE TABLE verse_tags(
    book INTEGER NOT NULL,
    chapter INTEGER NOT NULL,
    verse INTEGER NOT NULL,
    tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(book, chapter, verse, tag_id)
) STRICT;
CREATE INDEX idx_verse_tags_ref ON verse_tags(book, chapter);
CREATE INDEX idx_verse_tags_tag ON verse_tags(tag_id);
";

const MIGRATION_V4: &str = "
CREATE TABLE study_questions(
    id TEXT PRIMARY KEY,
    question TEXT NOT NULL,
    answer TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    answered_at INTEGER
) STRICT;
CREATE TABLE study_question_verses(
    question_id TEXT NOT NULL REFERENCES study_questions(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    module_id TEXT NOT NULL,
    book INTEGER NOT NULL,
    chapter INTEGER NOT NULL,
    verse INTEGER NOT NULL,
    comment TEXT NOT NULL DEFAULT '',
    PRIMARY KEY(question_id, position)
) STRICT;
";

const MIGRATION_V5: &str = "
ALTER TABLE chains ADD COLUMN bible_module TEXT;
ALTER TABLE chain_nodes ADD COLUMN bible_module TEXT;
";

/// Multiple personal notes may share one verse. The original address primary
/// key made each Save overwrite the previous note, so v6 gives every note an
/// independent portable id while preserving existing data.
const MIGRATION_V6: &str = "
ALTER TABLE verse_comments RENAME TO verse_comments_legacy;
DROP INDEX idx_verse_comments_ref;
CREATE TABLE verse_comments(
    id TEXT PRIMARY KEY,
    module_id TEXT NOT NULL,
    book INTEGER NOT NULL,
    chapter INTEGER NOT NULL,
    verse INTEGER NOT NULL,
    word_index INTEGER NOT NULL DEFAULT -1,
    note_relpath TEXT,
    body TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
CREATE INDEX idx_verse_comments_ref ON verse_comments(book, chapter, verse);
CREATE INDEX idx_verse_comments_anchor ON verse_comments(
    module_id, book, chapter, verse, word_index, created_at
);
INSERT INTO verse_comments(
    id, module_id, book, chapter, verse, word_index, note_relpath, body,
    created_at, updated_at
)
SELECT 'legacy-' || hex(randomblob(16)), module_id, book, chapter, verse,
       word_index, note_relpath, body, created_at, updated_at
FROM verse_comments_legacy;
DROP TABLE verse_comments_legacy;
";

/// Открытая БД состояния за мьютексом (sqlez Connection — !Send).
pub struct StateDb {
    inner: Mutex<Connection>,
}

impl StateDb {
    /// Открыть (создать) БД в каталоге данных приложения и применить миграции.
    pub fn open_default() -> Result<Arc<Self>> {
        Self::open_at(&paths::state_db_file())
    }

    pub fn open_at(path: &Path) -> Result<Arc<Self>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating state dir {}", parent.display()))?;
        }
        let path_str = path
            .to_str()
            .context("app_state.db path is not valid UTF-8")?;
        let connection = Connection::open_file(path_str);
        anyhow::ensure!(
            connection.persistent(),
            "Не удалось открыть {}",
            path.display()
        );
        Self::init(&connection).with_context(|| format!("migrating {}", path.display()))?;
        Ok(Arc::new(Self {
            inner: Mutex::new(connection),
        }))
    }

    /// Доступ к соединению под мьютексом.
    pub fn with<R>(&self, f: impl FnOnce(&Connection) -> Result<R>) -> Result<R> {
        let connection = self.inner.lock();
        f(&connection)
    }

    fn init(connection: &Connection) -> Result<()> {
        // CASCADE на chain_nodes/chain_edges работает только со включёнными
        // внешними ключами; pragma живёт на соединении, поэтому включаем
        // при каждом открытии.
        connection.exec("PRAGMA foreign_keys = ON")?()?;
        connection.exec(
            "CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT",
        )?()?;
        let version: Option<String> =
            connection.select_row("SELECT value FROM meta WHERE key = 'schema_version'")?()?;
        let version: i64 = version.as_deref().unwrap_or("0").parse().unwrap_or(0);
        if version > SCHEMA_VERSION {
            bail!(
                "app_state.db schema_version {version} новее поддерживаемой {SCHEMA_VERSION} — \
                 обновите приложение"
            );
        }
        connection.with_savepoint("migrate_state", || {
            for (migration_version, sql) in [
                (1, MIGRATION_V1),
                (2, MIGRATION_V2),
                (3, MIGRATION_V3),
                (4, MIGRATION_V4),
                (5, MIGRATION_V5),
                (6, MIGRATION_V6),
            ] {
                if version < migration_version {
                    for statement in sql.split(';').map(str::trim).filter(|sql| !sql.is_empty()) {
                        connection.exec(statement)?()?;
                    }
                }
            }
            connection.exec_bound(
                "INSERT INTO meta(key, value) VALUES ('schema_version', ?) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            )?(SCHEMA_VERSION.to_string())?;
            Ok(())
        })
    }

    /// In-memory БД для тестов (своих и соседних крейтов).
    pub fn in_memory() -> Result<Arc<Self>> {
        let connection = Connection::open_memory(None);
        Self::init(&connection)?;
        Ok(Arc::new(Self {
            inner: Mutex::new(connection),
        }))
    }
}
