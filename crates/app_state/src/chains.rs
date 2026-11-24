//! SQL-хранилище логических цепочек (таблицы миграции v2, §6.8).
//! Весь SQL цепочек живёт только здесь; типы и JSON-форма ссылок —
//! `bible_domain::chains`. Поиск узлов по стиху/главе — LIKE по
//! refs_json, как в форке: refs_json короткий, строк мало, а точную
//! проверку диапазонов делает домен на стороне вызывающего.

use std::sync::Arc;

use anyhow::{Result, ensure};
use bible_domain::chains::{Chain, ChainEdge, ChainNode, ChainNodeRow};

use crate::db::StateDb;

pub struct SqliteChainStore {
    db: Arc<StateDb>,
}

type ChainRow = (String, String, i64, i64);
type NodeRowTuple = (String, String, String, String, i64, i64, i64);
type EdgeRowTuple = (String, String, String);

const NODE_COLUMNS: &str = "id, chain_id, refs_json, assertion, created_at, updated_at, sort_key";

fn chain_from_row(row: ChainRow) -> Chain {
    let (id, title, created_at, updated_at) = row;
    Chain {
        id,
        title,
        created_at,
        updated_at,
    }
}

fn node_row_from_tuple(row: NodeRowTuple) -> ChainNodeRow {
    let (id, chain_id, refs_json, assertion, created_at, updated_at, sort_key) = row;
    ChainNodeRow {
        id,
        chain_id,
        refs_json,
        assertion,
        created_at,
        updated_at,
        sort_key,
    }
}

