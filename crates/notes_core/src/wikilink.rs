//! Wikilink parser for `[[target]]`, `[[target|alias]]`,
//! `[[target#fragment]]`, and `[[target#fragment|alias]]` syntax.
//!
//! Recognised target shapes:
//!
//!   `[[bible:John 3:16]]`            -> Bible chapter / verse
//!   `[[dict:<module>/<topic>]]`      -> dictionary article
//!   `[[commentary:<module>/<ref>]]`  -> commentary entry for a Bible reference
//!   `[[xref:<module>/<ref>]]`        -> cross-reference list for a Bible verse
//!   `[[note-name]]`                  -> Markdown file inside `bible.notes_dir`
//!
//! Bare targets without a typed prefix default to `Note`. The `dispatch`
//! callsite implements an Obsidian-compatible fallback: when a `Note` target
//! parses as a valid Bible reference (e.g. `[[John 3:16]]`) and no matching
//! note file exists, it is dispatched as a Bible link.
//!
//! The parser is byte-scan only; it does no markdown-AST work and is safe to
//! run on any string containing balanced `[[...]]` pairs. Wikilinks inside
//! triple-backtick fences are filtered out by `find_around_offset` via a
//! simple state machine.

use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wikilink {
    pub raw_range: Range<usize>,
    pub target: WikilinkTarget,
    pub fragment: Option<String>,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WikilinkTarget {
    /// Free-text Bible reference (`John 3:16`, `Иоан 3:16`).
    Bible(String),
    /// Numeric Bible reference produced when MyBible dictionary articles use
    /// `<a href="B:540 4:1">` style links — those carry a book *number*, not a
    /// localized name, so we preserve the integers instead of round-tripping
    /// through `parse_reference`.
    BibleNumeric {
        book: i64,
        chapter: u16,
        verse: Option<u16>,
        end_verse: Option<u16>,
    },
    /// Cross-references at a numeric verse (used by end-of-line gutter markers).
    /// Differs from `CrossReference` (which targets a specific module) by
    /// querying *all* loaded cross-reference modules and de-duplicating.
    XrefAtVerse {
        book: i64,
        chapter: u16,
        verse: u16,
    },
    /// User-authored comment list at a numeric verse (used by end-of-line
    /// gutter markers). Click opens the comment view modal which lets the
    /// user read, edit, add, and delete comments.
    CommentAtVerse {
        book: i64,
        chapter: u16,
        verse: u16,
    },
    Dict {
        module_id: String,
        topic: String,
    },
    Commentary {
        module_id: String,
        reference: String,
    },
    CrossReference {
        module_id: String,
        reference: String,
    },
    /// Logical chain — opens the Chains panel and surfaces the named chain (or
    /// the node identified by `fragment` on the outer `Wikilink`). The name is
    /// matched case-insensitively against `chains.title` in `annotations.db`.
    Chain(String),
    Note(String),
}

/// Find the wikilink that contains `offset`, if any. Skips over wikilinks
/// inside fenced code blocks so triple-backtick examples don't fire.
pub fn find_around_offset(text: &str, offset: usize) -> Option<Wikilink> {
    if !inside_prose(text, offset) {
        return None;
    }
    scan(text)
        .into_iter()
        .find(|link| link.raw_range.start <= offset && offset <= link.raw_range.end)
}

/// Find every wikilink in `text`. Code-fenced regions are skipped.
pub fn scan(text: &str) -> Vec<Wikilink> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut idx = 0;
    let mut in_fence = false;
    while idx + 1 < bytes.len() {
        if !in_fence
            && bytes[idx] == b'`'
            && bytes.get(idx + 1) == Some(&b'`')
            && bytes.get(idx + 2) == Some(&b'`')
        {
            in_fence = true;
            idx += 3;
            continue;
        }
        if in_fence
            && bytes[idx] == b'`'
            && bytes.get(idx + 1) == Some(&b'`')
            && bytes.get(idx + 2) == Some(&b'`')
        {
            in_fence = false;
            idx += 3;
            continue;
        }
        if in_fence {
            idx += 1;
            continue;
        }
        if bytes[idx] == b'[' && bytes.get(idx + 1) == Some(&b'[') {
            if let Some((link, end)) = parse_at(text, idx) {
                out.push(link);
                idx = end;
                continue;
            }
        }
        idx += 1;
    }
    out
}

