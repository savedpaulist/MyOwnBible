//! SQLite-backed link and metadata index for the notes vault.
//!
//! Lives at `<notes_dir>/notes.db`, separate from `annotations.db` so vault
//! migrations and re-indexes do not block the verse-comment FTS table.
//!
//! The store is the single writer for all index data: notes, aliases,
//! headings, blocks, links, tags, and a parallel FTS5 index for full-text
//! search. The orchestrator in `index.rs` calls `replace_note_record` after
//! parsing each file. Read-only queries (backlinks, resolution, switcher)
//! call dedicated `*_for_*` helpers — they never construct SQL outside this
//! module.
//!
//! Polymorphic `link.src_*` columns model four source kinds: a note in the
//! vault, a verse comment in `annotations.db`, a MyBible commentary entry,
//! or a MyBible dictionary article. Only `src_kind='note'` participates in
//! cascade deletion when a note file vanishes — the other source kinds are
//! refreshed by `cross_domain.rs` directly.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use parking_lot::Mutex;
use sqlez::connection::Connection;

/// Bump this when migrations change. The store always runs every
/// `CREATE … IF NOT EXISTS` statement — `user_version` is recorded so future
/// schema-rewrite migrations know whether they need to run.
pub const SCHEMA_VERSION: u32 = 1;

/// Open or create the notes database at `<notes_dir>/notes.db`.
///
/// Creates `notes_dir` if needed (mirrors `annotations::open`'s behaviour so
/// the two stores can be opened in any order). Idempotent.
pub fn open(notes_dir: &Path) -> Result<Arc<NotesStore>> {
    let path = notes_dir.join("notes.db");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating notes dir {parent:?}"))?;
    }
    let path_str = path.to_str().context("notes.db path is not valid UTF-8")?;
    let connection = Connection::open_file(path_str);
    init_schema(&connection).with_context(|| format!("initialising notes schema at {path:?}"))?;
    Ok(Arc::new(NotesStore {
        path,
        inner: Mutex::new(connection),
    }))
}

/// Open an in-memory database for tests. Each call gets a private database
/// — using a shared-cache URI causes lock contention between concurrent
/// test threads.
#[cfg(test)]
pub fn open_in_memory() -> Result<Arc<NotesStore>> {
    let connection = Connection::open_memory(None);
    init_schema(&connection)?;
    Ok(Arc::new(NotesStore {
        path: PathBuf::from(":memory:"),
        inner: Mutex::new(connection),
    }))
}

pub struct NotesStore {
    path: PathBuf,
    inner: Mutex<Connection>,
}

pub type NoteId = i64;

#[derive(Debug, Clone)]
pub struct NoteRow {
    pub id: NoteId,
    pub relpath: String,
    pub title: String,
    pub mtime: i64,
    pub blake3: String,
    pub body_chars: i64,
    pub indexed_at: i64,
}

#[derive(Debug, Clone)]
pub struct NoteSignature {
    pub mtime: i64,
    pub blake3: String,
}

#[derive(Debug, Clone)]
pub struct NoteSearchHit {
    pub id: NoteId,
    pub relpath: String,
    pub title: String,
    pub snippet: String,
}

#[derive(Debug, Clone)]
pub struct HeadingRow {
    pub slug: String,
    pub text: String,
    pub level: i64,
    pub byte_offset: i64,
}

#[derive(Debug, Clone)]
pub struct BlockRow {
    pub block_id: String,
    pub byte_offset: i64,
    pub text: String,
}

/// What kind of object the link originates from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkSource {
    Note,
    VerseComment,
    MyBibleCommentary,
    MyBibleDictionary,
}

impl LinkSource {
    pub fn as_token(&self) -> &'static str {
        match self {
            LinkSource::Note => "note",
            LinkSource::VerseComment => "verse_comment",
            LinkSource::MyBibleCommentary => "mybible_commentary",
            LinkSource::MyBibleDictionary => "mybible_dict",
        }
    }
}

/// What kind of object the link points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkTargetKind {
    Note,
    Bible,
    Dict,
    Commentary,
    Xref,
    CommentAtVerse,
    /// Link to a logical chain (`[[chain:Title]]`) resolved at click time
    /// against `annotations.db`. We don't index a separate target_id column
    /// for these — the title-in-`target_raw` is enough for `bible.rs` to
    /// dispatch.
    Chain,
    Broken,
}

impl LinkTargetKind {
    pub fn as_token(&self) -> &'static str {
        match self {
            LinkTargetKind::Note => "note",
            LinkTargetKind::Bible => "bible",
            LinkTargetKind::Dict => "dict",
            LinkTargetKind::Commentary => "commentary",
            LinkTargetKind::Xref => "xref",
            LinkTargetKind::CommentAtVerse => "comment_at_verse",
            LinkTargetKind::Chain => "chain",
            LinkTargetKind::Broken => "broken",
        }
    }
}

