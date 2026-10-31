use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{
    events::EventBridge, merge_models, merge_provider_options, models::map_model,
    restore_stored_options,
};
use crate::providers::api::EventSink;

#[test]
fn replacement_models_keep_additions_and_normalize_thinking() {
    let models = merge_models(
        "custom",
        &[json!({"id":"configured","isDefault":true,
        "thinkingOptions":[{"id":"deep","isDefault":true}]})],
        &[json!({"id":"extra","isDefault":true})],
        vec![json!({"id":"runtime"})],
        false,
    );
    assert_eq!(
        models,
        vec![
            json!({"provider":"custom","id":"configured","isDefault":false,
        "thinkingOptions":[{"id":"deep","isDefault":true}],"defaultThinkingOptionId":"deep"}),
            json!({"provider":"custom","id":"extra","isDefault":true})
        ]
    );
}

#[test]
fn additive_model_enables_compatibility_without_discarding_metadata() {
    let models = merge_models(
        "alias",
        &[json!({"id":"compat","label":"Configured"})],
        &[json!({"id":"other","isSelectable":false})],
        vec![json!({"id":"compat","isSelectable":false,"metadata":{"native":true}})],
        true,
    );
    assert_eq!(
        models[0],
        json!({"provider":"alias","id":"compat","label":"Configured",
        "isSelectable":true,"metadata":{"native":true}})
    );
    assert_eq!(models[1]["isSelectable"], false);
}

#[test]
fn model_merge_preserves_duplicates_and_all_additional_default_ids() {
    let models = merge_models(
        "alias",
        &[],
        &[
            json!({"id":"b","isDefault":true}),
            json!({"id":"c","isDefault":true}),
        ],
        vec![
            json!({"id":"a","isDefault":true}),
            json!({"id":"a","label":"duplicate"}),
        ],
        false,
    );
    assert_eq!(models.len(), 4);
    assert_eq!(models[0]["isDefault"], false);
    assert_eq!(models[1]["isDefault"], false);
    assert_eq!(models[2]["isDefault"], true);
    assert_eq!(models[3]["isDefault"], true);
}

#[test]
fn model_normalization_preserves_explicit_default_and_is_idempotent() {
    for explicit in [json!("manual"), json!("")] {
        let model = json!({"id":"model","defaultThinkingOptionId":explicit,
            "thinkingOptions":[{"id":"deep","isDefault":true}]});
        let mapped = map_model("alias", model);
        assert_eq!(mapped["defaultThinkingOptionId"], explicit);
        assert_eq!(map_model("alias", mapped.clone()), mapped);
    }
}

#[test]
fn options_merge_objects_but_replace_arrays_and_explicit_nulls() {
    let defaults = json!({"nested":{"keep":1,"replace":[1,2],"clear":{"secret":true}},"base":true});
    let overrides = json!({"nested":{"replace":[3],"clear":null},"__proto__":{"safe":true}});
    assert_eq!(
        merge_provider_options(&defaults, Some(&overrides)),
        Some(
            json!({"nested":{"keep":1,"replace":[3],"clear":null},"base":true,"__proto__":{"safe":true}})
        )
    );
    assert_eq!(merge_provider_options(&Value::Null, None), None);
    assert_eq!(merge_provider_options(&defaults, None), Some(defaults));
}

#[test]
fn imported_configuration_retains_native_fields_not_launch_options() {
    let mut imported = json!({"provider":"alias","cwd":"/native","model":"native-model",
        "providerOptions":{"defaultOnly":true,"nested":{"launch":true}}});
    restore_stored_options(&mut imported, Some(json!({"nested":{"stored":true}})));
    assert_eq!(
        imported,
        json!({"provider":"alias","cwd":"/native","model":"native-model",
        "providerOptions":{"nested":{"stored":true}}})
    );
    restore_stored_options(&mut imported, None);
    assert!(imported.get("providerOptions").is_none());
}

#[tokio::test]
async fn event_bridge_drains_bounded_queues_in_order_on_close() {
    let (destination, mut receiver) = EventSink::channel(1, CancellationToken::new());
    let (bridge, sink) = EventBridge::new("alias".into(), destination);
    let produce = async {
        for index in 0..600 {
            sink.emit(json!({"provider":"raw","index":index}))
                .await
                .unwrap();
        }
        bridge.finish().await.unwrap();
    };
    let consume = async {
        let mut count = 0;
        while let Some(event) = receiver.recv().await {
            assert_eq!(event, json!({"provider":"alias","index":count}));
            count += 1;
        }
        count
    };
    let (_, count) = tokio::join!(produce, consume);
    assert_eq!(count, 600);
}

#[tokio::test]
async fn dropped_bridge_cancels_only_its_owned_sink() {
    let parent = CancellationToken::new();
    let (destination, _receiver) = EventSink::channel(1, parent.clone());
    let (bridge, sink) = EventBridge::new("alias".into(), destination);
    drop(bridge);
    assert!(sink.emit(json!({"type":"state"})).await.is_err());
    assert!(!parent.is_cancelled());
}
