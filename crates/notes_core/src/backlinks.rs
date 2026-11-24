//! Read queries for the References panel.
//!
//! Every entry returned here is a [`LinkHit`] — a normalised view of one
//! row in the `link` table joined to whatever metadata the source kind
//! requires. The panel renders them grouped by source kind without having
//! to issue follow-up queries.
//!
//! All functions take `&NotesStore` (which holds the `Mutex<Connection>`)
//! and return `Result<Vec<LinkHit>>`. They do not allocate task handles —
//! the orchestrator runs them on a background spawn when a panel asks for
//! a refresh.

use anyhow::Result;
use sqlez::connection::Connection;

use crate::store::{NoteId, NotesStore};

/// One link entry shown in the References panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkHit {
    pub source: LinkSourceRef,
    pub byte_start: i64,
    pub byte_end: i64,
    pub line: i64,
    pub snippet: String,
    pub is_embed: bool,
    pub target_raw: String,
    pub target_fragment: Option<String>,
    pub target_alias: Option<String>,
}

/// Identifier for the originating object of a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkSourceRef {
    Note {
        id: NoteId,
        relpath: String,
        title: String,
    },
    VerseComment {
        book: i64,
        chapter: i64,
        verse: i64,
    },
    Commentary {
        module_id: String,
        reference: String,
    },
    Dictionary {
        module_id: String,
        topic: String,
    },
}

/// All notes / external sources whose `[[…]]` resolves to `note_id`.
pub fn backlinks_for_note(store: &NotesStore, note_id: NoteId) -> Result<Vec<LinkHit>> {
    let connection = store.lock_connection();
    fetch_hits(
        &connection,
        "SELECT \
            l.src_kind, l.src_note_id, l.src_verse_book, l.src_verse_chap, l.src_verse_vrs, \
            l.src_module_id, l.src_topic, \
            l.src_byte_start, l.src_byte_end, l.src_line, l.src_snippet, l.is_embed, \
            l.target_raw, l.target_fragment, l.target_alias, \
            COALESCE(src_note.relpath, ''), COALESCE(src_note.title, '') \
         FROM link l \
         LEFT JOIN note src_note ON src_note.id = l.src_note_id \
         WHERE l.target_kind = 'note' AND l.target_note_id = ?",
        BindKey::Int(note_id),
    )
}

/// All `[[…]]` links written from `note_id`. Includes broken links — use
/// [`unresolved_for_note`] to filter to that subset alone.
pub fn outgoing_for_note(store: &NotesStore, note_id: NoteId) -> Result<Vec<LinkHit>> {
    let connection = store.lock_connection();
    fetch_hits(
        &connection,
        "SELECT \
            l.src_kind, l.src_note_id, l.src_verse_book, l.src_verse_chap, l.src_verse_vrs, \
            l.src_module_id, l.src_topic, \
            l.src_byte_start, l.src_byte_end, l.src_line, l.src_snippet, l.is_embed, \
            l.target_raw, l.target_fragment, l.target_alias, \
            src_note.relpath, src_note.title \
         FROM link l \
         JOIN note src_note ON src_note.id = l.src_note_id \
         WHERE l.src_kind = 'note' AND l.src_note_id = ? \
         ORDER BY l.src_byte_start",
        BindKey::Int(note_id),
    )
}

/// Outgoing links from `note_id` whose target did not resolve.
pub fn unresolved_for_note(store: &NotesStore, note_id: NoteId) -> Result<Vec<LinkHit>> {
    let connection = store.lock_connection();
    fetch_hits(
        &connection,
        "SELECT \
            l.src_kind, l.src_note_id, l.src_verse_book, l.src_verse_chap, l.src_verse_vrs, \
            l.src_module_id, l.src_topic, \
            l.src_byte_start, l.src_byte_end, l.src_line, l.src_snippet, l.is_embed, \
            l.target_raw, l.target_fragment, l.target_alias, \
            src_note.relpath, src_note.title \
         FROM link l \
         JOIN note src_note ON src_note.id = l.src_note_id \
         WHERE l.src_kind = 'note' AND l.src_note_id = ? AND l.target_kind = 'broken' \
         ORDER BY l.src_byte_start",
        BindKey::Int(note_id),
    )
}