/// One link record passed to `replace_note_record`.
///
/// Source kind is implicit — `replace_note_record` always uses `src='note'`.
/// Cross-domain sources (verse_comment, mybible_*) are inserted through
/// dedicated `replace_external_links` calls so the polymorphic columns stay
/// consistent.
#[derive(Debug, Clone)]
pub struct LinkInsert {
    pub src_byte_start: i64,
    pub src_byte_end: i64,
    pub src_line: i64,
    pub src_snippet: String,
    pub is_embed: bool,
    pub target_kind: LinkTargetKind,
    pub target_note_id: Option<NoteId>,
    pub target_raw: String,
    pub target_book: Option<i64>,
    pub target_chapter: Option<i64>,
    pub target_verse: Option<i64>,
    pub target_end_verse: Option<i64>,
    pub target_module_id: Option<String>,
    pub target_topic: Option<String>,
    pub target_fragment: Option<String>,
    pub target_alias: Option<String>,
}

/// Snapshot of a single note's parsed state, written atomically by
/// `replace_note_record`.
#[derive(Debug, Clone)]
pub struct NoteRecord {
    pub relpath: String,
    pub title: String,
    pub mtime: i64,
    pub blake3: String,
    pub body_chars: i64,
    pub body_for_fts: String,
    pub aliases: Vec<String>,
    pub headings: Vec<HeadingRow>,
    pub blocks: Vec<BlockRow>,
    pub links: Vec<LinkInsert>,
    pub tags: Vec<TagInsert>,
}

#[derive(Debug, Clone)]
pub struct TagInsert {
    pub tag: String,
    pub byte_offset: Option<i64>,
}

impl NotesStore {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Acquire the underlying connection for read queries (`backlinks.rs`,
    /// `cross_domain.rs`). Holding this guard blocks writers, so keep
    /// queries short.
    pub(crate) fn lock_connection(&self) -> parking_lot::MutexGuard<'_, Connection> {
        self.inner.lock()
    }

    /// Return the stored signature `(mtime, blake3)` for `relpath`, if any.
    /// Used by the vault scanner to skip parsing files whose signature has
    /// not changed.
    pub fn note_signature(&self, relpath: &str) -> Result<Option<NoteSignature>> {
        let connection = self.inner.lock();
        let mut stmt = connection.select_bound::<&str, (i64, String)>(
            "SELECT mtime, blake3 FROM note WHERE relpath = ?",
        )?;
        let mut rows = stmt(relpath)?;
        Ok(rows
            .pop()
            .map(|(mtime, blake3)| NoteSignature { mtime, blake3 }))
    }

    /// Lookup `note.id` by exact relpath.
    pub fn note_id_by_relpath(&self, relpath: &str) -> Result<Option<NoteId>> {
        let connection = self.inner.lock();
        let mut stmt =
            connection.select_bound::<&str, i64>("SELECT id FROM note WHERE relpath = ?")?;
        let mut rows = stmt(relpath)?;
        Ok(rows.pop())
    }

    /// Get a single note row.
    pub fn get_note(&self, id: NoteId) -> Result<Option<NoteRow>> {
        let connection = self.inner.lock();
        let mut stmt = connection
            .select_bound::<i64, (i64, String, String, i64, String, i64, i64)>(
                "SELECT id, relpath, title, mtime, blake3, body_chars, indexed_at \
             FROM note WHERE id = ?",
            )?;
        let mut rows = stmt(id)?;
        Ok(rows.pop().map(row_to_note))
    }

    pub fn get_note_by_relpath(&self, relpath: &str) -> Result<Option<NoteRow>> {
        let connection = self.inner.lock();
        let mut stmt = connection
            .select_bound::<&str, (i64, String, String, i64, String, i64, i64)>(
                "SELECT id, relpath, title, mtime, blake3, body_chars, indexed_at \
             FROM note WHERE relpath = ?",
            )?;
        let mut rows = stmt(relpath)?;
        Ok(rows.pop().map(row_to_note))
    }

    /// Return all relpaths currently in the index. Used by the scanner to
    /// detect deleted files (in DB but not on disk).
    pub fn all_relpaths(&self) -> Result<Vec<String>> {
        let connection = self.inner.lock();
        let mut stmt = connection.select::<String>("SELECT relpath FROM note")?;
        Ok(stmt()?)
    }

    /// Return all notes (id, relpath, title) — used by Quick Switcher.
    pub fn all_notes_summary(&self) -> Result<Vec<(NoteId, String, String)>> {
        let connection = self.inner.lock();
        let mut stmt =
            connection.select::<(i64, String, String)>("SELECT id, relpath, title FROM note")?;
        Ok(stmt()?)
    }

