//! Логические цепочки (перенос `chains.rs` форка, доменная часть).
//!
//! Цепочка — DAG: узел несёт набор ссылок на стихи плюс свободный текст
//! утверждения, ребро читается «следовательно». Ссылки хранятся компактной
//! строкой вида `"500/3:16"` или `"540/13:1-3"` — форма не зависит от
//! реестра модулей, поэтому домену реестр не нужен.
//!
//! Здесь только типы и чистые функции: сериализация ссылок, проверка
//! покрытия стиха, топологическая сортировка (Kahn) с картами глубин и
//! родителей/детей для отрисовки, проверка цикла перед вставкой ребра.
//! Хранилище — `app_state::chains`, сценарии — `shell::chains`.

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::{Context as _, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::reference::Reference;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainNode {
    pub id: String,
    pub chain_id: String,
    pub refs: Vec<Reference>,
    pub assertion: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// Позиция среди «соседей» (узлов с тем же набором родителей);
    /// по умолчанию равна created_at при вставке.
    pub sort_key: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainEdge {
    pub chain_id: String,
    pub source_node_id: String,
    pub target_node_id: String,
}

/// Строковая форма ссылки для хранения: `500/3`, `500/3:16`, `500/3:16-18`.
pub fn reference_to_compact(reference: &Reference) -> String {
    match (reference.verse, reference.end_verse) {
        (None, _) => format!("{}/{}", reference.book_number, reference.chapter),
        (Some(verse), None) => format!("{}/{}:{verse}", reference.book_number, reference.chapter),
        (Some(verse), Some(end)) => {
            format!(
                "{}/{}:{verse}-{end}",
                reference.book_number, reference.chapter
            )
        }
    }
}

/// Разбор компактной формы. Битая строка — None (не ошибка: хранилище
/// может содержать ссылки из будущих версий).
pub fn parse_compact_reference(raw: &str) -> Option<Reference> {
    let (book_str, rest) = raw.split_once('/')?;
    let book_number: i64 = book_str.trim().parse().ok()?;
    let (chapter_str, verse_part) = match rest.split_once(':') {
        Some((chapter, verse)) => (chapter, Some(verse)),
        None => (rest, None),
    };
    let chapter: u16 = chapter_str.trim().parse().ok()?;
    let (verse, end_verse) = match verse_part {
        None => (None, None),
        Some(verses) => {
            let (start_str, end_str) = match verses.split_once('-') {
                Some((start, end)) => (start, Some(end)),
                None => (verses, None),
            };
            let start: u16 = start_str.trim().parse().ok()?;
            let end = match end_str {
                None => None,
                Some(end) => Some(end.trim().parse::<u16>().ok()?),
            };
            (Some(start), end)
        }
    };
    Some(Reference {
        book_number,
        chapter,
        verse,
        end_verse,
    })
}

/// Покрывает ли ссылка стих. Ссылка без стиха покрывает всю главу,
/// диапазон включает оба конца.
pub fn reference_covers(reference: &Reference, chapter: u16, verse: u16) -> bool {
    if reference.chapter != chapter {
        return false;
    }
    match (reference.verse, reference.end_verse) {
        (None, _) => true,
        (Some(single), None) => single == verse,
        (Some(start), Some(end)) => {
            let (low, high) = if start <= end {
                (start, end)
            } else {
                (end, start)
            };
            verse >= low && verse <= high
        }
    }
}

pub fn refs_to_json(refs: &[Reference]) -> String {
    let strings: Vec<String> = refs.iter().map(reference_to_compact).collect();
    serde_json::to_string(&strings).unwrap_or_else(|_| "[]".to_string())
}

pub fn parse_refs_json(raw: &str) -> Result<Vec<Reference>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let strings: Vec<String> = serde_json::from_str(trimmed)
        .with_context(|| format!("refs_json is not a JSON string-array: {trimmed:?}"))?;
    let mut refs = Vec::with_capacity(strings.len());
    for string in &strings {
        let reference = parse_compact_reference(string)
            .with_context(|| format!("bad compact reference {string:?}"))?;
        refs.push(reference);
    }
    Ok(refs)
}

/// Расширяет ли ссылка книгу/главу (для маркеров в чтении): та же книга,
/// и глава либо совпадает, либо ссылка без главы быть не может — глава
/// есть всегда, проверяем точное совпадение.
pub fn reference_in_chapter(reference: &Reference, book_number: i64, chapter: u16) -> bool {
    reference.book_number == book_number && reference.chapter == chapter
}

/// Строка БД для одного узла (колонки chain_nodes). Выделена, чтобы
/// app_state не знал о форме JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainNodeRow {
    pub id: String,
    pub chain_id: String,
    pub refs_json: String,
    pub assertion: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub sort_key: i64,
}

