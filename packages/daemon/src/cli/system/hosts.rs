use super::{error, pretty};
use crate::cli::{
    Context, flag, multiple, option, output::Output, positional, string, strings, switch,
};
use anyhow::{Context as _, Result, bail};
use clap::{ArgMatches, Command};
use serde_json::{Map, Value, json};
use std::time::Duration;

pub fn command() -> Command {
    let mut host = Command::new("host")
        .subcommand_required(true)
        .about("Manage hosts connected to the selected daemon")
        .subcommand(Command::new("ls"))
        .subcommand(
            Command::new("add")
                .arg(positional("connection", true))
                .arg(option("label", "label"))
                .arg(multiple("meta", "meta")),
        )
        .subcommand(
            Command::new("update")
                .arg(positional("registration-id", true))
                .arg(option("label", "label"))
                .arg(option("connection", "connection"))
                .arg(switch("replace-identity", "replace-identity"))
                .arg(multiple("meta", "meta"))
                .arg(multiple("remove-meta", "remove-meta")),
        );
    for action in ["inspect", "enable", "disable", "rm"] {
        host = host.subcommand(Command::new(action).arg(positional("registration-id", true)));
    }
    host
}
fn metadata(values: Vec<String>) -> Result<Map<String, Value>> {
    values
        .into_iter()
        .map(|value| {
            let (key, item) = value
                .split_once('=')
                .filter(|(key, _)| !key.is_empty())
                .ok_or_else(|| error("invalid_metadata", "Metadata must use key=value"))?;
            Ok((key.to_owned(), json!(item)))
        })
        .collect()
}
pub async fn execute(path: &[&str], m: &ArgMatches, ctx: &Context) -> Result<Output> {
    let action = *path.first().context("Host command required")?;
    let client = ctx.connect().await?;
    let result=async {
        client.require_feature("hostMultiplexing","Update the daemon to manage connected hosts.")?;
        let data=if action=="add" {
            let mut input=json!({"connection":string(m,"connection").context("Connection required")?,"metadata":metadata(strings(m,"meta"))?});
            if let Some(label)=string(m,"label"){input["label"]=json!(label);}
            let mut response=client.request(json!({"type":"hosts.add.request","input":input}),"hosts.add.response",Duration::from_secs(30)).await?;
            response.get_mut("host").filter(|host|host.is_object()).context("Invalid host response")?.take()
        } else {
            let mut list=client.request(json!({"type":"hosts.list.request"}),"hosts.list.response",Duration::from_secs(30)).await?;
            let mut data=list.pointer_mut("/directory/hosts").filter(|hosts|hosts.is_array()).context("Invalid host directory response")?.take();
            if action=="ls" {data} else {
                let id=string(m,"registration-id").context("Registration ID required")?;
                let hosts=data.as_array_mut().unwrap();
                let index=hosts.iter().position(|host|host["registrationId"]==id).ok_or_else(||error("host_unknown",format!("Unknown registration {id}")))?;
                let mut host=hosts.swap_remove(index);
                match action {
                    "inspect"=>return Ok(pretty(host)?.id("registrationId")),
                    "rm"=>{client.request(json!({"type":"hosts.remove.request","registrationId":id}),"hosts.remove.response",Duration::from_secs(30)).await?;host},
                    "update"|"enable"|"disable"=>{
                        let mut patch=json!({});
                        if action!="update" {patch["enabled"]=json!(action=="enable");}
                        if let Some(label)=string(m,"label"){patch["label"]=if label.is_empty(){Value::Null}else{json!(label)};}
                        if let Some(connection)=string(m,"connection"){patch["connection"]=json!(connection);}
                        if flag(m,"replace-identity"){patch["replaceIdentity"]=json!(true);}
                        let meta=strings(m,"meta");let remove=strings(m,"remove-meta");
                        if !meta.is_empty() || !remove.is_empty() {
                            let Value::Object(mut merged)=host.get_mut("metadata").context("Invalid host metadata")?.take() else{bail!("Invalid host metadata")};merged.extend(metadata(meta)?);
                            for key in remove {merged.remove(&key);}patch["metadata"]=Value::Object(merged);
                        }
                        let mut response=client.request(json!({"type":"hosts.update.request","registrationId":id,"patch":patch}),"hosts.update.response",Duration::from_secs(30)).await?;
                        response.get_mut("host").filter(|host|host.is_object()).context("Invalid host response")?.take()
                    }
                    _=>bail!("Unknown host command"),
                }
            }
        };
        let table=if ctx.output.format==crate::cli::output::Format::Table&&!ctx.output.quiet {
            let row=|host:&Value|->Result<Value>{
                let source=host["source"]["id"].as_str().zip(host["source"]["key"].as_str()).context("Invalid host source")?;
                Ok(json!({"registrationId":host["registrationId"],"label":host["label"],"serverId":host["serverId"],"enabled":host["enabled"],"status":host["status"],"source":format!("{}/{}",source.0,source.1)}))
            };
            Some(if let Some(hosts)=data.as_array(){Value::Array(hosts.iter().map(row).collect::<Result<Vec<_>>>()?)}else{row(&data)?})
        }else{None};
        let mut output=Output::new(data).id("registrationId").columns(&[("ID","registrationId"),("LABEL","label"),("SERVER","serverId"),("ENABLED","enabled"),("STATUS","status"),("SOURCE","source")]);
        if let Some(table)=table{output=output.table_data(table);}Ok(output)
    }.await;
    client.close().await;
    result
}