    /// All aliases across the vault, paired with their note id. Used by
    /// Quick Switcher to match by alias as well as title.
    pub fn all_aliases(&self) -> Result<Vec<(NoteId, String)>> {
        let connection = self.inner.lock();
        let mut stmt = connection.select::<(i64, String)>("SELECT note_id, alias FROM alias")?;
        Ok(stmt()?)
    }

    /// Headings for a single note in document order.
    pub fn headings_for_note(&self, id: NoteId) -> Result<Vec<HeadingRow>> {
        let connection = self.inner.lock();
        let mut stmt = connection.select_bound::<i64, (String, String, i64, i64)>(
            "SELECT slug, text, level, byte_offset FROM heading \
             WHERE note_id = ? ORDER BY byte_offset",
        )?;
        Ok(stmt(id)?
            .into_iter()
            .map(|(slug, text, level, byte_offset)| HeadingRow {
                slug,
                text,
                level,
                byte_offset,
            })
            .collect())
    }

    pub fn blocks_for_note(&self, id: NoteId) -> Result<Vec<BlockRow>> {
        let connection = self.inner.lock();
        let mut stmt = connection.select_bound::<i64, (String, i64, String)>(
            "SELECT block_id, byte_offset, text FROM block \
             WHERE note_id = ? ORDER BY byte_offset",
        )?;
        Ok(stmt(id)?
            .into_iter()
            .map(|(block_id, byte_offset, text)| BlockRow {
                block_id,
                byte_offset,
                text,
            })
            .collect())
    }

    /// Run an FTS5 MATCH query against `note_fts` and return hits joined to
    /// the note metadata. Used by the bible search-DSL extension when the
    /// user types `note:"…"` — the same expression that already searches
    /// `comments_fts` in `annotations.db` is extended to the markdown
    /// vault here.
    pub fn search_notes_fts(&self, match_expr: &str, limit: usize) -> Result<Vec<NoteSearchHit>> {
        let connection = self.inner.lock();
        let mut stmt = connection.select_bound::<(&str, i64), (i64, String, String, String)>(
            "SELECT n.id, n.relpath, n.title, snippet(note_fts, 1, '⟦', '⟧', '…', 64) AS snippet \
             FROM note_fts \
               JOIN note_fts_map m ON m.fts_rowid = note_fts.rowid \
               JOIN note n ON n.id = m.note_id \
             WHERE note_fts MATCH ? \
             ORDER BY rank \
             LIMIT ?",
        )?;
        let rows = stmt((match_expr, limit as i64))?;
        Ok(rows
            .into_iter()
            .map(|(id, relpath, title, snippet)| NoteSearchHit {
                id,
                relpath,
                title,
                snippet,
            })
            .collect())
    }

    /// Find every note with the supplied tag (inline `#tag` or frontmatter
    /// `tags: [tag]`).
    pub fn notes_with_tag(&self, tag: &str, limit: usize) -> Result<Vec<NoteSearchHit>> {
        let connection = self.inner.lock();
        let mut stmt = connection.select_bound::<(&str, i64), (i64, String, String)>(
            "SELECT DISTINCT n.id, n.relpath, n.title \
             FROM tag t JOIN note n ON n.id = t.note_id \
             WHERE t.tag = ? \
             ORDER BY n.relpath \
             LIMIT ?",
        )?;
        let rows = stmt((tag, limit as i64))?;
        Ok(rows
            .into_iter()
            .map(|(id, relpath, title)| NoteSearchHit {
                id,
                relpath,
                title,
                snippet: String::new(),
            })
            .collect())
    }

    /// Replace every record for a note in a single transaction. The note row
    /// is upserted by relpath; child rows (alias / heading / block / link /
    /// tag / fts) are deleted and re-inserted to keep parsing logic simple.
    pub fn replace_note_record(&self, record: &NoteRecord) -> Result<NoteId> {
        let connection = self.inner.lock();
        connection.exec("BEGIN")?()?;

        let note_id = upsert_note_row(&connection, record)?;
        delete_note_children(&connection, note_id)?;
        for alias in &record.aliases {
            insert_alias(&connection, note_id, alias)?;
        }
        for heading in &record.headings {
            insert_heading(&connection, note_id, heading)?;
        }
        for block in &record.blocks {
            insert_block(&connection, note_id, block)?;
        }
        for link in &record.links {
            insert_link_from_note(&connection, note_id, link)?;
        }
        for tag in &record.tags {
            insert_tag(&connection, note_id, tag)?;
        }
        update_note_fts(&connection, note_id, &record.title, &record.body_for_fts)?;

        connection.exec("COMMIT")?()?;
        Ok(note_id)
    }

