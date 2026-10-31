use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use super::{
    events::{self, EventBridge},
    with_provider,
};
use crate::providers::api::{AgentSession, EventSink, RewindTarget};

pub(super) struct Session {
    pub(super) inner: Arc<dyn AgentSession>,
    pub(super) provider: String,
    pub(super) bridge: EventBridge,
}

#[async_trait]
impl AgentSession for Session {
    fn provider(&self) -> &str {
        &self.provider
    }
    fn native_id(&self) -> Option<String> {
        self.inner.native_id()
    }
    fn capabilities(&self) -> &Value {
        self.inner.capabilities()
    }
    fn features(&self) -> Arc<[Value]> {
        self.inner.features()
    }
    fn initial_timeline(&self) -> &[Value] {
        self.inner.initial_timeline()
    }
    fn describe_persistence(&self) -> Option<Value> {
        self.inner
            .describe_persistence()
            .map(|handle| with_provider(handle, &self.provider))
    }
    fn pending_permissions(&self) -> Vec<Value> {
        self.inner.pending_permissions()
    }
    fn usage_session(&self) -> Option<Value> {
        self.inner.usage_session()
    }
    async fn start_turn(&self, prompt: Value, options: Value) -> Result<Value> {
        self.inner.start_turn(prompt, options).await
    }
    async fn steer_active_turn(&self, prompt: Value, options: Value) -> Result<Value> {
        self.inner.steer_active_turn(prompt, options).await
    }
    async fn stream_history(&self, sink: EventSink) -> Result<()> {
        events::stream_history(self.inner.as_ref(), &self.provider, sink).await
    }
    async fn runtime_info(&self) -> Result<Value> {
        Ok(with_provider(
            self.inner.runtime_info().await?,
            &self.provider,
        ))
    }
    async fn available_modes(&self) -> Result<Vec<Value>> {
        self.inner.available_modes().await
    }
    async fn current_mode(&self) -> Result<Option<String>> {
        self.inner.current_mode().await
    }
    async fn set_mode(&self, mode: &str) -> Result<Option<Value>> {
        self.inner.set_mode(mode).await
    }
    async fn respond_to_permission(
        &self,
        request_id: &str,
        response: Value,
    ) -> Result<Option<Value>> {
        self.inner.respond_to_permission(request_id, response).await
    }
    async fn interrupt(&self) -> Result<()> {
        self.inner.interrupt().await
    }
    async fn close(&self) -> Result<()> {
        self.inner.close().await?;
        self.bridge.finish().await
    }
    async fn list_commands(&self) -> Result<Vec<Value>> {
        self.inner.list_commands().await
    }
    async fn set_model(&self, model: Option<&str>) -> Result<()> {
        self.inner.set_model(model).await
    }
    async fn set_thinking_option(&self, option: Option<&str>) -> Result<Option<Value>> {
        self.inner.set_thinking_option(option).await
    }
    async fn set_feature(&self, feature: &str, value: Value) -> Result<()> {
        self.inner.set_feature(feature, value).await
    }
    async fn rewind(&self, target: RewindTarget, message_id: &str) -> Result<()> {
        self.inner.rewind(target, message_id).await
    }
    async fn try_out_of_band(&self, prompt: &Value) -> Result<bool> {
        self.inner.try_out_of_band(prompt).await
    }
}
