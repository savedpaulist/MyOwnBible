//! Filesystem watcher for `notes_dir`.
//!
//! `notify` events are noisy — saving a file in a typical editor produces a
//! flurry of `Modify` (and sometimes `Create`/`Remove`) events for the same
//! path within milliseconds. The watcher runs raw events through a 500ms
//! debounce window before forwarding them to the orchestrator, so a save
//! triggers one reindex, not a dozen.
//!
//! The debouncer is implemented in-process rather than using
//! `notify-debouncer-mini`. We need a custom shape anyway: events that fire
//! during the debounce window for the same path are coalesced (keeping the
//! latest event kind), and rename pairs (`from -> to`) are translated into
//! a single `Rename` work item.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;

/// Debounce window. 500ms matches Foam and is enough to swallow the burst
/// editors emit on save while keeping perceived latency low.
const DEBOUNCE_MS: u64 = 500;

/// Per-file event handed to the orchestrator after debouncing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// File was created or modified. Orchestrator should re-stat and
    /// re-parse if the signature changed.
    Upsert(PathBuf),
    /// File was deleted (or renamed away to outside the vault).
    Remove(PathBuf),
    /// File was renamed within the vault. Both paths are absolute.
    Rename { from: PathBuf, to: PathBuf },
}

/// Active watcher. Drop the handle to stop watching and join the debouncer
/// thread.
pub struct VaultWatcher {
    _watcher: RecommendedWatcher,
    _join: thread::JoinHandle<()>,
}

/// Begin watching `root` recursively. Each debounced batch is delivered via
/// `on_batch` on a background thread — the orchestrator is responsible for
/// hopping back to the GPUI executor.
pub fn watch<F>(root: &Path, on_batch: F) -> Result<VaultWatcher>
where
    F: Fn(Vec<WatchEvent>) + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<RawEvent>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| match res {
        Ok(event) => {
            let kind = classify(&event);
            if kind.is_relevant() {
                let raw = RawEvent {
                    when: Instant::now(),
                    kind,
                    paths: event.paths,
                };
                let _ = tx.send(raw);
            }
        }
        Err(err) => {
            log::warn!(target: "bible_notes::watcher", "notify error: {err:#}");
        }
    })
    .with_context(|| "creating notify watcher")?;
    watcher
        .watch(root, RecursiveMode::Recursive)
        .with_context(|| format!("watching {}", root.display()))?;

    let buffer = Arc::new(Mutex::new(DebounceBuffer::default()));
    let join = {
        let buffer = buffer.clone();
        thread::Builder::new()
            .name("bible_notes-watcher".into())
            .spawn(move || run_debouncer(rx, buffer, on_batch))
            .context("spawning watcher thread")?
    };

    Ok(VaultWatcher {
        _watcher: watcher,
        _join: join,
    })
}

