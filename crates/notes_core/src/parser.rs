//! Per-note Markdown parser.
//!
//! Consumes a `.md` file's bytes and produces a [`ParsedNote`] containing
//! every piece of structured information the indexer needs to write to
//! `notes.db`: frontmatter, headings, blocks (Obsidian `^block-id`),
//! wikilinks (forward and embed `![[…]]`), and tags (inline `#tag` plus
//! frontmatter `tags:`).
//!
//! Parsing strategy:
//!
//! - `frontmatter::split` peels off the YAML prelude and gives us the body
//!   range. We compute everything else against that body (so byte offsets
//!   stay consistent with the file's bytes — pulldown's offsets become
//!   `body.start + offset`).
//! - pulldown-cmark with `Options::ENABLE_WIKILINKS | TABLES | TASKLISTS |
//!   STRIKETHROUGH | FOOTNOTES` walks the body's events. We use it to
//!   identify headings (with their byte range and slug), code regions
//!   (fenced and inline) so we can mask them when scanning for tags and
//!   block IDs.
//! - `bible::wikilink::scan` (already code-fence aware) gives us the full
//!   list of wikilinks with `target_kind` discrimination. A 1-byte lookback
//!   in front of `raw_range.start` decides whether the link is a `![[…]]`
//!   embed.
//! - Block IDs (`^abc-id` at end of paragraph), tags (`#tag`), and per-line
//!   snippets are computed by direct byte scans over a "code mask" produced
//!   by pulldown.
//!
//! Returned offsets are absolute file offsets — `replace_note_record` writes
//! them straight into the `link.src_byte_*` columns.

use std::ops::Range;

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::frontmatter::{self, Frontmatter};
use crate::store::{BlockRow, HeadingRow, LinkInsert, LinkTargetKind, TagInsert};
use crate::wikilink::{self, Wikilink, WikilinkTarget};

/// Разрешение `[[bible:…]]`-цели в числовые координаты — обёртка над
/// `bible_domain::reference::parse` с поиском книги в реестре модулей.
/// Инъецировано замыканием (адаптация: в форке parser ходил напрямую в
/// bible::ModuleRegistry), чтобы notes_core не зависел от mybible.
pub type BibleResolver = dyn Fn(&str) -> Option<bible_domain::reference::Reference> + Send + Sync;

/// Snapshot the indexer hands to [`store::replace_note_record`].
#[derive(Debug, Clone)]
pub struct ParsedNote {
    pub frontmatter: Frontmatter,
    pub headings: Vec<HeadingRow>,
    pub blocks: Vec<BlockRow>,
    pub links: Vec<LinkInsert>,
    pub tags: Vec<TagInsert>,
    /// Plain-text approximation of the body, used as the FTS5 `body` column.
    /// Frontmatter is stripped; markdown markers are kept (FTS5's tokenizer
    /// will ignore punctuation anyway).
    pub body_for_fts: String,
}

/// Resolve a `[[bible:…]]` raw target into `(book_number, chapter, verse,
/// end_verse)` if the resolver can match the localised book name. Used by
/// the indexer to populate the `link.target_book/chapter/verse/end_verse`
/// columns at write time so backlink queries stay O(1).
pub fn resolve_bible_target(
    raw: &str,
    resolver: &BibleResolver,
) -> Option<(i64, i64, Option<i64>, Option<i64>)> {
    let reference = resolver(raw)?;
    Some((
        reference.book_number,
        reference.chapter as i64,
        reference.verse.map(|v| v as i64),
        reference.end_verse.map(|v| v as i64),
    ))
}