impl TryFrom<ChainNodeRow> for ChainNode {
    type Error = anyhow::Error;
    fn try_from(row: ChainNodeRow) -> Result<Self> {
        let refs = parse_refs_json(&row.refs_json)
            .with_context(|| format!("parsing refs_json for chain node {}", row.id))?;
        Ok(ChainNode {
            id: row.id,
            chain_id: row.chain_id,
            refs,
            assertion: row.assertion,
            created_at: row.created_at,
            updated_at: row.updated_at,
            sort_key: row.sort_key,
        })
    }
}

impl From<&ChainNode> for ChainNodeRow {
    fn from(node: &ChainNode) -> Self {
        ChainNodeRow {
            id: node.id.clone(),
            chain_id: node.chain_id.clone(),
            refs_json: refs_to_json(&node.refs),
            assertion: node.assertion.clone(),
            created_at: node.created_at,
            updated_at: node.updated_at,
            sort_key: node.sort_key,
        }
    }
}

/// Разрешённый граф одной цепочки: карты смежности, топопорядок (Kahn),
/// глубины (длина длиннейшего пути от корня — уровень отступа при
/// отрисовке), корни и листья.
#[derive(Debug, Clone)]
pub struct ChainGraph {
    pub chain_id: String,
    pub nodes: HashMap<String, ChainNode>,
    pub edges: Vec<ChainEdge>,
    pub parents: HashMap<String, Vec<String>>,
    pub children: HashMap<String, Vec<String>>,
    /// Узлы в топологическом порядке; внутри одного ранга — по
    /// (sort_key, created_at, id), чтобы перерисовки были стабильны.
    pub topo_order: Vec<String>,
    pub depth: HashMap<String, usize>,
    pub roots: Vec<String>,
    pub leaves: Vec<String>,
}

impl ChainGraph {
    pub fn build(chain_id: String, nodes: Vec<ChainNode>, edges: Vec<ChainEdge>) -> Result<Self> {
        let mut node_map: HashMap<String, ChainNode> = HashMap::with_capacity(nodes.len());
        for node in nodes {
            node_map.insert(node.id.clone(), node);
        }
        let mut parents: HashMap<String, Vec<String>> = HashMap::new();
        let mut children: HashMap<String, Vec<String>> = HashMap::new();
        let mut indegree: HashMap<String, usize> = HashMap::new();
        for edge in &edges {
            if !node_map.contains_key(&edge.source_node_id)
                || !node_map.contains_key(&edge.target_node_id)
            {
                bail!(
                    "chain {chain_id}: edge {} -> {} references a missing node",
                    edge.source_node_id,
                    edge.target_node_id
                );
            }
            parents
                .entry(edge.target_node_id.clone())
                .or_default()
                .push(edge.source_node_id.clone());
            children
                .entry(edge.source_node_id.clone())
                .or_default()
                .push(edge.target_node_id.clone());
            *indegree.entry(edge.target_node_id.clone()).or_insert(0) += 1;
            indegree.entry(edge.source_node_id.clone()).or_insert(0);
        }
        // Стабильный порядок внутри ранга.
        let rank_key = |id: &String| {
            node_map
                .get(id)
                .map(|node| (node.sort_key, node.created_at, id.clone()))
                .unwrap_or((i64::MAX, i64::MAX, id.clone()))
        };
        let mut queue: VecDeque<String> = node_map
            .keys()
            .filter(|id| indegree.get(*id).copied().unwrap_or(0) == 0)
            .cloned()
            .collect();
        let mut initial: Vec<String> = queue.iter().cloned().collect();
        initial.sort_by_key(rank_key);
        queue = initial.into_iter().collect();

        let mut topo_order: Vec<String> = Vec::with_capacity(node_map.len());
        let mut depth: HashMap<String, usize> = HashMap::new();
        let mut remaining = indegree.clone();
        while let Some(id) = queue.pop_front() {
            let node_depth = parents
                .get(&id)
                .map(|list| {
                    list.iter()
                        .filter_map(|parent| depth.get(parent).copied())
                        .max()
                        .map_or(0, |max| max + 1)
                })
                .unwrap_or(0);
            depth.insert(id.clone(), node_depth);
            if let Some(child_list) = children.get(&id) {
                let mut ready: Vec<String> = Vec::new();
                for child in child_list {
                    if let Some(entry) = remaining.get_mut(child) {
                        *entry = entry.saturating_sub(1);
                        if *entry == 0 {
                            ready.push(child.clone());
                        }
                    }
                }
                ready.sort_by_key(rank_key);
                for child in ready {
                    queue.push_back(child);
                }
            }
            topo_order.push(id);
        }
        if topo_order.len() != node_map.len() {
            bail!(
                "chain {chain_id}: graph has a cycle ({} of {} nodes ordered)",
                topo_order.len(),
                node_map.len()
            );
        }
        let roots: Vec<String> = node_map
            .keys()
            .filter(|id| parents.get(*id).map_or(true, Vec::is_empty))
            .cloned()
            .collect();
        let leaves: Vec<String> = node_map
            .keys()
            .filter(|id| children.get(*id).map_or(true, Vec::is_empty))
            .cloned()
            .collect();
        Ok(ChainGraph {
            chain_id,
            nodes: node_map,
            edges,
            parents,
            children,
            topo_order,
            depth,
            roots,
            leaves,
        })
    }

