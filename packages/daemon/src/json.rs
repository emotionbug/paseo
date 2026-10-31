//! JavaScript JSON/property ordering for durable identities shared with the TS daemon.
//! JSON `Value` strings are valid Unicode: they preserve supplementary characters,
//! but cannot represent JavaScript's isolated UTF-16 surrogate code units.
use anyhow::{Context, Result};
use icu_casemap::CaseMapper;
use icu_collator::{CaseLevel, Collator, CollatorOptions, Numeric, Strength};
use icu_locid::Locale;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::sync::LazyLock;

static DEFAULT_LOCALE: LazyLock<Locale> = LazyLock::new(default_locale);
thread_local! {
    // ICU4X 1.x's owned provider payloads are not Send/Sync. A thread-local
    // collator avoids rebuilding it for each key comparison without unsafe sharing.
    static COLLATOR: Collator = Collator::try_new(
        &(*DEFAULT_LOCALE).clone().into(),
        CollatorOptions::new(),
    ).expect("compiled ICU collation data must contain the default locale fallback");
    static NUMERIC_BASE_COLLATOR: Collator = {
        let mut options = CollatorOptions::new();
        options.numeric = Some(Numeric::On);
        options.strength = Some(Strength::Primary);
        options.case_level = Some(CaseLevel::Off);
        Collator::try_new(&(*DEFAULT_LOCALE).clone().into(), options)
            .expect("compiled ICU numeric collation data must contain the default locale fallback")
    };
    static CASE_MAPPER: CaseMapper = const { CaseMapper::new() };
}

/// Same default-locale comparison as `String.prototype.localeCompare` without options.
/// Collation equality deliberately has no ordinal tie-breaker: JS Array.sort is stable.
pub fn locale_compare(left: &str, right: &str) -> Ordering {
    COLLATOR.with(|collator| collator.compare(left, right))
}

/// `localeCompare(other, undefined, { numeric: true, sensitivity: "base" })`.
pub fn numeric_base_locale_compare(left: &str, right: &str) -> Ordering {
    NUMERIC_BASE_COLLATOR.with(|collator| collator.compare(left, right))
}

/// `String.prototype.toLocaleLowerCase()` using the same process-default locale.
pub fn locale_lowercase(value: &str) -> String {
    CASE_MAPPER.with(|mapper| mapper.lowercase_to_string(value, &DEFAULT_LOCALE.id))
}

/// JSON.stringify over JSON values: array-index properties precede other properties,
/// insertion order is retained otherwise, and every number is an ECMAScript Number.
pub fn stringify(value: &Value) -> Result<String> {
    let mut output = String::new();
    write_value(value, &mut output, None)?;
    Ok(output)
}

/// The receipt replacer recursively sorts object entries with localeCompare, then
/// Object.fromEntries/JSON.stringify reorder array-index keys numerically once more.
pub fn canonical_stringify(value: &Value) -> Result<String> {
    COLLATOR.with(|collator| {
        let mut output = String::new();
        write_value(value, &mut output, Some(collator))?;
        Ok(output)
    })
}

pub fn fingerprint(value: &Value) -> Result<String> {
    Ok(hex::encode(Sha256::digest(
        canonical_stringify(value)?.as_bytes(),
    )))
}

fn write_value(value: &Value, output: &mut String, collator: Option<&Collator>) -> Result<()> {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Number(value) => {
            let number = value
                .as_f64()
                .context("JSON number cannot be represented as an ECMAScript Number")?;
            if !number.is_finite() {
                output.push_str("null");
            } else if number == 0.0 {
                output.push('0');
            } else {
                output.push_str(ryu_js::Buffer::new().format_finite(number));
            }
        }
        Value::String(value) => quote(value, output),
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                write_value(value, output, collator)?;
            }
            output.push(']');
        }
        Value::Object(values) => {
            let mut entries: Vec<_> = values.iter().collect();
            // Object.entries already enumerates JS index properties first.
            entries.sort_by(|(left, _), (right, _)| property_order(left, right));
            if let Some(collator) = collator {
                entries.sort_by(|(left, _), (right, _)| collator.compare(left, right));
                entries.sort_by(|(left, _), (right, _)| property_order(left, right));
            }
            output.push('{');
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                quote(key, output);
                output.push(':');
                write_value(value, output, collator)?;
            }
            output.push('}');
        }
    }
    Ok(())
}
fn array_index(key: &str) -> Option<u32> {
    if key.is_empty()
        || (key.len() > 1 && key.starts_with('0'))
        || !key.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    key.parse::<u32>().ok().filter(|index| *index != u32::MAX)
}
fn property_order(left: &str, right: &str) -> Ordering {
    match (array_index(left), array_index(right)) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}
