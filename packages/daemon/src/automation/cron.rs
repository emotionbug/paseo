use anyhow::{Result, anyhow, bail, ensure};
use chrono::{DateTime, Datelike, Duration, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Cadence {
    #[serde(rename = "every")]
    Every {
        #[serde(rename = "everyMs")]
        every_ms: i64,
    },
    #[serde(rename = "cron")]
    Cron {
        expression: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        timezone: Option<String>,
    },
}

fn field(source: &str, min: u32, max: u32, name: &str) -> Result<u64> {
    let mut bits = 0u64;
    for part in source.split(',') {
        ensure!(!part.is_empty(), "Invalid cron {name} field");
        let mut step_parts = part.split('/');
        let base = step_parts.next().unwrap();
        let step = if let Some(s) = step_parts.next() {
            let n: f64 = s.parse().map_err(|_| anyhow!("Invalid cron {name} step"))?;
            ensure!(
                n.is_finite()
                    && n > 0.0
                    && n.fract() == 0.0
                    && s.bytes().all(|b| b.is_ascii_digit())
                    && ryu_js::Buffer::new().format_finite(n) == s
                    && step_parts.next().is_none(),
                "Invalid cron {name} step"
            );
            // Larger valid JS integers still select only the range's first value.
            n.min(u32::MAX as f64) as u32
        } else {
            1
        };
        let number = |s: &str| -> Result<u32> {
            ensure!(
                !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()),
                "Invalid cron {name} value"
            );
            s.parse().map_err(|_| anyhow!("Invalid cron {name} value"))
        };
        let (start, end) = if base == "*" {
            (min, max)
        } else if let Some((a, b)) = base.split_once('-') {
            let (a, b) = (number(a)?, number(b)?);
            ensure!(a <= b && a >= min && b <= max, "Invalid cron {name} range");
            (a, b)
        } else {
            let n = number(base)?;
            ensure!(n >= min && n <= max, "Invalid cron {name} value");
            (n, n)
        };
        for n in (start..=end).step_by(step as usize) {
            bits |= 1u64 << n;
        }
    }
    Ok(bits)
}

fn parse(expression: &str) -> Result<[u64; 5]> {
    let parts: Vec<_> = expression.split_whitespace().collect();
    ensure!(parts.len() == 5, "Cron expressions must have 5 fields");
    Ok([
        field(parts[0], 0, 59, "minute")?,
        field(parts[1], 0, 23, "hour")?,
        field(parts[2], 1, 31, "day-of-month")?,
        field(parts[3], 1, 12, "month")?,
        field(parts[4], 0, 6, "day-of-week")?,
    ])
}

fn zone(name: Option<&str>) -> Result<Tz> {
    let Some(name) = name else {
        return Ok(chrono_tz::UTC);
    };
    // Intl accepts IANA identifiers case-insensitively, including aliases.
    chrono_tz::TZ_VARIANTS
        .iter()
        .find(|tz| tz.name().eq_ignore_ascii_case(name))
        .copied()
        .ok_or_else(|| anyhow!("Invalid cron time zone: {name}"))
}

impl Cadence {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Every { every_ms } => {
                ensure!(*every_ms > 0, "everyMs must be a positive integer")
            }
            Self::Cron {
                expression,
                timezone,
            } => {
                parse(expression)?;
                zone(timezone.as_deref())?;
            }
        }
        Ok(())
    }
    pub fn next(&self, after: DateTime<Utc>) -> Result<DateTime<Utc>> {
        match self {
            Self::Every { every_ms } => {
                ensure!(*every_ms > 0, "everyMs must be a positive integer");
                after
                    .checked_add_signed(Duration::milliseconds(*every_ms))
                    .ok_or_else(|| anyhow!("Schedule timestamp overflow"))
            }
            Self::Cron {
                expression,
                timezone,
            } => {
                let fields = parse(expression)?;
                let tz = zone(timezone.as_deref())?;
                let timestamp = after
                    .timestamp()
                    .div_euclid(60)
                    .checked_add(1)
                    .and_then(|n| n.checked_mul(60))
                    .ok_or_else(|| anyhow!("Schedule timestamp overflow"))?;
                let mut cursor = Utc
                    .timestamp_opt(timestamp, 0)
                    .single()
                    .ok_or_else(|| anyhow!("Schedule timestamp overflow"))?;
                for _ in 0..366 * 24 * 60 {
                    let local = cursor.with_timezone(&tz);
                    let values = [
                        local.minute(),
                        local.hour(),
                        local.day(),
                        local.month(),
                        local.weekday().num_days_from_sunday(),
                    ];
                    if fields
                        .iter()
                        .zip(values)
                        .all(|(bits, v)| bits & (1u64 << v) != 0)
                    {
                        return Ok(cursor);
                    }
                    cursor = cursor
                        .checked_add_signed(Duration::minutes(1))
                        .ok_or_else(|| anyhow!("Schedule timestamp overflow"))?;
                }
                bail!("Unable to compute next run time for cron expression: {expression}")
            }
        }
    }
    pub fn advance_past(&self, cursor: DateTime<Utc>, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
        if cursor > now {
            return Ok(cursor);
        }
        if let Self::Every { every_ms } = self {
            ensure!(*every_ms > 0, "everyMs must be a positive integer");
            let slots = now
                .signed_duration_since(cursor)
                .num_milliseconds()
                .div_euclid(*every_ms)
                + 1;
            let delta = slots
                .checked_mul(*every_ms)
                .ok_or_else(|| anyhow!("Schedule timestamp overflow"))?;
            return cursor
                .checked_add_signed(Duration::milliseconds(delta))
                .ok_or_else(|| anyhow!("Schedule timestamp overflow"));
        }
        let mut next = cursor;
        while next <= now {
            next = self.next(next)?;
        }
        Ok(next)
    }
    pub fn inherit_timezone(&mut self, current: &Self) {
        if let (Self::Cron { timezone: next, .. }, Self::Cron { timezone: old, .. }) =
            (self, current)
        {
            if next.is_none() {
                *next = old.clone();
            }
        }
    }
    pub fn run_on_create(&self) -> bool {
        matches!(self, Self::Every { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }
    #[test]
    fn cron_uses_and_and_strict_next_minute() {
        let c = Cadence::Cron {
            expression: "0 9 1 * 1".into(),
            timezone: None,
        };
        assert_eq!(
            c.next(at("2026-05-01T00:00:00Z")).unwrap(),
            at("2026-06-01T09:00:00Z")
        );
        let c = Cadence::Cron {
            expression: "* * * * *".into(),
            timezone: None,
        };
        assert_eq!(
            c.next(at("2026-01-01T00:00:00Z")).unwrap(),
            at("2026-01-01T00:01:00Z")
        );
    }
    #[test]
    fn dst_gap_is_skipped_and_fold_has_two_instants() {
        let c = Cadence::Cron {
            expression: "30 2 * * *".into(),
            timezone: Some("America/New_York".into()),
        };
        assert_eq!(
            c.next(at("2026-03-08T06:59:00Z")).unwrap(),
            at("2026-03-09T06:30:00Z")
        );
        let c = Cadence::Cron {
            expression: "30 1 * * *".into(),
            timezone: Some("America/New_York".into()),
        };
        assert_eq!(
            c.next(at("2026-11-01T05:30:00Z")).unwrap(),
            at("2026-11-01T06:30:00Z")
        );
    }
}
