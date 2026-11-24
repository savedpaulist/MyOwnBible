//! YAML frontmatter parser for `.md` notes.
//!
//! Recognises the Obsidian-compatible block:
//!
//! ```text
//! ---
//! aliases: [Sermon, Mount Sermon]
//! tags: [theology, ethics]
//! verse: John 3:16
//! ---
//! Body of the note...
//! ```
//!
//! The opening `---` must be on the first line. The closing `---` must be on
//! its own line and is searched only within the file's leading prelude — if
//! it isn't found, we treat the file as having no frontmatter (matching
//! Obsidian's behaviour: a stray `---` further down is a thematic break).
//!
//! The parser returns the byte range of the frontmatter block (including the
//! trailing newline after the closing `---`) so the index can pick up
//! aliases/tags/title without re-scanning the whole file.

use std::collections::BTreeMap;

use anyhow::{Context as _, Result};
use serde::Deserialize;

/// Parsed frontmatter block.
#[derive(Debug, Default, Clone)]
pub struct Frontmatter {
    /// `aliases: [a, b]` — alternative titles for `[[wikilink]]` resolution.
    /// Single-string form `aliases: foo` is normalised into a one-element vec.
    pub aliases: Vec<String>,
    /// `tags: [a, b]` — tags from frontmatter (separate from inline `#tag`).
    /// Hash prefix is stripped: `tags: [#theology]` becomes `theology`.
    pub tags: Vec<String>,
    /// Title override from `title: …`. When `None`, callers fall back to the
    /// file's basename.
    pub title: Option<String>,
    /// All other key/value pairs preserved as YAML for round-tripping in the
    /// Properties widget.
    pub other: BTreeMap<String, serde_yaml::Value>,
    /// Byte range of the entire frontmatter block in the source, including
    /// the opening `---\n`, the YAML body, and the closing `---\n`.
    pub raw_range: std::ops::Range<usize>,
}

/// Strip the frontmatter block (if any) and return both the parsed
/// frontmatter and the byte range of the body that follows.
///
/// If the file lacks frontmatter, returns `Frontmatter::default()` with an
/// empty `raw_range` and the body range covering the whole file.
pub fn split(content: &str) -> Result<(Frontmatter, std::ops::Range<usize>)> {
    let Some(prefix) = content.strip_prefix("---") else {
        return Ok((Frontmatter::default(), 0..content.len()));
    };
    // The opening `---` must be followed by a newline (LF or CRLF).
    let after_open = if let Some(rest) = prefix.strip_prefix('\n') {
        rest
    } else if let Some(rest) = prefix.strip_prefix("\r\n") {
        rest
    } else {
        // `---` at start of file without a newline isn't a fence opener; treat
        // as plain content.
        return Ok((Frontmatter::default(), 0..content.len()));
    };
    let after_open_offset = content.len() - after_open.len();
    let Some((closing_offset, after_close_offset)) = find_closing_fence(content, after_open_offset)
    else {
        return Ok((Frontmatter::default(), 0..content.len()));
    };
    let yaml_str = &content[after_open_offset..closing_offset];
    let parsed: RawFrontmatter = if yaml_str.trim().is_empty() {
        RawFrontmatter::default()
    } else {
        serde_yaml::from_str::<serde_yaml::Value>(yaml_str)
            .with_context(|| "parsing frontmatter YAML")
            .and_then(|value| {
                serde_yaml::from_value::<RawFrontmatter>(value)
                    .with_context(|| "interpreting frontmatter")
            })
            .unwrap_or_else(|err| {
                log::warn!(target: "bible_notes::frontmatter", "{err:#}");
                RawFrontmatter::default()
            })
    };
    let frontmatter = Frontmatter::from_raw(parsed, 0..after_close_offset);
    Ok((frontmatter, after_close_offset..content.len()))
}

/// Internal representation that serde populates. We post-process into the
/// public `Frontmatter` shape so single-string `aliases:` / `tags:` work.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawFrontmatter {
    #[serde(deserialize_with = "string_or_seq", default)]
    aliases: Vec<String>,
    #[serde(deserialize_with = "string_or_seq", default)]
    tags: Vec<String>,
    title: Option<String>,
    #[serde(flatten)]
    other: BTreeMap<String, serde_yaml::Value>,
}

impl Frontmatter {
    fn from_raw(raw: RawFrontmatter, raw_range: std::ops::Range<usize>) -> Self {
        let RawFrontmatter {
            aliases,
            tags,
            title,
            other,
        } = raw;
        Self {
            aliases: aliases
                .into_iter()
                .map(|a| a.trim().to_string())
                .filter(|a| !a.is_empty())
                .collect(),
            tags: tags
                .into_iter()
                .map(|t| {
                    let trimmed = t.trim();
                    trimmed.strip_prefix('#').unwrap_or(trimmed).to_string()
                })
                .filter(|t| !t.is_empty())
                .collect(),
            title: title
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty()),
            other,
            raw_range,
        }
    }
}