    /// Delete a note (cascade removes alias / heading / block / link / tag /
    /// fts rows). Returns true if a row existed.
    pub fn delete_note(&self, relpath: &str) -> Result<bool> {
        let connection = self.inner.lock();
        let mut stmt = connection.exec_bound::<&str>("DELETE FROM note WHERE relpath = ?")?;
        stmt(relpath)?;
        let mut changes_stmt = connection.select::<i64>("SELECT changes()")?;
        let rows = changes_stmt()?;
        Ok(rows.first().copied().unwrap_or(0) > 0)
    }

    /// Replace every link from an external (non-note) source. Used by
    /// `cross_domain.rs` to refresh verse comments / commentary / dict
    /// modules. The deletion key is `(src_kind, src_*)` matching the source
    /// kind passed in.
    pub fn replace_external_links(
        &self,
        src: ExternalSource<'_>,
        links: &[LinkInsert],
    ) -> Result<()> {
        let connection = self.inner.lock();
        connection.exec("BEGIN")?()?;
        match src {
            ExternalSource::VerseComment {
                book_number,
                chapter,
                verse,
            } => {
                let mut stmt = connection.exec_bound::<(&str, i64, i64, i64)>(
                    "DELETE FROM link \
                     WHERE src_kind = ? AND src_verse_book = ? \
                       AND src_verse_chap = ? AND src_verse_vrs = ?",
                )?;
                stmt(("verse_comment", book_number, chapter, verse))?;
                for link in links {
                    insert_link_from_verse_comment(&connection, book_number, chapter, verse, link)?;
                }
            }
            ExternalSource::MyBibleCommentary { module_id, topic } => {
                let mut stmt = connection.exec_bound::<(&str, &str, &str)>(
                    "DELETE FROM link \
                     WHERE src_kind = ? AND src_module_id = ? AND src_topic = ?",
                )?;
                stmt(("mybible_commentary", module_id, topic))?;
                for link in links {
                    insert_link_from_module(
                        &connection,
                        "mybible_commentary",
                        module_id,
                        topic,
                        link,
                    )?;
                }
            }
            ExternalSource::MyBibleDictionary { module_id, topic } => {
                let mut stmt = connection.exec_bound::<(&str, &str, &str)>(
                    "DELETE FROM link \
                     WHERE src_kind = ? AND src_module_id = ? AND src_topic = ?",
                )?;
                stmt(("mybible_dict", module_id, topic))?;
                for link in links {
                    insert_link_from_module(&connection, "mybible_dict", module_id, topic, link)?;
                }
            }
        }
        connection.exec("COMMIT")?()?;
        Ok(())
    }