/// Tags belonging to a note (frontmatter + inline merged, deduped).
pub fn tags_for_note(store: &NotesStore, note_id: NoteId) -> Result<Vec<String>> {
    let connection = store.lock_connection();
    let mut stmt = connection.select_bound::<i64, String>(
        "SELECT DISTINCT tag FROM tag WHERE note_id = ? ORDER BY tag",
    )?;
    let rows = stmt(note_id)?;
    Ok(rows)
}

/// Anything that links to the given Bible verse, regardless of source.
pub fn backlinks_for_verse(
    store: &NotesStore,
    book: i64,
    chapter: i64,
    verse: i64,
) -> Result<Vec<LinkHit>> {
    let connection = store.lock_connection();
    let mut stmt = connection.select_bound::<(i64, i64, i64), HitRow>(
        "SELECT \
            l.src_kind, l.src_note_id, l.src_verse_book, l.src_verse_chap, l.src_verse_vrs, \
            l.src_module_id, l.src_topic, \
            l.src_byte_start, l.src_byte_end, l.src_line, l.src_snippet, l.is_embed, \
            l.target_raw, l.target_fragment, l.target_alias, \
            COALESCE(src_note.relpath, ''), COALESCE(src_note.title, '') \
         FROM link l \
         LEFT JOIN note src_note ON src_note.id = l.src_note_id \
         WHERE l.target_kind = 'bible' \
           AND l.target_book = ? AND l.target_chapter = ? AND l.target_verse = ?",
    )?;
    let rows = stmt((book, chapter, verse))?;
    Ok(rows.into_iter().filter_map(row_to_hit).collect())
}

/// Anything that links to the given dictionary topic.
pub fn backlinks_for_dict(
    store: &NotesStore,
    module_id: &str,
    topic: &str,
) -> Result<Vec<LinkHit>> {
    let connection = store.lock_connection();
    let mut stmt = connection.select_bound::<(&str, &str), HitRow>(
        "SELECT \
            l.src_kind, l.src_note_id, l.src_verse_book, l.src_verse_chap, l.src_verse_vrs, \
            l.src_module_id, l.src_topic, \
            l.src_byte_start, l.src_byte_end, l.src_line, l.src_snippet, l.is_embed, \
            l.target_raw, l.target_fragment, l.target_alias, \
            COALESCE(src_note.relpath, ''), COALESCE(src_note.title, '') \
         FROM link l \
         LEFT JOIN note src_note ON src_note.id = l.src_note_id \
         WHERE l.target_kind = 'dict' AND l.target_module_id = ? AND l.target_topic = ?",
    )?;
    let rows = stmt((module_id, topic))?;
    Ok(rows.into_iter().filter_map(row_to_hit).collect())
}

/// Anything that links to a commentary module reference.
pub fn backlinks_for_commentary(
    store: &NotesStore,
    module_id: &str,
    reference: &str,
) -> Result<Vec<LinkHit>> {
    let connection = store.lock_connection();
    let mut stmt = connection.select_bound::<(&str, &str), HitRow>(
        "SELECT \
            l.src_kind, l.src_note_id, l.src_verse_book, l.src_verse_chap, l.src_verse_vrs, \
            l.src_module_id, l.src_topic, \
            l.src_byte_start, l.src_byte_end, l.src_line, l.src_snippet, l.is_embed, \
            l.target_raw, l.target_fragment, l.target_alias, \
            COALESCE(src_note.relpath, ''), COALESCE(src_note.title, '') \
         FROM link l \
         LEFT JOIN note src_note ON src_note.id = l.src_note_id \
         WHERE l.target_kind = 'commentary' \
           AND l.target_module_id = ? AND l.target_topic = ?",
    )?;
    let rows = stmt((module_id, reference))?;
    Ok(rows.into_iter().filter_map(row_to_hit).collect())
}

