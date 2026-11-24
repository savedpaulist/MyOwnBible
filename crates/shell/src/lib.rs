//! Application: сценарии использования, Link Router, навигация (architecture.md §5).
//!
//! Не зависит от gpui-kit component; GPUI появится только в адаптерном
//! слое глобальных сервисов (этап 2).

pub mod annotations;
pub mod article;
pub mod chains;
pub mod layout;
pub mod navigation;
pub mod notes;
pub mod questions;
pub mod search;
pub mod services;
pub mod settings;
pub mod tags;
pub mod themes;
pub mod verse_context;

pub use annotations::{
    BookAnnotations, VerseMarks, book_annotations, clear_annotation, save_verse_comment,
    set_annotation, split_annotations, verse_comment,
};
pub use article::{
    ArticleViewModel, ReferenceViewModel, open_commentary, open_cross_references,
    open_dictionary_article,
};
pub use mybible::BookEntry;
pub use navigation::NavigationCommand;
pub use search::{SearchHit, SearchResultsViewModel, search_bible};
pub use services::{
    AppServices, BookViewModel, CurrentLocation, VerseItem, WordDescription, WordUsageBook,
    WordUsageHit, WordUsageViewModel, describe_word, open_book, redescribe, word_usage,
};
pub use verse_context::{VerseContextViewModel, verse_context};

pub mod sync;