fn parse_at(text: &str, at: usize) -> Option<(Wikilink, usize)> {
    let bytes = text.as_bytes();
    let inside_start = at + 2;
    let mut idx = inside_start;
    while idx + 1 < bytes.len() {
        if bytes[idx] == b']' && bytes.get(idx + 1) == Some(&b']') {
            let inside = &text[inside_start..idx];
            let end = idx + 2;
            let link = parse_inside(inside, at..end)?;
            return Some((link, end));
        }
        if bytes[idx] == b'\n' {
            // Wikilinks do not span lines.
            return None;
        }
        idx += 1;
    }
    None
}

fn parse_inside(inside: &str, raw_range: Range<usize>) -> Option<Wikilink> {
    let trimmed = inside.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (target_with_fragment, alias) = match trimmed.split_once('|') {
        Some((target, alias)) => (target.trim(), Some(alias.trim().to_string())),
        None => (trimmed, None),
    };
    let (target_str, fragment) = match target_with_fragment.split_once('#') {
        Some((target, frag)) => (target.trim(), Some(frag.trim().to_string())),
        None => (target_with_fragment, None),
    };
    let target = parse_target(target_str)?;
    Some(Wikilink {
        raw_range,
        target,
        fragment,
        alias,
    })
}

fn parse_target(raw: &str) -> Option<WikilinkTarget> {
    if let Some(rest) = raw.strip_prefix("bible:") {
        let body = rest.trim();
        if body.is_empty() {
            return None;
        }
        // `bible:@540/4:1` or `bible:@540/4:1-5` — numeric book id, used by
        // dictionary `B:NUM CHAP:VERSE` rewriting in `dictionary::strip_html`.
        // `@` is the disambiguator — `#` is reserved for fragment syntax and
        // gets eaten by `parse_inside` before we see it.
        if let Some(numeric) = parse_numeric_target(body) {
            return Some(numeric);
        }
        return Some(WikilinkTarget::Bible(body.to_string()));
    }
    if let Some(rest) = raw.strip_prefix("dict:") {
        let body = rest.trim();
        let (module_id, topic) = body.split_once('/')?;
        let module_id = module_id.trim();
        let topic = topic.trim();
        if module_id.is_empty() || topic.is_empty() {
            return None;
        }
        return Some(WikilinkTarget::Dict {
            module_id: module_id.to_string(),
            topic: topic.to_string(),
        });
    }
    if let Some(rest) = raw.strip_prefix("commentary:") {
        let body = rest.trim();
        let (module_id, reference) = body.split_once('/')?;
        let module_id = module_id.trim();
        let reference = reference.trim();
        if module_id.is_empty() || reference.is_empty() {
            return None;
        }
        return Some(WikilinkTarget::Commentary {
            module_id: module_id.to_string(),
            reference: reference.to_string(),
        });
    }
    if let Some(rest) = raw.strip_prefix("xref:") {
        let body = rest.trim();
        // Numeric form `xref:@BOOK/CHAP:VERSE` (no module): query every loaded
        // cross-reference module. Used by end-of-line markers. `@` is chosen
        // over `#` to avoid the latter being treated as a fragment separator.
        if let Some(numeric) = body.strip_prefix('@') {
            return parse_verse_marker_target(numeric, MarkerKind::Xref);
        }
        let (module_id, reference) = body.split_once('/')?;
        let module_id = module_id.trim();
        let reference = reference.trim();
        if module_id.is_empty() || reference.is_empty() {
            return None;
        }
        return Some(WikilinkTarget::CrossReference {
            module_id: module_id.to_string(),
            reference: reference.to_string(),
        });
    }
    if let Some(rest) = raw.strip_prefix("comment:") {
        // `comment:@BOOK/CHAP:VERSE` — opens the comment view modal.
        let body = rest.trim().strip_prefix('@')?;
        return parse_verse_marker_target(body, MarkerKind::Comment);
    }
    if let Some(rest) = raw.strip_prefix("note:") {
        let body = rest.trim();
        if body.is_empty() {
            return None;
        }
        return Some(WikilinkTarget::Note(body.to_string()));
    }
    if let Some(rest) = raw.strip_prefix("chain:") {
        let body = rest.trim();
        if body.is_empty() {
            return None;
        }
        return Some(WikilinkTarget::Chain(body.to_string()));
    }
    Some(WikilinkTarget::Note(raw.trim().to_string()))
}

