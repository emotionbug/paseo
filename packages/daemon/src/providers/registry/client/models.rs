use anyhow::Result;
use serde_json::Value;

use crate::providers::api::AgentProvider;

pub(in crate::providers::registry) fn map_model(provider: &str, mut model: Value) -> Value {
    model["provider"] = Value::String(provider.to_owned());
    if model
        .get("defaultThinkingOptionId")
        .is_none_or(Value::is_null)
    {
        let default = model
            .get("thinkingOptions")
            .and_then(Value::as_array)
            .and_then(|options| options.iter().find(|option| option["isDefault"] == true))
            .and_then(|option| option.get("id"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        if let Some(default) = default {
            model["defaultThinkingOptionId"] = Value::String(default);
        }
    }
    model
}

pub(in crate::providers::registry) fn resolve_configured_models(
    provider: &str,
    client: &dyn AgentProvider,
    models: &[Value],
) -> Result<Vec<Value>> {
    models
        .iter()
        .map(|model| client.resolve_configured_model(map_model(provider, model.clone())))
        .collect()
}

pub(in crate::providers::registry) fn merge_models(
    provider: &str,
    profile_models: &[Value],
    additional_models: &[Value],
    runtime_models: Vec<Value>,
    profile_models_are_additive: bool,
) -> Vec<Value> {
    let replacement = !profile_models.is_empty() && !profile_models_are_additive;
    let mut merged: Vec<Value> = if replacement {
        profile_models
            .iter()
            .cloned()
            .map(|model| map_model(provider, model))
            .collect()
    } else {
        runtime_models
            .into_iter()
            .map(|model| map_model(provider, model))
            .collect()
    };
    let additions = if replacement { &[][..] } else { profile_models };
    let mut has_additional_default = false;
    for model in additions.iter().chain(additional_models) {
        let additional = map_model(provider, model.clone());
        has_additional_default |= additional["isDefault"] == true;
        if let Some(existing) = merged
            .iter_mut()
            .find(|existing| existing["id"] == model["id"])
        {
            let enables_compatibility =
                existing["isSelectable"] == false && additional.get("isSelectable").is_none();
            let Value::Object(additional) = additional else {
                unreachable!("mapped model is an object")
            };
            existing
                .as_object_mut()
                .expect("model object")
                .extend(additional);
            if enables_compatibility {
                existing["isSelectable"] = Value::Bool(true);
            }
        } else {
            merged.push(additional);
        }
    }
    if has_additional_default {
        for model in &mut merged {
            if !additions
                .iter()
                .chain(additional_models)
                .any(|addition| addition["isDefault"] == true && addition["id"] == model["id"])
            {
                model["isDefault"] = Value::Bool(false);
            }
        }
    }
    merged
}
