//! Vault filesystem operations.
//!
//! This module owns:
//!
//! - Walking `notes_dir` to find `.md` files (recursive, follows the
//!   conventional Obsidian rule of skipping `.git`, `.trash`, dot-prefixed
//!   directories, and the runtime `notes.db`/`annotations.db` files).
//! - Cheap file signatures (`mtime + blake3`) so the indexer can skip
//!   re-parsing files that haven't changed since the last scan.
//! - Initial-scan and deletion-detection passes that hand a flat list of
//!   `WorkItem`s to the orchestrator.
//!
//! The notify-based file watcher lives in `watcher.rs` (next step) and
//! emits events that map back to the same `WorkItem` shape.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context as _, Result};
use rayon::prelude::*;
use walkdir::{DirEntry, WalkDir};

use crate::store::{NoteRecord, NoteSignature, NotesStore};

/// One unit of work produced by the scanner.
#[derive(Debug, Clone)]
pub enum WorkItem {
    /// File needs (re-)parsing. Carries the signature so the orchestrator
    /// can pass it through to `NoteRecord` without re-stat'ing the file.
    Reindex {
        relpath: String,
        absolute: PathBuf,
        signature: NoteSignature,
    },
    /// File was on disk previously (it's in `notes.db`) but is gone now.
    Delete { relpath: String },
}

/// Compute the relative path of `path` against `root`, normalised to use
/// `/` as the separator.
pub fn relpath_for(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let mut out = String::with_capacity(rel.as_os_str().len());
    for (i, comp) in rel.components().enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(comp.as_os_str().to_str()?);
    }
    Some(out)
}

/// Read a file's signature without parsing its contents. Returns the same
/// shape we store in `note.mtime / note.blake3` so the orchestrator can do
/// a single equality check before deciding to re-parse.
pub fn read_signature(path: &Path) -> Result<NoteSignature> {
    let metadata =
        fs::metadata(path).with_context(|| format!("reading metadata for {}", path.display()))?;
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let bytes = fs::read(path).with_context(|| format!("reading {} for hash", path.display()))?;
    let hash = blake3::hash(&bytes).to_hex().to_string();
    Ok(NoteSignature {
        mtime,
        blake3: hash,
    })
}

/// Read a `.md` file's full contents.
pub fn read_to_string(path: &Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

/// Walk `notes_dir` and produce a list of `.md` paths, skipping conventional
/// hidden / runtime files. Returns `(absolute, relpath)` pairs.
pub fn collect_markdown_files(root: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !is_skipped(e))
    {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.path().extension() != Some(OsStr::new("md")) {
            continue;
        }
        let abs = entry.path().to_path_buf();
        let Some(rel) = relpath_for(root, &abs) else {
            continue;
        };
        out.push((abs, rel));
    }
    out
}

fn is_skipped(entry: &DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy();
    if entry.depth() == 0 {
        return false;
    }
    if entry.file_type().is_dir() {
        if name.starts_with('.')
            || name == "node_modules"
            || name == ".trash"
            || name == ".obsidian"
        {
            return true;
        }
    }
    matches!(
        name.as_ref(),
        "notes.db"
            | "notes.db-wal"
            | "notes.db-shm"
            | "annotations.db"
            | "annotations.db-wal"
            | "annotations.db-shm"
    )
}

/// Initial scan against an existing notes.db. Returns one `WorkItem` per
/// changed-or-new file plus deletion events for rows whose file is gone.
///
/// Signature checks run in parallel via rayon — for big vaults the scan is
/// I/O-bound, but blake3 is fast enough that the overhead is negligible.
pub fn scan(root: &Path, store: &NotesStore) -> Result<Vec<WorkItem>> {
    let on_disk = collect_markdown_files(root);
    let stored: std::collections::HashSet<String> = store.all_relpaths()?.into_iter().collect();

    let reindex: Vec<WorkItem> = on_disk
        .par_iter()
        .filter_map(|(abs, rel)| {
            let signature = match read_signature(abs) {
                Ok(s) => s,
                Err(err) => {
                    log::warn!(
                        target: "bible_notes::vault",
                        "skipping {}: {err:#}",
                        abs.display()
                    );
                    return None;
                }
            };
            let stored_signature = match store.note_signature(rel) {
                Ok(sig) => sig,
                Err(err) => {
                    log::warn!(
                        target: "bible_notes::vault",
                        "stored signature lookup failed for {rel}: {err:#}"
                    );
                    return None;
                }
            };
            if matches!(&stored_signature, Some(s) if s.mtime == signature.mtime && s.blake3 == signature.blake3)
            {
                return None;
            }
            Some(WorkItem::Reindex {
                relpath: rel.clone(),
                absolute: abs.clone(),
                signature,
            })
        })
        .collect();

    let on_disk_set: std::collections::HashSet<String> =
        on_disk.iter().map(|(_, r)| r.clone()).collect();

    let mut work: Vec<WorkItem> = reindex;
    work.extend(stored.iter().filter_map(|rel| {
        if on_disk_set.contains(rel) {
            None
        } else {
            Some(WorkItem::Delete {
                relpath: rel.clone(),
            })
        }
    }));
    Ok(work)
}

