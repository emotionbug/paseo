use crate::transport::{RequestContext, has_capability};
use serde_json::Value;

#[derive(Clone, Copy)]
pub(super) struct Compatibility {
    pub all_providers: bool,
    pub custom_icons: bool,
    pub compact: bool,
    pub references: bool,
}

impl Compatibility {
    pub fn for_context(context: &RequestContext) -> Self {
        let capabilities = context.capabilities();
        Self {
            all_providers: has_capability(capabilities, "all_providers")
                || supports_all_providers(context.app_version()),
            custom_icons: has_capability(capabilities, "custom_mode_icons"),
            compact: has_capability(capabilities, "compact_provider_snapshots"),
            references: has_capability(capabilities, "provider_snapshot_references"),
        }
    }

    pub fn visible(self, provider: &str) -> bool {
        self.all_providers || matches!(provider, "claude" | "codex" | "opencode")
    }

    pub fn modes(self, modes: &mut Value) {
        if self.custom_icons {
            return;
        }
        if let Some(modes) = modes.as_array_mut() {
            for mode in modes {
                if mode["icon"].as_str().is_some_and(|icon| {
                    !icon.is_empty()
                        && !matches!(
                            icon,
                            "ShieldCheck" | "ShieldAlert" | "ShieldOff" | "ShieldQuestionMark"
                        )
                }) {
                    mode["icon"] = Value::String("ShieldCheck".into());
                }
            }
        }
    }
}

// Session.isAppVersionAtLeast uses Number (not semver), ignores prerelease suffixes,
// treats missing components as zero and lets NaN components compare equal.
fn supports_all_providers(version: Option<&str>) -> bool {
    let Some(version) = version.filter(|version| !version.is_empty()) else {
        return false;
    };
    let mut parts = version.split('-').next().unwrap_or("").split('.');
    for minimum in [0.0, 1.0, 45.0] {
        let actual = parts.next().map(js_number).unwrap_or(0.0);
        if actual > minimum {
            return true;
        }
        if actual < minimum {
            return false;
        }
    }
    true
}

fn js_number(value: &str) -> f64 {
    let value = value.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    if value.is_empty() {
        return 0.0;
    }
    for (prefixes, radix) in [(["0x", "0X"], 16), (["0b", "0B"], 2), (["0o", "0O"], 8)] {
        if let Some(digits) = prefixes
            .iter()
            .find_map(|prefix| value.strip_prefix(prefix))
        {
            if digits.is_empty() {
                return f64::NAN;
            }
            return digits
                .chars()
                .try_fold(0.0, |number, digit| {
                    digit
                        .to_digit(radix)
                        .map(|digit| number * f64::from(radix) + f64::from(digit))
                })
                .unwrap_or(f64::NAN);
        }
    }
    match value {
        "Infinity" | "+Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        _ if value
            .bytes()
            .any(|byte| byte.is_ascii_alphabetic() && !matches!(byte, b'e' | b'E')) =>
        {
            f64::NAN
        }
        _ => value.parse().unwrap_or(f64::NAN),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn version_gate_matches_session_number_comparison() {
        for version in [None, Some(""), Some("0.1.44"), Some("0.1"), Some("0.0.100")] {
            assert!(!supports_all_providers(version));
        }
        for version in [
            "0.1.45",
            "0.1.45-beta.4",
            "0.2",
            "1",
            "0.1.0x2d",
            "0.1.invalid",
        ] {
            assert!(supports_all_providers(Some(version)));
        }
    }
}
