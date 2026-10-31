//! Native terminal grid and the Paseo wire projection of it.
//!
//! Alacritty owns VT parsing, scrolling, character sets, reflow, and both screen
//! buffers. A second, incremental VTE parser observes Paseo's shell protocol and
//! input-mode extensions; it never attempts to emulate the display with lines.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte;
use alacritty_terminal::vte::ansi::{Color, Processor, Timeout};
use serde_json::{Value, json};

const HISTORY_LINES: usize = 1000;
const MAX_VISIBLE_HISTORY: usize = 500;

#[derive(Debug, Default)]
pub struct ScreenEffects {
    pub replies: Vec<u8>,
    /// OSC 633;D without a status is a completion with an unknown exit code.
    pub command_finished: Vec<Option<i32>>,
    /// An empty title explicitly clears the previous title.
    pub title: Option<String>,
}

#[derive(Default)]
struct PendingEffects {
    effects: ScreenEffects,
    suppress_reply: bool,
}

#[derive(Clone, Default)]
struct Events(Arc<Mutex<PendingEffects>>);

impl Events {
    fn reply(&self, bytes: &[u8]) {
        self.0
            .lock()
            .unwrap()
            .effects
            .replies
            .extend_from_slice(bytes);
    }

    fn suppress(&self, suppress: bool) {
        self.0.lock().unwrap().suppress_reply = suppress;
    }
}

impl EventListener for Events {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(text) => {
                let mut pending = self.0.lock().unwrap();
                if !pending.suppress_reply {
                    pending.effects.replies.extend_from_slice(text.as_bytes());
                }
            }
            Event::Title(title) => self.0.lock().unwrap().effects.title = Some(title),
            Event::ResetTitle => self.0.lock().unwrap().effects.title = Some(String::new()),
            // The server has no host clipboard or pixel-sized terminal window.
            // Color queries supported by terminal.ts are answered by Controls.
            _ => {}
        }
    }
}

/// Synchronized updates defer painting, not the authoritative headless grid.
/// Keeping the parser synchronous also lets DSR observe all preceding output.
#[derive(Default)]
struct HeadlessUpdates;

impl Timeout for HeadlessUpdates {
    fn set_timeout(&mut self, _: Duration) {}
    fn clear_timeout(&mut self) {}
    fn pending_timeout(&self) -> bool {
        false
    }
}

struct Size {
    rows: usize,
    cols: usize,
}

impl Size {
    fn new(rows: u16, cols: u16) -> Self {
        // Like xterm, keep space for a full-width character even for a 1-column
        // size request. Empty dimensions are never valid terminal geometry.
        Self {
            rows: usize::from(rows.max(1)),
            cols: usize::from(cols.max(2)),
        }
    }
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

#[derive(Default)]
struct InputModes {
    kitty: u16,
    kitty_stack: Vec<u16>,
    win32: bool,
    application_cursor: bool,
    bracketed_paste: bool,
    cursor_style: Option<&'static str>,
    cursor_blink: Option<bool>,
}

/// VTE normalizes an omitted numeric parameter to zero. The input-mode
/// protocol distinguishes CSI > u from CSI > 0 u, so retain presence only;
/// VTE remains responsible for recognizing and dispatching the sequence.
#[derive(Default)]
struct ParameterPresence {
    state: u8,
    parameter: usize,
    present: [bool; 2],
}

impl ParameterPresence {
    fn advance(&mut self, byte: u8) {
        if byte == 0x1b {
            self.state = 1;
        } else if self.state == 1 {
            self.state = if byte == b'[' { 2 } else { 0 };
            self.parameter = 0;
            self.present = [false; 2];
        } else if self.state == 2 {
            match byte {
                b'0'..=b'9' if self.parameter < 2 => self.present[self.parameter] = true,
                b';' => self.parameter = self.parameter.saturating_add(1),
                0x40..=0x7e | 0x18 | 0x1a => self.state = 0,
                _ => {}
            }
        }
    }
}

pub struct Screen {
    terminal: Term<Events>,
    parser: Processor<HeadlessUpdates>,
    control_parser: vte::Parser,
    parameter_presence: ParameterPresence,
    events: Events,
    modes: InputModes,
    title: Option<String>,
}

impl Screen {
    pub fn new(rows: u16, cols: u16) -> Self {
        let events = Events::default();
        let config = Config {
            scrolling_history: HISTORY_LINES,
            ..Config::default()
        };
        Self {
            terminal: Term::new(config, &Size::new(rows, cols), events.clone()),
            parser: Processor::new(),
            control_parser: vte::Parser::new(),
            parameter_presence: ParameterPresence::default(),
            events,
            modes: InputModes::default(),
            title: None,
        }
    }

