//! Direct, opt-in replication of user records through Google Drive.
mod credentials;
mod drive;
mod files;
mod journal;

use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, ensure};
use app_state::{
    db::StateDb,
    sync_data::{FORMAT_VERSION, Key},
    user_cross_refs::UserCrossRefs,
};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use self::drive::{Client, Drive, Login};
use self::files::{read_json, vault_root, write_json};
use self::journal::{Apply, Batch, Journal, Materialized, Revision, Upload};

pub use app_state::sync_data::Kind;

pub(crate) use files::{NOTE_WRITE, write_note};

#[derive(Clone, Serialize, Deserialize)]
pub struct Variant {
    pub id: Uuid,
    pub value: Option<Value>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Conflict {
    pub key: Key,
    pub variants: Vec<Variant>,
}

#[derive(Default, Clone)]
pub struct Status {
    pub configured: bool,
    pub bundled_client: bool,
    pub connected: bool,
    pub busy: bool,
    pub last_success: Option<u64>,
    pub error: Option<String>,
    pub conflicts: Vec<Conflict>,
}

pub struct SyncService {
    directory: PathBuf,
    db: Option<Arc<StateDb>>,
    cross_refs: Option<Arc<UserCrossRefs>>,
    status: RwLock<Status>,
    client: RwLock<Option<Client>>,
    access: Mutex<Option<Drive>>,
}

impl SyncService {
    /// Create the sync use-case service from the application composition root.
    /// UI code does not know storage paths or concrete database adapters.
    pub fn for_services(services: &crate::services::AppServices) -> Arc<Self> {
        Self::new(
            app_state::paths::data_dir().join("sync"),
            services.state_db.clone(),
            services.user_cross_refs.clone(),
        )
    }

