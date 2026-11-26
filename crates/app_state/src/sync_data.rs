//! Portable user records. SQLite row IDs and local paths are kept out of the format.
use std::collections::BTreeMap;

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlez::connection::Connection;

use crate::db::StateDb;

pub const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Annotation,
    VerseComment,
    Chain,
    Tag,
    Question,
    CrossReference,
    Note,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Key {
    pub kind: Kind,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub key: Key,
    pub value: Value,
}

pub type Records = BTreeMap<Key, Value>;

struct Table {
    name: &'static str,
    columns: &'static [&'static str],
    key: &'static [&'static str],
}

const ANNOTATIONS: Table = Table {
    name: "annotations",
    columns: &[
        "module_id",
        "book",
        "chapter",
        "verse",
        "word_index",
        "kind",
        "color",
        "updated_at",
    ],
    key: &[
        "module_id",
        "book",
        "chapter",
        "verse",
        "word_index",
        "kind",
    ],
};
const COMMENTS: Table = Table {
    name: "verse_comments",
    columns: &[
        "id",
        "module_id",
        "book",
        "chapter",
        "verse",
        "word_index",
        "note_relpath",
        "body",
        "created_at",
        "updated_at",
    ],
    key: &["id"],
};
const CHAINS: Table = Table {
    name: "chains",
    columns: &["id", "title", "created_at", "updated_at", "bible_module"],
    key: &["id"],
};
const NODES: Table = Table {
    name: "chain_nodes",
    columns: &[
        "id",
        "chain_id",
        "refs_json",
        "assertion",
        "created_at",
        "updated_at",
        "sort_key",
        "bible_module",
    ],
    key: &["id"],
};
const EDGES: Table = Table {
    name: "chain_edges",
    columns: &["chain_id", "source_node_id", "target_node_id", "created_at"],
    key: &["source_node_id", "target_node_id"],
};
const TAGS: Table = Table {
    name: "tags",
    columns: &["name", "color"],
    key: &["name"],
};
const VERSE_TAGS: Table = Table {
    name: "verse_tags",
    columns: &["book", "chapter", "verse", "created_at"],
    key: &["book", "chapter", "verse", "tag_id"],
};
const QUESTIONS: Table = Table {
    name: "study_questions",
    columns: &["id", "question", "answer", "created_at", "answered_at"],
    key: &["id"],
};
const QUESTION_VERSES: Table = Table {
    name: "study_question_verses",
    columns: &[
        "question_id",
        "position",
        "module_id",
        "book",
        "chapter",
        "verse",
        "comment",
    ],
    key: &["question_id", "position"],
};
const CROSS_REFS: Table = Table {
    name: "cross_references",
    columns: &[
        "book",
        "chapter",
        "verse",
        "verse_end",
        "book_to",
        "chapter_to",
        "verse_to_start",
        "verse_to_end",
        "votes",
    ],
    key: &[
        "book",
        "chapter",
        "verse",
        "verse_end",
        "book_to",
        "chapter_to",
        "verse_to_start",
        "verse_to_end",
    ],
};

const STATE_TABLES: [(Kind, &Table); 5] = [
    (Kind::Annotation, &ANNOTATIONS),
    (Kind::VerseComment, &COMMENTS),
    (Kind::Chain, &CHAINS),
    (Kind::Tag, &TAGS),
    (Kind::Question, &QUESTIONS),
];

impl Table {
    fn object(&self, prefix: &str) -> String {
        let columns = self
            .columns
            .iter()
            .map(|c| format!("'{c}', {prefix}{c}"))
            .collect::<Vec<_>>();
        format!("json_object({})", columns.join(", "))
    }