    pub fn process(&mut self, bytes: &[u8]) -> ScreenEffects {
        for &byte in bytes {
            let mut suppress = false;
            self.parameter_presence.advance(byte);
            self.control_parser.advance(
                &mut Controls {
                    terminal: &self.terminal,
                    modes: &mut self.modes,
                    events: &self.events,
                    suppress: &mut suppress,
                    parameter_presence: self.parameter_presence.present,
                },
                byte,
            );
            if suppress {
                self.events.suppress(true);
            }
            self.parser.advance(&mut self.terminal, byte);
            if suppress {
                self.events.suppress(false);
            }
        }
        let effects = std::mem::take(&mut self.events.0.lock().unwrap().effects);
        if let Some(title) = &effects.title {
            self.title = (!title.trim().is_empty()).then(|| title.clone());
        }
        self.terminal.reset_damage();
        effects
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.terminal.resize(Size::new(rows, cols));
    }

    pub fn snapshot(&self, wrapped: bool) -> Value {
        let rows = self.terminal.screen_lines();
        let history = self.terminal.history_size() as i32;
        let mut state = json!({
            "rows": rows,
            "cols": self.terminal.columns(),
            "grid": (0..rows as i32).map(|row| self.row_json(row)).collect::<Vec<_>>(),
            "scrollback": (-history..0).map(|row| self.row_json(row)).collect::<Vec<_>>(),
            "cursor": self.cursor_json(),
        });
        if let Some(title) = &self.title {
            state["title"] = json!(title);
        }
        if wrapped {
            state["gridWrapped"] = json!(
                (0..rows as i32)
                    .map(|r| self.wrapped(r))
                    .collect::<Vec<_>>()
            );
            state["scrollbackWrapped"] =
                json!((-history..0).map(|r| self.wrapped(r)).collect::<Vec<_>>());
        }
        state
    }

    fn row_json(&self, row: i32) -> Value {
        Value::Array(
            (0..self.terminal.columns())
                .map(|column| {
                    let cell = &self.terminal.grid()[Line(row)][Column(column)];
                    let style = Style::from(cell);
                    let mut value = json!({
                        "char": cell_text(cell),
                        "bold": style.flags.contains(Flags::BOLD),
                        "italic": style.flags.contains(Flags::ITALIC),
                        "underline": style.flags.intersects(Flags::ALL_UNDERLINES),
                        "dim": style.flags.contains(Flags::DIM),
                        "inverse": style.flags.contains(Flags::INVERSE),
                        "strikethrough": style.flags.contains(Flags::STRIKEOUT),
                    });
                    for (field, mode_field, color) in
                        [("fg", "fgMode", style.fg), ("bg", "bgMode", style.bg)]
                    {
                        if let Some((mode, color)) = color {
                            value[field] = json!(color);
                            value[mode_field] = json!(mode);
                        }
                    }
                    value
                })
                .collect(),
        )
    }

    fn cursor_json(&self) -> Value {
        let (row, col) = cursor_position(&self.terminal);
        let mut cursor = json!({ "row": row, "col": col });
        if !self.terminal.mode().contains(TermMode::SHOW_CURSOR) {
            cursor["hidden"] = json!(true);
        }
        if let Some(style) = self.modes.cursor_style {
            cursor["style"] = json!(style);
        }
        if let Some(blink) = self.modes.cursor_blink {
            cursor["blink"] = json!(blink);
        }
        cursor
    }

    fn wrapped(&self, row: i32) -> bool {
        self.terminal.grid()[Line(row)][Column(self.terminal.columns() - 1)]
            .flags
            .contains(Flags::WRAPLINE)
    }