/// Promote previously-broken `[[Note]]` links whose `target_raw` matches
/// the relpath of a newly-indexed note. Called after each note write so
/// later links resolve immediately.
pub fn reresolve_broken_to_note(store: &NotesStore, note_id: NoteId, relpath: &str) -> Result<()> {
    let connection = store.lock_connection();
    // The user can write `[[Theology/Faith]]` or `[[Theology/Faith.md]]`.
    // Match both shapes so either spelling becomes resolved.
    let bare = relpath.strip_suffix(".md").unwrap_or(relpath);
    let mut stmt = connection.exec_bound::<(i64, &str, &str)>(
        "UPDATE link SET target_kind = 'note', target_note_id = ? \
         WHERE target_kind = 'broken' AND (target_raw = ? OR target_raw = ?)",
    )?;
    stmt((note_id, relpath, bare))?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum BindKey {
    Int(i64),
}

#[allow(clippy::type_complexity)]
type HitRowGroupA = (
    String,         // src_kind
    Option<i64>,    // src_note_id
    Option<i64>,    // src_verse_book
    Option<i64>,    // src_verse_chap
    Option<i64>,    // src_verse_vrs
    Option<String>, // src_module_id
    Option<String>, // src_topic
);
type HitRowGroupB = (
    Option<i64>,    // src_byte_start
    Option<i64>,    // src_byte_end
    Option<i64>,    // src_line
    String,         // src_snippet
    i64,            // is_embed
    String,         // target_raw
    Option<String>, // target_fragment
);
type HitRowGroupC = (
    Option<String>, // target_alias
    String,         // src_note_relpath (COALESCE'd)
    String,         // src_note_title  (COALESCE'd)
);
type HitRow = (HitRowGroupA, HitRowGroupB, HitRowGroupC);

fn fetch_hits(connection: &Connection, sql: &str, key: BindKey) -> Result<Vec<LinkHit>> {
    let rows = match key {
        BindKey::Int(id) => {
            let mut stmt = connection.select_bound::<i64, HitRow>(sql)?;
            stmt(id)?
        }
    };
    Ok(rows.into_iter().filter_map(row_to_hit).collect())
}

fn row_to_hit(row: HitRow) -> Option<LinkHit> {
    let (
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
            src_byte_start,
            src_byte_end,
            src_line,
            src_snippet,
            is_embed,
            target_raw,
            target_fragment,
        ),
        (target_alias, src_note_relpath, src_note_title),
    ) = row;
    let source = match src_kind.as_str() {
        "note" => LinkSourceRef::Note {
            id: src_note_id?,
            relpath: src_note_relpath,
            title: src_note_title,
        },
        "verse_comment" => LinkSourceRef::VerseComment {
            book: src_verse_book?,
            chapter: src_verse_chap?,
            verse: src_verse_vrs?,
        },
        "mybible_commentary" => LinkSourceRef::Commentary {
            module_id: src_module_id?,
            reference: src_topic?,
        },
        "mybible_dict" => LinkSourceRef::Dictionary {
            module_id: src_module_id?,
            topic: src_topic?,
        },
        _ => return None,
    };
    Some(LinkHit {
        source,
        byte_start: src_byte_start.unwrap_or(0),
        byte_end: src_byte_end.unwrap_or(0),
        line: src_line.unwrap_or(0),
        snippet: src_snippet,
        is_embed: is_embed != 0,
        target_raw,
        target_fragment,
        target_alias,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::*;

    fn link(target_kind: LinkTargetKind, target_raw: &str) -> LinkInsert {
        LinkInsert {
            src_byte_start: 0,
            src_byte_end: 0,
            src_line: 0,
            src_snippet: format!("[[{target_raw}]]"),
            is_embed: false,
            target_kind,
            target_note_id: None,
            target_raw: target_raw.to_string(),
            target_book: None,
            target_chapter: None,
            target_verse: None,
            target_end_verse: None,
            target_module_id: None,
            target_topic: None,
            target_fragment: None,
            target_alias: None,
        }
    }

    fn record(relpath: &str, title: &str, links: Vec<LinkInsert>) -> NoteRecord {
        NoteRecord {
            relpath: relpath.into(),
            title: title.into(),
            mtime: 1,
            blake3: "h".into(),
            body_chars: 1,
            body_for_fts: title.into(),
            aliases: Vec::new(),
            headings: Vec::new(),
            blocks: Vec::new(),
            links,
            tags: Vec::new(),
        }
    }

    #[test]
    fn backlinks_for_note_returns_source_notes() {
        let store = open_in_memory().unwrap();
        let target_id = store
            .replace_note_record(&record("Bar.md", "Bar", Vec::new()))
            .unwrap();
        let mut foo_link = link(LinkTargetKind::Note, "Bar");
        foo_link.target_note_id = Some(target_id);
        store
            .replace_note_record(&record("Foo.md", "Foo", vec![foo_link]))
            .unwrap();

        let hits = backlinks_for_note(&store, target_id).unwrap();
        assert_eq!(hits.len(), 1);
        match &hits[0].source {
            LinkSourceRef::Note { relpath, title, .. } => {
                assert_eq!(relpath, "Foo.md");
                assert_eq!(title, "Foo");
            }
            other => panic!("unexpected source {other:?}"),
        }
    }

    #[test]
    fn outgoing_for_note_lists_typed_targets() {
        let store = open_in_memory().unwrap();
        let id = store
            .replace_note_record(&record(
                "Foo.md",
                "Foo",
                vec![
                    link(LinkTargetKind::Bible, "John 3:16"),
                    link(LinkTargetKind::Note, "Bar"),
                ],
            ))
            .unwrap();
        let out = outgoing_for_note(&store, id).unwrap();
        assert_eq!(out.len(), 2);
        let raws: Vec<_> = out.iter().map(|h| h.target_raw.clone()).collect();
        assert!(raws.contains(&"John 3:16".to_string()));
        assert!(raws.contains(&"Bar".to_string()));
    }

    #[test]
    fn unresolved_filters_to_broken() {
        let store = open_in_memory().unwrap();
        let id = store
            .replace_note_record(&record(
                "Foo.md",
                "Foo",
                vec![
                    link(LinkTargetKind::Note, "Existing"),
                    link(LinkTargetKind::Broken, "Missing"),
                ],
            ))
            .unwrap();
        let unresolved = unresolved_for_note(&store, id).unwrap();
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].target_raw, "Missing");
    }

    #[test]
    fn backlinks_for_verse_includes_external_sources() {
        let store = open_in_memory().unwrap();
        let mut nl = link(LinkTargetKind::Bible, "John 3:16");
        nl.target_book = Some(43);
        nl.target_chapter = Some(3);
        nl.target_verse = Some(16);
        store
            .replace_note_record(&record("Sermon.md", "Sermon", vec![nl.clone()]))
            .unwrap();
        store
            .replace_external_links(
                ExternalSource::VerseComment {
                    book_number: 43,
                    chapter: 3,
                    verse: 16,
                },
                std::slice::from_ref(&nl),
            )
            .unwrap();
        store
            .replace_external_links(
                ExternalSource::MyBibleDictionary {
                    module_id: "strongs",
                    topic: "G3056",
                },
                std::slice::from_ref(&nl),
            )
            .unwrap();
        let hits = backlinks_for_verse(&store, 43, 3, 16).unwrap();
        assert_eq!(hits.len(), 3);
        let mut kinds: Vec<&'static str> = hits
            .iter()
            .map(|h| match &h.source {
                LinkSourceRef::Note { .. } => "note",
                LinkSourceRef::VerseComment { .. } => "verse_comment",
                LinkSourceRef::Commentary { .. } => "commentary",
                LinkSourceRef::Dictionary { .. } => "dictionary",
            })
            .collect();
        kinds.sort();
        assert_eq!(kinds, vec!["dictionary", "note", "verse_comment"]);
    }

    #[test]
    fn tags_for_note_dedupes() {
        let store = open_in_memory().unwrap();
        let mut rec = record("Foo.md", "Foo", Vec::new());
        rec.tags = vec![
            TagInsert {
                tag: "theology".into(),
                byte_offset: None,
            },
            TagInsert {
                tag: "ethics".into(),
                byte_offset: Some(10),
            },
            TagInsert {
                tag: "theology".into(),
                byte_offset: Some(20),
            },
        ];
        let id = store.replace_note_record(&rec).unwrap();
        let tags = tags_for_note(&store, id).unwrap();
        assert_eq!(tags, vec!["ethics".to_string(), "theology".to_string()]);
    }

    #[test]
    fn reresolve_promotes_broken_links() {
        let store = open_in_memory().unwrap();
        store
            .replace_note_record(&record(
                "Foo.md",
                "Foo",
                vec![link(LinkTargetKind::Broken, "Bar")],
            ))
            .unwrap();
        let bar_id = store
            .replace_note_record(&record("Bar.md", "Bar", Vec::new()))
            .unwrap();
        reresolve_broken_to_note(&store, bar_id, "Bar.md").unwrap();
        let hits = backlinks_for_note(&store, bar_id).unwrap();
        assert_eq!(hits.len(), 1);
        match &hits[0].source {
            LinkSourceRef::Note { relpath, .. } => assert_eq!(relpath, "Foo.md"),
            other => panic!("unexpected source {other:?}"),
        }
    }
}