    /// Есть ли направленный путь `from -> to` по рёбрам графа.
    pub fn path_exists(&self, from: &str, to: &str) -> bool {
        if from == to {
            return true;
        }
        let mut visited: HashSet<&str> = HashSet::new();
        let mut stack: Vec<&str> = vec![from];
        while let Some(id) = stack.pop() {
            if !visited.insert(id) {
                continue;
            }
            if let Some(child_list) = self.children.get(id) {
                for child in child_list {
                    if child == to {
                        return true;
                    }
                    stack.push(child);
                }
            }
        }
        false
    }
}

/// Новый id узла/цепочки. UUID без внешних зависимостей: 128 бит из
/// времени и счётчика — достаточно для локальной БД, формат совпадает
/// с форком (дефисы, hex), чтобы базы были читаемы обеими версиями.
pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let high = nanos as u64;
    let low = (nanos >> 64) as u64 ^ counter.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        (high >> 32) as u32,
        (high >> 16) as u16,
        (high as u16 & 0x0fff) | 0x4000,
        ((low >> 48) as u16 & 0x3fff) | 0x8000,
        low & 0xffff_ffff_ffff
    )
}

/// Проверка «ребро не замкнёт цикл» перед вставкой: путь target -> source
/// уже существовать не должен.
pub fn would_create_cycle(
    graph: &ChainGraph,
    source_node_id: &str,
    target_node_id: &str,
) -> Result<()> {
    if source_node_id == target_node_id {
        return Err(anyhow!("узел цепочки не может следовать сам из себя"));
    }
    if graph.path_exists(target_node_id, source_node_id) {
        return Err(anyhow!(
            "ребро {source_node_id} -> {target_node_id} замкнёт цикл"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(book: i64, chapter: u16, verse: Option<u16>, end: Option<u16>) -> Reference {
        Reference {
            book_number: book,
            chapter,
            verse,
            end_verse: end,
        }
    }

    fn node(id: &str, chain: &str, sort_key: i64) -> ChainNode {
        ChainNode {
            id: id.to_string(),
            chain_id: chain.to_string(),
            refs: Vec::new(),
            assertion: String::new(),
            created_at: sort_key,
            updated_at: sort_key,
            sort_key,
        }
    }

    fn edge(chain: &str, from: &str, to: &str) -> ChainEdge {
        ChainEdge {
            chain_id: chain.to_string(),
            source_node_id: from.to_string(),
            target_node_id: to.to_string(),
        }
    }

    #[test]
    fn compact_reference_roundtrip() {
        for reference in [
            reference(500, 3, None, None),
            reference(500, 3, Some(16), None),
            reference(540, 13, Some(1), Some(3)),
        ] {
            let compact = reference_to_compact(&reference);
            assert_eq!(parse_compact_reference(&compact), Some(reference));
        }
        assert_eq!(
            reference_to_compact(&reference(500, 3, None, None)),
            "500/3"
        );
        assert_eq!(
            reference_to_compact(&reference(500, 3, Some(16), None)),
            "500/3:16"
        );
        assert_eq!(
            reference_to_compact(&reference(540, 13, Some(1), Some(3))),
            "540/13:1-3"
        );
    }

    #[test]
    fn parse_compact_rejects_garbage() {
        assert_eq!(parse_compact_reference(""), None);
        assert_eq!(parse_compact_reference("abc/3:16"), None);
        assert_eq!(parse_compact_reference("500/x:16"), None);
        assert_eq!(parse_compact_reference("500/3:x"), None);
        assert_eq!(parse_compact_reference("500/3:1-y"), None);
    }

    #[test]
    fn refs_json_roundtrip() {
        let refs = vec![
            reference(500, 3, Some(16), None),
            reference(540, 13, Some(1), Some(3)),
        ];
        let json = refs_to_json(&refs);
        assert_eq!(json, "[\"500/3:16\",\"540/13:1-3\"]");
        assert_eq!(parse_refs_json(&json).unwrap(), refs);
        assert_eq!(parse_refs_json("").unwrap(), Vec::new());
        assert!(parse_refs_json("not json").is_err());
        assert!(parse_refs_json("[\"bad\"]").is_err());
    }

    #[test]
    fn reference_covers_rules() {
        let whole_chapter = reference(500, 3, None, None);
        assert!(reference_covers(&whole_chapter, 3, 1));
        assert!(reference_covers(&whole_chapter, 3, 99));
        assert!(!reference_covers(&whole_chapter, 4, 1));

        let single = reference(500, 3, Some(16), None);
        assert!(reference_covers(&single, 3, 16));
        assert!(!reference_covers(&single, 3, 15));

        let range = reference(500, 3, Some(14), Some(18));
        assert!(reference_covers(&range, 3, 14));
        assert!(reference_covers(&range, 3, 16));
        assert!(reference_covers(&range, 3, 18));
        assert!(!reference_covers(&range, 3, 13));
        assert!(!reference_covers(&range, 3, 19));

        // Перевёрнутый диапазон трактуем симметрично (как форк).
        let flipped = reference(500, 3, Some(18), Some(14));
        assert!(reference_covers(&flipped, 3, 16));
    }

    #[test]
    fn graph_topological_order_and_depth() {
        // a -> b -> d, a -> c -> d (ромб)
        let graph = ChainGraph::build(
            "ch".to_string(),
            vec![
                node("a", "ch", 1),
                node("b", "ch", 2),
                node("c", "ch", 3),
                node("d", "ch", 4),
            ],
            vec![
                edge("ch", "a", "b"),
                edge("ch", "a", "c"),
                edge("ch", "b", "d"),
                edge("ch", "c", "d"),
            ],
        )
        .unwrap();
        assert_eq!(graph.topo_order, vec!["a", "b", "c", "d"]);
        assert_eq!(graph.depth.get("a"), Some(&0));
        assert_eq!(graph.depth.get("b"), Some(&1));
        assert_eq!(graph.depth.get("c"), Some(&1));
        assert_eq!(graph.depth.get("d"), Some(&2));
        assert_eq!(graph.roots, vec!["a"]);
        assert_eq!(graph.leaves, vec!["d"]);
        assert_eq!(
            graph.parents.get("d").unwrap(),
            &vec!["b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn graph_detects_cycle() {
        let result = ChainGraph::build(
            "ch".to_string(),
            vec![node("a", "ch", 1), node("b", "ch", 2)],
            vec![edge("ch", "a", "b"), edge("ch", "b", "a")],
        );
        assert!(result.is_err());
    }

    #[test]
    fn graph_rejects_dangling_edge() {
        let result = ChainGraph::build(
            "ch".to_string(),
            vec![node("a", "ch", 1)],
            vec![edge("ch", "a", "ghost")],
        );
        assert!(result.is_err());
    }

    #[test]
    fn cycle_check_before_insert() {
        let graph = ChainGraph::build(
            "ch".to_string(),
            vec![node("a", "ch", 1), node("b", "ch", 2), node("c", "ch", 3)],
            vec![edge("ch", "a", "b")],
        )
        .unwrap();
        // b -> a замкнёт петлю, a -> c и c -> a — нормально, a -> a — запрет.
        assert!(would_create_cycle(&graph, "b", "a").is_err());
        assert!(would_create_cycle(&graph, "a", "a").is_err());
        assert!(would_create_cycle(&graph, "a", "c").is_ok());
        assert!(would_create_cycle(&graph, "b", "c").is_ok());
    }

    #[test]
    fn new_id_looks_like_uuid_and_unique() {
        let first = new_id();
        let second = new_id();
        assert_ne!(first, second);
        assert_eq!(first.len(), 36);
        assert_eq!(first.chars().filter(|c| *c == '-').count(), 4);
    }
}