/// Locate the `---` closing fence (on its own line) starting from
/// `after_open_offset`. Returns `(offset_of_closing, offset_after_newline)`.
fn find_closing_fence(content: &str, after_open_offset: usize) -> Option<(usize, usize)> {
    let mut search_offset = after_open_offset;
    while search_offset < content.len() {
        // Find the next line.
        let line_end = content[search_offset..]
            .find('\n')
            .map(|i| search_offset + i)
            .unwrap_or(content.len());
        let line = &content[search_offset..line_end];
        let stripped = line.trim_end_matches('\r');
        if stripped == "---" || stripped == "..." {
            // Found closing fence. The frontmatter byte range ends after the
            // newline that follows the closing fence.
            let after_newline = if line_end < content.len() {
                line_end + 1
            } else {
                line_end
            };
            return Some((search_offset, after_newline));
        }
        if line_end >= content.len() {
            return None;
        }
        search_offset = line_end + 1;
    }
    None
}

/// Accept either `aliases: foo` or `aliases: [foo, bar]`. serde_yaml's default
/// `Vec<String>` deserializer rejects scalars; this visitor unifies both.
fn string_or_seq<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    use std::fmt;

    struct V;

    impl<'de> Visitor<'de> for V {
        type Value = Vec<String>;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a string or a sequence of strings")
        }

        fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Self::Value, E> {
            Ok(vec![value.to_string()])
        }

        fn visit_string<E: de::Error>(self, value: String) -> std::result::Result<Self::Value, E> {
            Ok(vec![value])
        }

        fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: de::SeqAccess<'de>,
        {
            let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0));
            while let Some(item) = seq.next_element::<serde_yaml::Value>()? {
                if let Some(s) = item.as_str() {
                    out.push(s.to_string());
                } else if let Some(n) = item.as_i64() {
                    out.push(n.to_string());
                }
            }
            Ok(out)
        }

        fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }
    }

    deserializer.deserialize_any(V)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_frontmatter_returns_full_body() {
        let content = "# Hello\nbody";
        let (fm, body) = split(content).unwrap();
        assert!(fm.aliases.is_empty());
        assert!(fm.tags.is_empty());
        assert_eq!(fm.raw_range, 0..0);
        assert_eq!(body, 0..content.len());
    }

    #[test]
    fn parses_aliases_and_tags() {
        let content = "---\naliases: [Sermon, Mount Sermon]\ntags: [theology, ethics]\n---\nBody\n";
        let (fm, body) = split(content).unwrap();
        assert_eq!(fm.aliases, vec!["Sermon", "Mount Sermon"]);
        assert_eq!(fm.tags, vec!["theology", "ethics"]);
        assert_eq!(
            &content[fm.raw_range.clone()],
            "---\naliases: [Sermon, Mount Sermon]\ntags: [theology, ethics]\n---\n"
        );
        assert_eq!(&content[body], "Body\n");
    }

    #[test]
    fn accepts_string_alias_form() {
        let content = "---\naliases: Sermon\ntags: theology\n---\n";
        let (fm, _) = split(content).unwrap();
        assert_eq!(fm.aliases, vec!["Sermon"]);
        assert_eq!(fm.tags, vec!["theology"]);
    }

    #[test]
    fn strips_hash_prefix_from_tags() {
        let content = "---\ntags: [\"#theology\", \"#ethics\"]\n---\n";
        let (fm, _) = split(content).unwrap();
        assert_eq!(fm.tags, vec!["theology", "ethics"]);
    }

    #[test]
    fn captures_custom_fields() {
        let content = "---\nverse: John 3:16\nauthor: anon\n---\nBody";
        let (fm, body) = split(content).unwrap();
        assert!(fm.other.contains_key("verse"));
        assert!(fm.other.contains_key("author"));
        assert_eq!(&content[body], "Body");
    }

    #[test]
    fn missing_closing_fence_treats_as_no_frontmatter() {
        let content = "---\naliases: [Sermon]\nbody without closing";
        let (fm, body) = split(content).unwrap();
        assert!(fm.aliases.is_empty());
        assert_eq!(body, 0..content.len());
    }

    #[test]
    fn malformed_yaml_falls_back_to_empty() {
        let content = "---\nthis is :: not valid : yaml ::: at all\n---\nBody";
        let (fm, _body) = split(content).unwrap();
        assert!(fm.aliases.is_empty());
        assert!(fm.tags.is_empty());
        // Still consumes the block so body offset is past the closing ---.
        assert!(fm.raw_range.end > 0);
    }

    #[test]
    fn handles_crlf_line_endings() {
        let content = "---\r\naliases: [Foo]\r\n---\r\nBody";
        let (fm, body) = split(content).unwrap();
        assert_eq!(fm.aliases, vec!["Foo"]);
        assert_eq!(&content[body], "Body");
    }

    #[test]
    fn three_dot_closer_also_works() {
        let content = "---\naliases: [Foo]\n...\nBody";
        let (fm, body) = split(content).unwrap();
        assert_eq!(fm.aliases, vec!["Foo"]);
        assert_eq!(&content[body], "Body");
    }

    #[test]
    fn empty_frontmatter_block() {
        let content = "---\n---\nBody";
        let (fm, body) = split(content).unwrap();
        assert!(fm.aliases.is_empty());
        assert!(fm.tags.is_empty());
        assert_eq!(&content[body], "Body");
    }

    #[test]
    fn three_dashes_without_newline_is_plain_text() {
        let content = "---hello";
        let (fm, body) = split(content).unwrap();
        assert_eq!(fm.raw_range, 0..0);
        assert_eq!(body, 0..content.len());
    }

    #[test]
    fn title_field_extracted() {
        let content = "---\ntitle: My Note\n---\nBody";
        let (fm, _) = split(content).unwrap();
        assert_eq!(fm.title.as_deref(), Some("My Note"));
    }
}
