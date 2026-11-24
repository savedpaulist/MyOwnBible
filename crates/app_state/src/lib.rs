//! Infrastructure состояния приложения (architecture.md §4.2).
//! Своя БД app_state.db, свои миграции, ничего чужого.

pub mod annotations;
pub mod chains;
pub mod db;
pub mod paths;
pub mod questions;
pub mod session;
pub mod settings;
pub mod sync_data;
pub mod tags;
pub mod user_cross_refs;
