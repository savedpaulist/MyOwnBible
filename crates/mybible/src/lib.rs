//! Infrastructure: чтение модулей MyBible (architecture.md §4.1).

pub mod db;
pub mod html;
pub mod morphology;
pub mod registry;
pub mod search_index;

pub use db::ReadOnlyDb;
pub use registry::{BookEntry, CommentaryEntry, DictionaryEntry, ModuleRegistry, SharedRegistry};
pub use search_index::{
    CommentarySearchRow, DictionarySearchRow, IndexMeta, SearchIndex, SearchRow,
};