    /// Drop every external-source row of the given kind. Used by
    /// cross-domain rebuilds when a module is removed.
    pub fn purge_external_source_kind(&self, src: LinkSource) -> Result<()> {
        if src == LinkSource::Note {
            anyhow::bail!("note source rows are managed by replace_note_record / delete_note");
        }
        let connection = self.inner.lock();
        let mut stmt = connection.exec_bound::<&str>("DELETE FROM link WHERE src_kind = ?")?;
        stmt(src.as_token())?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ExternalSource<'a> {
    VerseComment {
        book_number: i64,
        chapter: i64,
        verse: i64,
    },
    MyBibleCommentary {
        module_id: &'a str,
        topic: &'a str,
    },
    MyBibleDictionary {
        module_id: &'a str,
        topic: &'a str,
    },
}

fn row_to_note(row: (i64, String, String, i64, String, i64, i64)) -> NoteRow {
    let (id, relpath, title, mtime, blake3, body_chars, indexed_at) = row;
    NoteRow {
        id,
        relpath,
        title,
        mtime,
        blake3,
        body_chars,
        indexed_at,
    }
}

fn upsert_note_row(connection: &Connection, record: &NoteRecord) -> Result<NoteId> {
    let mut stmt = connection.exec_bound::<(&str, &str, i64, &str, i64)>(
        "INSERT INTO note (relpath, title, mtime, blake3, body_chars, indexed_at) \
         VALUES (?, ?, ?, ?, ?, strftime('%s', 'now')) \
         ON CONFLICT(relpath) DO UPDATE SET \
            title = excluded.title, \
            mtime = excluded.mtime, \
            blake3 = excluded.blake3, \
            body_chars = excluded.body_chars, \
            indexed_at = excluded.indexed_at",
    )?;
    stmt((
        record.relpath.as_str(),
        record.title.as_str(),
        record.mtime,
        record.blake3.as_str(),
        record.body_chars,
    ))?;
    let mut id_stmt =
        connection.select_bound::<&str, i64>("SELECT id FROM note WHERE relpath = ?")?;
    let mut rows = id_stmt(record.relpath.as_str())?;
    rows.pop()
        .with_context(|| format!("note row missing after upsert for {}", record.relpath))
}

fn delete_note_children(connection: &Connection, note_id: NoteId) -> Result<()> {
    for sql in [
        "DELETE FROM alias WHERE note_id = ?",
        "DELETE FROM heading WHERE note_id = ?",
        "DELETE FROM block WHERE note_id = ?",
        "DELETE FROM link WHERE src_kind = 'note' AND src_note_id = ?",
        "DELETE FROM tag WHERE note_id = ?",
    ] {
        let mut stmt = connection.exec_bound::<i64>(sql)?;
        stmt(note_id)?;
    }
    Ok(())
}

fn insert_alias(connection: &Connection, note_id: NoteId, alias: &str) -> Result<()> {
    let mut stmt = connection
        .exec_bound::<(i64, &str)>("INSERT OR IGNORE INTO alias (note_id, alias) VALUES (?, ?)")?;
    stmt((note_id, alias))?;
    Ok(())
}

fn insert_heading(connection: &Connection, note_id: NoteId, heading: &HeadingRow) -> Result<()> {
    let mut stmt = connection.exec_bound::<(i64, &str, &str, i64, i64)>(
        "INSERT OR REPLACE INTO heading (note_id, slug, text, level, byte_offset) \
         VALUES (?, ?, ?, ?, ?)",
    )?;
    stmt((
        note_id,
        heading.slug.as_str(),
        heading.text.as_str(),
        heading.level,
        heading.byte_offset,
    ))?;
    Ok(())
}

fn insert_block(connection: &Connection, note_id: NoteId, block: &BlockRow) -> Result<()> {
    let mut stmt = connection.exec_bound::<(i64, &str, i64, &str)>(
        "INSERT OR REPLACE INTO block (note_id, block_id, byte_offset, text) \
         VALUES (?, ?, ?, ?)",
    )?;
    stmt((
        note_id,
        block.block_id.as_str(),
        block.byte_offset,
        block.text.as_str(),
    ))?;
    Ok(())
}

/// Sentinel byte offset for frontmatter-sourced tags. Stored as `-1` since
/// the SQLite PRIMARY KEY constraint disallows nullable expressions.
const FRONTMATTER_TAG_OFFSET: i64 = -1;

fn insert_tag(connection: &Connection, note_id: NoteId, tag: &TagInsert) -> Result<()> {
    let mut stmt = connection.exec_bound::<(i64, &str, i64)>(
        "INSERT OR IGNORE INTO tag (note_id, tag, byte_offset) VALUES (?, ?, ?)",
    )?;
    stmt((
        note_id,
        tag.tag.as_str(),
        tag.byte_offset.unwrap_or(FRONTMATTER_TAG_OFFSET),
    ))?;
    Ok(())
}

// sqlez's `Bind` trait stops at 10-element tuples. Group the 23 link columns
// into three nested tuples — sqlez's tuple Bind is recursive, so a
// tuple-of-tuples binds the right total column count.
type LinkBindGroupA<'a> = (
    &'a str,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<&'a str>,
    Option<&'a str>,
);
type LinkBindGroupB<'a> = (
    Option<i64>,
    Option<i64>,
    Option<i64>,
    &'a str,
    i64,
    &'a str,
    Option<i64>,
    &'a str,
);
type LinkBindGroupC<'a> = (
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
);
type LinkBindRow<'a> = (LinkBindGroupA<'a>, LinkBindGroupB<'a>, LinkBindGroupC<'a>);

fn link_bind_row<'a>(
    src_kind: &'a str,
    src_note_id: Option<NoteId>,
    src_verse_book: Option<i64>,
    src_verse_chap: Option<i64>,
    src_verse_vrs: Option<i64>,
    src_module_id: Option<&'a str>,
    src_topic: Option<&'a str>,
    link: &'a LinkInsert,
) -> LinkBindRow<'a> {
    (
        (
            src_kind,
            src_note_id,
            src_verse_book,
            src_verse_chap,
            src_verse_vrs,
            src_module_id,
            src_topic,
        ),
        (
            Some(link.src_byte_start),
            Some(link.src_byte_end),
            Some(link.src_line),
            link.src_snippet.as_str(),
            if link.is_embed { 1 } else { 0 },
            link.target_kind.as_token(),
            link.target_note_id,
            link.target_raw.as_str(),
        ),
        (
            link.target_book,
            link.target_chapter,
            link.target_verse,
            link.target_end_verse,
            link.target_module_id.as_deref(),
            link.target_topic.as_deref(),
            link.target_fragment.as_deref(),
            link.target_alias.as_deref(),
        ),
    )
}