fn quote(value: &str, output: &mut String) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    output.push('"');
    let mut start = 0;
    for (index, byte) in value.bytes().enumerate() {
        let escape = match byte {
            b'"' => "\\\"",
            b'\\' => "\\\\",
            b'\x08' => "\\b",
            b'\x0c' => "\\f",
            b'\n' => "\\n",
            b'\r' => "\\r",
            b'\t' => "\\t",
            0..=0x1f => "",
            _ => continue,
        };
        output.push_str(&value[start..index]);
        if escape.is_empty() {
            output.push_str("\\u00");
            output.push(HEX[(byte >> 4) as usize] as char);
            output.push(HEX[(byte & 15) as usize] as char);
        } else {
            output.push_str(escape);
        }
        start = index + 1;
    }
    output.push_str(&value[start..]);
    output.push('"');
}

fn default_locale() -> Locale {
    normalize_locale(&platform_locale()).unwrap_or_else(|| "und".parse().expect("valid und locale"))
}
fn normalize_locale(raw: &str) -> Option<Locale> {
    let (base, modifier) = raw
        .split_once('@')
        .map_or((raw, None), |(base, modifier)| (base, Some(modifier)));
    let base = base.split('.').next().unwrap_or(base);
    // V8 maps ICU's POSIX default to ordinary en-US, not POSIX collation.
    if matches!(base, "" | "C" | "POSIX" | "en_US_POSIX" | "en-US-posix") {
        return "en-US".parse().ok();
    }
    let mut tag = base.replace('_', "-");
    if let Some(modifier) = modifier {
        match modifier {
            "latin" | "cyrillic" => {
                let script = if modifier == "latin" { "Latn" } else { "Cyrl" };
                let boundary = tag.find('-').unwrap_or(tag.len());
                tag.insert_str(boundary, &format!("-{script}"));
            }
            "" | "euro" => (),
            other => {
                // ICU POSIX modifiers without keywords become locale variants.
                // Collation keywords become the equivalent Unicode extension.
                if let Some(collation) = other.strip_prefix("collation=") {
                    let collation = match collation {
                        "phonebook" => "phonebk",
                        "traditional" => "trad",
                        "dictionary" => "dict",
                        value => value,
                    };
                    tag.push_str("-u-co-");
                    tag.push_str(collation);
                } else if !other.contains('=') {
                    tag.push('-');
                    tag.push_str(other);
                }
            }
        }
    }
    tag.parse().ok()
}
#[cfg(unix)]
fn platform_locale() -> String {
    // Query only: do not mutate the process locale. ICU's POSIX path queries
    // LC_MESSAGES first and consults these environment variables for C/POSIX.
    let locale = unsafe {
        let pointer = libc::setlocale(libc::LC_MESSAGES, std::ptr::null());
        if pointer.is_null() {
            None
        } else {
            Some(
                std::ffi::CStr::from_ptr(pointer)
                    .to_string_lossy()
                    .into_owned(),
            )
        }
    };
    if let Some(locale) = locale.filter(|locale| !matches!(locale.as_str(), "" | "C" | "POSIX")) {
        return locale;
    }
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(value) = std::env::var_os(key) {
            return value.to_string_lossy().into_owned();
        }
    }
    "en-US".to_owned()
}
#[cfg(windows)]
fn platform_locale() -> String {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetUserDefaultLocaleName(locale_name: *mut u16, length: i32) -> i32;
    }
    let mut locale = [0_u16; 85];
    let length = unsafe { GetUserDefaultLocaleName(locale.as_mut_ptr(), locale.len() as i32) };
    if length > 1 {
        String::from_utf16_lossy(&locale[..length as usize - 1])
    } else {
        "en-US".to_owned()
    }
}
#[cfg(not(any(unix, windows)))]
fn platform_locale() -> String {
    "en-US".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn canonical_en(value: &Value) -> Result<String> {
        let locale: Locale = "en-US".parse().unwrap();
        let collator = Collator::try_new(&locale.into(), CollatorOptions::new()).unwrap();
        let mut output = String::new();
        write_value(value, &mut output, Some(&collator))?;
        Ok(output)
    }
    #[test]
    fn numeric_properties_are_indices_only_with_canonical_uint32_spelling() -> Result<()> {
        let value: Value = serde_json::from_str(
            r#"{"z":0,"10":10,"2":2,"01":1,"4294967295":9,"4294967294":8,"-0":0}"#,
        )?;
        assert_eq!(
            stringify(&value)?,
            r#"{"2":2,"10":10,"4294967294":8,"z":0,"01":1,"4294967295":9,"-0":0}"#
        );
        assert_eq!(
            canonical_en(&value)?,
            r#"{"2":2,"10":10,"4294967294":8,"-0":0,"01":1,"4294967295":9,"z":0}"#
        );
        Ok(())
    }
    #[test]
    fn ecmascript_numbers_and_exponent_boundaries() -> Result<()> {
        let value = json!([-0.0, 1.0, 1e-7, 1e-6, 1e20, 1e21, 9007199254740993_u64]);
        assert_eq!(
            stringify(&value)?,
            "[0,1,1e-7,0.000001,100000000000000000000,1e+21,9007199254740992]"
        );
        Ok(())
    }
    #[test]
    fn unicode_case_accents_korean_and_supplementary_strings() -> Result<()> {
        let value: Value =
            serde_json::from_str("{\"나\":1,\"가\":2,\"é\":3,\"E\":4,\"e\":5,\"😀\":\"𐐀\"}")?;
        assert_eq!(
            canonical_en(&value)?,
            "{\"😀\":\"𐐀\",\"e\":5,\"E\":4,\"é\":3,\"가\":2,\"나\":1}"
        );
        assert_eq!(
            stringify(&json!("\u{2028}\u{2029}😀\0\n\\\""))?,
            "\"\u{2028}\u{2029}😀\\u0000\\n\\\\\\\"\""
        );
        Ok(())
    }
    #[test]
    fn canonically_equal_keys_keep_stable_input_order() -> Result<()> {
        let value: Value = serde_json::from_str("{\"é\":1,\"é\":2}")?;
        assert_eq!(canonical_en(&value)?, "{\"é\":1,\"é\":2}");
        Ok(())
    }
    #[test]
    fn nested_objects_canonicalize_without_sorting_arrays() -> Result<()> {
        assert_eq!(
            canonical_en(&json!({"z":[{"b":1,"a":2},null],"a":true}))?,
            r#"{"a":true,"z":[{"a":2,"b":1},null]}"#
        );
        Ok(())
    }
    #[test]
    fn posix_locale_normalization() {
        assert_eq!(
            normalize_locale("ko_KR.UTF-8").unwrap().to_string(),
            "ko-KR"
        );
        assert_eq!(normalize_locale("C.UTF-8").unwrap().to_string(), "en-US");
        assert_eq!(
            normalize_locale("sr_RS.UTF-8@latin").unwrap().to_string(),
            "sr-Latn-RS"
        );
        assert_eq!(
            normalize_locale("de_DE@collation=phonebook")
                .unwrap()
                .to_string(),
            "de-DE-u-co-phonebk"
        );
    }
    #[test]
    fn locale_casing_handles_turkic_lithuanian_and_final_sigma() {
        let mapper = CaseMapper::new();
        for locale in ["tr", "az"] {
            assert_eq!(
                mapper.lowercase_to_string("Iİ I\u{307}", &locale.parse().unwrap()),
                "ıi i"
            );
        }
        assert_eq!(
            mapper.lowercase_to_string("I\u{301}", &"lt".parse().unwrap()),
            "i\u{307}\u{301}"
        );
        assert_eq!(
            mapper.lowercase_to_string("ΟΣ ΟΣΑ", &"el".parse().unwrap()),
            "ος οσα"
        );
    }
    #[test]
    fn numeric_base_collation_ignores_case_and_accents() {
        let mut options = CollatorOptions::new();
        options.numeric = Some(Numeric::On);
        options.strength = Some(Strength::Primary);
        options.case_level = Some(CaseLevel::Off);
        let collator =
            Collator::try_new(&"en-US".parse::<Locale>().unwrap().into(), options).unwrap();
        assert_eq!(collator.compare("task2", "task10"), Ordering::Less);
        assert_eq!(collator.compare("É2", "e2"), Ordering::Equal);
    }
    #[test]
    fn canonical_fingerprint_hashes_the_serialized_utf8_bytes() -> Result<()> {
        assert_eq!(
            fingerprint(&json!({}))?,
            "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
        let first: Value = serde_json::from_str(r#"{"z":1,"a":{"b":2,"a":3}}"#)?;
        let second: Value = serde_json::from_str(r#"{"a":{"a":3,"b":2},"z":1}"#)?;
        assert_eq!(fingerprint(&first)?, fingerprint(&second)?);
        Ok(())
    }
}
