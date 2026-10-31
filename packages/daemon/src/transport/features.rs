use serde_json::{Value, json};

pub fn features(config: &Value, hoplite: bool) -> Value {
    let mut result = serde_json::Map::new();
    for name in [
        "connectedHosts",
        "hostMultiplexing",
        "relayProtectedSecurity",
        "usageSources",
        "ownedSubscriptions",
        "agentRequestReceipts",
        "workspaceRequestReceipts",
        "creationLifecycle",
        "hubAgentRpc",
        "backgroundWorkspaces",
        "directorySync",
        "workspaceSetupRun",
        "workspaceSetupObservation",
        "workspaceSortTimes",
        "providersSnapshot",
        "providersSnapshotCwd",
        "checkoutForgeSetAutoMerge",
        "checkoutGithubSetAutoMerge",
        "githubCheckDetails",
        "forgeCheckDetails",
        "forgeSearch",
        "daemonConfigReload",
        "pushTokenRevocation",
        "plugins",
        "pluginManagement",
        "pluginGitManagement",
        "pluginSourceInstallation",
        "pluginSourceUpdates",
        "pluginLogs",
        "pluginThemes",
        "pluginSettings",
        "pluginTimelineItems",
        "skillManagement",
        "terminal-restore-modes",
        "terminal-input-mode-replay",
        "terminal-size-ownership",
        "workspaceTerminals",
        "rewind",
        "agentTimelinePromptIndex",
        "agentHistorySearch",
        "checkoutRefresh",
        "workspaceMultiplicity",
        "projectRemove",
        "projectAdd",
        "projectList",
        "worktreeRestore",
        "workspaceRecovery",
        "workspaceFileEditing",
        "fileByteWrites",
        "providerUsageList",
        "agentDetach",
        "agentThinkingUpdate",
        "daemonDiagnostics",
        "agentForkContext",
        "agentForkContextCursor",
        "providerSubagents",
        "projectedSubagentTimeline",
        "providerSubagentNesting",
        "workspacePinning",
        "workspaceMarkUnread",
        "hubRelationship",
        "projectGithubClone",
        "workspaceGithubRepositorySearch",
        "projectCreateDirectory",
        "commitsList",
        "commitBaseClassification",
        "providerRemoval",
        "importSessionWorkspaceTarget",
        "importSessionSearch",
        "forgeProviders",
        "selectiveAgentTimeline",
        "explicitEventSubscriptions",
        "canonicalSubmittedPrompts",
        "agentMessageProvenance",
        "agentMessageBatching",
        "stableProjectIdentity",
        "workspaceScriptManagement",
        "projectCustomIcon",
        "fsEntryOps",
        "fsEntryDuplicate",
        "checkoutDiscardChanges",
        "checkoutDiffAll",
        "agentProfiles",
        "agentConfigApply",
        "workspaceLabels",
        "daemonStatusRpc",
    ] {
        result.insert(name.to_owned(), Value::Bool(true));
    }
    result.insert(
        "daemonSelfUpdate".into(),
        json!(config["desktopManaged"].as_bool() != Some(true)),
    );
    result.insert("hopliteProvider".into(), json!(hoplite));
    if config["relayEnabledMutable"].as_bool().unwrap_or(true) {
        result.insert("relayConfig".into(), Value::Bool(true));
    }
    Value::Object(result)
}

pub fn speech_capabilities(readiness: &Value) -> Value {
    let state = |key: &str| {
        let item = &readiness[key];
        let reason = if item["available"] == true {
            String::new()
        } else if readiness["voiceFeature"]["reasonCode"] == "model_download_in_progress" {
            let base = readiness["voiceFeature"]["message"]
                .as_str()
                .unwrap_or("")
                .trim();
            if base.contains("Try again in a few minutes") {
                base.to_owned()
            } else {
                format!("{base} Try again in a few minutes.")
            }
        } else {
            item["message"].as_str().unwrap_or("").to_owned()
        };
        json!({"enabled":item["enabled"],"reason":reason})
    };
    json!({"voice":{"dictation":state("dictation"),"voice":state("realtimeVoice")}})
}