    /// Matches TerminalInputModeTracker.getPreamble, including its ordering and
    /// replacement (rather than bitwise) semantics for CSI = flags ; mode u.
    pub fn replay_preamble(&self) -> String {
        let mut result = String::new();
        if self.modes.kitty > 0 {
            let _ = write!(result, "\x1b[={};1u", self.modes.kitty);
        }
        if self.modes.win32 {
            result.push_str("\x1b[?9001h");
        }
        if self.modes.application_cursor {
            result.push_str("\x1b[?1h");
        }
        if self.modes.bracketed_paste {
            result.push_str("\x1b[?2004h");
        }
        result
    }

    /// ANSI rendering follows protocol/terminal-snapshot.ts. The caller resets
    /// its rendering surface before applying a snapshot; live restores carry
    /// only the input preamble. The caller supplies 200 for an absent visible
    /// scrollback limit. Full snapshots always contain all retained history.
    pub fn restore(&self, mode: &str, scrollback: usize) -> Vec<u8> {
        if mode == "live" {
            return self.replay_preamble().into_bytes();
        }
        let history = self.terminal.history_size();
        let history = if mode == "visible-snapshot" {
            history.min(scrollback.min(MAX_VISIBLE_HISTORY))
        } else {
            history
        };
        let mut output = String::new();
        let last = self.terminal.screen_lines() as i32 - 1;
        for row in -(history as i32)..=last {
            let wrapped = self.wrapped(row);
            self.render_row(row, wrapped, &mut output);
            if row < last && !wrapped {
                output.push_str("\r\n");
            }
        }
        output.push_str("\x1b[0m");
        if let Some(style) = self.modes.cursor_style {
            let base = match style {
                "underline" => 3,
                "bar" => 5,
                _ => 1,
            };
            let code = base + usize::from(self.modes.cursor_blink == Some(false));
            let _ = write!(output, "\x1b[{code} q");
        }
        let (row, col) = cursor_position(&self.terminal);
        let _ = write!(output, "\x1b[{};{}H", row + 1, col + 1);
        output.push_str(if self.terminal.mode().contains(TermMode::SHOW_CURSOR) {
            "\x1b[?25h"
        } else {
            "\x1b[?25l"
        });
        output.push_str(&self.replay_preamble());
        output.into_bytes()
    }

    fn render_row(&self, row: i32, wrapped: bool, output: &mut String) {
        let cells = &self.terminal.grid()[Line(row)];
        let default = Style::default();
        let length = if wrapped {
            self.terminal.columns()
        } else {
            (0..self.terminal.columns())
                .rfind(|&column| {
                    let cell = &cells[Column(column)];
                    cell.c != ' ' && cell.c != '\t'
                        || cell.zerowidth().is_some_and(|chars| !chars.is_empty())
                        || cell.flags.contains(Flags::WIDE_CHAR_SPACER)
                        || Style::from(cell) != default
                })
                .map_or(0, |column| column + 1)
        };
        let mut previous = default;
        for column in 0..length {
            let cell = &cells[Column(column)];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let style = Style::from(cell);
            if style != previous {
                style.write_ansi(output);
                previous = style;
            }
            append_cell(output, cell);
        }
        if previous != default {
            output.push_str("\x1b[0m");
        }
    }

    /// Source capture is plain rendered cell text even with stripAnsi=false:
    /// SGR is not synthesized. Bounds address physical history+viewport rows,
    /// are inclusive, and are compared before JavaScript slice truncation.
    pub fn capture(
        &self,
        start: Option<f64>,
        end: Option<f64>,
        _strip: bool,
    ) -> (Vec<String>, usize) {
        let history = self.terminal.history_size();
        let total = history + self.terminal.screen_lines();
        let resolve = |number: Option<f64>, fallback: usize| {
            let number = number.unwrap_or(fallback as f64);
            let index = if number < 0.0 {
                total as f64 + number
            } else {
                number
            };
            if index < 0.0 {
                0.0
            } else if index >= total as f64 {
                (total - 1) as f64
            } else {
                index
            }
        };
        let start = resolve(start, 0);
        let end = resolve(end, total - 1);
        if start > end {
            return (Vec::new(), total);
        }
        let end_exclusive = ((end + 1.0) as usize).min(total);
        let mut lines = Vec::with_capacity(end_exclusive - start as usize);
        for index in start as usize..end_exclusive {
            let row = Line(index as i32 - history as i32);
            let mut text = String::new();
            for column in 0..self.terminal.columns() {
                append_cell(&mut text, &self.terminal.grid()[row][Column(column)]);
            }
            text.truncate(text.trim_end_matches(js_whitespace).len());
            lines.push(text);
        }
        (lines, total)
    }

