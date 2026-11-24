//! Application boundary for study questions.
//!
//! The presentation layer works with these use cases and never receives a
//! database handle. SQLite remains an implementation detail of `app_state`.

use anyhow::{Context as _, Result};

use crate::services::AppServices;

pub use app_state::questions::{Question, QuestionSummary, QuestionVerse};

fn database(services: &AppServices) -> Result<&app_state::db::StateDb> {
    services
        .state_db
        .as_deref()
        .context("База вопросов недоступна.")
}

pub fn available(services: &AppServices) -> bool {
    services.state_db.is_some()
}

pub fn list(services: &AppServices) -> Result<Vec<QuestionSummary>> {
    database(services)?.questions()
}

pub fn get(services: &AppServices, id: &str) -> Result<Option<Question>> {
    database(services)?.question(id)
}

pub fn save_if_current(
    services: &AppServices,
    id: &str,
    question: &str,
    answer: &str,
    answered_at: Option<i64>,
    links: &[QuestionVerse],
    previous: Option<&serde_json::Value>,
) -> Result<()> {
    database(services)?.save_question_if_current(id, question, answer, answered_at, links, previous)
}
