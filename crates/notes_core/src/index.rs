//! Indexer orchestrator: turn `vault::WorkItem` and `watcher::WatchEvent`
//! streams into `notes.db` writes, and broadcast change events to
//! subscribers.
//!
//! Перенос `index.rs` форка без GPUI (адаптация, пометка в architecture.md):
//! - глобал `GlobalNotesIndex` не перенесён — владельцем `Arc<NotesIndex>`
//!   является shell::AppServices;
//! - `bible::ModuleRegistry` заменён на инъецированный `BibleResolver`
//!   (замыкание над bible_domain::reference::parse + mybible lookup_book);
//! - rename-очередь форка (PendingRename для переписывания ссылок при
//!   переименовании заметки) в v1 не перенесена: переименование файла
//!   обрабатывается как пара Delete+Reindex, ссылки на старое имя
//!   остаются Broken до правки вручную.
//!
//! Resolution of `[[Note]]` targets is two-phase: when we write note A, any
//! `[[B]]` whose target B is missing from the index becomes a `Broken`
//! row. Later, when we index B, we re-resolve every prior broken link by
//! `target_raw` and patch the `target_kind` / `target_note_id` columns.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use parking_lot::Mutex;

use crate::backlinks;
use crate::parser::{self, BibleResolver};
use crate::store::{NoteId, NoteSignature, NotesStore};
use crate::vault::{self, WorkItem};
use crate::watcher::WatchEvent;

#[derive(Debug, Clone)]
pub enum IndexEvent {
    /// One or more notes were upserted. Carries their relative paths so
    /// subscribers can decide whether the active panel needs a refresh.
    Updated { relpaths: Vec<String> },
    /// One or more notes were removed.
    Deleted { relpaths: Vec<String> },
}

pub struct NotesIndex {
    store: Arc<NotesStore>,
    indexing: Mutex<()>,
    notes_dir: PathBuf,
    resolver: Option<Arc<BibleResolver>>,
    subscribers: Mutex<Vec<Box<dyn Fn(&IndexEvent) + Send + Sync + 'static>>>,
}

impl NotesIndex {
    /// Open `notes_dir/notes.db` (creating it if missing) and prepare the
    /// orchestrator. The initial scan does not run automatically — call
    /// [`initial_scan`] for that (on a background thread).
    pub fn open(notes_dir: PathBuf, resolver: Option<Arc<BibleResolver>>) -> Result<Self> {
        let store = crate::store::open(&notes_dir)
            .with_context(|| format!("opening notes index at {}", notes_dir.display()))?;
        Ok(Self {
            store,
            indexing: Mutex::new(()),
            notes_dir,
            resolver,
            subscribers: Mutex::new(Vec::new()),
        })
    }

    pub fn store(&self) -> &Arc<NotesStore> {
        &self.store
    }

    pub fn notes_dir(&self) -> &Path {
        &self.notes_dir
    }

    /// Subscribe to index events. Returns an opaque token; drop the
    /// `SubscriptionToken` to unsubscribe.
    pub fn subscribe(
        self: &Arc<Self>,
        callback: impl Fn(&IndexEvent) + Send + Sync + 'static,
    ) -> SubscriptionToken {
        let mut subs = self.subscribers.lock();
        subs.push(Box::new(callback));
        SubscriptionToken {
            index: Arc::downgrade(self),
            position: subs.len() - 1,
        }
    }

    fn fire(&self, event: &IndexEvent) {
        let subs = self.subscribers.lock();
        for sub in subs.iter() {
            sub(event);
        }
    }

    /// Walk `notes_dir`, parse and write every changed file, delete
    /// vanished rows. Synchronous — run this on a background task.
    pub fn initial_scan(&self) -> Result<()> {
        let work = vault::scan(&self.notes_dir, &self.store)?;
        self.apply_work(work)
    }