    pub(crate) fn exit_lines(&self, limit: usize) -> Vec<String> {
        let history = self.terminal.history_size() as i32;
        let mut lines: Vec<String> = Vec::new();
        for row in -history..self.terminal.screen_lines() as i32 {
            let mut text = String::new();
            for column in 0..self.terminal.columns() {
                append_cell(&mut text, &self.terminal.grid()[Line(row)][Column(column)]);
            }
            text.truncate(text.trim_end_matches(js_whitespace).len());
            if row > -history && self.wrapped(row - 1) && !lines.is_empty() {
                lines.last_mut().unwrap().push_str(&text);
            } else {
                lines.push(text);
            }
        }
        while lines
            .last()
            .is_some_and(|line| line.trim_matches(js_whitespace).is_empty())
        {
            lines.pop();
        }
        let first = lines
            .iter()
            .position(|line| !line.trim_matches(js_whitespace).is_empty())
            .unwrap_or(lines.len());
        let start = first.max(lines.len().saturating_sub(limit));
        lines.drain(..start);
        lines
    }
}

fn cursor_position(terminal: &Term<Events>) -> (usize, usize) {
    let cursor = &terminal.grid().cursor;
    (
        cursor.point.line.0 as usize,
        cursor.point.column.0 + usize::from(cursor.input_needs_wrap),
    )
}

fn js_whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}'
        | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}'
        | '\u{3000}' | '\u{feff}')
}

fn append_cell(text: &mut String, cell: &Cell) {
    if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
        return;
    }
    text.push(if cell.c == '\t' { ' ' } else { cell.c });
    if let Some(chars) = cell.zerowidth() {
        text.extend(chars);
    }
}

fn cell_text(cell: &Cell) -> String {
    let mut text = String::new();
    append_cell(&mut text, cell);
    text
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Style {
    fg: Option<(u8, u32)>,
    bg: Option<(u8, u32)>,
    flags: Flags,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            fg: None,
            bg: None,
            flags: Flags::empty(),
        }
    }
}

impl From<&Cell> for Style {
    fn from(cell: &Cell) -> Self {
        fn color(color: Color) -> Option<(u8, u32)> {
            match color {
                Color::Named(name) if (name as u32) < 16 => Some((1, name as u32)),
                Color::Indexed(index) => Some((2, u32::from(index))),
                Color::Spec(rgb) => Some((
                    3,
                    (u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b),
                )),
                _ => None,
            }
        }
        let flags = cell.flags
            & (Flags::BOLD
                | Flags::DIM
                | Flags::ITALIC
                | Flags::ALL_UNDERLINES
                | Flags::INVERSE
                | Flags::STRIKEOUT);
        Self {
            fg: color(cell.fg),
            bg: color(cell.bg),
            flags,
        }
    }
}

impl Style {
    fn write_ansi(self, output: &mut String) {
        output.push_str("\x1b[0");
        for (flag, code) in [
            (Flags::BOLD, 1),
            (Flags::DIM, 2),
            (Flags::ITALIC, 3),
            (Flags::ALL_UNDERLINES, 4),
            (Flags::INVERSE, 7),
            (Flags::STRIKEOUT, 9),
        ] {
            if self.flags.intersects(flag) {
                let _ = write!(output, ";{code}");
            }
        }
        for (background, color) in [(false, self.fg), (true, self.bg)] {
            match color {
                Some((1, value)) => {
                    let base = match (background, value >= 8) {
                        (false, false) => 30,
                        (true, false) => 40,
                        (false, true) => 90,
                        (true, true) => 100,
                    };
                    let _ = write!(output, ";{}", base + value % 8);
                }
                Some((2, value)) => {
                    let _ = write!(output, ";{};5;{value}", if background { 48 } else { 38 });
                }
                Some((3, value)) => {
                    let _ = write!(
                        output,
                        ";{};2;{};{};{}",
                        if background { 48 } else { 38 },
                        (value >> 16) & 255,
                        (value >> 8) & 255,
                        value & 255
                    );
                }
                _ => {}
            }
        }
        output.push('m');
    }
}