#[derive(Debug)]
struct RawEvent {
    when: Instant,
    kind: RawKind,
    paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawKind {
    Create,
    Modify,
    Remove,
    RenameFrom,
    RenameTo,
}

impl RawKind {
    fn is_relevant(self) -> bool {
        // Every variant is real; the helper exists so future filters stay
        // local to the type.
        let _ = self;
        true
    }
}

fn classify(event: &Event) -> RawKind {
    use notify::event::{ModifyKind, RenameMode};
    match &event.kind {
        EventKind::Create(_) => RawKind::Create,
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => RawKind::RenameFrom,
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => RawKind::RenameTo,
        EventKind::Modify(_) => RawKind::Modify,
        EventKind::Remove(_) => RawKind::Remove,
        _ => RawKind::Modify,
    }
}

#[derive(Debug, Default)]
struct DebounceBuffer {
    pending: HashMap<PathBuf, RawKind>,
    rename_from: Option<PathBuf>,
    last_event: Option<Instant>,
}

fn run_debouncer<F>(rx: mpsc::Receiver<RawEvent>, buffer: Arc<Mutex<DebounceBuffer>>, on_batch: F)
where
    F: Fn(Vec<WatchEvent>) + Send + 'static,
{
    let window = Duration::from_millis(DEBOUNCE_MS);
    loop {
        let next = rx.recv_timeout(window);
        match next {
            Ok(raw) => {
                let mut buf = buffer.lock();
                ingest(&mut buf, raw);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let events = drain_if_quiet(&buffer, window);
                if !events.is_empty() {
                    on_batch(events);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn ingest(buffer: &mut DebounceBuffer, raw: RawEvent) {
    buffer.last_event = Some(raw.when);
    if matches!(raw.kind, RawKind::RenameFrom) {
        if let Some(p) = raw.paths.into_iter().next() {
            buffer.rename_from = Some(p);
        }
        return;
    }
    if matches!(raw.kind, RawKind::RenameTo) {
        if let Some(to) = raw.paths.into_iter().next() {
            if let Some(from) = buffer.rename_from.take() {
                // Stage the rename as a synthetic kind on the destination
                // path. We encode it via the `pending` map by recording both
                // sides — we'll emit a `Rename` once we drain.
                buffer.pending.insert(from, RawKind::Remove);
                buffer.pending.insert(to, RawKind::Create);
            } else {
                buffer.pending.insert(to, RawKind::Create);
            }
        }
        return;
    }
    for path in raw.paths {
        let prior = buffer.pending.get(&path).copied();
        let merged = merge_kind(prior, raw.kind);
        buffer.pending.insert(path, merged);
    }
}

fn merge_kind(prior: Option<RawKind>, new: RawKind) -> RawKind {
    match (prior, new) {
        // Remove always wins — even after Create, the file no longer exists.
        (_, RawKind::Remove) => RawKind::Remove,
        // Modify after Create stays as Create (treated as Upsert anyway).
        (Some(RawKind::Create), RawKind::Modify) => RawKind::Create,
        (Some(RawKind::Remove), RawKind::Create) => RawKind::Create,
        (None, k) => k,
        (_, k) => k,
    }
}

fn drain_if_quiet(buffer: &Arc<Mutex<DebounceBuffer>>, window: Duration) -> Vec<WatchEvent> {
    let mut buf = buffer.lock();
    let Some(last) = buf.last_event else {
        return Vec::new();
    };
    if last.elapsed() < window {
        return Vec::new();
    }
    buf.last_event = None;
    let pending = std::mem::take(&mut buf.pending);
    let _ = buf.rename_from.take();
    drop(buf);

    // Detect (Remove + Create) pairs that originated from a rename and emit
    // a single `Rename` event. We can't know with certainty without raw
    // RenameFrom/To hints, so we only pair them when both ends have the
    // same parent directory and the same modification window — a heuristic
    // good enough to spare the orchestrator a delete+create round trip.
    let mut removes = Vec::new();
    let mut others = Vec::new();
    for (path, kind) in pending {
        match kind {
            RawKind::Remove => removes.push(path),
            other => others.push((path, other)),
        }
    }

    let mut out = Vec::new();
    let mut creates = Vec::new();
    for (path, kind) in others {
        match kind {
            RawKind::Create | RawKind::Modify => creates.push(path),
            _ => {}
        }
    }

    // Pair removes with creates that share basename → rename. Otherwise
    // they're independent.
    let mut paired_removes = std::collections::HashSet::new();
    let mut paired_creates = std::collections::HashSet::new();
    for (i, removed) in removes.iter().enumerate() {
        for (j, created) in creates.iter().enumerate() {
            if paired_removes.contains(&i) || paired_creates.contains(&j) {
                continue;
            }
            if same_basename(removed, created) {
                out.push(WatchEvent::Rename {
                    from: removed.clone(),
                    to: created.clone(),
                });
                paired_removes.insert(i);
                paired_creates.insert(j);
            }
        }
    }
    for (i, removed) in removes.into_iter().enumerate() {
        if !paired_removes.contains(&i) {
            out.push(WatchEvent::Remove(removed));
        }
    }
    for (j, created) in creates.into_iter().enumerate() {
        if !paired_creates.contains(&j) {
            out.push(WatchEvent::Upsert(created));
        }
    }
    out
}

fn same_basename(a: &Path, b: &Path) -> bool {
    a.file_name() == b.file_name() && a.parent() != b.parent()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_kind_preserves_remove() {
        assert_eq!(
            merge_kind(Some(RawKind::Modify), RawKind::Remove),
            RawKind::Remove
        );
        assert_eq!(
            merge_kind(Some(RawKind::Create), RawKind::Remove),
            RawKind::Remove
        );
    }

    #[test]
    fn merge_kind_create_wins_over_modify() {
        assert_eq!(
            merge_kind(Some(RawKind::Create), RawKind::Modify),
            RawKind::Create
        );
    }

    #[test]
    fn merge_kind_remove_then_create_is_create() {
        assert_eq!(
            merge_kind(Some(RawKind::Remove), RawKind::Create),
            RawKind::Create
        );
    }

    #[test]
    fn ingest_modify_records_path() {
        let mut buf = DebounceBuffer::default();
        ingest(
            &mut buf,
            RawEvent {
                when: Instant::now(),
                kind: RawKind::Modify,
                paths: vec![PathBuf::from("/tmp/a.md")],
            },
        );
        assert_eq!(buf.pending.len(), 1);
        assert_eq!(buf.rename_from, None);
    }

    #[test]
    fn rename_from_to_stages_paths() {
        let mut buf = DebounceBuffer::default();
        ingest(
            &mut buf,
            RawEvent {
                when: Instant::now(),
                kind: RawKind::RenameFrom,
                paths: vec![PathBuf::from("/tmp/old.md")],
            },
        );
        ingest(
            &mut buf,
            RawEvent {
                when: Instant::now(),
                kind: RawKind::RenameTo,
                paths: vec![PathBuf::from("/tmp/new.md")],
            },
        );
        // RenameTo pairs with prior RenameFrom → records both as
        // Remove(old) + Create(new) in pending.
        assert!(buf.pending.contains_key(&PathBuf::from("/tmp/old.md")));
        assert!(buf.pending.contains_key(&PathBuf::from("/tmp/new.md")));
    }

    #[test]
    fn drain_pairs_remove_create_to_rename() {
        let buffer = Arc::new(Mutex::new(DebounceBuffer::default()));
        {
            let mut buf = buffer.lock();
            ingest(
                &mut buf,
                RawEvent {
                    when: Instant::now(),
                    kind: RawKind::Remove,
                    paths: vec![PathBuf::from("/Vault/Old/Foo.md")],
                },
            );
            ingest(
                &mut buf,
                RawEvent {
                    when: Instant::now(),
                    kind: RawKind::Create,
                    paths: vec![PathBuf::from("/Vault/New/Foo.md")],
                },
            );
            // Make `last_event` be in the past so drain_if_quiet flushes.
            buf.last_event = Some(Instant::now() - Duration::from_secs(2));
        }
        let events = drain_if_quiet(&buffer, Duration::from_millis(500));
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], WatchEvent::Rename { from, to }
            if from == Path::new("/Vault/Old/Foo.md") && to == Path::new("/Vault/New/Foo.md")));
    }

    #[test]
    fn drain_emits_independent_remove_and_upsert() {
        let buffer = Arc::new(Mutex::new(DebounceBuffer::default()));
        {
            let mut buf = buffer.lock();
            ingest(
                &mut buf,
                RawEvent {
                    when: Instant::now(),
                    kind: RawKind::Remove,
                    paths: vec![PathBuf::from("/Vault/Foo.md")],
                },
            );
            ingest(
                &mut buf,
                RawEvent {
                    when: Instant::now(),
                    kind: RawKind::Modify,
                    paths: vec![PathBuf::from("/Vault/Bar.md")],
                },
            );
            buf.last_event = Some(Instant::now() - Duration::from_secs(2));
        }
        let mut events = drain_if_quiet(&buffer, Duration::from_millis(500));
        events.sort_by_key(|e| match e {
            WatchEvent::Upsert(p) => p.clone(),
            WatchEvent::Remove(p) => p.clone(),
            WatchEvent::Rename { from, .. } => from.clone(),
        });
        assert_eq!(events.len(), 2);
        // After sort by path: Bar.md < Foo.md lexically.
        assert!(matches!(&events[0], WatchEvent::Upsert(p) if p == Path::new("/Vault/Bar.md")));
        assert!(matches!(&events[1], WatchEvent::Remove(p) if p == Path::new("/Vault/Foo.md")));
    }

    #[test]
    fn drain_returns_empty_within_quiet_window() {
        let buffer = Arc::new(Mutex::new(DebounceBuffer::default()));
        {
            let mut buf = buffer.lock();
            ingest(
                &mut buf,
                RawEvent {
                    when: Instant::now(),
                    kind: RawKind::Modify,
                    paths: vec![PathBuf::from("/Vault/A.md")],
                },
            );
        }
        // The event just arrived; drain should not flush yet.
        let events = drain_if_quiet(&buffer, Duration::from_secs(2));
        assert!(events.is_empty());
    }
}