/// Build a `NoteRecord` from a freshly-read file. Caller is the orchestrator
/// — this exists in `vault.rs` so the parser doesn't depend on filesystem
/// types.
pub fn build_record(
    relpath: &str,
    signature: NoteSignature,
    parsed: crate::parser::ParsedNote,
) -> NoteRecord {
    let title = relpath
        .rsplit('/')
        .next()
        .unwrap_or(relpath)
        .strip_suffix(".md")
        .unwrap_or(relpath)
        .to_string();
    let body_chars = parsed.body_for_fts.chars().count() as i64;
    NoteRecord {
        relpath: relpath.to_string(),
        title,
        mtime: signature.mtime,
        blake3: signature.blake3,
        body_chars,
        body_for_fts: parsed.body_for_fts,
        aliases: parsed.frontmatter.aliases.clone(),
        headings: parsed.headings,
        blocks: parsed.blocks,
        links: parsed.links,
        tags: parsed.tags,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(dir: &Path, rel: &str, body: &str) {
        let abs = dir.join(rel);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&abs, body).unwrap();
    }

    #[test]
    fn collects_only_markdown_files() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "Foo.md", "");
        write(dir.path(), "Notes/Bar.md", "");
        write(dir.path(), "image.png", "");
        write(dir.path(), ".obsidian/config.json", "");
        let mut out: Vec<_> = collect_markdown_files(dir.path())
            .into_iter()
            .map(|(_, r)| r)
            .collect();
        out.sort();
        assert_eq!(out, vec!["Foo.md", "Notes/Bar.md"]);
    }

    #[test]
    fn relpath_uses_forward_slash() {
        let root = Path::new("/tmp/notes");
        let p = Path::new("/tmp/notes/Theology/Faith.md");
        assert_eq!(relpath_for(root, p).as_deref(), Some("Theology/Faith.md"));
    }

    #[test]
    fn signature_changes_on_edit() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a.md");
        fs::write(&path, "first").unwrap();
        let s1 = read_signature(&path).unwrap();
        // Force an mtime bump even on file systems that round to the second.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::write(&path, "second").unwrap();
        let s2 = read_signature(&path).unwrap();
        assert_ne!(s1.blake3, s2.blake3);
    }

    #[test]
    fn scan_returns_reindex_for_new_files() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "Foo.md", "# Foo\n");
        let store = crate::store::open_in_memory().unwrap();
        let work = scan(dir.path(), &store).unwrap();
        assert_eq!(work.len(), 1);
        assert!(matches!(work[0], WorkItem::Reindex { .. }));
    }

    #[test]
    fn scan_returns_delete_for_missing_files() {
        let dir = TempDir::new().unwrap();
        let store = crate::store::open_in_memory().unwrap();
        // Pre-seed with a phantom note.
        let parsed = crate::parser::parse("# Ghost\n", None);
        let record = build_record(
            "Ghost.md",
            NoteSignature {
                mtime: 1,
                blake3: "x".into(),
            },
            parsed,
        );
        store.replace_note_record(&record).unwrap();
        let work = scan(dir.path(), &store).unwrap();
        assert!(
            work.iter()
                .any(|w| matches!(w, WorkItem::Delete { relpath } if relpath == "Ghost.md"))
        );
    }

    #[test]
    fn scan_skips_unchanged_signature() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "A.md", "# Hello\n");
        let store = crate::store::open_in_memory().unwrap();
        // First scan — should yield reindex.
        let work = scan(dir.path(), &store).unwrap();
        // Apply it (so signature is recorded).
        for item in &work {
            if let WorkItem::Reindex {
                relpath,
                absolute,
                signature,
            } = item
            {
                let body = read_to_string(absolute).unwrap();
                let parsed = crate::parser::parse(&body, None);
                let record = build_record(relpath, signature.clone(), parsed);
                store.replace_note_record(&record).unwrap();
            }
        }
        // Second scan — same file, same hash, should yield no work.
        let work = scan(dir.path(), &store).unwrap();
        assert!(work.is_empty(), "expected no work, got {:?}", work);
    }
}
