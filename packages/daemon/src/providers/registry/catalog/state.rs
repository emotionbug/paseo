use super::super::ProviderEntry;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::{Semaphore, watch};
use tokio_util::sync::CancellationToken;

pub(super) const GLOBAL: &str = "paseo:global";

#[derive(Clone)]
pub(crate) struct SnapshotRecord {
    pub entry: Value,
    pub content_hash: String,
}

#[derive(Clone)]
pub(crate) struct Snapshot {
    pub cwd: String,
    pub records: Vec<Arc<SnapshotRecord>>,
}

pub(super) struct ProviderState {
    pub entry: Arc<ProviderEntry>,
    pub initial: Arc<SnapshotRecord>,
    pub limit: Arc<Semaphore>,
    pub cancel: CancellationToken,
    pub replacement_models: Option<Result<Vec<Value>, String>>,
}

#[derive(Clone)]
pub(super) struct Binding {
    pub id: u64,
    pub key: Option<String>,
    pub failure: Option<Arc<SnapshotRecord>>,
    pub force: bool,
    pub done: watch::Receiver<bool>,
}

impl Binding {
    pub fn superseding(
        id: u64,
        previous: Option<&Self>,
        force: bool,
        done: watch::Receiver<bool>,
    ) -> Self {
        Self {
            id,
            key: previous.and_then(|binding| binding.key.clone()),
            failure: previous.and_then(|binding| binding.failure.clone()),
            force: force || previous.is_some_and(|binding| binding.force),
            done,
        }
    }

    pub fn fail_key(&mut self, initial: &SnapshotRecord, error: &str) {
        self.force = false;
        self.key = None;
        let mut failure = initial.entry.clone();
        failure["status"] = json!("error");
        failure["error"] = json!(error);
        self.failure = Some(identify(failure));
    }
}

pub(super) struct Load {
    pub id: u64,
    pub done: watch::Receiver<bool>,
    pub cancel: CancellationToken,
}

#[derive(Default)]
pub(super) struct Catalog {
    pub result: Option<Arc<SnapshotRecord>>,
    pub stale: bool,
    pub load: Option<Load>,
}

impl Catalog {
    pub fn reusable(&self, force: bool) -> bool {
        !force && (self.load.is_some() || self.result.is_some() && !self.stale)
    }
}

pub(super) struct Target {
    pub bindings: HashMap<String, Binding>,
    pub snapshot: Snapshot,
}

#[derive(Default)]
pub(super) struct Cache {
    pub order: Vec<String>,
    pub providers: HashMap<String, Arc<ProviderState>>,
    pub catalogs: HashMap<String, HashMap<String, Catalog>>,
    pub targets: HashMap<String, Target>,
    pub target_order: Vec<String>,
    pub sequence: u64,
    pub stopped: bool,
}

impl Cache {
    pub fn next_id(&mut self) -> u64 {
        self.sequence = self
            .sequence
            .checked_add(1)
            .expect("catalog sequence exhausted");
        self.sequence
    }

    pub fn target(&mut self, cwd: &str) -> &mut Target {
        if !self.targets.contains_key(cwd) {
            let records = self
                .order
                .iter()
                .map(|provider| self.providers[provider].initial.clone())
                .collect();
            self.targets.insert(
                cwd.to_owned(),
                Target {
                    bindings: HashMap::new(),
                    snapshot: Snapshot {
                        cwd: cwd.to_owned(),
                        records,
                    },
                },
            );
            self.target_order.push(cwd.to_owned());
        }
        self.targets.get_mut(cwd).expect("target was inserted")
    }

    pub fn selected(&self, providers: Option<&[String]>) -> Vec<String> {
        match providers.filter(|providers| !providers.is_empty()) {
            None => self.order.clone(),
            Some(providers) => {
                let mut seen = HashSet::new();
                providers
                    .iter()
                    .filter(|id| self.providers.contains_key(*id) && seen.insert(id.as_str()))
                    .cloned()
                    .collect()
            }
        }
    }

    pub fn publish(&mut self, cwds: &[String]) -> Vec<(Snapshot, Snapshot)> {
        if self.stopped {
            return Vec::new();
        }
        let mut transitions = Vec::new();
        for cwd in cwds {
            let Some(target) = self.targets.get_mut(cwd) else {
                continue;
            };
            let records = self
                .order
                .iter()
                .map(|provider| {
                    let binding = target.bindings.get(provider);
                    binding
                        .and_then(|binding| binding.failure.as_ref())
                        .or_else(|| {
                            binding
                                .and_then(|binding| binding.key.as_ref())
                                .and_then(|key| self.catalogs.get(key)?.get(provider))
                                .and_then(|catalog| catalog.result.as_ref())
                        })
                        .unwrap_or(&self.providers[provider].initial)
                        .clone()
                })
                .collect::<Vec<_>>();
            if same_records(&target.snapshot.records, &records) {
                continue;
            }
            let current = Snapshot {
                cwd: cwd.clone(),
                records,
            };
            let previous = std::mem::replace(&mut target.snapshot, current.clone());
            transitions.push((previous, current));
        }
        transitions
    }
}

pub(super) fn identify(entry: Value) -> Arc<SnapshotRecord> {
    let mut content = entry.clone();
    // shift_remove retains the source's property order after object rest.
    if let Some(object) = content.as_object_mut() {
        object.shift_remove("fetchedAt");
    }
    let encoded = crate::json::stringify(&Value::Array(vec![
        json!("paseo.provider-result/1"),
        content,
    ]))
    .expect("JSON values have a JavaScript JSON representation");
    Arc::new(SnapshotRecord {
        entry,
        content_hash: URL_SAFE_NO_PAD.encode(Sha256::digest(encoded.as_bytes())),
    })
}

pub(super) fn initial(entry: &ProviderEntry) -> Arc<SnapshotRecord> {
    let mut value = json!({
        "provider": entry.spec.id,
        "status": if entry.enabled { "loading" } else { "unavailable" },
        "enabled": entry.enabled,
        "source": entry.source,
    });
    for field in ["label", "description", "iconSvg"] {
        if let Some(field_value) = entry.definition.get(field) {
            value[field] = field_value.clone();
        }
    }
    value["defaultModeId"] = entry
        .definition
        .get("defaultModeId")
        .cloned()
        .unwrap_or(Value::Null);
    identify(value)
}

pub(super) fn same_records(
    previous: &[Arc<SnapshotRecord>],
    current: &[Arc<SnapshotRecord>],
) -> bool {
    previous.len() == current.len()
        && previous.iter().zip(current).all(|(before, after)| {
            Arc::ptr_eq(before, after)
                || (before.entry["provider"] == after.entry["provider"]
                    && before.content_hash == after.content_hash
                    && before.entry.get("fetchedAt") == after.entry.get("fetchedAt"))
        })
}

pub(super) async fn completed(mut receiver: watch::Receiver<bool>) {
    while !*receiver.borrow_and_update() {
        if receiver.changed().await.is_err() {
            break;
        }
    }
}