    pub fn new(
        directory: PathBuf,
        db: Option<Arc<StateDb>>,
        cross_refs: Option<Arc<UserCrossRefs>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            directory,
            db,
            cross_refs,
            status: RwLock::new(Status::default()),
            client: RwLock::new(None),
            access: Mutex::new(None),
        })
    }

    pub fn initialize(&self) -> Result<()> {
        let _operation = self.lock_journal()?;
        let journal = self.read_journal()?;
        let bundled = Client::bundled()?.filter(|client| {
            journal
                .client_id
                .as_deref()
                .is_none_or(|id| id == client.id())
        });
        self.status.write().bundled_client = bundled.is_some();
        *self.client.write() = match bundled {
            Some(client) => Some(client),
            None => read_json::<Client>(&self.client_path())?,
        };
        if journal.enabled {
            ensure!(
                self.client.read().as_ref().map(Client::id) == journal.client_id.as_deref(),
                "Восстановите OAuth-клиент, с которым подключён Google Drive"
            );
        }
        self.publish(&journal)?;
        Ok(())
    }

    fn client_path(&self) -> PathBuf {
        self.directory.join("google-desktop.json")
    }

    fn lock_journal(&self) -> Result<File> {
        std::fs::create_dir_all(&self.directory)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.directory.join("journal.lock"))?;
        lock.try_lock()
            .context("Журнал синхронизации уже открыт другим процессом")?;
        Ok(lock)
    }

    fn journal_path(&self) -> PathBuf {
        self.directory.join("journal-v1.json")
    }

    fn read_journal(&self) -> Result<Journal> {
        Ok(read_json(&self.journal_path())?.unwrap_or_default())
    }

    fn save_journal(&self, journal: &Journal) -> Result<()> {
        write_json(&self.journal_path(), journal, false)
    }

    pub fn status(&self) -> Status {
        self.status.read().clone()
    }

    pub fn report_error(&self, error: &anyhow::Error) {
        self.status.write().error = Some(format!("{error:#}"));
    }

    pub fn configure(&self, path: &Path) -> Result<()> {
        let _operation = self.lock_journal()?;
        let client = Client::from_google_file(path)?;
        let journal = self.read_journal()?;
        ensure!(!journal.enabled, "Сначала отключите синхронизацию");
        ensure!(
            journal
                .client_id
                .as_deref()
                .is_none_or(|id| id == client.id()),
            "Для этой библиотеки уже выбран другой OAuth-клиент"
        );
        write_json(&self.client_path(), &client, true)?;
        *self.client.write() = Some(client);
        self.publish(&journal)
    }

    pub fn start_login(&self) -> Result<Login> {
        self.client
            .read()
            .as_ref()
            .context("Добавьте файл OAuth-клиента Google")?
            .start_login()
    }

    pub fn complete_login(&self, login: Login) -> Result<()> {
        let _operation = self.lock_journal()?;
        let mut journal = self.read_journal()?;
        let client = self
            .client
            .read()
            .clone()
            .context("Добавьте файл OAuth-клиента Google")?;
        let login_changed = client.id() != login.client_id();
        ensure!(
            !login_changed,
            "OAuth-клиент изменился. Начните подключение заново"
        );
        let (drive, refresh) = login.wait()?;
        let account = drive.account_id()?;
        ensure!(
            journal.account.as_deref().is_none_or(|id| id == account),
            "Эта библиотека привязана к другому Google Drive"
        );
        credentials::store(&self.credential_id(), &refresh)?;
        journal.account = Some(account);
        journal.client_id = Some(client.id().to_owned());
        journal.enabled = true;
        self.save_journal(&journal)?;
        *self.access.lock() = Some(drive);
        self.publish(&journal)
    }

    pub fn disconnect(&self) -> Result<()> {
        let _operation = self.lock_journal()?;
        let mut journal = self.read_journal()?;
        journal.enabled = false;
        self.save_journal(&journal)?;
        *self.access.lock() = None;
        self.publish(&journal)?;
        credentials::clear(&self.credential_id())
    }

    fn credential_id(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(self.directory.to_string_lossy().as_bytes())
        )
    }

    pub fn run(&self, vault: Option<PathBuf>) -> Result<bool> {
        let _operation = self.lock_journal()?;
        if !self.status.read().connected {
            return Ok(false);
        }
        self.status.write().busy = true;
        let result = self.sync(vault);
        {
            let mut status = self.status.write();
            status.busy = false;
            status.error = result.as_ref().err().map(|error| format!("{error:#}"));
        }
        result
    }

    fn sync(&self, vault: Option<PathBuf>) -> Result<bool> {
        let mut journal = self.read_journal()?;
        if !journal.enabled {
            return Ok(false);
        }

        let root = vault_root(vault)?;
        let new_vault = root.is_some() && journal.vault.is_none();
        self.bind_vault(&mut journal, root.as_deref())?;
        let recovered = self.finish_apply(&mut journal, root.as_deref())?;
        let pending = journal.outgoing.len();
        journal.capture(&self.snapshot(root.as_deref())?, root.is_some())?;
        if new_vault || journal.outgoing.len() != pending {
            self.save_journal(&journal)?;
        }

        let drive = self.drive()?;
        let (cursor, batches) = drive.download(journal.cursor.as_deref())?;
        let cursor_changed = journal.cursor.as_deref() != Some(&cursor) || !batches.is_empty();
        for batch in batches {
            ensure!(
                batch.version == FORMAT_VERSION,
                "Обновите BibleOWN для чтения журнала Google Drive"
            );
            for revision in batch.revisions {
                journal.history.insert(revision)?;
            }
        }
        if cursor_changed {
            journal.history.heads()?;
            journal.cursor = Some(cursor);
            self.save_journal(&journal)?;
        }
        let mut changed = recovered | self.materialize(&mut journal, root.as_deref())?;

        while journal.upload.is_some() || !journal.outgoing.is_empty() {
            if journal.upload.is_none() {
                let mut revisions = Vec::new();
                let mut bytes = 0;
                for revision in &journal.outgoing {
                    bytes += serde_json::to_vec(revision)?.len();
                    if (bytes > 4 * 1024 * 1024 || revisions.len() >= 128) && !revisions.is_empty()
                    {
                        break;
                    }
                    revisions.push(revision.clone());
                }
                journal.upload = Some(Upload {
                    drive_id: drive.new_id()?,
                    batch: Batch {
                        version: FORMAT_VERSION,
                        revisions,
                    },
                });
                self.save_journal(&journal)?;
            }
            let upload = journal
                .upload
                .as_ref()
                .context("Не подготовлена отправка")?;
            drive.upload(upload)?;
            let sent = upload.batch.revisions.len();
            journal.outgoing.drain(..sent);
            journal.upload = None;
            self.save_journal(&journal)?;
        }

        // A save from an open editor can arrive while we wait for the network.
        // Recording it here gives it the base the editor actually saw.
        let current = self.snapshot(root.as_deref())?;
        journal.capture(&current, root.is_some())?;
        changed |= self.materialize(&mut journal, root.as_deref())?;
        journal.last_success = Some(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs());
        self.save_journal(&journal)?;
        self.publish(&journal)?;
        Ok(changed)
    }

    fn drive(&self) -> Result<Drive> {
        let mut access = self.access.lock();
        if let Some(drive) = access.as_ref().filter(|drive| drive.is_fresh()) {
            return Ok(drive.clone());
        }
        let client = self
            .client
            .read()
            .clone()
            .context("Добавьте файл OAuth-клиента Google")?;
        let token =
            credentials::load(&self.credential_id())?.context("Подключите Google Drive заново")?;
        let drive = client.refresh(&token)?;
        *access = Some(drive.clone());
        Ok(drive)
    }

    fn bind_vault(&self, journal: &mut Journal, root: Option<&Path>) -> Result<()> {
        if let Some(root) = root {
            ensure!(
                journal.vault.as_deref().is_none_or(|path| path == root),
                "Выбрана другая папка заметок. Верните подключённую папку"
            );
            journal.vault = Some(root.to_path_buf());
        }
        Ok(())
    }

    fn finish_apply(&self, journal: &mut Journal, root: Option<&Path>) -> Result<bool> {
        if journal.apply.is_empty() {
            return Ok(false);
        }

        let mut changed = false;
        let mut concurrent_edit = false;
        let mut first_error = None;
        for apply in std::mem::take(&mut journal.apply) {
            if apply.next.key.kind == Kind::Note && root.is_none() {
                journal.apply.push(apply);
                continue;
            }
            match self.replace(
                root,
                &apply.next.key,
                apply.previous.as_ref(),
                apply.next.value.as_ref(),
            ) {
                Ok(true) => {
                    changed = true;
                    journal.set_materialized(apply.next);
                }
                Ok(false) => concurrent_edit = true,
                Err(error) => {
                    first_error.get_or_insert(error);
                    journal.apply.push(apply);
                }
            }
        }
        if concurrent_edit {
            journal.capture(&self.snapshot(root)?, root.is_some())?;
        }
        self.save_journal(journal)?;
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(changed)
    }

    fn materialize(&self, journal: &mut Journal, root: Option<&Path>) -> Result<bool> {
        let deferred: std::collections::BTreeSet<_> = journal
            .apply
            .iter()
            .map(|apply| apply.next.key.clone())
            .collect();
        let mut queued = false;
        for (key, parents) in journal.history.heads()? {
            if (key.kind == Kind::Note && root.is_none()) || deferred.contains(&key) {
                continue;
            }
            let Some(value) = journal.history.merge(&parents) else {
                continue;
            };
            let previous = journal.materialized(&key);
            if previous.is_some_and(|record| record.parents == parents) {
                continue;
            }
            let previous = previous.and_then(|record| record.value.clone());
            journal.apply.push(Apply {
                previous,
                next: Materialized {
                    key,
                    value,
                    parents,
                },
            });
            queued = true;
        }
        if queued {
            self.save_journal(journal)?;
        }
        let changed = self.finish_apply(journal, root)?;
        self.publish(journal)?;
        Ok(changed)
    }

    pub fn resolve(&self, key: &Key, variant: Uuid, vault: Option<PathBuf>) -> Result<()> {
        let _operation = self.lock_journal()?;
        let root = vault_root(vault)?;
        let mut journal = self.read_journal()?;
        self.bind_vault(&mut journal, root.as_deref())?;
        self.finish_apply(&mut journal, root.as_deref())?;
        let current = self.snapshot(root.as_deref())?;
        let before = journal
            .materialized(key)
            .map(|record| record.parents.clone());
        journal.capture(&current, root.is_some())?;
        self.save_journal(&journal)?;
        self.publish(&journal)?;
        ensure!(
            before
                == journal
                    .materialized(key)
                    .map(|record| record.parents.clone()),
            "Запись изменилась. Откройте конфликт заново"
        );
        let parents = journal
            .history
            .heads()?
            .remove(key)
            .context("Запись не найдена")?;
        ensure!(
            parents.contains(&variant) && journal.history.merge(&parents).is_none(),
            "Конфликт уже изменился"
        );
        let revision = Revision::new(
            key.clone(),
            parents,
            journal.history.value(variant).cloned(),
        );
        journal.commit(revision)?;
        self.save_journal(&journal)?;
        self.materialize(&mut journal, root.as_deref())?;
        Ok(())
    }

    fn conflicts(journal: &Journal) -> Result<Vec<Conflict>> {
        let mut conflicts = Vec::new();
        for (key, heads) in journal.history.heads()? {
            if journal.history.merge(&heads).is_some() {
                continue;
            }
            let variants = heads
                .into_iter()
                .map(|id| Variant {
                    id,
                    value: journal.history.value(id).cloned(),
                })
                .collect();
            conflicts.push(Conflict { key, variants });
        }
        Ok(conflicts)
    }

    fn publish(&self, journal: &Journal) -> Result<()> {
        let conflicts = Self::conflicts(journal)?;
        let mut status = self.status.write();
        status.configured = self.client.read().is_some();
        status.connected = journal.enabled;
        status.last_success = journal.last_success;
        status.error = None;
        status.conflicts = conflicts;
        Ok(())
    }
}