fn edge_from_row(row: EdgeRowTuple) -> ChainEdge {
    let (chain_id, source_node_id, target_node_id) = row;
    ChainEdge {
        chain_id,
        source_node_id,
        target_node_id,
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

impl SqliteChainStore {
    pub fn new(db: Arc<StateDb>) -> Self {
        Self { db }
    }

    pub fn bible_modules_for_chain(
        &self,
        id: &str,
    ) -> Result<(Option<String>, std::collections::HashMap<String, String>)> {
        self.db.with(|connection| {
            let module: Option<String> = connection.select_row_bound(
                "SELECT bible_module FROM chains WHERE id = ? AND bible_module IS NOT NULL",
            )?(id)?;
            let overrides: Vec<(String, String)> = connection.select_bound(
                "SELECT id, bible_module FROM chain_nodes WHERE chain_id = ? AND bible_module IS NOT NULL",
            )?(id)?;
            Ok((module, overrides.into_iter().collect()))
        })
    }

    pub fn set_bible_module(
        &self,
        chain: &str,
        node: Option<&str>,
        module: Option<&str>,
    ) -> Result<()> {
        self.db.with(|connection| {
            if let Some(node) = node {
                connection.exec_bound(
                    "UPDATE chain_nodes SET bible_module = ? WHERE chain_id = ? AND id = ?",
                )?((module, chain, node))?;
            } else {
                connection.exec_bound("UPDATE chains SET bible_module = ? WHERE id = ?")?((
                    module, chain,
                ))?;
            }
            Ok(())
        })
    }

    pub fn list_chains(&self) -> Result<Vec<Chain>> {
        self.db.with(|connection| {
            let rows: Vec<ChainRow> = connection.select(
                "SELECT id, title, created_at, updated_at \
                 FROM chains ORDER BY updated_at DESC",
            )?()?;
            Ok(rows.into_iter().map(chain_from_row).collect())
        })
    }

    pub fn chain_get(&self, id: &str) -> Result<Option<Chain>> {
        self.db.with(|connection| {
            let row: Option<ChainRow> = connection.select_row_bound(
                "SELECT id, title, created_at, updated_at FROM chains WHERE id = ?",
            )?(id.to_string())?;
            Ok(row.map(chain_from_row))
        })
    }

    /// Создать или переименовать. created_at не трогаем при обновлении.
    pub fn chain_upsert(&self, id: &str, title: &str) -> Result<()> {
        let now = now_unix();
        self.db.with(|connection| {
            connection.exec_bound(
                "INSERT INTO chains(id, title, created_at, updated_at) \
                 VALUES (?, ?, ?, ?) \
                 ON CONFLICT(id) DO UPDATE SET title = excluded.title, \
                     updated_at = excluded.updated_at",
            )?((id.to_string(), title.to_string(), now, now))?;
            Ok(())
        })
    }

    pub fn chain_delete(&self, id: &str) -> Result<()> {
        self.db.with(|connection| {
            connection.exec_bound("DELETE FROM chains WHERE id = ?")?(id.to_string())?;
            Ok(())
        })
    }

    pub fn nodes_for_chain(&self, chain_id: &str) -> Result<Vec<ChainNode>> {
        self.db.with(|connection| {
            let rows: Vec<NodeRowTuple> = connection.select_bound(&format!(
                "SELECT {NODE_COLUMNS} FROM chain_nodes \
                     WHERE chain_id = ? ORDER BY sort_key, created_at"
            ))?(chain_id.to_string())?;
            rows.into_iter()
                .map(|row| ChainNode::try_from(node_row_from_tuple(row)).map_err(Into::into))
                .collect()
        })
    }

    pub fn node_get(&self, id: &str) -> Result<Option<ChainNode>> {
        self.db.with(|connection| {
            let row: Option<NodeRowTuple> = connection.select_row_bound(&format!(
                "SELECT {NODE_COLUMNS} FROM chain_nodes WHERE id = ?"
            ))?(id.to_string())?;
            match row {
                None => Ok(None),
                Some(row) => Ok(Some(ChainNode::try_from(node_row_from_tuple(row))?)),
            }
        })
    }

    /// Вставить или обновить узел. created_at/sort_key сохраняются при
    /// обновлении, если вызывающий не передал свои (он передаёт — снимок
    /// существующего узла читается в shell).
    pub fn node_upsert(&self, node: &ChainNode) -> Result<()> {
        self.db
            .with(|connection| Self::write_node(connection, node))
    }

    fn write_node(connection: &sqlez::connection::Connection, node: &ChainNode) -> Result<()> {
        let row = ChainNodeRow::from(node);
        connection.exec_bound(
            "INSERT INTO chain_nodes(\
                id, chain_id, refs_json, assertion, created_at, updated_at, sort_key) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET \
                refs_json = excluded.refs_json, \
                assertion = excluded.assertion, \
                updated_at = excluded.updated_at, \
                sort_key = excluded.sort_key",
        )?((
            row.id,
            row.chain_id,
            row.refs_json,
            row.assertion,
            row.created_at,
            row.updated_at,
            row.sort_key,
        ))?;
        Ok(())
    }

    /// Insert a prerequisite, conclusion, or split one existing edge atomically.
    pub fn insert_node(
        &self,
        node: &ChainNode,
        source: Option<&str>,
        target: Option<&str>,
    ) -> Result<()> {
        self.db.with(|conn| conn.with_savepoint("insert_chain_node", || {
            let existing: Option<i64> =
                conn.select_row_bound("SELECT 1 FROM chain_nodes WHERE id = ?")?(node.id.as_str())?;
            ensure!(existing.is_none(), "Узел с таким идентификатором уже существует.");
            for id in source.into_iter().chain(target) {
                let chain_id: Option<String> = conn.select_row_bound(
                    "SELECT chain_id FROM chain_nodes WHERE id = ?",
                )?(id)?;
                ensure!(chain_id.as_deref() == Some(node.chain_id.as_str()), "Узел цепочки не найден.");
            }
            if let (Some(source), Some(target)) = (source, target) {
                let existing: Option<i64> = conn.select_row_bound(
                    "SELECT 1 FROM chain_edges WHERE chain_id = ? AND source_node_id = ? AND target_node_id = ?",
                )?((node.chain_id.as_str(), source, target))?;
                ensure!(existing.is_some(), "Связь между узлами уже изменена.");
                conn.exec_bound("DELETE FROM chain_edges WHERE source_node_id = ? AND target_node_id = ?")?((source, target))?;
            }
            Self::write_node(conn, node)?;
            let mut insert_edge = conn.exec_bound(
                "INSERT INTO chain_edges (chain_id, source_node_id, target_node_id, created_at)
                 VALUES (?, ?, ?, ?)",
            )?;
            let id = node.id.as_str();
            for (from, to) in source.map(|source| (source, id)).into_iter().chain(target.map(|target| (id, target))) {
                insert_edge((node.chain_id.as_str(), from, to, node.created_at))?;
            }
            Ok(())
        }))
    }

    pub fn node_delete(&self, id: &str) -> Result<()> {
        self.db.with(|connection| {
            connection.exec_bound("DELETE FROM chain_nodes WHERE id = ?")?(id.to_string())?;
            Ok(())
        })
    }

    pub fn edges_for_chain(&self, chain_id: &str) -> Result<Vec<ChainEdge>> {
        self.db.with(|connection| {
            let rows: Vec<EdgeRowTuple> = connection.select_bound(
                "SELECT chain_id, source_node_id, target_node_id \
                 FROM chain_edges WHERE chain_id = ?",
            )?(chain_id.to_string())?;
            Ok(rows.into_iter().map(edge_from_row).collect())
        })
    }

    pub fn edge_insert(&self, edge: &ChainEdge) -> Result<()> {
        self.db.with(|connection| {
            connection.exec_bound(
                "INSERT OR IGNORE INTO chain_edges(\
                    chain_id, source_node_id, target_node_id, created_at) \
                 VALUES (?, ?, ?, ?)",
            )?((
                edge.chain_id.clone(),
                edge.source_node_id.clone(),
                edge.target_node_id.clone(),
                now_unix(),
            ))?;
            Ok(())
        })
    }

    pub fn edge_delete(&self, source_node_id: &str, target_node_id: &str) -> Result<()> {
        self.db.with(|connection| {
            connection.exec_bound(
                "DELETE FROM chain_edges \
                 WHERE source_node_id = ? AND target_node_id = ?",
            )?((source_node_id.to_string(), target_node_id.to_string()))?;
            Ok(())
        })
    }

    /// Узлы, чей refs_json упоминает стих (LIKE по компактной форме).
    /// Диапазоны (`500/3:14-18`) ловит только точная строка — точную
    /// проверку покрытия делает домен в shell; здесь отдаём кандидатов
    /// по книге/главе, чтобы не читать всю таблицу.
    pub fn nodes_referencing_chapter(
        &self,
        book_number: i64,
        chapter: u16,
    ) -> Result<Vec<ChainNode>> {
        let pattern = format!("%\"{book_number}/{chapter}%");
        self.db.with(|connection| {
            let rows: Vec<NodeRowTuple> = connection.select_bound(&format!(
                "SELECT {NODE_COLUMNS} FROM chain_nodes \
                     WHERE refs_json LIKE ? ORDER BY updated_at DESC"
            ))?(pattern)?;
            rows.into_iter()
                .map(|row| ChainNode::try_from(node_row_from_tuple(row)).map_err(Into::into))
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bible_domain::reference::Reference;

    fn store() -> SqliteChainStore {
        SqliteChainStore::new(StateDb::in_memory().unwrap())
    }

    fn sample_node(id: &str, chain: &str, refs: Vec<Reference>) -> ChainNode {
        ChainNode {
            id: id.to_string(),
            chain_id: chain.to_string(),
            refs,
            assertion: format!("assertion {id}"),
            created_at: 100,
            updated_at: 200,
            sort_key: 100,
        }
    }

    #[test]
    fn chain_crud() {
        let store = store();
        store.chain_upsert("c1", "Первая").unwrap();
        store.chain_upsert("c2", "Вторая").unwrap();
        let chains = store.list_chains().unwrap();
        assert_eq!(chains.len(), 2);

        store.chain_upsert("c1", "Переименованная").unwrap();
        let chain = store.chain_get("c1").unwrap().unwrap();
        assert_eq!(chain.title, "Переименованная");

        store.chain_delete("c2").unwrap();
        assert!(store.chain_get("c2").unwrap().is_none());
        assert_eq!(store.list_chains().unwrap().len(), 1);
    }

    #[test]
    fn node_and_edge_crud_with_cascade() {
        let store = store();
        store.chain_upsert("c1", "Цепочка").unwrap();
        store
            .node_upsert(&sample_node(
                "n1",
                "c1",
                vec![Reference {
                    book_number: 500,
                    chapter: 3,
                    verse: Some(16),
                    end_verse: None,
                }],
            ))
            .unwrap();
        store
            .node_upsert(&sample_node(
                "n2",
                "c1",
                vec![Reference {
                    book_number: 500,
                    chapter: 3,
                    verse: Some(14),
                    end_verse: Some(18),
                }],
            ))
            .unwrap();
        store
            .edge_insert(&ChainEdge {
                chain_id: "c1".into(),
                source_node_id: "n1".into(),
                target_node_id: "n2".into(),
            })
            .unwrap();

        let nodes = store.nodes_for_chain("c1").unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].refs[0].verse, Some(16));
        let edges = store.edges_for_chain("c1").unwrap();
        assert_eq!(edges.len(), 1);

        // Обновление узла сохраняет created_at/sort_key из снимка.
        let mut edited = store.node_get("n1").unwrap().unwrap();
        edited.assertion = "новое утверждение".into();
        store.node_upsert(&edited).unwrap();
        let reread = store.node_get("n1").unwrap().unwrap();
        assert_eq!(reread.assertion, "новое утверждение");
        assert_eq!(reread.created_at, 100);

        // Каскад: удаление узла сносит его рёбра, удаление цепочки — узлы.
        store.node_delete("n2").unwrap();
        assert!(store.edges_for_chain("c1").unwrap().is_empty());
        store.node_upsert(&sample_node("n3", "c1", vec![])).unwrap();
        store.chain_delete("c1").unwrap();
        assert!(store.nodes_for_chain("c1").unwrap().is_empty());
    }

    #[test]
    fn referencing_chapter_finds_exact_and_range() {
        let store = store();
        store.chain_upsert("c1", "Цепочка").unwrap();
        store
            .node_upsert(&sample_node(
                "n1",
                "c1",
                vec![Reference {
                    book_number: 500,
                    chapter: 3,
                    verse: Some(16),
                    end_verse: None,
                }],
            ))
            .unwrap();
        store
            .node_upsert(&sample_node(
                "n2",
                "c1",
                vec![Reference {
                    book_number: 500,
                    chapter: 3,
                    verse: Some(1),
                    end_verse: Some(5),
                }],
            ))
            .unwrap();
        store
            .node_upsert(&sample_node(
                "n3",
                "c1",
                vec![Reference {
                    book_number: 510,
                    chapter: 3,
                    verse: Some(16),
                    end_verse: None,
                }],
            ))
            .unwrap();

        let hits = store.nodes_referencing_chapter(500, 3).unwrap();
        let mut ids: Vec<&str> = hits.iter().map(|node| node.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["n1", "n2"]);
        assert!(store.nodes_referencing_chapter(500, 4).unwrap().is_empty());
    }
}