fn insert_link_from_note(
    connection: &Connection,
    src_note_id: NoteId,
    link: &LinkInsert,
) -> Result<()> {
    let mut stmt = connection.exec_bound::<LinkBindRow<'_>>(LINK_INSERT_SQL)?;
    stmt(link_bind_row(
        "note",
        Some(src_note_id),
        None,
        None,
        None,
        None,
        None,
        link,
    ))?;
    Ok(())
}

fn insert_link_from_verse_comment(
    connection: &Connection,
    book: i64,
    chapter: i64,
    verse: i64,
    link: &LinkInsert,
) -> Result<()> {
    let mut stmt = connection.exec_bound::<LinkBindRow<'_>>(LINK_INSERT_SQL)?;
    stmt(link_bind_row(
        "verse_comment",
        None,
        Some(book),
        Some(chapter),
        Some(verse),
        None,
        None,
        link,
    ))?;
    Ok(())
}

fn insert_link_from_module(
    connection: &Connection,
    src_kind: &'static str,
    module_id: &str,
    topic: &str,
    link: &LinkInsert,
) -> Result<()> {
    let mut stmt = connection.exec_bound::<LinkBindRow<'_>>(LINK_INSERT_SQL)?;
    stmt(link_bind_row(
        src_kind,
        None,
        None,
        None,
        None,
        Some(module_id),
        Some(topic),
        link,
    ))?;
    Ok(())
}

const LINK_INSERT_SQL: &str = "\
    INSERT INTO link ( \
        src_kind, src_note_id, \
        src_verse_book, src_verse_chap, src_verse_vrs, \
        src_module_id, src_topic, \
        src_byte_start, src_byte_end, src_line, src_snippet, \
        is_embed, \
        target_kind, target_note_id, target_raw, \
        target_book, target_chapter, target_verse, target_end_verse, \
        target_module_id, target_topic, \
        target_fragment, target_alias \
    ) VALUES ( \
        ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ? \
    )";

fn update_note_fts(
    connection: &Connection,
    note_id: NoteId,
    title: &str,
    body: &str,
) -> Result<()> {
    let mut existing_stmt = connection
        .select_bound::<i64, i64>("SELECT fts_rowid FROM note_fts_map WHERE note_id = ?")?;
    let existing = existing_stmt(note_id)?.pop();
    if let Some(rowid) = existing {
        let mut delete_stmt =
            connection.exec_bound::<i64>("DELETE FROM note_fts WHERE rowid = ?")?;
        delete_stmt(rowid)?;
    }
    let mut insert_fts = connection
        .exec_bound::<(&str, &str)>("INSERT INTO note_fts (title, body) VALUES (?, ?)")?;
    insert_fts((title, body))?;
    let mut last_rowid = connection.select::<i64>("SELECT last_insert_rowid()")?;
    let rowid = last_rowid()?
        .first()
        .copied()
        .context("last_insert_rowid returned no row after FTS insert")?;
    let mut map_stmt = connection.exec_bound::<(i64, i64)>(
        "INSERT INTO note_fts_map (note_id, fts_rowid) VALUES (?, ?) \
         ON CONFLICT(note_id) DO UPDATE SET fts_rowid = excluded.fts_rowid",
    )?;
    map_stmt((note_id, rowid))?;
    Ok(())
}