/// Parse `content` into a [`ParsedNote`]. `resolver`, when supplied,
/// resolves `[[bible:…]]` targets to numeric book/chapter/verse — pass
/// `None` from tests where the registry isn't initialised.
pub fn parse(content: &str, resolver: Option<&BibleResolver>) -> ParsedNote {
    let (frontmatter, body_range) = frontmatter::split(content).unwrap_or_else(|err| {
        log::warn!(target: "bible_notes::parser", "frontmatter parse error: {err:#}");
        (Frontmatter::default(), 0..content.len())
    });
    let body = &content[body_range.clone()];

    let mut events = Vec::new();
    let parser = Parser::new_ext(body, parser_options()).into_offset_iter();
    let mut code_mask = vec![false; body.len()];
    let mut headings = Vec::new();
    let mut current_heading: Option<HeadingInProgress> = None;

    for (event, range) in parser {
        match &event {
            Event::Start(Tag::Heading { level, .. }) => {
                current_heading = Some(HeadingInProgress {
                    level: heading_level_to_int(*level),
                    text: String::new(),
                    range: range.clone(),
                });
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(in_progress) = current_heading.take() {
                    headings.push(HeadingRow {
                        slug: bible_domain::normalize::slug_for_anchor(&in_progress.text),
                        text: in_progress.text.trim().to_string(),
                        level: in_progress.level,
                        byte_offset: (in_progress.range.start + body_range.start) as i64,
                    });
                }
            }
            Event::Code(text) => {
                mask_range(&mut code_mask, range.clone());
                if let Some(h) = current_heading.as_mut() {
                    h.text.push_str(text);
                }
            }
            Event::Start(Tag::CodeBlock(_)) => mask_range(&mut code_mask, range.clone()),
            Event::Text(text) => {
                if let Some(h) = current_heading.as_mut() {
                    h.text.push_str(text);
                }
            }
            _ => {}
        }
        events.push((event, range));
    }
    drop(events);

    let mut links = Vec::with_capacity(8);
    for link in wikilink::scan(body) {
        let absolute_range =
            (link.raw_range.start + body_range.start)..(link.raw_range.end + body_range.start);
        let is_embed = absolute_range.start > 0
            && content.as_bytes().get(absolute_range.start - 1) == Some(&b'!');
        let line = line_at_offset(content, absolute_range.start);
        let snippet = snippet_around(content, absolute_range.clone());
        let (
            target_kind,
            target_note_id,
            target_book,
            target_chapter,
            target_verse,
            target_end,
            target_module_id,
            target_topic,
        ) = classify_target(&link, resolver);
        links.push(LinkInsert {
            src_byte_start: absolute_range.start as i64,
            src_byte_end: absolute_range.end as i64,
            src_line: line as i64,
            src_snippet: snippet,
            is_embed,
            target_kind,
            target_note_id,
            target_raw: target_raw(&link),
            target_book,
            target_chapter,
            target_verse,
            target_end_verse: target_end,
            target_module_id,
            target_topic,
            target_fragment: link.fragment.clone(),
            target_alias: link.alias.clone(),
        });
    }

    let blocks = scan_blocks(content, body_range.clone(), &code_mask);
    let tags = collect_tags(content, body_range.clone(), &code_mask, &frontmatter);

    let body_for_fts = strip_markdown_for_fts(body);

    ParsedNote {
        frontmatter,
        headings,
        blocks,
        links,
        tags,
        body_for_fts,
    }
}

/// Resolve note targets after the parse — separate pass because note
/// resolution requires hitting `notes.db` and we want `parse()` to stay
/// pure. `resolver` returns `Some(note_id)` for an unambiguous path,
/// `None` for broken links.
pub fn resolve_note_targets(
    parsed: &mut ParsedNote,
    mut resolver: impl FnMut(&str) -> Option<crate::store::NoteId>,
) {
    for link in &mut parsed.links {
        if matches!(link.target_kind, LinkTargetKind::Note) {
            match resolver(&link.target_raw) {
                Some(id) => link.target_note_id = Some(id),
                None => {
                    link.target_kind = LinkTargetKind::Broken;
                    link.target_note_id = None;
                }
            }
        }
    }
}

fn parser_options() -> Options {
    Options::ENABLE_WIKILINKS
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
}

