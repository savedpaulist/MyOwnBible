//! Сценарии логических цепочек (§6.8). UI зовёт только эти функции —
//! SQL живёт в `app_state::chains`, правила графа в `bible_domain::chains`.
//! Подписи ссылок форматируются короткими именами книг активного
//! (или первого) перевода — как подписи результатов поиска.

use std::collections::{HashMap, HashSet};

use anyhow::{Context as _, Result, anyhow};
use bible_domain::chains::{self, ChainEdge, ChainGraph, ChainNode, reference_covers};
use bible_domain::{ids::ModuleId, reference::Reference};

use crate::services::AppServices;

/// Строка списка цепочек.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainSummary {
    pub id: String,
    pub title: String,
    pub node_count: usize,
}

/// Подпись одной ссылки узла: «Ин 3:16» + разобранная ссылка для перехода.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefLabel {
    pub label: String,
    pub reference: Reference,
}

/// Узел в порядке отрисовки (топопорядок графа).
#[derive(Debug, Clone)]
pub struct ChainNodeView {
    pub node: ChainNode,
    pub bible_module: Option<ModuleId>,
    pub labels: Vec<RefLabel>,
    pub depth: usize,
    /// Сколько родителей/детей — для пометок merge/branch при рендере.
    pub parents: usize,
    pub children: usize,
}

/// Открытая цепочка, готовая к отрисовке.
#[derive(Debug, Clone)]
pub struct ChainView {
    pub id: String,
    pub title: String,
    pub nodes: Vec<ChainNodeView>,
    pub bible_module: Option<ModuleId>,
    pub edges: Vec<ChainEdge>,
}

/// Попадание «стих используется в цепочке» (полоса в панели «Контекст»).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainHit {
    pub id: String,
    pub title: String,
}

fn store(services: &AppServices) -> Result<&app_state::chains::SqliteChainStore> {
    services
        .chains
        .as_deref()
        .ok_or_else(|| anyhow!("хранилище цепочек недоступно (app_state.db не открыта)"))
}

/// Короткие имена книг активного перевода (настройка, иначе первый).
fn book_labels(services: &AppServices) -> HashMap<i64, String> {
    let mut labels = HashMap::new();
    let Some(module) = services.default_translation() else {
        return labels;
    };
    let mut registry = services.registry.lock();
    for entry in registry.books_of(&module) {
        labels.insert(entry.book_number, entry.short_name);
    }
    labels
}

pub fn format_reference(reference: &Reference, labels: &HashMap<i64, String>) -> String {
    let book = labels
        .get(&reference.book_number)
        .cloned()
        .unwrap_or_else(|| format!("Кн{}", reference.book_number));
    match (reference.verse, reference.end_verse) {
        (None, _) => format!("{book} {}", reference.chapter),
        (Some(verse), None) => format!("{book} {}:{verse}", reference.chapter),
        (Some(verse), Some(end)) => format!("{book} {}:{verse}-{end}", reference.chapter),
    }
}

pub fn list_chains(services: &AppServices) -> Vec<ChainSummary> {
    let Ok(store) = store(services) else {
        return Vec::new();
    };
    let chains = store.list_chains().unwrap_or_else(|error| {
        log::warn!("chains list failed: {error:#}");
        Vec::new()
    });
    chains
        .into_iter()
        .map(|chain| {
            let node_count = store
                .nodes_for_chain(&chain.id)
                .map(|nodes| nodes.len())
                .unwrap_or(0);
            ChainSummary {
                id: chain.id,
                title: chain.title,
                node_count,
            }
        })
        .collect()
}

pub fn create_chain(services: &AppServices, title: &str) -> Result<ChainSummary> {
    let store = store(services)?;
    let id = chains::new_id();
    store.chain_upsert(&id, title)?;
    Ok(ChainSummary {
        id,
        title: title.to_string(),
        node_count: 0,
    })
}

pub fn rename_chain(services: &AppServices, id: &str, title: &str) -> Result<()> {
    store(services)?.chain_upsert(id, title)
}

pub fn delete_chain(services: &AppServices, id: &str) -> Result<()> {
    store(services)?.chain_delete(id)
}