struct Controls<'a> {
    terminal: &'a Term<Events>,
    modes: &'a mut InputModes,
    events: &'a Events,
    suppress: &'a mut bool,
    parameter_presence: [bool; 2],
}

impl Controls<'_> {
    fn reply(&mut self, response: &[u8]) {
        self.events.reply(response);
        *self.suppress = true;
    }
}

impl vte::Perform for Controls<'_> {
    fn osc_dispatch(&mut self, params: &[&[u8]], _: bool) {
        match params {
            [b"633", b"D"] => {
                self.events
                    .0
                    .lock()
                    .unwrap()
                    .effects
                    .command_finished
                    .push(None);
            }
            [b"633", b"D", status] => {
                let digits = status.strip_prefix(b"-").unwrap_or(status);
                if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) {
                    if let Ok(status) = std::str::from_utf8(status).unwrap_or("").parse::<i32>() {
                        self.events
                            .0
                            .lock()
                            .unwrap()
                            .effects
                            .command_finished
                            .push(Some(status));
                    }
                }
            }
            [code @ (b"10" | b"11" | b"12"), query] => {
                if std::str::from_utf8(query).is_ok_and(|s| s.trim_matches(js_whitespace) == "?") {
                    let color = if *code == b"11" {
                        "0b0b/0b0b/0b0b"
                    } else {
                        "e6e6/e6e6/e6e6"
                    };
                    self.reply(
                        format!(
                            "\x1b]{};rgb:{color}\x1b\\",
                            std::str::from_utf8(code).unwrap()
                        )
                        .as_bytes(),
                    );
                }
            }
            _ => {}
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        ignore: bool,
        action: char,
    ) {
        if ignore || params.iter().any(|p| p.len() != 1) {
            return;
        }
        let mut values = params.iter();
        let first = values.next().and_then(|v| v.first()).copied().unwrap_or(0);
        match (action, intermediates) {
            ('c', []) if first == 0 && params.len() <= 1 => self.reply(b"\x1b[?62;4;22c"),
            ('n', []) if first == 5 && params.len() == 1 => self.reply(b"\x1b[0n"),
            ('n', [] | [b'?']) if first == 6 && params.len() == 1 => {
                let (row, col) = cursor_position(self.terminal);
                let private = if intermediates.is_empty() { "" } else { "?" };
                self.reply(format!("\x1b[{private}{};{}R", row + 1, col + 1).as_bytes());
            }
            ('h' | 'l', [b'?']) => {
                let enabled = action == 'h';
                for mode in params.iter().filter_map(|p| p.first()) {
                    match mode {
                        1 => self.modes.application_cursor = enabled,
                        12 => self.modes.cursor_blink = Some(enabled),
                        2004 => self.modes.bracketed_paste = enabled,
                        9001 => self.modes.win32 = enabled,
                        _ => {}
                    }
                }
            }
            ('q', [b' ']) if first <= 6 => {
                self.modes.cursor_style = Some(match first {
                    3 | 4 => "underline",
                    5 | 6 => "bar",
                    _ => "block",
                });
                self.modes.cursor_blink = Some(first == 0 || first % 2 == 1);
            }
            ('u', [b'>']) => {
                self.modes.kitty_stack.push(self.modes.kitty);
                self.modes.kitty = if self.parameter_presence[0] { first } else { 1 };
            }
            ('u', [b'=']) => {
                let mode = if self.parameter_presence[1] {
                    values.next().and_then(|v| v.first()).copied().unwrap_or(1)
                } else {
                    1
                };
                self.modes.kitty = if mode == 0 { 0 } else { first };
            }
            ('u', [b'<']) => {
                let count = usize::from(first.max(1));
                let length = self.modes.kitty_stack.len();
                self.modes.kitty = if count <= length {
                    self.modes.kitty_stack[length - count]
                } else {
                    0
                };
                self.modes
                    .kitty_stack
                    .truncate(length.saturating_sub(count));
            }
            ('u', [b'?']) => self.reply(format!("\x1b[?{}u", self.modes.kitty).as_bytes()),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if !ignore && intermediates.is_empty() && byte == b'c' {
            *self.modes = InputModes::default();
        }
    }
}

#[cfg(test)]
#[path = "screen_tests.rs"]
mod tests;