fn mask_range(mask: &mut [bool], range: Range<usize>) {
    let end = range.end.min(mask.len());
    for byte in &mut mask[range.start.min(end)..end] {
        *byte = true;
    }
}

fn heading_level_to_int(level: HeadingLevel) -> i64 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[derive(Debug)]
struct HeadingInProgress {
    level: i64,
    text: String,
    range: Range<usize>,
}

fn line_at_offset(content: &str, offset: usize) -> usize {
    content[..offset.min(content.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
        + 1
}

fn snippet_around(content: &str, range: Range<usize>) -> String {
    const PAD: usize = 80;
    let start = range.start.saturating_sub(PAD);
    let end = (range.end + PAD).min(content.len());
    let mut start = start;
    while start > 0 && !content.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = end;
    while end < content.len() && !content.is_char_boundary(end) {
        end += 1;
    }
    content[start..end].replace('\n', " ").trim().to_string()
}

fn target_raw(link: &Wikilink) -> String {
    match &link.target {
        WikilinkTarget::Bible(s) => s.clone(),
        WikilinkTarget::BibleNumeric {
            book,
            chapter,
            verse,
            end_verse,
        } => match (verse, end_verse) {
            (Some(v), Some(e)) => format!("@{book}/{chapter}:{v}-{e}"),
            (Some(v), None) => format!("@{book}/{chapter}:{v}"),
            (None, _) => format!("@{book}/{chapter}"),
        },
        WikilinkTarget::XrefAtVerse {
            book,
            chapter,
            verse,
        } => {
            format!("@{book}/{chapter}:{verse}")
        }
        WikilinkTarget::CommentAtVerse {
            book,
            chapter,
            verse,
        } => {
            format!("@{book}/{chapter}:{verse}")
        }
        WikilinkTarget::Dict { topic, .. } => topic.clone(),
        WikilinkTarget::Commentary { reference, .. } => reference.clone(),
        WikilinkTarget::CrossReference { reference, .. } => reference.clone(),
        WikilinkTarget::Note(name) => name.clone(),
        WikilinkTarget::Chain(name) => name.clone(),
    }
}

#[allow(clippy::type_complexity)]
fn classify_target(
    link: &Wikilink,
    resolver: Option<&BibleResolver>,
) -> (
    LinkTargetKind,
    Option<crate::store::NoteId>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<String>,
) {
    match &link.target {
        WikilinkTarget::Bible(raw) => {
            let resolved = resolver.and_then(|r| resolve_bible_target(raw, r));
            let (book, chapter, verse, end) = match resolved {
                Some((b, c, v, e)) => (Some(b), Some(c), v, e),
                None => (None, None, None, None),
            };
            (
                LinkTargetKind::Bible,
                None,
                book,
                chapter,
                verse,
                end,
                None,
                None,
            )
        }
        WikilinkTarget::BibleNumeric {
            book,
            chapter,
            verse,
            end_verse,
        } => (
            LinkTargetKind::Bible,
            None,
            Some(*book),
            Some(*chapter as i64),
            verse.map(|v| v as i64),
            end_verse.map(|v| v as i64),
            None,
            None,
        ),
        WikilinkTarget::XrefAtVerse {
            book,
            chapter,
            verse,
        } => (
            LinkTargetKind::Xref,
            None,
            Some(*book),
            Some(*chapter as i64),
            Some(*verse as i64),
            None,
            None,
            None,
        ),
        WikilinkTarget::CommentAtVerse {
            book,
            chapter,
            verse,
        } => (
            LinkTargetKind::CommentAtVerse,
            None,
            Some(*book),
            Some(*chapter as i64),
            Some(*verse as i64),
            None,
            None,
            None,
        ),
        WikilinkTarget::Dict { module_id, topic } => (
            LinkTargetKind::Dict,
            None,
            None,
            None,
            None,
            None,
            Some(module_id.clone()),
            Some(topic.clone()),
        ),
        WikilinkTarget::Commentary {
            module_id,
            reference,
        } => (
            LinkTargetKind::Commentary,
            None,
            None,
            None,
            None,
            None,
            Some(module_id.clone()),
            Some(reference.clone()),
        ),
        WikilinkTarget::CrossReference {
            module_id,
            reference,
        } => (
            LinkTargetKind::Xref,
            None,
            None,
            None,
            None,
            None,
            Some(module_id.clone()),
            Some(reference.clone()),
        ),
        WikilinkTarget::Note(_) => (
            LinkTargetKind::Note,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ),
        // Chain targets are resolved at click time against `annotations.db`.
        // We don't index a target id here — the title in `target_raw` is the
        // lookup key, and the dispatcher does the work.
        WikilinkTarget::Chain(_) => (
            LinkTargetKind::Chain,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ),
    }
}

/// Find Obsidian-style block IDs (`^block-id`) at the end of paragraphs.
/// We look only at non-code bytes (`code_mask[i] == false`) so block IDs
/// inside code fences are ignored.
fn scan_blocks(content: &str, body_range: Range<usize>, code_mask: &[bool]) -> Vec<BlockRow> {
    let body = &content[body_range.clone()];
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if code_mask.get(i).copied().unwrap_or(false) {
            i += 1;
            continue;
        }
        if bytes[i] == b'^' && (i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'\n') {
            let id_start = i + 1;
            let mut id_end = id_start;
            while id_end < bytes.len()
                && (bytes[id_end].is_ascii_alphanumeric() || bytes[id_end] == b'-')
            {
                id_end += 1;
            }
            if id_end > id_start && (id_end == bytes.len() || bytes[id_end] == b'\n') {
                let id = body[id_start..id_end].to_string();
                let paragraph = paragraph_around(body, i);
                let absolute_offset = (i + body_range.start) as i64;
                out.push(BlockRow {
                    block_id: id,
                    byte_offset: absolute_offset,
                    text: paragraph,
                });
                i = id_end;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn paragraph_around(body: &str, byte_offset: usize) -> String {
    let bytes = body.as_bytes();
    let mut start = byte_offset;
    while start > 0 {
        if bytes[start - 1] == b'\n' && (start < 2 || bytes[start - 2] == b'\n') {
            break;
        }
        start -= 1;
    }
    let mut end = byte_offset;
    while end < bytes.len() {
        if bytes[end] == b'\n' && bytes.get(end + 1) == Some(&b'\n') {
            break;
        }
        end += 1;
    }
    while start < bytes.len() && !body.is_char_boundary(start) {
        start += 1;
    }
    while end <= bytes.len() && !body.is_char_boundary(end) {
        end += 1;
    }
    body[start..end].trim().to_string()
}

fn collect_tags(
    content: &str,
    body_range: Range<usize>,
    code_mask: &[bool],
    frontmatter: &Frontmatter,
) -> Vec<TagInsert> {
    let mut out = Vec::new();
    for tag in &frontmatter.tags {
        out.push(TagInsert {
            tag: tag.clone(),
            byte_offset: None,
        });
    }
    let body = &content[body_range.clone()];
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if code_mask.get(i).copied().unwrap_or(false) {
            i += 1;
            continue;
        }
        if bytes[i] == b'#' {
            let preceded_by_boundary = i == 0
                || matches!(
                    bytes[i - 1],
                    b' ' | b'\n' | b'\t' | b'(' | b'[' | b'{' | b','
                );
            if !preceded_by_boundary {
                i += 1;
                continue;
            }
            // `#` followed by another `#` is a heading marker — skip.
            if bytes.get(i + 1) == Some(&b'#') {
                i += 1;
                continue;
            }
            let name_start = i + 1;
            let mut name_end = name_start;
            while name_end < bytes.len() {
                let b = bytes[name_end];
                if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'/') {
                    name_end += 1;
                } else {
                    break;
                }
            }
            if name_end > name_start {
                let name = body[name_start..name_end].to_string();
                // Reject pure numbers (otherwise `#1` becomes a tag).
                if name.chars().any(|c| c.is_ascii_alphabetic()) {
                    let absolute = (i + body_range.start) as i64;
                    out.push(TagInsert {
                        tag: name,
                        byte_offset: Some(absolute),
                    });
                }
                i = name_end;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Strip markdown punctuation for FTS indexing. We don't need a perfect
/// renderer — FTS5's tokenizer is whitespace+punctuation aware — but we do
/// strip `[[…]]` brackets and code fences so the indexed text matches
/// what the user reads.
fn strip_markdown_for_fts(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '[' && chars.peek() == Some(&'[') {
            chars.next();
            // Read until `]]`, capture target / alias.
            let mut buffer = String::new();
            while let Some(c) = chars.next() {
                if c == ']' && chars.peek() == Some(&']') {
                    chars.next();
                    break;
                }
                buffer.push(c);
            }
            // Prefer alias if present.
            let display = match buffer.split_once('|') {
                Some((_, alias)) => alias.trim().to_string(),
                None => buffer
                    .split_once('#')
                    .map(|(target, _)| target.trim().to_string())
                    .unwrap_or_else(|| buffer.trim().to_string()),
            };
            // Drop scheme prefix `bible:` etc. for FTS.
            let display = display
                .split_once(':')
                .map(|(_, rest)| rest.trim().to_string())
                .unwrap_or(display);
            out.push_str(&display);
        } else if ch == '`' {
            // Drop backticks; FTS still tokenises the content.
            continue;
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(content: &str) -> ParsedNote {
        parse(content, None)
    }

    #[test]
    fn parses_headings() {
        let content = "# First\n## Second level\nbody\n";
        let parsed = parse_str(content);
        assert_eq!(parsed.headings.len(), 2);
        assert_eq!(parsed.headings[0].text, "First");
        assert_eq!(parsed.headings[0].slug, "first");
        assert_eq!(parsed.headings[0].level, 1);
        assert_eq!(parsed.headings[1].text, "Second level");
        assert_eq!(parsed.headings[1].slug, "second-level");
        assert_eq!(parsed.headings[1].level, 2);
    }

    #[test]
    fn extracts_wikilinks_with_embed_flag() {
        let content = "see [[Foo Bar]] and ![[Diagram.png]]\n";
        let parsed = parse_str(content);
        assert_eq!(parsed.links.len(), 2);
        let foo = &parsed.links[0];
        assert_eq!(foo.target_raw, "Foo Bar");
        assert!(matches!(foo.target_kind, LinkTargetKind::Note));
        assert!(!foo.is_embed);
        let diagram = &parsed.links[1];
        assert_eq!(diagram.target_raw, "Diagram.png");
        assert!(diagram.is_embed);
    }

    #[test]
    fn classifies_typed_targets() {
        let content = "see [[bible:John 3:16]] and [[dict:strongs/G3056]]\n";
        let parsed = parse_str(content);
        assert_eq!(parsed.links.len(), 2);
        assert!(matches!(parsed.links[0].target_kind, LinkTargetKind::Bible));
        assert_eq!(parsed.links[0].target_raw, "John 3:16");
        assert!(matches!(parsed.links[1].target_kind, LinkTargetKind::Dict));
        assert_eq!(parsed.links[1].target_module_id.as_deref(), Some("strongs"));
        assert_eq!(parsed.links[1].target_topic.as_deref(), Some("G3056"));
    }

    #[test]
    fn picks_up_block_ids() {
        let content = "First paragraph. ^abc\n\nSecond.\n^xyz\n";
        let parsed = parse_str(content);
        let ids: Vec<_> = parsed.blocks.iter().map(|b| b.block_id.as_str()).collect();
        assert_eq!(ids, vec!["abc", "xyz"]);
    }

    #[test]
    fn skips_block_ids_inside_code() {
        let content = "Paragraph.\n\n```\n^abc\n```\nbody\n";
        let parsed = parse_str(content);
        assert!(parsed.blocks.is_empty());
    }

    #[test]
    fn collects_inline_tags() {
        let content = "Some thoughts on #theology and #ethics here.\n";
        let parsed = parse_str(content);
        let names: Vec<_> = parsed.tags.iter().map(|t| t.tag.clone()).collect();
        assert_eq!(names, vec!["theology", "ethics"]);
    }

    #[test]
    fn skips_numeric_only_hash() {
        let content = "Verse #1 reference.\n";
        let parsed = parse_str(content);
        assert!(parsed.tags.is_empty());
    }

    #[test]
    fn skips_tags_in_code() {
        let content = "Plain #real then `#fake` then\n```\n#also-fake\n```\n";
        let parsed = parse_str(content);
        let names: Vec<_> = parsed.tags.iter().map(|t| t.tag.clone()).collect();
        assert_eq!(names, vec!["real"]);
    }

    #[test]
    fn merges_frontmatter_tags() {
        let content = "---\naliases: [Foo]\ntags: [theology]\n---\n#ethics here\n";
        let parsed = parse_str(content);
        assert_eq!(parsed.frontmatter.aliases, vec!["Foo"]);
        let names: Vec<_> = parsed.tags.iter().map(|t| t.tag.clone()).collect();
        assert!(names.contains(&"theology".to_string()));
        assert!(names.contains(&"ethics".to_string()));
        // Frontmatter tag has no byte_offset.
        let theology = parsed.tags.iter().find(|t| t.tag == "theology").unwrap();
        assert!(theology.byte_offset.is_none());
        let ethics = parsed.tags.iter().find(|t| t.tag == "ethics").unwrap();
        assert!(ethics.byte_offset.is_some());
    }

    #[test]
    fn snippet_includes_context() {
        let content = "lorem ipsum dolor [[Target]] sit amet\n";
        let parsed = parse_str(content);
        assert_eq!(parsed.links.len(), 1);
        assert!(parsed.links[0].src_snippet.contains("[[Target]]"));
    }

    #[test]
    fn line_numbers_are_one_based() {
        let content = "line one\nline two [[Target]]\n";
        let parsed = parse_str(content);
        assert_eq!(parsed.links[0].src_line, 2);
    }

    #[test]
    fn body_offsets_are_absolute() {
        let content = "---\naliases: [Foo]\n---\n# Heading\n[[Target]]";
        let parsed = parse_str(content);
        let h = &parsed.headings[0];
        // Heading byte_offset must lie in body region, not frontmatter.
        assert!(h.byte_offset > 0);
        let h_in_content = &content[h.byte_offset as usize..];
        assert!(h_in_content.starts_with("# Heading"));
        let l = &parsed.links[0];
        let l_in_content = &content[l.src_byte_start as usize..l.src_byte_end as usize];
        assert_eq!(l_in_content, "[[Target]]");
    }

    #[test]
    fn fts_body_drops_brackets_and_backticks() {
        let content = "see [[Foo|the foo]] and `code` here";
        let parsed = parse_str(content);
        assert!(parsed.body_for_fts.contains("the foo"));
        assert!(!parsed.body_for_fts.contains("[[Foo"));
        assert!(!parsed.body_for_fts.contains('`'));
    }

    #[test]
    fn resolve_note_targets_marks_broken_links() {
        let mut parsed = parse_str("[[Existing]] [[NotPresent]]");
        resolve_note_targets(&mut parsed, |name| match name {
            "Existing" => Some(42),
            _ => None,
        });
        assert_eq!(parsed.links[0].target_note_id, Some(42));
        assert!(matches!(parsed.links[0].target_kind, LinkTargetKind::Note));
        assert!(matches!(
            parsed.links[1].target_kind,
            LinkTargetKind::Broken
        ));
        assert!(parsed.links[1].target_note_id.is_none());
    }
}
