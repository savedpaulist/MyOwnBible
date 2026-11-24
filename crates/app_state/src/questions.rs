use anyhow::Result;
use bible_domain::ids::{BookNumber, ModuleId, VerseRef};

use crate::db::StateDb;

pub struct QuestionSummary {
    pub id: String,
    pub question: String,
    pub created: String,
    pub answered: Option<String>,
}

pub struct Question {
    pub summary: QuestionSummary,
    pub answer: String,
    pub answered_at: Option<i64>,
    pub links: Vec<QuestionVerse>,
    pub version: serde_json::Value,
}

#[derive(Clone)]
pub struct QuestionVerse {
    pub module: ModuleId,
    pub reference: VerseRef,
    pub comment: String,
}

impl StateDb {
    pub fn questions(&self) -> Result<Vec<QuestionSummary>> {
        self.with(|conn| {
            let rows: Vec<(String, String, String, Option<String>)> = conn.select(
                "SELECT id, question, datetime(created_at, 'unixepoch', 'localtime'),
                        datetime(answered_at, 'unixepoch', 'localtime')
                 FROM study_questions ORDER BY created_at DESC, rowid DESC",
            )?()?;
            Ok(rows
                .into_iter()
                .map(|(id, question, created, answered)| QuestionSummary {
                    id,
                    question,
                    created,
                    answered,
                })
                .collect())
        })
    }

    pub fn question(&self, id: &str) -> Result<Option<Question>> {
        self.with(|conn| {
            let row: Option<(String, String, String, Option<String>, Option<i64>)> =
                conn.select_row_bound(
                    "SELECT question, answer, datetime(created_at, 'unixepoch', 'localtime'),
                        datetime(answered_at, 'unixepoch', 'localtime'), answered_at
                 FROM study_questions WHERE id = ?",
                )?(id)?;
            let Some((question, answer, created, answered, answered_at)) = row else {
                return Ok(None);
            };
            let rows: Vec<(String, i64, i64, i64, String)> = conn.select_bound(
                "SELECT module_id, book, chapter, verse, comment FROM study_question_verses
                 WHERE question_id = ? ORDER BY position",
            )?(id)?;
            let links = rows
                .into_iter()
                .map(|(module, book, chapter, verse, comment)| QuestionVerse {
                    module: ModuleId(module),
                    reference: VerseRef::single(BookNumber(book), chapter as u16, verse as u16),
                    comment,
                })
                .collect();
            Ok(Some(Question {
                summary: QuestionSummary {
                    id: id.to_owned(),
                    question,
                    created,
                    answered,
                },
                answer,
                answered_at,
                links,
                version: crate::sync_data::question_record(conn, id)?
                    .ok_or_else(|| anyhow::anyhow!("Вопрос не найден"))?,
            }))
        })
    }

    pub fn save_question(
        &self,
        id: &str,
        question: &str,
        answer: &str,
        answered_at: Option<i64>,
        links: &[QuestionVerse],
    ) -> Result<()> {
        self.write_question(id, question, answer, answered_at, links, None)
    }

    pub fn save_question_if_current(
        &self,
        id: &str,
        question: &str,
        answer: &str,
        answered_at: Option<i64>,
        links: &[QuestionVerse],
        previous: Option<&serde_json::Value>,
    ) -> Result<()> {
        self.write_question(id, question, answer, answered_at, links, Some(previous))
    }

    fn write_question(
        &self,
        id: &str,
        question: &str,
        answer: &str,
        answered_at: Option<i64>,
        links: &[QuestionVerse],
        previous: Option<Option<&serde_json::Value>>,
    ) -> Result<()> {
        anyhow::ensure!(!question.trim().is_empty(), "Введите вопрос.");
        self.with(|conn| conn.with_savepoint("save_study_question", || {
            if let Some(previous) = previous {
                let current = crate::sync_data::question_record(conn, id)?;
                anyhow::ensure!(current.as_ref() == previous,
                    "Вопрос изменился на другом устройстве. Скопируйте текст черновика перед тем, как открыть обновлённый вопрос.");
            }
            conn.exec_bound(
                "INSERT INTO study_questions (id, question, answer, created_at, answered_at)
                 VALUES (?, ?, ?, unixepoch(), ?)
                 ON CONFLICT(id) DO UPDATE SET question = excluded.question, answer = excluded.answer,
                 answered_at = excluded.answered_at",
            )?((id, question, answer, answered_at))?;

            conn.exec_bound("DELETE FROM study_question_verses WHERE question_id = ?")?(id)?;
            let mut insert = conn.exec_bound(
                "INSERT INTO study_question_verses
                 (question_id, position, module_id, book, chapter, verse, comment)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )?;
            for (position, link) in links.iter().enumerate() {
                insert((id, position as i64, link.module.0.as_str(), link.reference.book.0,
                    i64::from(link.reference.chapter), i64::from(link.reference.verse), link.comment.as_str()))?;
            }
            Ok(())
        }))
    }
}