enum MarkerKind {
    Xref,
    Comment,
}

/// Parse `BOOK/CHAP:VERSE` body into a `XrefAtVerse` or `CommentAtVerse`
/// target. The verse component is mandatory — markers always pin to a
/// specific verse, never a chapter range.
fn parse_verse_marker_target(body: &str, kind: MarkerKind) -> Option<WikilinkTarget> {
    let body = body.trim();
    let (book_str, rest) = body.split_once('/')?;
    let book: i64 = book_str.trim().parse().ok()?;
    let (chapter_str, verse_str) = rest.split_once(':')?;
    let chapter: u16 = chapter_str.trim().parse().ok()?;
    let verse: u16 = verse_str.trim().parse().ok()?;
    Some(match kind {
        MarkerKind::Xref => WikilinkTarget::XrefAtVerse {
            book,
            chapter,
            verse,
        },
        MarkerKind::Comment => WikilinkTarget::CommentAtVerse {
            book,
            chapter,
            verse,
        },
    })
}

/// Parse `@NUM/CHAP:VERSE` (with optional `-VERSE`) into a numeric Bible
/// target. The leading `@` is what distinguishes it from name-based forms;
/// `dictionary::strip_html` emits this so the wikilink layer can dispatch
/// without re-parsing the localised book name.
/// Public entry-point for parsing the URL portion of a `bible:` markdown
/// link (`bible:@N/CH:V` or `bible:@N/CH:V-V2`). Unlike `scan`, this works
/// on the URL string itself — used by hover-popover URL routing where the
/// markdown renderer has already extracted the destination from
/// `[label](bible:@…)`.
pub fn parse_bible_url(url: &str) -> Option<WikilinkTarget> {
    let body = url.strip_prefix("bible:")?.trim();
    parse_numeric_target(body)
}

fn parse_numeric_target(body: &str) -> Option<WikilinkTarget> {
    let body = body.strip_prefix('@')?;
    let (book_str, rest) = body.split_once('/')?;
    let book: i64 = book_str.trim().parse().ok()?;
    let (chapter_str, verse_part) = match rest.split_once(':') {
        Some((chap, verse)) => (chap, Some(verse)),
        None => (rest, None),
    };
    let chapter: u16 = chapter_str.trim().parse().ok()?;
    let (verse, end_verse) = match verse_part.map(|v| v.trim()) {
        Some(spec) if !spec.is_empty() => match spec.split_once('-') {
            Some((v, end)) => (v.trim().parse::<u16>().ok(), end.trim().parse::<u16>().ok()),
            None => (spec.parse::<u16>().ok(), None),
        },
        _ => (None, None),
    };
    Some(WikilinkTarget::BibleNumeric {
        book,
        chapter,
        verse,
        end_verse,
    })
}