    /// Translate a `WatchEvent` batch into the `WorkItem` shape the indexer
    /// understands and apply it.
    pub fn apply_watch_events(&self, events: Vec<WatchEvent>) -> Result<()> {
        let mut work = Vec::with_capacity(events.len() * 2);
        for event in events {
            match event {
                WatchEvent::Upsert(path) => {
                    let Some(rel) = vault::relpath_for(&self.notes_dir, &path) else {
                        continue;
                    };
                    if !path.extension().is_some_and(|e| e == "md") {
                        continue;
                    }
                    let Ok(signature) = vault::read_signature(&path) else {
                        continue;
                    };
                    let stored = self.store.note_signature(&rel)?;
                    if matches!(&stored, Some(s) if s.mtime == signature.mtime && s.blake3 == signature.blake3)
                    {
                        continue;
                    }
                    work.push(WorkItem::Reindex {
                        relpath: rel,
                        absolute: path,
                        signature,
                    });
                }
                WatchEvent::Remove(path) => {
                    let Some(rel) = vault::relpath_for(&self.notes_dir, &path) else {
                        continue;
                    };
                    if !path.extension().is_some_and(|e| e == "md") {
                        continue;
                    }
                    work.push(WorkItem::Delete { relpath: rel });
                }
                WatchEvent::Rename { from, to } => {
                    if let Some(from_rel) = vault::relpath_for(&self.notes_dir, &from) {
                        work.push(WorkItem::Delete { relpath: from_rel });
                    }
                    if let Some(to_rel) = vault::relpath_for(&self.notes_dir, &to)
                        && to.extension().is_some_and(|e| e == "md")
                        && let Ok(signature) = vault::read_signature(&to)
                    {
                        work.push(WorkItem::Reindex {
                            relpath: to_rel,
                            absolute: to,
                            signature,
                        });
                    }
                }
            }
        }
        self.apply_work(work)
    }

    fn apply_work(&self, work: Vec<WorkItem>) -> Result<()> {
        if work.is_empty() {
            return Ok(());
        }
        let mut updated = Vec::new();
        let mut deleted = Vec::new();
        let mut newly_indexed_ids = Vec::new();

        for item in work {
            let _indexing = self.indexing.lock();
            match item {
                WorkItem::Reindex {
                    relpath, absolute, ..
                } => {
                    // A watcher or an editor may have indexed a newer version during the scan.
                    let Ok(current) = vault::read_signature(&absolute) else {
                        continue;
                    };
                    if self.store.note_signature(&relpath)?.is_some_and(|stored| {
                        stored.mtime == current.mtime && stored.blake3 == current.blake3
                    }) {
                        continue;
                    }
                    match self.reindex_one(&relpath, &absolute, current) {
                        Ok(id) => {
                            updated.push(relpath);
                            newly_indexed_ids.push(id);
                        }
                        Err(err) => log::warn!(
                            target: "notes_core::index",
                            "reindex {relpath}: {err:#}"
                        ),
                    }
                }
                WorkItem::Delete { relpath } => {
                    if !self.notes_dir.join(&relpath).is_file()
                        && let Ok(true) = self.store.delete_note(&relpath)
                    {
                        deleted.push(relpath);
                    }
                }
            }
        }

        // Re-resolve any broken links that pointed at the newly-indexed
        // notes so backlinks queries pick them up immediately.
        for id in newly_indexed_ids {
            if let Err(err) = self.reresolve_broken_to(id) {
                log::warn!(target: "notes_core::index", "reresolve: {err:#}");
            }
        }

        if !updated.is_empty() {
            self.fire(&IndexEvent::Updated { relpaths: updated });
        }
        if !deleted.is_empty() {
            self.fire(&IndexEvent::Deleted { relpaths: deleted });
        }
        Ok(())
    }

    fn reindex_one(
        &self,
        relpath: &str,
        absolute: &Path,
        signature: NoteSignature,
    ) -> Result<NoteId> {
        let body = vault::read_to_string(absolute)?;
        let mut parsed = parser::parse(&body, self.resolver.as_deref());
        // Resolve note targets against current index state.
        let store = self.store.clone();
        parser::resolve_note_targets(&mut parsed, |raw| {
            store
                .note_id_by_relpath(&note_target_to_relpath(raw))
                .ok()
                .flatten()
        });
        let record = vault::build_record(relpath, signature, parsed);
        self.store.replace_note_record(&record)
    }

    /// After a note appears in the index, fix every prior `Broken` link
    /// whose `target_raw` resolves to its relpath.
    fn reresolve_broken_to(&self, new_id: NoteId) -> Result<()> {
        let Some(row) = self.store.get_note(new_id)? else {
            return Ok(());
        };
        backlinks::reresolve_broken_to_note(&self.store, new_id, &row.relpath)
    }
}

/// Subscribers stored as boxed closures keyed by insertion index. Drop the
/// token to remove. We don't compact the vector on drop (cost negligible —
/// subscribers are panel-lifetime).
pub struct SubscriptionToken {
    index: std::sync::Weak<NotesIndex>,
    position: usize,
}

impl Drop for SubscriptionToken {
    fn drop(&mut self) {
        let Some(index) = self.index.upgrade() else {
            return;
        };
        let mut subs = index.subscribers.lock();
        if self.position < subs.len() {
            // Replace with a no-op so positions of later subscribers stay
            // stable.
            subs[self.position] = Box::new(|_| {});
        }
    }
}

