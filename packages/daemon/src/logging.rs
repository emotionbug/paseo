use anyhow::Result;
use serde_json::{Map, Value, json};
use std::{fmt, io::IsTerminal, sync::LazyLock};
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{
    fmt::{FmtContext, FormatEvent, FormatFields, format::Writer},
    registry::LookupSpan,
};

static HOSTNAME: LazyLock<std::io::Result<String>> =
    LazyLock::new(|| hostname::get().map(|name| name.to_string_lossy().into_owned()));
struct Fields(Map<String, Value>);
impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().into(), json!(value));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().into(), json!(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().into(), json!(value));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().into(), json!(value));
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.0.insert(field.name().into(), json!(value));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let text = format!("{value:?}");
        let value = if matches!(field.name(), "headers" | "req") {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        } else {
            Value::String(text)
        };
        self.0.insert(field.name().into(), value);
    }
}
fn remove_credentials(value: &mut Value) {
    if let Some(object) = value.as_object_mut() {
        for key in [
            "authorization",
            "Authorization",
            "sec-websocket-protocol",
            "Sec-WebSocket-Protocol",
        ] {
            object.remove(key);
        }
    }
}
struct LogFormat {
    pretty: bool,
    color: bool,
}
impl<S, N> FormatEvent<S, N> for LogFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _context: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let metadata = event.metadata();
        let mut fields = Fields(Map::new());
        event.record(&mut fields);
        let message = fields.0.remove("message").unwrap_or(Value::Null);
        let (priority, label, color) = match *metadata.level() {
            tracing::Level::TRACE => (10, "TRACE", "\x1b[90m"),
            tracing::Level::DEBUG => (20, "DEBUG", "\x1b[34m"),
            tracing::Level::INFO => (30, "INFO", "\x1b[32m"),
            tracing::Level::WARN => (40, "WARN", "\x1b[33m"),
            tracing::Level::ERROR => (50, "ERROR", "\x1b[31m"),
        };
        let mut record = Value::Object(fields.0);
        remove_credentials(&mut record);
        if let Some(headers) = record.get_mut("headers") {
            remove_credentials(headers);
        }
        if let Some(headers) = record.get_mut("req").and_then(|req| req.get_mut("headers")) {
            remove_credentials(headers);
        }
        if self.pretty {
            write!(writer, "[{}] ", chrono::Local::now().format("%H:%M:%S%.3f"))?;
            if self.color {
                write!(writer, "{color}{label}\x1b[0m")?;
            } else {
                write!(writer, "{label}")?;
            }
            write!(
                writer,
                " ({}): {}",
                metadata.target(),
                message.as_str().unwrap_or("")
            )?;
            if record.as_object().is_some_and(|object| !object.is_empty()) {
                write!(writer, " {record}")?;
            }
            writeln!(writer)
        } else {
            record["level"] = json!(priority);
            record["time"] = json!(chrono::Utc::now().timestamp_millis());
            record["pid"] = json!(std::process::id());
            record["hostname"] = json!(HOSTNAME.as_ref().expect("Logger hostname initialized"));
            record["daemonVersion"] = json!(env!("CARGO_PKG_VERSION"));
            record["name"] = json!(metadata.target());
            record["msg"] = message;
            writeln!(writer, "{record}")
        }
    }
}

/// Worker output is captured and rotated by its native supervisor. The source
/// worker also disables the independent logger file sink; do not write it twice.
pub fn initialize(log: &Value) -> Result<()> {
    let level = log["console"]["level"]
        .as_str()
        .or_else(|| log["level"].as_str())
        .unwrap_or("info");
    let level = match level {
        "trace" => tracing::level_filters::LevelFilter::TRACE,
        "debug" => tracing::level_filters::LevelFilter::DEBUG,
        "info" => tracing::level_filters::LevelFilter::INFO,
        "warn" => tracing::level_filters::LevelFilter::WARN,
        "error" => tracing::level_filters::LevelFilter::ERROR,
        "fatal" => tracing::level_filters::LevelFilter::OFF,
        _ => anyhow::bail!("Invalid daemon log level: {level}"),
    };
    let format = log["console"]["format"]
        .as_str()
        .or_else(|| log["format"].as_str())
        .unwrap_or("json");
    if !matches!(format, "json" | "pretty") {
        anyhow::bail!("Invalid daemon log format: {format}");
    }
    HOSTNAME
        .as_ref()
        .map_err(|error| anyhow::anyhow!("Cannot resolve log hostname: {error}"))?;
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .event_format(LogFormat {
            pretty: format == "pretty",
            color: std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        })
        .try_init()
        .map_err(|error| anyhow::anyhow!("Cannot initialize daemon logging: {error}"))
}