/// Загрузить цепочку целиком для отрисовки. None — нет такой цепочки.
pub fn load_chain(services: &AppServices, id: &str) -> Result<Option<ChainView>> {
    let store = store(services)?;
    let Some(chain) = store.chain_get(id)? else {
        return Ok(None);
    };
    let nodes = store.nodes_for_chain(id)?;
    let edges = store.edges_for_chain(id)?;
    let graph = ChainGraph::build(chain.id.clone(), nodes, edges.clone())
        .with_context(|| format!("loading chain {id}"))?;
    let (bible_module, module_overrides) = store.bible_modules_for_chain(id)?;
    let labels = book_labels(services);
    let nodes = graph
        .topo_order
        .iter()
        .filter_map(|node_id| {
            let node = graph.nodes.get(node_id)?;
            let labels_for_node = node
                .refs
                .iter()
                .map(|reference| RefLabel {
                    label: format_reference(reference, &labels),
                    reference: reference.clone(),
                })
                .collect();
            Some(ChainNodeView {
                depth: graph.depth.get(node_id).copied().unwrap_or(0),
                parents: graph.parents.get(node_id).map_or(0, Vec::len),
                children: graph.children.get(node_id).map_or(0, Vec::len),
                labels: labels_for_node,
                node: node.clone(),
                bible_module: module_overrides.get(node_id).cloned().map(ModuleId),
            })
        })
        .collect();
    Ok(Some(ChainView {
        id: chain.id,
        title: chain.title,
        nodes,
        bible_module: bible_module.map(ModuleId),
        edges,
    }))
}

pub fn set_bible_module(
    services: &AppServices,
    chain: &str,
    node: Option<&str>,
    module: Option<&ModuleId>,
) -> Result<()> {
    store(services)?.set_bible_module(chain, node, module.map(|id| id.0.as_str()))
}

/// Новый узел: корнем (after=None) или следствием узла `after`
/// (ребро after -> new). Цикл невозможен — узел свежий.
pub fn add_node(
    services: &AppServices,
    chain_id: &str,
    after: Option<&str>,
    refs: Vec<Reference>,
    assertion: String,
) -> Result<ChainNode> {
    insert_node(services, chain_id, after, None, refs, assertion)
}

