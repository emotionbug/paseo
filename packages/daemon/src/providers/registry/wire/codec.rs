use super::{
    super::catalog::{Snapshot, SnapshotRecord},
    compat::Compatibility,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub(super) fn visible_records(
    snapshot: &Snapshot,
    compatibility: Compatibility,
) -> impl Iterator<Item = &SnapshotRecord> {
    snapshot
        .records
        .iter()
        .map(|record| record.as_ref())
        .filter(move |record| {
            compatibility.visible(record.entry["provider"].as_str().unwrap_or(""))
        })
}

pub(super) fn same_records(
    previous: &Snapshot,
    current: &Snapshot,
    compatibility: Compatibility,
) -> bool {
    let mut previous = visible_records(previous, compatibility);
    let mut current = visible_records(current, compatibility);
    loop {
        match (previous.next(), current.next()) {
            (None, None) => return true,
            (Some(before), Some(after))
                if before.entry["provider"] == after.entry["provider"]
                    && before.content_hash == after.content_hash
                    && before.entry.get("fetchedAt") == after.entry.get("fetchedAt") =>
            {
                ()
            }
            _ => return false,
        }
    }
}

pub(super) fn payload(
    snapshot: &Snapshot,
    compatibility: Compatibility,
    request: Option<&Value>,
    generated_at: String,
) -> Value {
    let mut payload = json!({});
    if snapshot.cwd != "paseo:global" {
        payload["cwd"] = json!(snapshot.cwd);
    }
    payload["generatedAt"] = json!(generated_at);
    if !compatibility.compact {
        payload["entries"] = Value::Array(client_entries(snapshot, compatibility));
        return payload;
    }

    let records = visible_records(snapshot, compatibility)
        .map(|record| {
            json!([
                record.entry["provider"],
                record.content_hash,
                if compatibility.references {
                    &Value::Null
                } else {
                    &record.entry["fetchedAt"]
                }
            ])
        })
        .collect::<Vec<_>>();
    let identity = json!([
        "paseo.providers-snapshot/1",
        if compatibility.references {
            "references"
        } else {
            "embedded"
        },
        if compatibility.custom_icons {
            "icons"
        } else {
            "legacy-icons"
        },
        records
    ]);
    let hash = URL_SAFE_NO_PAD.encode(Sha256::digest(
        crate::json::stringify(&identity)
            .expect("snapshot identities contain only JSON strings and arrays")
            .as_bytes(),
    ));
    payload["entries"] = json!([]);
    payload["snapshotHash"] = json!(hash);
    if compatibility.references {
        let mut fetched_at = serde_json::Map::new();
        for record in visible_records(snapshot, compatibility) {
            if let Some(timestamp) = record
                .entry
                .get("fetchedAt")
                .filter(|timestamp| timestamp.as_str().is_some_and(|text| !text.is_empty()))
            {
                fetched_at.insert(
                    record.entry["provider"]
                        .as_str()
                        .expect("snapshot provider is a string")
                        .to_owned(),
                    timestamp.clone(),
                );
            }
        }
        payload["fetchedAt"] = Value::Object(fetched_at);
    }
    if request.is_some_and(|request| request["ifNoneMatch"].as_str() == Some(hash.as_str())) {
        payload["notModified"] = json!(true);
        return payload;
    }
    // A reference push advertises identity/freshness only. Explicit pulls supply content.
    if request.is_none() && compatibility.references {
        return payload;
    }
    let mut entries = client_entries(snapshot, compatibility);
    if compatibility.references {
        for entry in &mut entries {
            entry
                .as_object_mut()
                .expect("snapshot entry is an object")
                .shift_remove("fetchedAt");
        }
    }
    payload["compactSnapshot"] = compact(entries);
    payload
}

fn client_entries(snapshot: &Snapshot, compatibility: Compatibility) -> Vec<Value> {
    visible_records(snapshot, compatibility)
        .map(|record| {
            let mut entry = record.entry.clone();
            if let Some(modes) = entry.get_mut("modes") {
                compatibility.modes(modes);
            }
            entry
        })
        .collect()
}

// Preserve model extension fields and absent-vs-null distinctions, just as the
// protocol's object-rest codec does. Thinking sets are shared across all providers.
pub(super) fn compact(mut entries: Vec<Value>) -> Value {
    let mut sets = Vec::new();
    let mut indexes = HashMap::new();
    for entry in &mut entries {
        let Some(models) = entry.get_mut("models").and_then(Value::as_array_mut) else {
            continue;
        };
        for model in models {
            let fields = model.as_object_mut().expect("catalog model is an object");
            fields.shift_remove("provider");
            let options = fields.shift_remove("thinkingOptions");
            let default = fields.shift_remove("defaultThinkingOptionId");
            let Some(options) = options else {
                if let Some(default) = default {
                    fields.insert("defaultThinkingOptionId".into(), default);
                }
                continue;
            };
            let mut set = json!({"options": options});
            if let Some(default) = default {
                set["defaultOptionId"] = default;
            }
            let key = crate::json::stringify(&set).expect("catalog thinking sets are JSON values");
            let next = sets.len();
            let index = *indexes.entry(key).or_insert_with(|| {
                sets.push(set);
                next
            });
            fields.insert("thinkingSet".into(), json!(index));
        }
    }
    json!({"entries": entries, "thinkingSets": sets})
}