fn init_schema(connection: &Connection) -> Result<()> {
    let stmts: &[&str] = &[
        "PRAGMA foreign_keys = ON",
        "CREATE TABLE IF NOT EXISTS note ( \
             id          INTEGER PRIMARY KEY, \
             relpath     TEXT UNIQUE NOT NULL, \
             title       TEXT NOT NULL, \
             mtime       INTEGER NOT NULL, \
             blake3      TEXT NOT NULL, \
             body_chars  INTEGER NOT NULL, \
             indexed_at  INTEGER NOT NULL \
         )",
        "CREATE INDEX IF NOT EXISTS idx_note_title ON note(title)",
        "CREATE TABLE IF NOT EXISTS alias ( \
             note_id INTEGER NOT NULL REFERENCES note(id) ON DELETE CASCADE, \
             alias   TEXT NOT NULL, \
             PRIMARY KEY (note_id, alias) \
         )",
        "CREATE INDEX IF NOT EXISTS idx_alias_alias ON alias(alias)",
        "CREATE TABLE IF NOT EXISTS heading ( \
             note_id     INTEGER NOT NULL REFERENCES note(id) ON DELETE CASCADE, \
             slug        TEXT NOT NULL, \
             text        TEXT NOT NULL, \
             level       INTEGER NOT NULL, \
             byte_offset INTEGER NOT NULL, \
             PRIMARY KEY (note_id, byte_offset) \
         )",
        "CREATE INDEX IF NOT EXISTS idx_heading_slug ON heading(note_id, slug)",
        "CREATE TABLE IF NOT EXISTS block ( \
             note_id     INTEGER NOT NULL REFERENCES note(id) ON DELETE CASCADE, \
             block_id    TEXT NOT NULL, \
             byte_offset INTEGER NOT NULL, \
             text        TEXT NOT NULL, \
             PRIMARY KEY (note_id, block_id) \
         )",
        "CREATE TABLE IF NOT EXISTS link ( \
             id              INTEGER PRIMARY KEY, \
             src_kind        TEXT NOT NULL, \
             src_note_id     INTEGER REFERENCES note(id) ON DELETE CASCADE, \
             src_verse_book  INTEGER, \
             src_verse_chap  INTEGER, \
             src_verse_vrs   INTEGER, \
             src_module_id   TEXT, \
             src_topic       TEXT, \
             src_byte_start  INTEGER, \
             src_byte_end    INTEGER, \
             src_line        INTEGER, \
             src_snippet     TEXT NOT NULL, \
             is_embed        INTEGER NOT NULL, \
             target_kind     TEXT NOT NULL, \
             target_note_id  INTEGER REFERENCES note(id) ON DELETE SET NULL, \
             target_raw      TEXT NOT NULL, \
             target_book     INTEGER, \
             target_chapter  INTEGER, \
             target_verse    INTEGER, \
             target_end_verse INTEGER, \
             target_module_id TEXT, \
             target_topic    TEXT, \
             target_fragment TEXT, \
             target_alias    TEXT \
         )",
        "CREATE INDEX IF NOT EXISTS idx_link_src_note \
             ON link(src_note_id) WHERE src_kind = 'note'",
        "CREATE INDEX IF NOT EXISTS idx_link_target_note \
             ON link(target_note_id) WHERE target_kind = 'note'",
        "CREATE INDEX IF NOT EXISTS idx_link_target_bible \
             ON link(target_book, target_chapter, target_verse) WHERE target_kind = 'bible'",
        "CREATE INDEX IF NOT EXISTS idx_link_target_dict \
             ON link(target_module_id, target_topic) WHERE target_kind = 'dict'",
        "CREATE INDEX IF NOT EXISTS idx_link_target_raw \
             ON link(target_kind, target_raw)",
        "CREATE INDEX IF NOT EXISTS idx_link_src_verse \
             ON link(src_verse_book, src_verse_chap, src_verse_vrs) \
             WHERE src_kind = 'verse_comment'",
        "CREATE INDEX IF NOT EXISTS idx_link_src_module \
             ON link(src_kind, src_module_id, src_topic) \
             WHERE src_kind IN ('mybible_commentary', 'mybible_dict')",
        // `byte_offset = -1` is the sentinel for frontmatter-sourced tags
        // (no physical position in the body).
        "CREATE TABLE IF NOT EXISTS tag ( \
             note_id     INTEGER NOT NULL REFERENCES note(id) ON DELETE CASCADE, \
             tag         TEXT NOT NULL, \
             byte_offset INTEGER NOT NULL DEFAULT -1, \
             PRIMARY KEY (note_id, tag, byte_offset) \
         )",
        "CREATE INDEX IF NOT EXISTS idx_tag_tag ON tag(tag)",
        "CREATE VIRTUAL TABLE IF NOT EXISTS note_fts USING fts5( \
             title, body, \
             tokenize = 'unicode61 remove_diacritics 2' \
         )",
        "CREATE TABLE IF NOT EXISTS note_fts_map ( \
             note_id   INTEGER PRIMARY KEY REFERENCES note(id) ON DELETE CASCADE, \
             fts_rowid INTEGER NOT NULL UNIQUE \
         )",
        "CREATE TRIGGER IF NOT EXISTS note_fts_delete \
             AFTER DELETE ON note_fts_map BEGIN \
                 DELETE FROM note_fts WHERE rowid = OLD.fts_rowid; \
             END",
    ];
    for sql in stmts {
        connection.exec(sql)?()?;
    }
    connection.exec(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record() -> NoteRecord {
        NoteRecord {
            relpath: "Theology/Faith.md".into(),
            title: "Faith".into(),
            mtime: 1_700_000_000,
            blake3: "deadbeef".into(),
            body_chars: 64,
            body_for_fts: "Faith without works is dead.".into(),
            aliases: vec!["faith".into(), "Pistis".into()],
            headings: vec![HeadingRow {
                slug: "introduction".into(),
                text: "Introduction".into(),
                level: 1,
                byte_offset: 0,
            }],
            blocks: vec![BlockRow {
                block_id: "abc123".into(),
                byte_offset: 100,
                text: "First paragraph.".into(),
            }],
            links: vec![LinkInsert {
                src_byte_start: 10,
                src_byte_end: 30,
                src_line: 1,
                src_snippet: "...see [[bible:John 3:16]]...".into(),
                is_embed: false,
                target_kind: LinkTargetKind::Bible,
                target_note_id: None,
                target_raw: "John 3:16".into(),
                target_book: Some(43),
                target_chapter: Some(3),
                target_verse: Some(16),
                target_end_verse: None,
                target_module_id: None,
                target_topic: None,
                target_fragment: None,
                target_alias: None,
            }],
            tags: vec![TagInsert {
                tag: "theology".into(),
                byte_offset: Some(40),
            }],
        }
    }

    #[test]
    fn open_creates_schema() {
        let store = open_in_memory().expect("open_in_memory");
        // Re-running open is a no-op (CREATE IF NOT EXISTS).
        init_schema(&store.inner.lock()).unwrap();
    }

    #[test]
    fn replace_note_record_round_trip() {
        let store = open_in_memory().unwrap();
        let id = store.replace_note_record(&sample_record()).unwrap();
        let row = store.get_note(id).unwrap().expect("note row");
        assert_eq!(row.relpath, "Theology/Faith.md");
        assert_eq!(row.title, "Faith");

        let aliases = store.all_aliases().unwrap();
        assert_eq!(aliases.len(), 2);
        let mut alias_strings: Vec<_> = aliases.into_iter().map(|(_, a)| a).collect();
        alias_strings.sort();
        assert_eq!(
            alias_strings,
            vec!["Pistis".to_string(), "faith".to_string()]
        );

        let headings = store.headings_for_note(id).unwrap();
        assert_eq!(headings.len(), 1);
        assert_eq!(headings[0].slug, "introduction");

        let blocks = store.blocks_for_note(id).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].block_id, "abc123");

        let summary = store.all_notes_summary().unwrap();
        assert_eq!(summary.len(), 1);

        let signature = store.note_signature("Theology/Faith.md").unwrap();
        let signature = signature.expect("signature present");
        assert_eq!(signature.mtime, 1_700_000_000);
        assert_eq!(signature.blake3, "deadbeef");
    }

    #[test]
    fn replace_note_record_replaces_children() {
        let store = open_in_memory().unwrap();
        store.replace_note_record(&sample_record()).unwrap();
        let mut next = sample_record();
        next.aliases = vec!["only".into()];
        next.headings.clear();
        next.blocks.clear();
        next.links.clear();
        next.tags.clear();
        let id = store.replace_note_record(&next).unwrap();
        assert_eq!(store.headings_for_note(id).unwrap().len(), 0);
        assert_eq!(store.blocks_for_note(id).unwrap().len(), 0);
        let aliases = store.all_aliases().unwrap();
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].1, "only");
    }

    #[test]
    fn delete_note_cascades() {
        let store = open_in_memory().unwrap();
        let id = store.replace_note_record(&sample_record()).unwrap();
        let removed = store.delete_note("Theology/Faith.md").unwrap();
        assert!(removed);
        assert!(store.get_note(id).unwrap().is_none());
        assert!(store.all_aliases().unwrap().is_empty());
        assert!(store.headings_for_note(id).unwrap().is_empty());
    }

    #[test]
    fn external_links_round_trip() {
        let store = open_in_memory().unwrap();
        let link = LinkInsert {
            src_byte_start: 0,
            src_byte_end: 0,
            src_line: 0,
            src_snippet: "see [[Faith]]".into(),
            is_embed: false,
            target_kind: LinkTargetKind::Note,
            target_note_id: None,
            target_raw: "Faith".into(),
            target_book: None,
            target_chapter: None,
            target_verse: None,
            target_end_verse: None,
            target_module_id: None,
            target_topic: None,
            target_fragment: None,
            target_alias: None,
        };
        store
            .replace_external_links(
                ExternalSource::VerseComment {
                    book_number: 43,
                    chapter: 3,
                    verse: 16,
                },
                std::slice::from_ref(&link),
            )
            .unwrap();
        // Replace with empty list — should clear the row.
        store
            .replace_external_links(
                ExternalSource::VerseComment {
                    book_number: 43,
                    chapter: 3,
                    verse: 16,
                },
                &[],
            )
            .unwrap();
        // Module source.
        store
            .replace_external_links(
                ExternalSource::MyBibleDictionary {
                    module_id: "strongs",
                    topic: "G3056",
                },
                std::slice::from_ref(&link),
            )
            .unwrap();
        store
            .purge_external_source_kind(LinkSource::MyBibleDictionary)
            .unwrap();
    }
}