    fn key_from(&self, value: &Value) -> Result<String> {
        let parts = self
            .key
            .iter()
            .map(|column| {
                value
                    .get(column)
                    .filter(|value| value.is_string() || value.is_i64())
                    .with_context(|| format!("Отсутствует ключ {column} ({})", self.name))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(serde_json::to_string(&parts)?)
    }

    fn delete(&self, db: &Connection, id: &str) -> Result<()> {
        let parts: Vec<Value> = serde_json::from_str(id)?;
        ensure!(parts.len() == self.key.len(), "Неверный ключ записи");
        let predicates = self
            .key
            .iter()
            .enumerate()
            .map(|(i, column)| format!("{column} = json_extract(?1, '$[{i}]')"))
            .collect::<Vec<_>>();
        db.exec_bound(&format!(
            "DELETE FROM {} WHERE {}",
            self.name,
            predicates.join(" AND ")
        ))?(id)?;
        Ok(())
    }

    fn insert(&self, db: &Connection, value: &Value) -> Result<()> {
        ensure!(value.is_object(), "Неверная запись {}", self.name);
        let columns = self
            .columns
            .iter()
            .copied()
            .chain((self.name == VERSE_TAGS.name).then_some("tag_id"))
            .collect::<Vec<_>>();
        let args = columns
            .iter()
            .map(|column| format!("json_extract(?1, '$.{column}')"))
            .collect::<Vec<_>>();
        db.exec_bound(&format!(
            "INSERT INTO {} ({}) VALUES ({})",
            self.name,
            columns.join(", "),
            args.join(", ")
        ))?(serde_json::to_string(value)?)?;
        Ok(())
    }

    fn array(&self, predicate: &str, order: &str) -> String {
        format!(
            "(SELECT json_group_array(json(row_data)) FROM (SELECT {} AS row_data FROM {} WHERE {} ORDER BY {}))",
            self.object(""),
            self.name,
            predicate,
            order
        )
    }
}

fn select_records(db: &Connection, kind: Kind, table: &Table, id: Option<&str>) -> Result<Records> {
    let mut object = table.object("root.");
    let children = match kind {
        Kind::Chain => vec![
            ("nodes", NODES.array("chain_id = root.id", "sort_key, id")),
            (
                "edges",
                EDGES.array("chain_id = root.id", "source_node_id, target_node_id"),
            ),
        ],
        Kind::Question => vec![(
            "verses",
            QUESTION_VERSES.array("question_id = root.id", "position"),
        )],
        Kind::Tag => vec![(
            "verses",
            VERSE_TAGS.array("tag_id = root.id", "book, chapter, verse"),
        )],
        _ => Vec::new(),
    };
    for (name, sql) in children {
        object = format!("json_set({object}, '$.{name}', json({sql}))");
    }
    let mut query = format!("SELECT {object} FROM {} AS root", table.name);
    let rows = if let Some(id) = id {
        let predicates = table
            .key
            .iter()
            .enumerate()
            .map(|(i, column)| format!("root.{column} = json_extract(?1, '$[{i}]')"))
            .collect::<Vec<_>>();
        query.push_str(" WHERE ");
        query.push_str(&predicates.join(" AND "));
        db.select_bound::<&str, String>(&query)?(id)?
    } else {
        db.select::<String>(&query)?()?
    };
    rows.into_iter()
        .map(|row| {
            let value = serde_json::from_str(&row)?;
            let key = Key {
                kind: kind.clone(),
                id: table.key_from(&value)?,
            };
            Ok((key, value))
        })
        .collect()
}

fn replace(db: &Connection, table: &Table, key: &Key, value: Option<&Value>) -> Result<()> {
    if let Some(value) = value {
        ensure!(
            table.key_from(value)? == key.id,
            "Ключ записи не совпадает с её содержимым"
        );
    }
    db.with_savepoint("sync_record", || {
        // Keep the local tag ID when editing a tag, since the UI may reference it.
        if key.kind == Kind::Tag {
            let [name]: [String; 1] = serde_json::from_str(&key.id)?;
            let id: Option<i64> =
                db.select_bound::<&str, i64>("SELECT id FROM tags WHERE name = ?")?(&name)?.pop();
            if let (Some(value), Some(id)) = (value, id) {
                let color = value["color"].as_str().context("Некорректный цвет тега")?;
                db.exec_bound("UPDATE tags SET color = ? WHERE id = ?")?((color, id))?;
                db.exec_bound("DELETE FROM verse_tags WHERE tag_id = ?")?(id)?;
                return insert_tag_verses(db, value, id);
            }
        }
        table.delete(db, &key.id)?;
        let Some(value) = value else { return Ok(()) };
        table.insert(db, value)?;
        match key.kind {
            Kind::Chain => {
                insert_children(db, value, "nodes", &NODES, "chain_id")?;
                insert_children(db, value, "edges", &EDGES, "chain_id")?;
            }
            Kind::Question => {
                insert_children(db, value, "verses", &QUESTION_VERSES, "question_id")?
            }
            Kind::Tag => {
                let id = db.select_row::<i64>("SELECT last_insert_rowid()")?()?
                    .context("Нет ID тега")?;
                insert_tag_verses(db, value, id)?;
            }
            _ => {}
        }
        Ok(())
    })
}

fn insert_children(
    db: &Connection,
    parent: &Value,
    field: &str,
    table: &Table,
    foreign_key: &str,
) -> Result<()> {
    for child in parent[field]
        .as_array()
        .context("Некорректный список дочерних записей")?
    {
        ensure!(
            child.get(foreign_key) == parent.get("id"),
            "Неверный родитель записи"
        );
        table.insert(db, child)?;
    }
    Ok(())
}

fn insert_tag_verses(db: &Connection, value: &Value, id: i64) -> Result<()> {
    for verse in value["verses"]
        .as_array()
        .context("Некорректные стихи тега")?
    {
        let mut verse = verse.as_object().context("Некорректный стих тега")?.clone();
        verse.insert("tag_id".into(), id.into());
        VERSE_TAGS.insert(db, &verse.into())?;
    }
    Ok(())
}

impl StateDb {
    pub fn sync_records(&self) -> Result<Records> {
        self.with(|db| {
            let mut records = Records::new();
            for (kind, table) in STATE_TABLES {
                records.extend(select_records(db, kind, table, None)?);
            }
            Ok(records)
        })
    }

    pub fn sync_replace(
        &self,
        key: &Key,
        expected: Option<&Value>,
        target: Option<&Value>,
    ) -> Result<bool> {
        let table = STATE_TABLES
            .iter()
            .find(|(kind, _)| kind == &key.kind)
            .map(|(_, table)| *table)
            .context("Неизвестный тип записи")?;
        self.with(|db| compare_and_replace(db, table, key, expected, target))
    }
}

pub(crate) fn question_record(db: &Connection, id: &str) -> Result<Option<Value>> {
    let key = serde_json::to_string(&[id])?;
    let mut records = select_records(db, Kind::Question, &QUESTIONS, Some(&key))?;
    Ok(records.pop_first().map(|(_, value)| value))
}

fn compare_and_replace(
    db: &Connection,
    table: &Table,
    key: &Key,
    expected: Option<&Value>,
    target: Option<&Value>,
) -> Result<bool> {
    let current = select_records(db, key.kind.clone(), table, Some(&key.id))?;
    let current = current.get(key);
    if current == target {
        return Ok(true);
    }
    if current != expected {
        return Ok(false);
    }
    replace(db, table, key, target)?;
    Ok(true)
}

pub(crate) fn cross_refs_snapshot(db: &Connection) -> Result<Records> {
    select_records(db, Kind::CrossReference, &CROSS_REFS, None)
}

pub(crate) fn cross_refs_replace(
    db: &Connection,
    key: &Key,
    expected: Option<&Value>,
    target: Option<&Value>,
) -> Result<bool> {
    compare_and_replace(db, &CROSS_REFS, key, expected, target)
}
