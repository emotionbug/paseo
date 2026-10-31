use serde_json::{Value, json};
use std::sync::LazyLock;

static DEFINITIONS: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../protocol/src/provider-definitions.json"
    ))
    .expect("valid shared provider manifest")
});

pub fn builtins() -> &'static [Value] {
    DEFINITIONS["builtins"]
        .as_array()
        .expect("builtin provider definitions")
}

pub fn development() -> &'static [Value] {
    DEFINITIONS["development"]
        .as_array()
        .expect("development provider definitions")
}

pub fn native_plugin_definitions() -> Vec<Value> {
    [
        ("antigravity","antigravity-provider","Antigravity","Antigravity CLI on your host",false,"agy",include_str!("../../../../plugins/antigravity-provider/icon.svg")),
        ("hoplite","hoplite-provider","Hoplite","Hoplite's coding agent through the local `hoplite acp` bridge",true,"hoplite",include_str!("../../../../plugins/hoplite-provider/icon.svg")),
        ("muse","muse-provider","Muse Code","Plugin provider Muse Code",false,"muse",include_str!("../../../../plugins/muse-provider/icon.svg")),
    ].into_iter().map(|(id,plugin,label,description,required,command,icon)| json!({
        "id":id,"pluginId":plugin,"nativeFamily":id,"label":label,"description":description,
        "requiresConfiguration":required,"command":if id=="hoplite" {vec![command,"acp"]}else{vec![command]},
        "defaultModeId":null,"modes":[],"iconSvg":icon,"supportsExactMcpPreapproval":true
    })).collect()
}