pub fn insert_node(
    services: &AppServices,
    chain_id: &str,
    source: Option<&str>,
    target: Option<&str>,
    refs: Vec<Reference>,
    assertion: String,
) -> Result<ChainNode> {
    let store = store(services)?;
    if store.chain_get(chain_id)?.is_none() {
        return Err(anyhow!("нет такой цепочки: {chain_id}"));
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let node = ChainNode {
        id: chains::new_id(),
        chain_id: chain_id.to_string(),
        refs,
        assertion,
        created_at: now,
        updated_at: now,
        sort_key: now,
    };
    store.insert_node(&node, source, target)?;
    Ok(node)
}

/// Обновить ссылки и утверждение существующего узла.
pub fn update_node(
    services: &AppServices,
    node_id: &str,
    refs: Vec<Reference>,
    assertion: String,
) -> Result<()> {
    let store = store(services)?;
    let mut node = store
        .node_get(node_id)?
        .ok_or_else(|| anyhow!("нет такого узла: {node_id}"))?;
    node.refs = refs;
    node.assertion = assertion;
    node.updated_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    store.node_upsert(&node)
}

pub fn delete_node(services: &AppServices, node_id: &str) -> Result<()> {
    store(services)?.node_delete(node_id)
}

/// Добавить узел «в конец цепочки» (этап 23, перенос AppendLeaf форка):
/// новый узел становится следствием всех текущих листьев; в пустой
/// цепочке — свободным корнем.
pub fn append_to_chain(
    services: &AppServices,
    chain_id: &str,
    refs: Vec<Reference>,
    assertion: String,
) -> Result<ChainNode> {
    let store = store(services)?;
    if store.chain_get(chain_id)?.is_none() {
        return Err(anyhow!("нет такой цепочки: {chain_id}"));
    }
    let nodes = store.nodes_for_chain(chain_id)?;
    let edges = store.edges_for_chain(chain_id)?;
    let graph = ChainGraph::build(chain_id.to_string(), nodes, edges)
        .with_context(|| format!("loading chain {chain_id}"))?;
    if graph.leaves.is_empty() {
        return add_node(services, chain_id, None, refs, assertion);
    }
    let mut leaves = graph.leaves.clone();
    leaves.sort();
    let first = leaves.remove(0);
    let node = add_node(services, chain_id, Some(&first), refs, assertion)?;
    for leaf in leaves {
        store.edge_insert(&ChainEdge {
            chain_id: chain_id.to_string(),
            source_node_id: leaf,
            target_node_id: node.id.clone(),
        })?;
    }
    Ok(node)
}

/// Цепочки, использующие стих (полоса в «Контексте»). Кандидаты из БД
/// по главе, точная проверка диапазонов — доменом.
pub fn chains_for_verse(
    services: &AppServices,
    book_number: i64,
    chapter: u16,
    verse: u16,
) -> Vec<ChainHit> {
    let Ok(store) = store(services) else {
        return Vec::new();
    };
    let candidates = store
        .nodes_referencing_chapter(book_number, chapter)
        .unwrap_or_else(|error| {
            log::warn!("chains_for_verse query failed: {error:#}");
            Vec::new()
        });
    let mut seen: HashSet<String> = HashSet::new();
    let mut hits = Vec::new();
    for node in candidates {
        let covered = node.refs.iter().any(|reference| {
            reference.book_number == book_number && reference_covers(reference, chapter, verse)
        });
        if !covered || !seen.insert(node.chain_id.clone()) {
            continue;
        }
        if let Ok(Some(chain)) = store.chain_get(&node.chain_id) {
            hits.push(ChainHit {
                id: chain.id,
                title: chain.title,
            });
        }
    }
    hits
}

/// Стихи книги, помеченные цепочками: глава -> набор стихов (маркер ⛓
/// в чтении). Ссылка на главу целиком помечает первый стих (эвристика
/// форка — редкий случай, а маркер должен быть где-то виден).
pub fn verses_with_chains_in_book(
    services: &AppServices,
    book_number: i64,
    chapters: &HashSet<u16>,
) -> HashMap<u16, HashSet<u16>> {
    let Ok(store) = store(services) else {
        return HashMap::new();
    };
    let mut out: HashMap<u16, HashSet<u16>> = HashMap::new();
    for chapter in chapters {
        let candidates = store
            .nodes_referencing_chapter(book_number, *chapter)
            .unwrap_or_else(|error| {
                log::warn!("verses_with_chains query failed: {error:#}");
                Vec::new()
            });
        for node in candidates {
            for reference in &node.refs {
                if reference.book_number != book_number || reference.chapter != *chapter {
                    continue;
                }
                match (reference.verse, reference.end_verse) {
                    (None, _) => {
                        out.entry(*chapter).or_default().insert(1);
                    }
                    (Some(start), end) => {
                        let end = end.unwrap_or(start).max(start);
                        for verse in start..=end {
                            out.entry(*chapter).or_default().insert(verse);
                        }
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Arc;

    use app_state::db::StateDb;
    use bible_domain::ids::{BookNumber, ModuleId};
    use bible_domain::ports::ModuleKind;
    use mybible::{ModuleRegistry, SharedRegistry};
    use parking_lot::{Mutex, RwLock};

    use crate::services::AppServices;

    fn write_bible(dir: &Path, name: &str) {
        let path = dir.join(format!("{name}.sqlite3"));
        let connection = sqlez::connection::Connection::open_file(path.to_str().unwrap());
        connection
            .exec("CREATE TABLE info(name TEXT, value TEXT)")
            .unwrap()()
        .unwrap();
        connection
            .exec("INSERT INTO info VALUES('language','ru'),('short_name','ТЕСТ')")
            .unwrap()()
        .unwrap();
        connection
            .exec(
                "CREATE TABLE books(\
                   book_number INTEGER, short_name TEXT, long_name TEXT)",
            )
            .unwrap()()
        .unwrap();
        connection
            .exec("INSERT INTO books VALUES(500,'Ин','Иоанн')")
            .unwrap()()
        .unwrap();
        connection
            .exec(
                "CREATE TABLE verses(\
                   book_number INTEGER, chapter INTEGER, verse INTEGER, text TEXT)",
            )
            .unwrap()()
        .unwrap();
        connection
            .exec("INSERT INTO verses VALUES(500, 3, 16, 'текст')")
            .unwrap()()
        .unwrap();
        drop(connection);
    }

    fn services(dir: &Path) -> AppServices {
        let mut settings = app_state::settings::Settings::default();
        settings.modules_dir = dir.to_path_buf();
        AppServices {
            settings: RwLock::new(settings),
            registry: SharedRegistry::new(ModuleRegistry::scan(dir)),
            current_location: Arc::new(Mutex::new(None)),
            state_db: None,
            user_cross_refs: None,
            annotations: None,
            chains: Some(Arc::new(app_state::chains::SqliteChainStore::new(
                StateDb::in_memory().unwrap(),
            ))),
            tags: None,
            search_index: None,
            index_building: Arc::new(Mutex::new(std::collections::HashSet::new())),
            notes: RwLock::new(None),
            _notes_watcher: Mutex::new(None),
        }
    }

    fn reference(book: i64, chapter: u16, verse: Option<u16>, end: Option<u16>) -> Reference {
        Reference {
            book_number: book,
            chapter,
            verse,
            end_verse: end,
        }
    }

    #[test]
    fn full_chain_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        write_bible(dir.path(), "TST");
        let services = services(dir.path());

        let chain = create_chain(&services, "О любви").unwrap();
        assert_eq!(list_chains(&services).len(), 1);

        let root = add_node(
            &services,
            &chain.id,
            None,
            vec![reference(500, 3, Some(16), None)],
            "Бог возлюбил мир".into(),
        )
        .unwrap();
        let second = add_node(
            &services,
            &chain.id,
            Some(&root.id),
            vec![reference(500, 3, Some(17), Some(18))],
            "сын отдан".into(),
        )
        .unwrap();
        let _ = second;

        let view = load_chain(&services, &chain.id).unwrap().unwrap();
        assert_eq!(view.title, "О любви");
        assert_eq!(view.nodes.len(), 2);
        assert_eq!(view.nodes[0].depth, 0);
        assert_eq!(view.nodes[1].depth, 1);
        assert_eq!(view.nodes[1].parents, 1);
        // Подпись по книгам тестового перевода.
        assert_eq!(view.nodes[0].labels[0].label, "Ин 3:16");

        // Полоса «стих в цепочках».
        let hits = chains_for_verse(&services, 500, 3, 16);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "О любви");
        assert!(chains_for_verse(&services, 500, 3, 1).is_empty());

        // Маркеры книги.
        let chapters: HashSet<u16> = [3u16].into_iter().collect();
        let marks = verses_with_chains_in_book(&services, 500, &chapters);
        assert!(marks.get(&3).unwrap().contains(&16));

        // Правка узла.
        update_node(
            &services,
            &root.id,
            vec![reference(500, 3, Some(15), None)],
            "обновлено".into(),
        )
        .unwrap();
        let view = load_chain(&services, &chain.id).unwrap().unwrap();
        assert_eq!(view.nodes[0].labels[0].label, "Ин 3:15");
        assert!(chains_for_verse(&services, 500, 3, 16).is_empty());
        assert_eq!(chains_for_verse(&services, 500, 3, 15).len(), 1);

        // Удаление узла и цепочки.
        delete_node(&services, &root.id).unwrap();
        let view = load_chain(&services, &chain.id).unwrap().unwrap();
        assert_eq!(view.nodes.len(), 1);
        delete_chain(&services, &chain.id).unwrap();
        assert!(list_chains(&services).is_empty());
    }

    #[test]
    fn range_reference_marks_all_covered_verses() {
        let dir = tempfile::tempdir().unwrap();
        write_bible(dir.path(), "TST");
        let services = services(dir.path());

        let chain = create_chain(&services, "Диапазон").unwrap();
        add_node(
            &services,
            &chain.id,
            None,
            vec![reference(500, 3, Some(14), Some(16))],
            "три стиха".into(),
        )
        .unwrap();

        for verse in [14, 15, 16] {
            assert_eq!(
                chains_for_verse(&services, 500, 3, verse).len(),
                1,
                "стих {verse} должен попадать в цепочку"
            );
        }
        assert!(chains_for_verse(&services, 500, 3, 13).is_empty());

        let chapters: HashSet<u16> = [3u16].into_iter().collect();
        let marks = verses_with_chains_in_book(&services, 500, &chapters);
        let verses = marks.get(&3).unwrap();
        assert!(verses.contains(&14) && verses.contains(&15) && verses.contains(&16));
        assert!(!verses.contains(&13));
    }

    #[test]
    fn whole_chapter_reference_marks_first_verse() {
        let dir = tempfile::tempdir().unwrap();
        write_bible(dir.path(), "TST");
        let services = services(dir.path());

        let chain = create_chain(&services, "Глава").unwrap();
        add_node(
            &services,
            &chain.id,
            None,
            vec![reference(500, 3, None, None)],
            "вся глава".into(),
        )
        .unwrap();

        // Любой стих главы покрывается (reference_covers), маркер — на первом.
        assert_eq!(chains_for_verse(&services, 500, 3, 7).len(), 1);
        let chapters: HashSet<u16> = [3u16].into_iter().collect();
        let marks = verses_with_chains_in_book(&services, 500, &chapters);
        assert_eq!(
            marks.get(&3).unwrap().iter().copied().collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn labels_fall_back_to_book_number() {
        let labels: HashMap<i64, String> = HashMap::new();
        assert_eq!(
            format_reference(&reference(777, 1, Some(1), None), &labels),
            "Кн777 1:1"
        );
    }

    #[test]
    fn append_to_chain_links_all_leaves() {
        let dir = tempfile::tempdir().unwrap();
        write_bible(dir.path(), "TST");
        let services = services(dir.path());

        let chain = create_chain(&services, "Ветвление").unwrap();
        let root = add_node(
            &services,
            &chain.id,
            None,
            vec![reference(500, 3, Some(1), None)],
            "корень".into(),
        )
        .unwrap();
        // Второй корень — у цепочки два листа.
        let second_root = add_node(
            &services,
            &chain.id,
            None,
            vec![reference(500, 3, Some(2), None)],
            "второй корень".into(),
        )
        .unwrap();

        let appended = append_to_chain(
            &services,
            &chain.id,
            vec![reference(500, 3, Some(3), None)],
            "следствие обоих".into(),
        )
        .unwrap();

        let view = load_chain(&services, &chain.id).unwrap().unwrap();
        let appended_view = view
            .nodes
            .iter()
            .find(|node| node.node.id == appended.id)
            .unwrap();
        assert_eq!(appended_view.parents, 2);
        assert_eq!(appended_view.depth, 1);
        // Оба прежних листа стали родителями.
        let graph = ChainGraph::build(
            chain.id.clone(),
            store(&services)
                .unwrap()
                .nodes_for_chain(&chain.id)
                .unwrap(),
            store(&services)
                .unwrap()
                .edges_for_chain(&chain.id)
                .unwrap(),
        )
        .unwrap();
        assert!(graph.path_exists(&root.id, &appended.id));
        assert!(graph.path_exists(&second_root.id, &appended.id));
        let _ = view;
    }

    #[test]
    fn append_to_empty_chain_creates_root() {
        let dir = tempfile::tempdir().unwrap();
        write_bible(dir.path(), "TST");
        let services = services(dir.path());

        let chain = create_chain(&services, "Пустая").unwrap();
        let node = append_to_chain(
            &services,
            &chain.id,
            vec![reference(500, 3, Some(16), None)],
            "первый узел".into(),
        )
        .unwrap();
        let view = load_chain(&services, &chain.id).unwrap().unwrap();
        assert_eq!(view.nodes.len(), 1);
        assert_eq!(view.nodes[0].depth, 0);
        assert_eq!(view.nodes[0].parents, 0);
        let _ = node;
    }

    #[test]
    fn add_node_requires_existing_chain() {
        let dir = tempfile::tempdir().unwrap();
        write_bible(dir.path(), "TST");
        let services = services(dir.path());
        let result = add_node(&services, "missing", None, vec![], "текст".into());
        assert!(result.is_err());
    }

    #[test]
    fn unused_imports_guard() {
        // ModuleKind/BookNumber/ModuleId нужны write_bible косвенно; проверка
        // что реестр видит тестовый модуль.
        use bible_domain::ports::ModuleCatalog;
        let dir = tempfile::tempdir().unwrap();
        write_bible(dir.path(), "TST");
        let registry = SharedRegistry::new(ModuleRegistry::scan(dir.path()));
        let meta = registry.module(&ModuleId::from("TST")).unwrap();
        assert_eq!(meta.kind, ModuleKind::BibleTranslation);
        let mut guard = registry.lock();
        assert!(
            guard
                .books_of(&meta.id)
                .iter()
                .any(|b| b.book_number == 500)
        );
        drop(guard);
        let _ = BookNumber(500);
    }
}
