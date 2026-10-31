use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::io::{self, IsTerminal, Write};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Table,
    Json,
    Yaml,
}
#[derive(Clone)]
pub struct OutputOptions {
    pub format: Format,
    pub quiet: bool,
    pub headers: bool,
    pub color: bool,
}
impl Default for OutputOptions {
    fn default() -> Self {
        Self {
            format: Format::Table,
            quiet: false,
            headers: true,
            color: true,
        }
    }
}
pub struct Output {
    pub data: Value,
    pub table: Option<Value>,
    pub human: Option<String>,
    pub quiet: Option<String>,
    pub columns: Vec<(&'static str, &'static str)>,
    pub min_widths: Vec<usize>,
    pub id_field: &'static str,
    pub exit_code: i32,
    pub silent: bool,
    pub color: Option<(&'static str, fn(&Value, &Value) -> Option<u8>)>,
}
impl Output {
    pub fn new(data: Value) -> Self {
        Self {
            data,
            table: None,
            human: None,
            quiet: None,
            columns: Vec::new(),
            min_widths: Vec::new(),
            id_field: "id",
            exit_code: 0,
            silent: false,
            color: None,
        }
    }
    pub fn human(mut self, text: impl Into<String>) -> Self {
        self.human = Some(text.into());
        self
    }
    pub fn quiet_text(mut self, text: impl Into<String>) -> Self {
        self.quiet = Some(text.into());
        self
    }
    pub fn table_data(mut self, data: Value) -> Self {
        self.table = Some(data);
        self
    }
    pub fn columns(mut self, columns: &[(&'static str, &'static str)]) -> Self {
        self.columns = columns.to_vec();
        self
    }
    pub fn widths(mut self, widths: &[usize]) -> Self {
        self.min_widths = widths.to_vec();
        self
    }
    pub fn id(mut self, field: &'static str) -> Self {
        self.id_field = field;
        self
    }
    pub fn exit(mut self, code: i32) -> Self {
        self.exit_code = code;
        self
    }
    pub fn silent(mut self) -> Self {
        self.silent = true;
        self
    }
    pub fn color_column(
        mut self,
        key: &'static str,
        color: fn(&Value, &Value) -> Option<u8>,
    ) -> Self {
        self.color = Some((key, color));
        self
    }
    pub fn render(&self, options: &OutputOptions) -> Result<String> {
        if self.silent {
            return Ok(String::new());
        }
        if options.quiet {
            if let Some(text) = &self.quiet {
                return Ok(text.clone());
            }
        }
        if options.quiet {
            return Ok(match &self.data {
                Value::Array(rows) => rows
                    .iter()
                    .filter_map(|r| r.get(self.id_field))
                    .map(quiet_id)
                    .collect::<Vec<_>>()
                    .join("\n"),
                row => row.get(self.id_field).map(quiet_id).unwrap_or_default(),
            });
        }
        match options.format {
            Format::Json => Ok(serde_json::to_string_pretty(&self.data)?),
            Format::Yaml => Ok(serde_yaml::to_string(&self.data)?.trim_end().into()),
            Format::Table => {
                if let Some(human) = &self.human {
                    return Ok(human.clone());
                }
                if self.columns.is_empty() {
                    return Ok(String::new());
                }
                let data = self.table.as_ref().unwrap_or(&self.data);
                let rows: Vec<&Value> = if let Some(rows) = data.as_array() {
                    rows.iter().collect()
                } else {
                    vec![data]
                };
                if rows.is_empty() {
                    return Ok(String::new());
                }
                let color_enabled = options.color && io::stdout().is_terminal();
                let mut values: Vec<Vec<String>> = rows
                    .iter()
                    .map(|row| {
                        self.columns
                            .iter()
                            .map(|(_, key)| {
                                let value = field(row, key);
                                let text = cell(value);
                                if color_enabled {
                                    if let Some((column, color)) = self.color {
                                        if column == *key {
                                            if let Some(code) = color(value, row) {
                                                return format!("\u{1b}[{code}m{text}\u{1b}[39m");
                                            }
                                        }
                                    }
                                }
                                text
                            })
                            .collect()
                    })
                    .collect();
                if options.headers {
                    values.insert(
                        0,
                        self.columns
                            .iter()
                            .map(|(header, _)| (*header).into())
                            .collect(),
                    );
                }
                let widths: Vec<usize> = (0..self.columns.len())
                    .map(|column| {
                        values
                            .iter()
                            .map(|r| visible_length(&r[column]))
                            .max()
                            .unwrap_or(0)
                            .max(self.min_widths.get(column).copied().unwrap_or(0))
                    })
                    .collect();
                Ok(values
                    .into_iter()
                    .enumerate()
                    .map(|(row_index, row)| {
                        let text = row
                            .into_iter()
                            .enumerate()
                            .map(|(index, value)| {
                                let padding = widths[index].saturating_sub(visible_length(&value));
                                format!("{value}{}", " ".repeat(padding))
                            })
                            .collect::<Vec<_>>()
                            .join("  ");
                        if row_index == 0
                            && options.headers
                            && options.color
                            && io::stdout().is_terminal()
                        {
                            format!("\u{1b}[1m{text}\u{1b}[22m")
                        } else {
                            text
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
        }
    }
}
fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    key.split('.')
        .fold(value, |value, key| value.get(key).unwrap_or(&Value::Null))
}
fn cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(v) => v.to_string(),
        Value::Array(values) => values.iter().map(cell).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
        _ => value.to_string(),
    }
}
fn visible_length(text: &str) -> usize {
    static ANSI: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"[\x1b\x{009b}][\[\]()#;?]*(?:[0-9]{1,4}(?:;[0-9]{0,4})*)?[0-9A-ORZcf-nqry=><]",
        )
        .expect("static ANSI expression")
    });
    ANSI.replace_all(text, "").encode_utf16().count()
}
fn quiet_id(value: &Value) -> String {
    match value {
        Value::Array(values) => values.iter().map(cell).collect::<Vec<_>>().join("\n"),
        _ => cell(value),
    }
}
pub fn stdout(text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    let result = writeln!(io::stdout().lock(), "{text}");
    match result {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => other.map_err(Into::into),
    }
}
pub fn stdout_chunk(text: &str) -> Result<()> {
    let mut out = io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|_| out.flush()) {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        result => result.map_err(Into::into),
    }
}
pub fn error(error: &anyhow::Error, options: &OutputOptions) -> Result<()> {
    let code = if let Some(error) = error.downcast_ref::<crate::client::ClientError>() {
        error.code.as_str()
    } else if let Some(error) = error.downcast_ref::<crate::daemon_control::DaemonInstanceError>() {
        error.code.as_str()
    } else {
        "UNKNOWN_ERROR"
    };
    let details = error
        .downcast_ref::<crate::client::ClientError>()
        .and_then(|error| error.details.as_ref());
    let mut value = json!({"error":{"code":code,"message":error.to_string()}});
    if let Some(details) = details {
        value["error"]["details"] = details.clone();
    }
    let text = match options.format {
        Format::Json => serde_json::to_string_pretty(&value)?,
        Format::Yaml => serde_yaml::to_string(&value)?,
        Format::Table => {
            let prefix = if options.color && io::stderr().is_terminal() {
                "\u{1b}[31mError: \u{1b}[39m"
            } else {
                "Error: "
            };
            let mut text = format!("{prefix}{error}");
            if let Some(details) = details
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                text.push('\n');
                text.push_str(details);
            }
            text
        }
    };
    writeln!(io::stderr().lock(), "{}", text.trim_end()).map_err(|e| anyhow!(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn structured_output_preserves_null_and_arrays() {
        let out = Output::new(json!([{"id":"first","value":null}])).columns(&[("ID", "id")]);
        let options = OutputOptions {
            format: Format::Json,
            ..Default::default()
        };
        assert_eq!(
            serde_json::from_str::<Value>(&out.render(&options).unwrap()).unwrap(),
            out.data
        );
    }
    #[test]
    fn quiet_precedes_json() {
        let out = Output::new(json!([{"id":"first"},{"id":"second"}]));
        let options = OutputOptions {
            format: Format::Json,
            quiet: true,
            ..Default::default()
        };
        assert_eq!(out.render(&options).unwrap(), "first\nsecond");
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    #[test]
    fn human_projection_never_rewrites_machine_or_quiet_ids() {
        let output = Output::new(json!({"id":"full-identity","status":null}))
            .table_data(json!({"id":"full","status":"-"}))
            .columns(&[("ID", "id"), ("STATUS", "status")]);
        let table = OutputOptions {
            headers: false,
            color: false,
            ..Default::default()
        };
        assert_eq!(output.render(&table).unwrap(), "full  -");
        assert_eq!(
            serde_json::from_str::<Value>(
                &output
                    .render(&OutputOptions {
                        format: Format::Json,
                        ..table.clone()
                    })
                    .unwrap()
            )
            .unwrap()["id"],
            "full-identity"
        );
        assert_eq!(
            output
                .render(&OutputOptions {
                    quiet: true,
                    ..table
                })
                .unwrap(),
            "full-identity"
        );
    }
    #[test]
    fn streaming_completion_does_not_append_null() {
        for format in [Format::Table, Format::Json, Format::Yaml] {
            assert_eq!(
                Output::new(Value::Null)
                    .silent()
                    .render(&OutputOptions {
                        format,
                        ..Default::default()
                    })
                    .unwrap(),
                ""
            );
        }
    }
}
