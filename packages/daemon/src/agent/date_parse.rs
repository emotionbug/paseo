// Native port of V8's DateParser (dateparser.h, dateparser-inl.h, dateparser.cc):
// https://github.com/v8/v8/tree/main/src/date
// Copyright 2011, 2014 the V8 project authors. All rights reserved.
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met:
// * Redistributions of source code must retain the above copyright notice,
//   this list of conditions and the following disclaimer.
// * Redistributions in binary form must reproduce the above copyright notice,
//   this list of conditions and the following disclaimer in the documentation
//   and/or other materials provided with the distribution.
// * Neither the name of Google Inc. nor the names of its contributors may be
//   used to endorse or promote products derived from this software without
//   specific prior written permission.
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
// AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
// IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
// ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR CONTRIBUTORS BE
// LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
// CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
// SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
// INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
// CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
// ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
// POSSIBILITY OF SUCH DAMAGE.

const DAY_MS: i64 = 86_400_000;
const TIME_CLIP: i64 = 8_640_000_000_000_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Keyword {
    Invalid,
    Month,
    Zone,
    Separator,
    Meridiem,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Token {
    End,
    Invalid,
    Unknown,
    Space,
    Symbol(char),
    Number(i32, usize),
    Word(Keyword, i32, usize),
}
impl Token {
    fn number(self) -> Option<i32> {
        if let Self::Number(n, _) = self {
            Some(n)
        } else {
            None
        }
    }
    fn fixed(self, length: usize, low: i32, high: i32) -> bool {
        matches!(self, Self::Number(n,l) if l==length && (low..=high).contains(&n))
    }
    fn sign(self) -> Option<i32> {
        match self {
            Self::Symbol('+') => Some(1),
            Self::Symbol('-') => Some(-1),
            _ => None,
        }
    }
    fn z(self) -> bool {
        self == Self::Word(Keyword::Zone, 0, 1)
    }
    fn milliseconds(self) -> Option<i32> {
        let Self::Number(mut n, mut length) = self else {
            return None;
        };
        if length < 3 {
            n *= if length == 1 { 100 } else { 10 };
        } else if length > 3 {
            length = length.min(9);
            n /= 10_i32.pow((length - 3) as u32);
        }
        Some(n)
    }
}
fn space(c: char) -> bool {
    matches!(
        c,
        '\u{0009}' | '\u{000b}' | '\u{000c}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}
fn line(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}
struct Scanner<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    next: Token,
}
impl<'a> Scanner<'a> {
    fn new(input: &'a str) -> Self {
        let mut this = Self {
            chars: input.chars().peekable(),
            next: Token::End,
        };
        this.next = this.scan();
        this
    }
    fn peek(&self) -> Token {
        self.next
    }
    fn take(&mut self) -> Token {
        let token = self.next;
        self.next = self.scan();
        token
    }
    fn skip(&mut self, c: char) -> bool {
        if self.next == Token::Symbol(c) {
            self.take();
            true
        } else {
            false
        }
    }
    fn scan(&mut self) -> Token {
        let Some(&c) = self.chars.peek() else {
            return Token::End;
        };
        if c == '\0' {
            return Token::End;
        }
        if c.is_ascii_digit() {
            let mut n = 0;
            let mut length = 0;
            let mut significant = 0;
            let mut leading = true;
            while let Some(&digit) = self.chars.peek() {
                if !digit.is_ascii_digit() {
                    break;
                }
                self.chars.next();
                length += 1;
                if leading && digit == '0' {
                    continue;
                }
                leading = false;
                if significant < 9 {
                    n = n * 10 + (digit as i32 - '0' as i32);
                }
                significant += 1;
            }
            return Token::Number(n, length);
        }
        if matches!(c, ':' | '-' | '+' | '.' | ')') {
            self.chars.next();
            return Token::Symbol(c);
        }
        if c >= 'A' && !space(c) {
            let mut prefix = ['\0'; 3];
            let mut length = 0;
            while let Some(&c) = self.chars.peek() {
                if c < 'A' || space(c) || c == '\0' {
                    break;
                }
                self.chars.next();
                if length < 3 {
                    prefix[length] = c.to_ascii_lowercase();
                }
                length += c.len_utf16();
            }
            let months = [
                "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
            ];
            for (index, month) in months.iter().enumerate() {
                if month.chars().eq(prefix) {
                    return Token::Word(Keyword::Month, index as i32 + 1, length);
                }
            }
            let names = [
                ("am", Keyword::Meridiem, 0),
                ("pm", Keyword::Meridiem, 12),
                ("ut", Keyword::Zone, 0),
                ("utc", Keyword::Zone, 0),
                ("z", Keyword::Zone, 0),
                ("gmt", Keyword::Zone, 0),
                ("cdt", Keyword::Zone, -5),
                ("cst", Keyword::Zone, -6),
                ("edt", Keyword::Zone, -4),
                ("est", Keyword::Zone, -5),
                ("mdt", Keyword::Zone, -6),
                ("mst", Keyword::Zone, -7),
                ("pdt", Keyword::Zone, -7),
                ("pst", Keyword::Zone, -8),
                ("t", Keyword::Separator, 0),
            ];
            for (name, kind, value) in names {
                if length == name.len()
                    && name
                        .chars()
                        .chain(std::iter::repeat('\0'))
                        .take(3)
                        .eq(prefix)
                {
                    return Token::Word(kind, value, length);
                }
            }
            return Token::Word(Keyword::Invalid, 0, length);
        }
        self.chars.next();
        if space(c) || line(c) {
            return Token::Space;
        }
        if c == '(' {
            let mut balance = 1;
            while let Some(&c) = self.chars.peek() {
                if c == '\0' {
                    break;
                }
                self.chars.next();
                if c == '(' {
                    balance += 1;
                } else if c == ')' {
                    balance -= 1;
                    if balance == 0 {
                        break;
                    }
                }
            }
        }
        Token::Unknown
    }
}
#[derive(Default)]
struct Day {
    values: [i32; 3],
    length: usize,
    month: Option<i32>,
    iso: bool,
}
impl Day {
    fn add(&mut self, n: i32) -> bool {
        if self.length == 3 {
            return false;
        }
        self.values[self.length] = n;
        self.length += 1;
        true
    }
    fn write(mut self) -> Option<(i32, i32, i32)> {
        if self.length == 0 {
            return None;
        }
        while self.length < 3 {
            self.add(1);
        }
        let [a, b, c] = self.values;
        let (mut year, month, day) = if let Some(month) = self.month {
            if !(1..=31).contains(&a) {
                (a, month, b)
            } else {
                (b, month, a)
            }
        } else if self.iso || !(1..=31).contains(&a) {
            (a, b, c)
        } else {
            (c, a, b)
        };
        if !self.iso {
            if (0..=49).contains(&year) {
                year += 2000;
            } else if (50..=99).contains(&year) {
                year += 1900;
            }
        }
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return None;
        }
        Some((year, month, day))
    }
}
#[derive(Default)]
struct Time {
    values: [i32; 4],
    length: usize,
    hour_offset: Option<i32>,
}
impl Time {
    fn add(&mut self, n: i32) -> bool {
        if self.length == 4 {
            return false;
        }
        self.values[self.length] = n;
        self.length += 1;
        true
    }
    fn final_value(&mut self, n: i32) -> bool {
        if !self.add(n) {
            return false;
        }
        while self.length < 4 {
            self.add(0);
        }
        true
    }
    fn expecting(&self, n: i32) -> bool {
        match self.length {
            1 | 2 => (0..=59).contains(&n),
            3 => (0..=999).contains(&n),
            _ => false,
        }
    }
    fn write(mut self) -> Option<i64> {
        if let Some(offset) = self.hour_offset {
            if !(0..=12).contains(&self.values[0]) {
                return None;
            }
            self.values[0] = self.values[0] % 12 + offset;
        }
        let [h, m, s, ms] = self.values;
        if !((0..=23).contains(&h)
            && (0..=59).contains(&m)
            && (0..=59).contains(&s)
            && (0..=999).contains(&ms))
            && !(h == 24 && m == 0 && s == 0 && ms == 0)
        {
            return None;
        }
        Some(((h as i64 * 60 + m as i64) * 60 + s as i64) * 1000 + ms as i64)
    }
}
#[derive(Default)]
struct Zone {
    sign: Option<i32>,
    hour: Option<i32>,
    minute: Option<i32>,
}
impl Zone {
    fn set(&mut self, n: i32) {
        self.sign = Some(if n < 0 { -1 } else { 1 });
        self.hour = Some(n.abs());
        self.minute = Some(0);
    }
    fn utc(&self) -> bool {
        self.hour == Some(0) && self.minute == Some(0)
    }
    fn expecting(&self, n: i32) -> bool {
        self.hour.is_some() && self.minute.is_none() && (0..=59).contains(&n)
    }
    fn write(&self) -> Option<Option<i64>> {
        let Some(sign) = self.sign else {
            return Some(None);
        };
        let seconds = (self.hour.unwrap_or(0) as u32)
            .wrapping_mul(3600)
            .wrapping_add((self.minute.unwrap_or(0) as u32).wrapping_mul(60));
        if seconds > i32::MAX as u32 {
            return None;
        }
        Some(Some(seconds as i64 * sign as i64 * 1000))
    }
}
fn iso(scanner: &mut Scanner<'_>, day: &mut Day, time: &mut Time, zone: &mut Zone) -> Token {
    if let Some(sign) = scanner.peek().sign() {
        let token = scanner.take();
        if !scanner.peek().fixed(6, 0, 999999) {
            return token;
        }
        let year = scanner.take().number().unwrap();
        if sign < 0 && year == 0 {
            return token;
        }
        day.add(sign * year);
    } else if scanner.peek().fixed(4, 0, 9999) {
        day.add(scanner.take().number().unwrap());
    } else {
        return scanner.take();
    }
    if scanner.skip('-') {
        if !scanner.peek().fixed(2, 1, 12) {
            return scanner.take();
        }
        day.add(scanner.take().number().unwrap());
        if scanner.skip('-') {
            if !scanner.peek().fixed(2, 1, 31) {
                return scanner.take();
            }
            day.add(scanner.take().number().unwrap());
        }
    }
    if matches!(scanner.peek(), Token::Word(Keyword::Separator, _, _)) {
        scanner.take();
        if !scanner.peek().fixed(2, 0, 24) {
            return Token::Invalid;
        }
        let h = scanner.take().number().unwrap();
        time.add(h);
        if !scanner.skip(':') || !scanner.peek().fixed(2, 0, if h == 24 { 0 } else { 59 }) {
            return Token::Invalid;
        }
        time.add(scanner.take().number().unwrap());
        if scanner.skip(':') {
            if !scanner.peek().fixed(2, 0, if h == 24 { 0 } else { 59 }) {
                return Token::Invalid;
            }
            time.add(scanner.take().number().unwrap());
            if scanner.skip('.') {
                if scanner.peek().number().is_none()
                    || h == 24 && scanner.peek().number() != Some(0)
                {
                    return Token::Invalid;
                }
                time.add(scanner.take().milliseconds().unwrap());
            }
        }
        if scanner.peek().z() {
            scanner.take();
            zone.set(0);
        } else if let Some(sign) = scanner.peek().sign() {
            scanner.take();
            zone.sign = Some(sign);
            if scanner.peek().fixed(4, 0, 9999) {
                let n = scanner.take().number().unwrap();
                if n / 100 > 23 || n % 100 > 59 {
                    return Token::Invalid;
                }
                zone.hour = Some(n / 100);
                zone.minute = Some(n % 100);
            } else {
                if !scanner.peek().fixed(2, 0, 23) {
                    return Token::Invalid;
                }
                zone.hour = scanner.take().number();
                if !scanner.skip(':') || !scanner.peek().fixed(2, 0, 59) {
                    return Token::Invalid;
                }
                zone.minute = scanner.take().number();
            }
        }
        if scanner.peek() != Token::End {
            return Token::Invalid;
        }
    } else if scanner.peek() != Token::End {
        return scanner.take();
    }
    if zone.hour.is_none() && time.length == 0 {
        zone.set(0);
    }
    day.iso = true;
    Token::End
}
#[derive(Debug, PartialEq, Eq)]
struct Parsed {
    wall_ms: i64,
    offset_ms: Option<i64>,
}
fn parse(input: &str) -> Option<Parsed> {
    let mut scanner = Scanner::new(input);
    let mut day = Day::default();
    let mut time = Time::default();
    let mut zone = Zone::default();
    let mut token = iso(&mut scanner, &mut day, &mut time, &mut zone);
    if token == Token::Invalid {
        return None;
    }
    let mut number_seen = day.length != 0;
    while token != Token::End {
        if let Token::Number(n, _) = token {
            number_seen = true;
            if scanner.skip(':') {
                if scanner.skip(':') {
                    if time.length != 0 {
                        return None;
                    }
                    time.add(n);
                    time.add(0);
                } else {
                    if !time.add(n) {
                        return None;
                    }
                    if scanner.peek() == Token::Symbol('.') {
                        scanner.take();
                    }
                }
            } else if scanner.skip('.') && time.expecting(n) {
                time.add(n);
                let ms = scanner.take().milliseconds()?;
                time.final_value(ms);
            } else if zone.expecting(n) {
                zone.minute = Some(n);
            } else if time.expecting(n) {
                time.final_value(n);
                let next = scanner.peek();
                if next != Token::End && next != Token::Space && !next.z() && next.sign().is_none()
                {
                    return None;
                }
            } else {
                if !day.add(n) {
                    return None;
                }
                scanner.skip('-');
            }
        } else if let Token::Word(kind, value, _) = token {
            match kind {
                Keyword::Meridiem if time.length != 0 => time.hour_offset = Some(value),
                Keyword::Month => {
                    day.month = Some(value);
                    scanner.skip('-');
                }
                Keyword::Zone if number_seen => zone.set(value),
                _ => {
                    if number_seen || scanner.peek().number().is_some() {
                        return None;
                    }
                }
            }
        } else if let Some(sign) = token.sign().filter(|_| zone.utc() || time.length != 0) {
            zone.sign = Some(sign);
            let (n, length) = if let Token::Number(n, length) = scanner.peek() {
                scanner.take();
                (n, length)
            } else {
                (0, 0)
            };
            number_seen = true;
            if scanner.peek() == Token::Symbol(':') {
                zone.hour = Some(n);
                zone.minute = None;
            } else if length == 1 || length == 2 {
                zone.hour = Some(n);
                zone.minute = Some(0);
            } else if length == 3 || length == 4 {
                zone.hour = Some(n / 100);
                zone.minute = Some(n % 100);
            } else {
                return None;
            }
        } else if (token.sign().is_some() || token == Token::Symbol(')')) && number_seen {
            return None;
        }
        token = scanner.take();
    }
    let (year, month, date) = day.write()?;
    let wall_ms = civil_days(year as i64, month as i64, date as i64)
        .checked_mul(DAY_MS)?
        .checked_add(time.write()?)?;
    Some(Parsed {
        wall_ms,
        offset_ms: zone.write()?,
    })
}
// Proleptic Gregorian arithmetic deliberately normalizes February 30/31 and
// 24:00; constructing a strict calendar date here would reject JS-valid input.
fn civil_days(mut year: i64, month: i64, day: i64) -> i64 {
    year -= i64::from(month <= 2);
    let era = year.div_euclid(400);
    let y = year - era * 400;
    let m = month + if month > 2 { -3 } else { 9 };
    era * 146097 + (y * 365 + y / 4 - y / 100 + (153 * m + 2) / 5 + day - 1) - 719468
}
fn resolve(parsed: Parsed, mut local: impl FnMut(i64) -> Option<i64>) -> Option<i64> {
    let utc = if let Some(offset) = parsed.offset_ms {
        parsed.wall_ms.checked_sub(offset)?
    } else {
        if parsed.wall_ms.unsigned_abs() > (TIME_CLIP + 30 * DAY_MS) as u64 {
            return None;
        }
        local(parsed.wall_ms)?
    };
    (utc.unsigned_abs() <= TIME_CLIP as u64).then_some(utc)
}
/// Date.parse's NaN is represented by None; callers retain their source error.
pub(super) fn parse_date(input: &str) -> anyhow::Result<Option<i64>> {
    let Some(parsed) = parse(input) else {
        return Ok(None);
    };
    let mut failure = None;
    let result = resolve(parsed, |wall| match local_to_utc(wall) {
        Ok(value) => value,
        Err(error) => {
            failure = Some(error);
            None
        }
    });
    match failure {
        Some(error) => Err(error),
        None => Ok(result),
    }
}

// Resolve the host timezone's actual historical offsets, not a fixed current
// offset. Two surrounding samples follow V8's 19-day transition-spacing bound.
// Of repeated local times choose the earlier instant; for skipped local times
// advance through the gap using the offset before the transition ("compatible").
fn resolve_local(wall: i64, mut offset_at: impl FnMut(i64) -> Option<i64>) -> Option<i64> {
    let offsets = [
        offset_at(wall - 19 * DAY_MS)?,
        offset_at(wall)?,
        offset_at(wall + 19 * DAY_MS)?,
    ];
    let mut exact = None;
    let mut forward: Option<(i64, i64)> = None;
    for (index, offset) in offsets.iter().enumerate() {
        if offsets[..index].contains(offset) {
            continue;
        }
        let utc = wall.checked_sub(*offset)?;
        let observed = utc.checked_add(offset_at(utc)?)?;
        if observed == wall {
            exact = Some(exact.map_or(utc, |previous: i64| previous.min(utc)));
        } else if observed > wall && forward.is_none_or(|(previous, _)| observed < previous) {
            forward = Some((observed, utc));
        }
    }
    exact.or_else(|| forward.map(|(_, utc)| utc))
}
#[cfg(unix)]
fn local_to_utc(wall: i64) -> anyhow::Result<Option<i64>> {
    Ok(resolve_local(wall, local_offset))
}
#[cfg(unix)]
fn local_offset(utc: i64) -> Option<i64> {
    let seconds = libc::time_t::try_from(utc.div_euclid(1000)).ok()?;
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    // localtime_r initializes the caller-owned result on success and honors the
    // process TZ/system zone. tm_gmtoff retains historical second-level offsets.
    let result = unsafe { libc::localtime_r(&seconds, tm.as_mut_ptr()) };
    if result.is_null() {
        return None;
    }
    let tm = unsafe { tm.assume_init() };
    Some(tm.tm_gmtoff as i64 * 1000)
}
#[cfg(not(unix))]
fn local_to_utc(wall: i64) -> anyhow::Result<Option<i64>> {
    let zone = jiff::tz::TimeZone::try_system()
        .map_err(|error| anyhow::anyhow!("Unable to resolve system timezone: {error}"))?;
    Ok(resolve_local(wall, |utc| {
        let timestamp = jiff::Timestamp::from_millisecond(utc)
            .or_else(|_| {
                // Beyond Jiff's civil range, POSIX continuation rules repeat every
                // Gregorian 400 years. The prehistory mapping remains before the
                // IANA database's first transition and keeps its historical offset.
                let cycle = 146097 * DAY_MS;
                let start = civil_days(if utc >= 0 { 2400 } else { -400 }, 1, 1) * DAY_MS;
                jiff::Timestamp::from_millisecond(start + (utc - start).rem_euclid(cycle))
            })
            .ok()?;
        Some(zone.to_offset(timestamp).seconds() as i64 * 1000)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn utc(input: &str) -> Option<i64> {
        resolve(parse(input)?, Some)
    }
    #[test]
    fn legacy_words_offsets_and_comments() {
        assert_eq!(utc("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(
            utc("December 17, 1995 03:24:00 GMT-0800"),
            utc("1995-12-17T11:24:00Z")
        );
        assert_eq!(
            utc("garbage (nested (ignored)) Jan-1-1970 12:00 AM UT"),
            Some(0)
        );
        assert_eq!(utc("Janua 1, 1970 12:00 PM PST"), Some(20 * 3600000));
        assert_eq!(utc("1970/1/1 0::01 GMT"), Some(1000));
        assert_eq!(utc("1970/1/1 00:00:00.12 GMT"), Some(120));
        assert_eq!(utc("garbage1970/1/1"), None);
        assert_eq!(utc("Jan 1 1970 (ignored\0February 4 2020"), Some(0));
        assert_eq!(utc("Jan 1 1970 (unclosed comment"), Some(0));
        assert_eq!(utc("1970-01-01T00:00:00Z junk"), None);
    }
    #[test]
    fn iso_overflow_extended_years_and_timeclip() {
        assert_eq!(utc("1970"), Some(0));
        assert_eq!(utc("1970-01"), Some(0));
        assert_eq!(utc("2023-02-29"), utc("2023-03-01"));
        assert_eq!(utc("2020-02-31T24:00:00.000Z"), utc("2020-03-03T00:00Z"));
        assert_eq!(utc("1970-01-01T00:00:00.123456789012Z"), Some(123));
        assert_eq!(utc("1970-01-01T24:00:00.001Z"), None);
        assert_eq!(utc("1970-01-01T00:00+2400"), None);
        assert_eq!(utc("+275760-09-13T00:00:00.000Z"), Some(TIME_CLIP));
        assert_eq!(utc("+275760-09-13T00:00:00.001Z"), None);
        assert_eq!(utc("-271821-04-20T00:00:00.000Z"), Some(-TIME_CLIP));
        assert_eq!(utc("-271821-04-19T23:59:59.999Z"), None);
        assert_eq!(utc("+999999-01-01"), None);
        assert_eq!(utc("1970-01-01T00:00:00Z\0ignored"), Some(0));
    }
    #[test]
    fn partial_iso_falls_back_without_losing_consumed_components() {
        assert_eq!(utc("49/1/1"), utc("2049-01-01"));
        assert_eq!(utc("50/1/1"), utc("1950-01-01"));
        assert_eq!(utc("1"), utc("2001-01-01"));
        assert_eq!(utc("0"), utc("2000-01-01"));
        assert_eq!(utc("Jan 1"), utc("2001-01-01"));
        assert_eq!(utc("32"), utc("2032-01-01"));
        assert_eq!(utc("13"), None);
        assert_eq!(utc(""), None);
        assert_eq!(utc("January"), None);
        assert_eq!(parse("1970-01-01").unwrap().offset_ms, Some(0));
        assert_eq!(parse("1970-01-01 ").unwrap().offset_ms, None);
        assert_eq!(parse("1970-01-01T00:00").unwrap().offset_ms, None);
    }
    #[test]
    fn local_gap_fold_historical_seconds_and_clipping() {
        let hour = 3600000;
        assert_eq!(
            resolve_local(2 * hour, |utc| Some(if utc < 0 { 0 } else { 4 * hour })),
            Some(2 * hour)
        );
        assert_eq!(
            resolve_local(2 * hour, |utc| Some(if utc < 0 { 4 * hour } else { 0 })),
            Some(-2 * hour)
        );
        assert_eq!(resolve_local(0, |_| Some(1172000)), Some(-1172000));
        assert_eq!(
            resolve(
                Parsed {
                    wall_ms: TIME_CLIP + hour,
                    offset_ms: Some(hour)
                },
                Some
            ),
            Some(TIME_CLIP)
        );
        assert_eq!(
            resolve(
                Parsed {
                    wall_ms: TIME_CLIP + hour,
                    offset_ms: None
                },
                |value| Some(value - hour)
            ),
            Some(TIME_CLIP)
        );
        assert_eq!(
            resolve(
                Parsed {
                    wall_ms: TIME_CLIP + 31 * DAY_MS,
                    offset_ms: None
                },
                |_| Some(0)
            ),
            None
        );
        assert_eq!(
            resolve(
                Parsed {
                    wall_ms: i64::MIN,
                    offset_ms: Some(0)
                },
                Some
            ),
            None
        );
        assert_eq!(
            resolve(
                Parsed {
                    wall_ms: i64::MIN,
                    offset_ms: None
                },
                Some
            ),
            None
        );
    }
}
