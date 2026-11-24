//! Read-only подключение к модулю MyBible (перенесено из форка, bible/src/db.rs).
//!
//! Одно соединение на файл за `parking_lot::Mutex`; запись в модули
//! невозможна по контракту (architecture.md §3.1: ресурсы read-only).

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use parking_lot::Mutex;
use sqlez::connection::Connection;

pub struct ReadOnlyDb {
    inner: Mutex<Connection>,
}

impl ReadOnlyDb {
    pub fn open(path: &Path) -> Result<Arc<Self>> {
        let path_str = path.to_str().context("module path is not valid UTF-8")?;
        let connection = Connection::open_file_readonly(path_str)
            .with_context(|| format!("opening sqlite module at {}", path.display()))?;
        Ok(Arc::new(Self {
            inner: Mutex::new(connection),
        }))
    }

    pub fn with<R>(&self, f: impl FnOnce(&Connection) -> Result<R>) -> Result<R> {
        let connection = self.inner.lock();
        f(&connection)
    }

    /// Обёртка над готовым соединением — для тестов на in-memory БД.
    #[cfg(test)]
    pub fn from_connection_for_tests(connection: Connection) -> Self {
        Self {
            inner: Mutex::new(connection),
        }
    }
}
