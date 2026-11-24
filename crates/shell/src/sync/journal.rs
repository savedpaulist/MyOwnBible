use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, ensure};
use app_state::sync_data::{Key, Record, Records};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Revision {
    pub id: Uuid,
    pub key: Key,
    pub parents: Vec<Uuid>,
    pub value: Option<Value>,
}

impl Revision {
    pub fn new(key: Key, parents: Vec<Uuid>, value: Option<Value>) -> Self {
        Self {
            id: Uuid::now_v7(),
            key,
            parents,
            value,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct History {
    revisions: BTreeMap<Uuid, Revision>,
}

impl History {
    pub fn insert(&mut self, revision: Revision) -> Result<()> {
        ensure!(
            !revision.parents.contains(&revision.id),
            "Ревизия ссылается на себя"
        );
        if let Some(existing) = self.revisions.get(&revision.id) {
            ensure!(existing == &revision, "Содержимое ревизии изменилось");
        } else {
            self.revisions.insert(revision.id, revision);
        }
        Ok(())
    }

    pub fn heads(&self) -> Result<BTreeMap<Key, Vec<Uuid>>> {
        let mut heads = BTreeMap::<Key, Vec<Uuid>>::new();
        let mut superseded = BTreeSet::new();
        for revision in self.revisions.values() {
            for parent in &revision.parents {
                let parent = self
                    .revisions
                    .get(parent)
                    .ok_or_else(|| anyhow::anyhow!("Не найдена предыдущая ревизия"))?;
                ensure!(
                    parent.key == revision.key,
                    "Ревизия ссылается на другую запись"
                );
                // UUIDs identify revisions only. Order comes from parent links, not device time.
                superseded.insert(parent.id);
            }
        }
        for (id, revision) in &self.revisions {
            if !superseded.contains(id) {
                heads.entry(revision.key.clone()).or_default().push(*id);
            }
        }
        Ok(heads)
    }

    pub fn value(&self, id: Uuid) -> Option<&Value> {
        self.revisions
            .get(&id)
            .and_then(|revision| revision.value.as_ref())
    }

    fn ancestors(&self, id: Uuid) -> BTreeSet<Uuid> {
        let mut result = BTreeSet::new();
        let mut pending = vec![id];
        while let Some(id) = pending.pop() {
            if result.insert(id)
                && let Some(revision) = self.revisions.get(&id)
            {
                pending.extend(revision.parents.iter().copied());
            }
        }
        result
    }

    // The outer Option indicates whether automatic merge is safe. The inner
    // Option contains the merged value, including deletion.
    pub fn merge(&self, heads: &[Uuid]) -> Option<Option<Value>> {
        let (first, rest) = heads.split_first()?;
        let first_value = self.value(*first);
        if rest.iter().all(|id| self.value(*id) == first_value) {
            return Some(first_value.cloned());
        }

        let mut common = self.ancestors(*first);
        for id in rest {
            let ancestors = self.ancestors(*id);
            common.retain(|ancestor| ancestors.contains(ancestor));
        }
        let mut superseded = BTreeSet::new();
        for id in &common {
            if let Some(revision) = self.revisions.get(id) {
                superseded.extend(revision.parents.iter().copied());
            }
        }
        common.retain(|id| !superseded.contains(id));
        if common.len() != 1 {
            return None;
        }
        let base = self.value(*common.first()?)?.as_object()?;
        let mut result = base.clone();
        for id in heads {
            let version = self.value(*id)?.as_object()?;
            for (field, value) in version {
                if field == "updated_at" {
                    if value.as_i64() > result.get(field).and_then(Value::as_i64) {
                        result.insert(field.clone(), value.clone());
                    }
                    continue;
                }
                if base.get(field) == Some(value) {
                    continue;
                }
                if result.get(field) != base.get(field) && result.get(field) != Some(value) {
                    return None;
                }
                result.insert(field.clone(), value.clone());
            }
        }
        Some(Some(Value::Object(result)))
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Materialized {
    pub key: Key,
    pub value: Option<Value>,
    pub parents: Vec<Uuid>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Apply {
    pub previous: Option<Value>,
    pub next: Materialized,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Batch {
    pub version: u32,
    pub revisions: Vec<Revision>,
}

#[derive(Serialize, Deserialize)]
pub struct Upload {
    pub drive_id: String,
    pub batch: Batch,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Journal {
    pub enabled: bool,
    pub client_id: Option<String>,
    pub account: Option<String>,
    pub cursor: Option<String>,
    pub vault: Option<std::path::PathBuf>,
    pub history: History,
    pub applied: Vec<Materialized>,
    pub outgoing: Vec<Revision>,
    pub upload: Option<Upload>,
    pub apply: Vec<Apply>,
    pub last_success: Option<u64>,
}

impl Journal {
    pub fn materialized(&self, key: &Key) -> Option<&Materialized> {
        self.applied
            .binary_search_by(|item| item.key.cmp(key))
            .ok()
            .map(|index| &self.applied[index])
    }

    pub fn set_materialized(&mut self, item: Materialized) {
        match self
            .applied
            .binary_search_by(|existing| existing.key.cmp(&item.key))
        {
            Ok(index) => self.applied[index] = item,
            Err(index) => self.applied.insert(index, item),
        }
    }

    pub fn commit(&mut self, revision: Revision) -> Result<()> {
        self.history.insert(revision.clone())?;
        self.outgoing.push(revision);
        Ok(())
    }

    pub fn capture(&mut self, current: &Records, notes_enabled: bool) -> Result<()> {
        let keys: BTreeSet<_> = current
            .keys()
            .chain(self.applied.iter().map(|record| &record.key))
            .cloned()
            .collect();
        for key in keys {
            if key.kind == app_state::sync_data::Kind::Note && !notes_enabled {
                continue;
            }
            let previous = self.materialized(&key);
            let value = current.get(&key);
            if previous.and_then(|it| it.value.as_ref()) == value {
                continue;
            }
            let parents = previous.map(|it| it.parents.clone()).unwrap_or_default();
            let revision = Revision::new(key.clone(), parents, value.cloned());
            let id = revision.id;
            self.commit(revision)?;
            self.set_materialized(Materialized {
                key,
                value: value.cloned(),
                parents: vec![id],
            });
        }
        Ok(())
    }
}

pub fn records(values: Records) -> Vec<Record> {
    values
        .into_iter()
        .map(|(key, value)| Record { key, value })
        .collect()
}