/// Translate the raw target of a `[[Note]]` link into the relpath the store
/// indexes by. Per the agreed convention, links use full paths
/// (`[[Theology/Faith]]`). We append `.md` if missing — Obsidian also
/// accepts naked names.
pub(crate) fn note_target_to_relpath(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.ends_with(".md") {
        trimmed.to_string()
    } else {
        format!("{trimmed}.md")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    fn write(dir: &Path, rel: &str, body: &str) {
        let abs = dir.join(rel);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&abs, body).unwrap();
    }

    #[test]
    fn initial_scan_indexes_files() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "Foo.md", "# Foo\n[[Bar]]\n");
        write(dir.path(), "Bar.md", "# Bar\n");
        let index = NotesIndex::open(dir.path().to_path_buf(), None).unwrap();
        index.initial_scan().unwrap();
        let foo = index.store.get_note_by_relpath("Foo.md").unwrap().unwrap();
        let bar = index.store.get_note_by_relpath("Bar.md").unwrap().unwrap();
        assert_eq!(foo.title, "Foo");
        assert_eq!(bar.title, "Bar");
    }

    #[test]
    fn bible_links_resolve_with_injected_resolver() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "A.md", "[[bible:Ин 3:16]]\n");
        let resolver: Arc<BibleResolver> = Arc::new(|raw| {
            bible_domain::reference::parse(raw, |name| {
                (bible_domain::reference::normalize_book_key(name) == "ин").then_some(500)
            })
        });
        let index = NotesIndex::open(dir.path().to_path_buf(), Some(resolver)).unwrap();
        index.initial_scan().unwrap();
        let hits = backlinks::backlinks_for_verse(index.store(), 500, 3, 16).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].target_raw, "Ин 3:16");
    }

    #[test]
    fn delete_event_removes_note() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "Foo.md", "Body");
        let index = NotesIndex::open(dir.path().to_path_buf(), None).unwrap();
        index.initial_scan().unwrap();
        assert!(index.store.get_note_by_relpath("Foo.md").unwrap().is_some());
        let abs = dir.path().join("Foo.md");
        fs::remove_file(&abs).unwrap();
        index
            .apply_watch_events(vec![WatchEvent::Remove(abs)])
            .unwrap();
        assert!(index.store.get_note_by_relpath("Foo.md").unwrap().is_none());
    }

    #[test]
    fn upsert_event_indexes_new_file() {
        let dir = TempDir::new().unwrap();
        let index = NotesIndex::open(dir.path().to_path_buf(), None).unwrap();
        index.initial_scan().unwrap();
        assert!(index.store.get_note_by_relpath("New.md").unwrap().is_none());

        write(dir.path(), "New.md", "# New\n");
        let abs = dir.path().join("New.md");
        index
            .apply_watch_events(vec![WatchEvent::Upsert(abs)])
            .unwrap();
        let row = index.store.get_note_by_relpath("New.md").unwrap();
        assert!(row.is_some());
    }

    #[test]
    fn rename_event_remaps_relpath() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "Old.md", "Body");
        let index = NotesIndex::open(dir.path().to_path_buf(), None).unwrap();
        index.initial_scan().unwrap();
        let old = dir.path().join("Old.md");
        let new = dir.path().join("New.md");
        fs::rename(&old, &new).unwrap();
        index
            .apply_watch_events(vec![WatchEvent::Rename { from: old, to: new }])
            .unwrap();
        assert!(index.store.get_note_by_relpath("Old.md").unwrap().is_none());
        assert!(index.store.get_note_by_relpath("New.md").unwrap().is_some());
    }

    #[test]
    fn subscribers_fire_on_updates() {
        let dir = TempDir::new().unwrap();
        let index = Arc::new(NotesIndex::open(dir.path().to_path_buf(), None).unwrap());
        let counter = Arc::new(AtomicUsize::new(0));
        let _token = {
            let counter = counter.clone();
            index.subscribe(move |evt| match evt {
                IndexEvent::Updated { .. } | IndexEvent::Deleted { .. } => {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            })
        };
        write(dir.path(), "A.md", "Body");
        index.initial_scan().unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        let abs = dir.path().join("A.md");
        fs::remove_file(&abs).unwrap();
        index
            .apply_watch_events(vec![WatchEvent::Remove(abs)])
            .unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn target_relpath_drops_dotmd_when_present() {
        assert_eq!(super::note_target_to_relpath("Foo.md"), "Foo.md");
        assert_eq!(super::note_target_to_relpath("Foo"), "Foo.md");
        assert_eq!(
            super::note_target_to_relpath("Theology/Faith"),
            "Theology/Faith.md"
        );
    }
}
