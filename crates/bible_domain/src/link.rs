//! Внутренние ссылки приложения (architecture.md §3.4).
//!
//! Семантическая цель ссылки распознаётся на этапе парсинга (MyBible HTML,
//! wikilinks в заметках), UI получает уже готовый `LinkTarget`.

use crate::ids::{ModuleId, VerseRef};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkTarget {
    /// Библейское место: [[bible:Иоан 3:16]], <a href="B:540 4:1">.
    Bible(VerseRef),
    /// Словарная статья: [[dict:strongs/G2570]], <a href="ADAM">.
    DictionaryEntry { module: ModuleId, topic: String },
    /// Комментарий к месту Писания.
    Commentary {
        module: ModuleId,
        reference: VerseRef,
    },
    /// Перекрёстные ссылки на стих.
    CrossReferences {
        module: ModuleId,
        reference: VerseRef,
    },
    /// Заметка из хранилища (relpath без .md или заголовок).
    Note(String),
    /// Внешний URL — открывается только системным браузером.
    ExternalUrl(String),
}
