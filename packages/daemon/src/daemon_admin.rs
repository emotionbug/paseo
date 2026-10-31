mod diagnostics;
#[cfg(test)]
mod tests;
pub(crate) mod updater;

use crate::{state::AppState, transport::RequestContext};
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::{sync::Weak, time::Instant};
use tokio::sync::Mutex;

pub struct DaemonAdminService {
    app: Weak<AppState>,
    update: Mutex<()>,
    started: Instant,
}
impl DaemonAdminService {
    pub fn new(app: Weak<AppState>) -> Self {
        Self {
            app,
            update: Mutex::new(()),
            started: Instant::now(),
        }
    }
    pub async fn handle(&self, context: &RequestContext, message: &Value) -> Result<bool> {
        let kind = message["type"].as_str().unwrap_or("");
        if !matches!(
            kind,
            "diagnostics.request"
                | "daemon.update.request"
                | "restart_server_request"
                | "shutdown_server_request"
        ) {
            return Ok(false);
        }
        let app = self
            .app
            .upgrade()
            .ok_or_else(|| anyhow!("Daemon is shutting down"))?;
        let request = crate::transport::request_id(message).context("Missing requestId")?;
        match kind{
            "diagnostics.request"=>{
                match diagnostics::collect(&app,self.started).await{
                    Ok(diagnostic)=>context.send(json!({"type":"diagnostics.response","payload":{"requestId":request,"diagnostic":diagnostic}})).await?,
                    Err(error)=>context.send(json!({"type":"rpc_error","payload":{"requestId":request,"requestType":kind,"error":error.to_string()}})).await?,
                }
            }
            "restart_server_request"|"shutdown_server_request"=>{
                let restart=kind=="restart_server_request";
                let mut payload=json!({"status":if restart{"restart_requested"}else{"shutdown_requested"},"clientId":context.client_id(),"requestId":request});
                if restart{if let Some(reason)=message["reason"].as_str().filter(|s|!s.trim().is_empty()){payload["reason"]=json!(reason)}}
                context.send_flushed(json!({"type":"status","payload":payload})).await?;
                app.request_shutdown(restart);
            }
            "daemon.update.request"=>{
                let Ok(_guard)=self.update.try_lock()else{context.send(json!({"type":"rpc_error","payload":{"requestId":request,"requestType":kind,"code":"already_updating","error":"An update is already in progress"}})).await?;return Ok(true)};
                let result=async{
                    if app.config.snapshot()["desktopManaged"]==true||std::env::var("PASEO_DESKTOP_MANAGED").ok().as_deref()==Some("1"){anyhow::bail!("This daemon is managed by Paseo Desktop. Update Paseo Desktop on the host.")}
                    progress(context,request,"starting").await?;
                    progress(context,request,"downloading").await?;
                    let prepared=updater::prepare(&app.executable,false).await?;
                    progress(context,request,"installing").await?;
                    updater::install(&app.home,prepared).await
                }.await;
                match result{
                    Ok(version)=>{
                        // A disconnected request cannot strand an already committed update:
                        // flush a final reply when possible, but always drain this old worker.
                        let reply=async{progress(context,request,"complete").await?;context.send_flushed(json!({"type":"daemon.update.response","payload":{"requestId":request,"success":true,"error":Value::Null,"previousVersion":env!("CARGO_PKG_VERSION"),"newVersion":version}})).await}.await;
                        app.request_shutdown(true);
                        reply?;
                    }
                    Err(error)=>context.send(json!({"type":"daemon.update.response","payload":{"requestId":request,"success":false,"error":error.to_string(),"previousVersion":env!("CARGO_PKG_VERSION"),"newVersion":Value::Null}})).await?,
                }
            }
            _=>unreachable!(),
        }
        Ok(true)
    }
}
async fn progress(context: &RequestContext, request: &str, phase: &str) -> Result<()> {
    context
        .send(
            json!({"type":"daemon.update.progress","payload":{"requestId":request,"phase":phase}}),
        )
        .await
}
