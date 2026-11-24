use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail, ensure};
use app_state::sync_data::{FORMAT_VERSION, Key, Kind, Record, Records};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use tempfile::{Builder, NamedTempFile};
use walkdir::WalkDir;

use super::journal::{Journal, records};
use super::{Conflict, SyncService};

#[derive(Serialize, Deserialize)]
struct Export {
    format: String,
    version: u32,
    records: Vec<Record>,
    #[serde(default)]
    conflicts: Vec<Conflict>,
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match File::open(path) {
        Ok(file) => Ok(Some(
            serde_json::from_reader(BufReader::new(file))
                .with_context(|| format!("Чтение {}", path.display()))?,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn write_json(path: &Path, value: &impl Serialize, pretty: bool) -> Result<()> {
    let parent = path.parent().context("Нет каталога для записи")?;
    fs::create_dir_all(parent)?;
    let mut file = NamedTempFile::new_in(parent)?;
    {
        let mut writer = BufWriter::new(file.as_file_mut());
        if pretty {
            serde_json::to_writer_pretty(&mut writer, value)?;
        } else {
            serde_json::to_writer(&mut writer, value)?;
        }
        writer.flush()?;
    }
    file.as_file().sync_all()?;
    file.persist(path)?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

pub fn vault_root(path: Option<PathBuf>) -> Result<Option<PathBuf>> {
    path.map(|path| {
        let root = path.canonicalize().context("Папка заметок недоступна")?;
        ensure!(root.is_dir(), "Папка заметок недоступна");
        Ok(root)
    })
    .transpose()
}

fn note_path(root: &Path, relpath: &str) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    for part in relpath.split('/') {
        ensure!(
            !part.is_empty()
                && !part.starts_with('.')
                && part != "node_modules"
                && !part.contains(['\\', ':', '\0']),
            "Недопустимый путь заметки"
        );
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(meta) => ensure!(
                !meta.file_type().is_symlink(),
                "Путь заметки содержит символическую ссылку"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    ensure!(
        path.extension() == Some(OsStr::new("md")),
        "Поддерживаются заметки Markdown"
    );
    Ok(path)
}

fn read_note(path: &Path) -> Result<Option<Value>> {
    match fs::read_to_string(path) {
        Ok(body) => Ok(Some(body.into())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn note_records(root: &Path) -> Result<Records> {
    let mut result = Records::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            entry.depth() == 0 || (!name.starts_with('.') && name != "node_modules")
        })
    {
        let entry = entry.context("Не удалось прочитать папку заметок")?;
        if entry.file_type().is_file() && entry.path().extension() == Some(OsStr::new("md")) {
            let id = notes_core::vault::relpath_for(root, entry.path())
                .context("Имя заметки не в UTF-8")?;
            let path = note_path(root, &id)?;
            let body = read_note(&path)?.context("Заметка изменилась при чтении")?;
            result.insert(
                Key {
                    kind: Kind::Note,
                    id,
                },
                body,
            );
        }
    }
    Ok(result)
}

// Serialize app writes against sync's compare-and-replace. Files edited by
// other programs are checked again immediately before being replaced.
pub(crate) static NOTE_WRITE: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

pub fn replace_note(
    root: &Path,
    id: &str,
    expected: Option<&Value>,
    target: Option<&Value>,
) -> Result<bool> {
    let _guard = NOTE_WRITE.lock();
    let path = note_path(root, id)?;
    let current = read_note(&path)?;
    if current.as_ref() == target {
        return Ok(true);
    }
    if current.as_ref() != expected {
        return Ok(false);
    }
    if let Some(value) = target {
        let body = value.as_str().context("Некорректный текст заметки")?;
        write_note(&path, body)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(true)
}

pub(crate) fn write_note(path: &Path, body: &str) -> Result<()> {
    let parent = path.parent().context("Нет каталога для заметки")?;
    fs::create_dir_all(parent)?;
    let mut file = NamedTempFile::new_in(parent)?;
    file.write_all(body.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

impl SyncService {
    pub(super) fn snapshot(&self, root: Option<&Path>) -> Result<Records> {
        let mut records = self
            .db
            .as_ref()
            .context("Хранилище приложения недоступно")?
            .sync_records()?;
        records.extend(
            self.cross_refs
                .as_ref()
                .context("Хранилище параллельных мест недоступно")?
                .sync_records()?,
        );
        if let Some(root) = root {
            records.extend(note_records(root)?);
        }
        Ok(records)
    }

    pub(super) fn replace(
        &self,
        root: Option<&Path>,
        key: &Key,
        expected: Option<&Value>,
        target: Option<&Value>,
    ) -> Result<bool> {
        match key.kind {
            Kind::Note => replace_note(
                root.context("Выберите папку для заметок")?,
                &key.id,
                expected,
                target,
            ),
            Kind::CrossReference => self
                .cross_refs
                .as_ref()
                .context("Хранилище параллельных мест недоступно")?
                .sync_replace(key, expected, target),
            _ => self
                .db
                .as_ref()
                .context("Хранилище приложения недоступно")?
                .sync_replace(key, expected, target),
        }
    }

    pub fn export(&self, parent: &Path, vault: Option<PathBuf>) -> Result<PathBuf> {
        let _operation = self.lock_journal()?;
        let root = vault_root(vault)?;
        let parent = parent.canonicalize()?;
        ensure!(
            !root.as_ref().is_some_and(|root| parent.starts_with(root)),
            "Выберите папку за пределами хранилища заметок"
        );
        let mut records = self.snapshot(root.as_deref())?;
        let conflicts = match self
            .read_journal()
            .and_then(|journal| Self::conflicts(&journal))
        {
            Ok(conflicts) => conflicts,
            Err(error) => {
                log::warn!("Could not include sync conflict variants in export: {error:#}");
                Vec::new()
            }
        };

        let temp = Builder::new()
            .prefix(".bibleown-export-")
            .tempdir_in(&parent)?;
        let notes_root = temp.path().join("notes");
        let mut note_keys = Vec::new();
        for (key, value) in &records {
            if key.kind == Kind::Note {
                let path = note_path(&notes_root, &key.id)?;
                write_note(&path, value.as_str().context("Некорректная заметка")?)?;
                note_keys.push(key.clone());
            }
        }
        for key in note_keys {
            records.remove(&key);
        }
        write_json(
            &temp.path().join("bibleown.json"),
            &Export {
                format: "bibleown-user-data".into(),
                version: FORMAT_VERSION,
                records: self::records(records),
                conflicts,
            },
            true,
        )?;
        let destination = parent.join(format!("BibleOWN-{}", uuid::Uuid::now_v7()));
        fs::rename(temp.path(), &destination)?;
        Ok(destination)
    }

    pub fn import(&self, folder: &Path, vault: Option<PathBuf>) -> Result<()> {
        let _operation = self.lock_journal()?;
        let exported: Export =
            read_json(&folder.join("bibleown.json"))?.context("В папке нет bibleown.json")?;
        ensure!(
            exported.format == "bibleown-user-data" && exported.version == FORMAT_VERSION,
            "Неизвестный формат экспорта"
        );
        let notes = folder.join("notes");
        let mut incoming: Records = exported
            .records
            .into_iter()
            .map(|record| (record.key, record.value))
            .collect();
        if notes.exists() {
            incoming.extend(note_records(
                &vault_root(Some(notes))?.context("Нет заметок")?,
            )?);
        }
        let root = vault_root(vault)?;
        if incoming.keys().any(|key| key.kind == Kind::Note) && root.is_none() {
            bail!("Укажите папку заметок перед импортом");
        }

        let mut journal: Journal = self.read_journal()?;
        self.bind_vault(&mut journal, root.as_deref())?;
        self.finish_apply(&mut journal, root.as_deref())?;
        let current = self.snapshot(root.as_deref())?;
        journal.capture(&current, root.is_some())?;
        for (key, value) in incoming {
            if current.get(&key) != Some(&value) {
                journal.commit(super::journal::Revision::new(key, Vec::new(), Some(value)))?;
            }
        }
        for conflict in exported.conflicts {
            let mut values: Vec<Option<Value>> = Vec::new();
            for variant in conflict.variants {
                if !values.contains(&variant.value)
                    && variant.value.as_ref() != current.get(&conflict.key)
                {
                    values.push(variant.value.clone());
                    journal.commit(super::journal::Revision::new(
                        conflict.key.clone(),
                        Vec::new(),
                        variant.value,
                    ))?;
                }
            }
        }
        self.save_journal(&journal)?;
        self.materialize(&mut journal, root.as_deref()).map(|_| ())
    }
}