fn inside_prose(text: &str, offset: usize) -> bool {
    let bytes = text.as_bytes();
    let mut idx = 0;
    let mut in_fence = false;
    while idx < offset && idx + 2 < bytes.len() {
        if bytes[idx] == b'`'
            && bytes.get(idx + 1) == Some(&b'`')
            && bytes.get(idx + 2) == Some(&b'`')
        {
            in_fence = !in_fence;
            idx += 3;
            continue;
        }
        idx += 1;
    }
    !in_fence
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bible_target() {
        let link = scan("see [[bible:John 3:16]] please");
        assert_eq!(link.len(), 1);
        let link = &link[0];
        assert_eq!(link.target, WikilinkTarget::Bible("John 3:16".to_string()));
        assert_eq!(link.fragment, None);
        assert_eq!(link.alias, None);
        assert_eq!(
            &"see [[bible:John 3:16]] please"[link.raw_range.clone()],
            "[[bible:John 3:16]]"
        );
    }

    #[test]
    fn parses_dict_target() {
        let link = scan("[[dict:strongs/G2570]]");
        assert_eq!(link.len(), 1);
        assert_eq!(
            link[0].target,
            WikilinkTarget::Dict {
                module_id: "strongs".to_string(),
                topic: "G2570".to_string(),
            }
        );
    }

    #[test]
    fn parses_note_with_alias() {
        let link = scan("[[Sermon on the Mount|the Sermon]]");
        assert_eq!(link.len(), 1);
        assert_eq!(
            link[0].target,
            WikilinkTarget::Note("Sermon on the Mount".to_string())
        );
        assert_eq!(link[0].alias.as_deref(), Some("the Sermon"));
    }

    #[test]
    fn parses_fragment() {
        let link = scan("[[Notes#Beatitudes]]");
        assert_eq!(link.len(), 1);
        assert_eq!(link[0].fragment.as_deref(), Some("Beatitudes"));
    }

    #[test]
    fn parses_fragment_and_alias() {
        let link = scan("[[Notes#Beatitudes|the Beatitudes]]");
        assert_eq!(link.len(), 1);
        assert_eq!(link[0].fragment.as_deref(), Some("Beatitudes"));
        assert_eq!(link[0].alias.as_deref(), Some("the Beatitudes"));
    }

    #[test]
    fn skips_code_fenced_links() {
        let text = "before [[real]] middle\n```\n[[fenced]]\n```\nafter [[real2]]";
        let links = scan(text);
        let targets: Vec<_> = links
            .iter()
            .map(|l| match &l.target {
                WikilinkTarget::Note(name) => name.clone(),
                _ => "?".to_string(),
            })
            .collect();
        assert_eq!(targets, vec!["real".to_string(), "real2".to_string()]);
    }

    #[test]
    fn does_not_span_newline() {
        let links = scan("[[broken\nlink]]");
        assert!(links.is_empty());
    }

    #[test]
    fn find_around_offset_locates_link() {
        let text = "see [[bible:John 3:16]] please";
        let inside = text.find("John").unwrap();
        let found = find_around_offset(text, inside).unwrap();
        assert_eq!(found.target, WikilinkTarget::Bible("John 3:16".to_string()));
    }

    #[test]
    fn find_around_offset_returns_none_outside_link() {
        let text = "no wikilink here";
        assert!(find_around_offset(text, 5).is_none());
    }

    #[test]
    fn rejects_empty_target() {
        let links = scan("[[]]");
        assert!(links.is_empty());
    }

    #[test]
    fn rejects_invalid_dict() {
        let links = scan("[[dict:onlymodule]]");
        assert!(links.is_empty());
    }

    #[test]
    fn parses_xref_at_verse_numeric() {
        let links = scan("see [[xref:@10/1:1]]");
        assert_eq!(
            links[0].target,
            WikilinkTarget::XrefAtVerse {
                book: 10,
                chapter: 1,
                verse: 1
            }
        );
    }

    #[test]
    fn parses_comment_at_verse() {
        let links = scan("note [[comment:@500/3:16]]");
        assert_eq!(
            links[0].target,
            WikilinkTarget::CommentAtVerse {
                book: 500,
                chapter: 3,
                verse: 16
            }
        );
    }

    #[test]
    fn parses_bible_numeric() {
        // The dictionary HTML rewrite emits `bible:@BOOK/CHAP:VERSE` for
        // `<a href="B:540 4:1">` style links.
        let links = scan("[[bible:@540/4:1]]");
        assert_eq!(
            links[0].target,
            WikilinkTarget::BibleNumeric {
                book: 540,
                chapter: 4,
                verse: Some(1),
                end_verse: None
            }
        );
    }

    #[test]
    fn parses_bible_numeric_range() {
        let links = scan("[[bible:@540/4:1-5]]");
        assert_eq!(
            links[0].target,
            WikilinkTarget::BibleNumeric {
                book: 540,
                chapter: 4,
                verse: Some(1),
                end_verse: Some(5)
            }
        );
    }

    #[test]
    fn xref_with_module_still_parses() {
        // The module-form must keep working alongside the new numeric form.
        let links = scan("[[xref:tskBible/Gen 1:1]]");
        assert!(matches!(
            links[0].target,
            WikilinkTarget::CrossReference { .. }
        ));
    }
}
