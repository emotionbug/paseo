use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Result, anyhow};
use serde::Serialize;
use serde_json::Value;

use super::projection::same_json_fingerprint;

pub const DEFAULT_MAX_TOMBSTONES: usize = 5_000;

type IdProjection = fn(&Value) -> Result<&str>;
type Fingerprint = fn(&Value) -> String;

struct StoredValue {
    seq: u64,
    inserted_at: u64,
    value: Value,
    fingerprint: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct VersionedValue<'a> {
    pub seq: u64,
    pub value: &'a Value,
}

#[derive(Debug, Serialize)]
pub struct VersionedRemoval<'a> {
    pub id: &'a str,
    pub seq: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionRead<'a> {
    pub mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    pub head_seq: u64,
    pub values: Vec<VersionedValue<'a>>,
    pub removals: Vec<VersionedRemoval<'a>>,
}

/// Latest visible state, not an event log. Removed records are retained only up
/// to the tombstone limit; cursors requiring discarded removals get a snapshot.
pub struct VersionedCollection {
    values: HashMap<String, StoredValue>,
    tombstones: HashMap<String, u64>,
    tombstones_by_seq: BTreeMap<u64, String>,
    get_id: IdProjection,
    fingerprint: Option<Fingerprint>,
    max_tombstones: usize,
    expired_through_seq: u64,
    head_seq: u64,
}

impl VersionedCollection {
    pub fn new(get_id: IdProjection) -> Self {
        Self::with_options(get_id, None, DEFAULT_MAX_TOMBSTONES)
    }

    pub fn with_options(
        get_id: IdProjection,
        fingerprint: Option<Fingerprint>,
        max_tombstones: usize,
    ) -> Self {
        Self {
            values: HashMap::new(),
            tombstones: HashMap::new(),
            tombstones_by_seq: BTreeMap::new(),
            get_id,
            fingerprint,
            max_tombstones,
            expired_through_seq: 0,
            head_seq: 0,
        }
    }

    pub fn head_seq(&self) -> u64 {
        self.head_seq
    }

    pub fn sequence_for(&self, id: &str) -> Option<u64> {
        self.values
            .get(id)
            .map(|entry| entry.seq)
            .or_else(|| self.tombstones.get(id).copied())
    }

    /// Returns the new sequence, or None when the projection is unchanged.
    pub fn replace(&mut self, value: Value) -> Result<Option<u64>> {
        let id = (self.get_id)(&value)?;
        let fingerprint = self.fingerprint.map(|project| project(&value));
        if let Some(previous) = self.values.get(id) {
            let unchanged = match (&previous.fingerprint, &fingerprint) {
                (Some(previous), Some(next)) => previous == next,
                _ => same_json_fingerprint(&previous.value, &value),
            };
            if unchanged {
                return Ok(None);
            }
        }
        let seq = self.advance()?;
        let inserted_at = self.values.get(id).map_or(seq, |entry| entry.inserted_at);
        if let Some(removed_seq) = self.tombstones.remove(id) {
            self.tombstones_by_seq.remove(&removed_seq);
        }
        self.values.insert(
            id.to_owned(),
            StoredValue {
                seq,
                inserted_at,
                value,
                fingerprint,
            },
        );
        Ok(Some(seq))
    }

    pub fn remove(&mut self, id: &str) -> Result<Option<u64>> {
        if !self.values.contains_key(id) {
            return Ok(None);
        }
        let seq = self.advance()?;
        self.values.remove(id);
        self.tombstones.insert(id.to_owned(), seq);
        self.tombstones_by_seq.insert(seq, id.to_owned());
        while self.tombstones_by_seq.len() > self.max_tombstones {
            if let Some((expired, id)) = self.tombstones_by_seq.pop_first() {
                self.tombstones.remove(&id);
                self.expired_through_seq = self.expired_through_seq.max(expired);
            }
        }
        Ok(Some(seq))
    }

    pub fn replace_all(&mut self, values: Vec<Value>) -> Result<()> {
        // Validate before changing the visible state; preserve source iteration
        // order (including duplicate IDs), and Map insertion order for removals.
        let retained: HashSet<String> = values
            .iter()
            .map(|value| (self.get_id)(value).map(str::to_owned))
            .collect::<Result<_>>()?;
        for value in values {
            self.replace(value)?;
        }
        let mut removed: Vec<_> = self
            .values
            .iter()
            .filter(|(id, _)| !retained.contains(id.as_str()))
            .map(|(id, entry)| (entry.inserted_at, id.clone()))
            .collect();
        removed.sort_unstable_by_key(|(inserted_at, _)| *inserted_at);
        for (_, id) in removed {
            self.remove(&id)?;
        }
        Ok(())
    }

    pub fn read_snapshot(&self, reason: &'static str) -> CollectionRead<'_> {
        CollectionRead {
            mode: "snapshot",
            reason: Some(reason),
            head_seq: self.head_seq,
            values: self.sorted_values(None),
            removals: Vec::new(),
        }
    }

    pub fn read_after(&self, after_seq: u64) -> CollectionRead<'_> {
        if after_seq < self.expired_through_seq || after_seq > self.head_seq {
            return self.read_snapshot("cursor_expired");
        }
        CollectionRead {
            mode: "changes",
            reason: None,
            head_seq: self.head_seq,
            values: self.sorted_values(Some(after_seq)),
            removals: self
                .tombstones_by_seq
                .range((
                    std::ops::Bound::Excluded(after_seq),
                    std::ops::Bound::Unbounded,
                ))
                .map(|(&seq, id)| VersionedRemoval { id, seq })
                .collect(),
        }
    }

    fn sorted_values(&self, after_seq: Option<u64>) -> Vec<VersionedValue<'_>> {
        let mut values: Vec<_> = self
            .values
            .values()
            .filter(|entry| after_seq.is_none_or(|seq| entry.seq > seq))
            .map(|entry| VersionedValue {
                seq: entry.seq,
                value: &entry.value,
            })
            .collect();
        // Every published sequence is unique, so the source ID tie-breaker is
        // unreachable for collection-produced state.
        values.sort_unstable_by_key(|entry| entry.seq);
        values
    }

    fn advance(&mut self) -> Result<u64> {
        self.head_seq = self
            .head_seq
            .checked_add(1)
            .ok_or_else(|| anyhow!("Directory sequence exhausted"))?;
        Ok(self.head_seq)
    }
}
