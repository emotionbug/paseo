use super::*;

fn lines(screen: &Screen) -> Vec<String> {
    screen.capture(None, None, true).0
}

#[test]
fn styled_unicode_cells_keep_all_wire_attributes() {
    let mut screen = Screen::new(2, 8);
    screen.process("\x1b[1;2;3;4;7;9;38;2;12;34;56;48;5;123m好e\u{301}".as_bytes());
    let state = screen.snapshot(true);
    let cell = &state["grid"][0][0];
    assert_eq!(cell["char"], "好");
    assert_eq!(cell["fg"], 0x0c2238);
    assert_eq!(cell["fgMode"], 3);
    assert_eq!(cell["bg"], 123);
    assert_eq!(cell["bgMode"], 2);
    for attribute in [
        "bold",
        "dim",
        "italic",
        "underline",
        "inverse",
        "strikethrough",
    ] {
        assert_eq!(cell[attribute], true, "{attribute}");
    }
    assert_eq!(state["grid"][0][1]["char"], "");
    assert_eq!(state["grid"][0][2]["char"], "e\u{301}");
    assert_eq!(lines(&screen), ["好e\u{301}", ""]);
}

#[test]
fn chunk_boundaries_do_not_change_cells_effects_or_responses() {
    let bytes = "\x1b]2;build;title\x1b\\\x1b[3;2m好e\u{301}\x1b[0m\r\nabcdefghijk\x1b[2;4H\x1b[6n\x1b[?6n\x1b]633;D;-2\x1b\\\x1b]633;D\x07\x1b[?u".as_bytes();
    let mut whole = Screen::new(3, 6);
    let expected = whole.process(bytes);
    for chunk_size in 1..=bytes.len() {
        let mut screen = Screen::new(3, 6);
        let mut replies = Vec::new();
        let mut finished = Vec::new();
        let mut title = None;
        for chunk in bytes.chunks(chunk_size) {
            let effects = screen.process(chunk);
            replies.extend(effects.replies);
            finished.extend(effects.command_finished);
            if effects.title.is_some() {
                title = effects.title;
            }
        }
        assert_eq!(
            screen.snapshot(true),
            whole.snapshot(true),
            "chunk size {chunk_size}"
        );
        assert_eq!(replies, expected.replies, "chunk size {chunk_size}");
        assert_eq!(finished, vec![Some(-2), None]);
        assert_eq!(title, Some("build;title".to_owned()));
    }
}

#[test]
fn queries_use_source_capabilities_colors_and_current_cursor() {
    let mut screen = Screen::new(3, 10);
    let effects = screen.process(
        b"\x1b[c\x1b[0c\x1b[5n\x1b[2;4H\x1b[6n\x1b[?6n\x1b]10;?\x07\x1b]11; ? \x1b\\\x1b]12;?\x07",
    );
    assert_eq!(effects.replies, b"\x1b[?62;4;22c\x1b[?62;4;22c\x1b[0n\x1b[2;4R\x1b[?2;4R\x1b]10;rgb:e6e6/e6e6/e6e6\x1b\\\x1b]11;rgb:0b0b/0b0b/0b0b\x1b\\\x1b]12;rgb:e6e6/e6e6/e6e6\x1b\\");
    assert_eq!(
        screen.process(b"\x1b[1;1H0123456789\x1b[6n").replies,
        b"\x1b[1;11R"
    );
}

#[test]
fn completion_osc_accepts_only_exact_command_finished_shapes() {
    let mut screen = Screen::new(2, 20);
    let effects = screen.process(b"\x1b]633;D\x07\x1b]633;D;0\x07\x1b]633;D;-9\x07\x1b]633;D;+1\x07\x1b]633;D; 1\x07\x1b]633;D;1;extra\x07\x1b]633;D;\x07\x1b]633;C\x07");
    assert_eq!(effects.command_finished, [None, Some(0), Some(-9)]);
    assert_eq!(lines(&screen), ["", ""]);
}

#[test]
fn input_mode_stack_and_defaults_match_source_tracker() {
    let mut screen = Screen::new(2, 20);
    screen.process(b"\x1b[>u\x1b[?9001;1;2004h");
    assert_eq!(
        screen.replay_preamble(),
        "\x1b[=1;1u\x1b[?9001h\x1b[?1h\x1b[?2004h"
    );
    screen.process(b"\x1b[>0u");
    assert_eq!(screen.process(b"\x1b[?u").replies, b"\x1b[?0u");
    screen.process(b"\x1b[<u");
    assert_eq!(screen.process(b"\x1b[?u").replies, b"\x1b[?1u");
    screen.process(b"\x1b[=7;2u");
    assert_eq!(screen.process(b"\x1b[?u").replies, b"\x1b[?7u");
    screen.process(b"\x1b[=4;u");
    assert_eq!(screen.process(b"\x1b[?u").replies, b"\x1b[?4u");
    screen.process(b"\x1b[=5;0u\x1b[?9001;1;2004l");
    assert_eq!(screen.replay_preamble(), "");
    screen.process(b"\x1b[>;1u");
    assert_eq!(screen.process(b"\x1b[?u").replies, b"\x1b[?1u");
    assert_eq!(screen.restore("live", 0), b"\x1b[=1;1u");
}

#[test]
fn capture_uses_inclusive_physical_ranges_and_js_fractional_bounds() {
    let mut screen = Screen::new(4, 10);
    screen.process(b"\x1b[31mzero\x1b[0m\r\none\r\ntwo\r\nthree");
    assert_eq!(
        screen.capture(None, None, true),
        (
            vec!["zero".into(), "one".into(), "two".into(), "three".into()],
            4
        )
    );
    assert_eq!(
        screen.capture(Some(-2.0), None, false),
        (vec!["two".into(), "three".into()], 4)
    );
    assert_eq!(screen.capture(Some(-0.5), None, true), (vec![], 4));
    assert_eq!(screen.capture(None, Some(-0.5), true).0.len(), 4);
    assert_eq!(screen.capture(Some(1.9), Some(1.1), true), (vec![], 4));
    assert_eq!(
        screen.capture(Some(1.1), Some(1.9), true),
        (vec!["one".into()], 4)
    );
    assert_eq!(
        screen.capture(Some(-999.0), Some(0.0), true),
        (vec!["zero".into()], 4)
    );
    assert_eq!(
        screen.capture(Some(999.0), None, true),
        (vec!["three".into()], 4)
    );
}

#[test]
fn history_is_bounded_and_wrap_flags_cross_the_viewport_boundary() {
    let mut screen = Screen::new(2, 4);
    screen.process(b"abcdefghijkl");
    let state = screen.snapshot(true);
    assert_eq!(state["scrollbackWrapped"], json!([true]));
    assert_eq!(state["gridWrapped"], json!([true, false]));
    assert_eq!(lines(&screen), ["abcd", "efgh", "ijkl"]);
    assert!(screen.snapshot(false).get("gridWrapped").is_none());
    assert!(screen.snapshot(false).get("scrollbackWrapped").is_none());

    let mut screen = Screen::new(3, 20);
    for line in 0..1005 {
        screen.process(format!("{line}\r\n").as_bytes());
    }
    assert_eq!(screen.capture(None, None, true).1, HISTORY_LINES + 3);
    assert_eq!(screen.capture(Some(0.0), Some(0.0), true).0, ["3"]);
    assert_eq!(screen.capture(Some(-2.0), None, true).0, ["1004", ""]);
}

#[test]
fn alternate_screen_is_isolated_from_primary_content_and_history() {
    let mut screen = Screen::new(2, 12);
    screen.process(b"old\r\nprimary\r\nlast");
    let primary = screen.snapshot(true);
    screen.process(b"\x1b[?1049h\x1b[H\x1b[2Jalternate");
    assert_eq!(lines(&screen), ["alternate", ""]);
    assert_eq!(screen.snapshot(true)["scrollback"], json!([]));
    screen.process(b"\x1b[?1049l");
    assert_eq!(screen.snapshot(true), primary);
}

#[test]
fn cursor_presentation_and_title_can_change_and_reset() {
    let mut screen = Screen::new(2, 10);
    assert!(screen.snapshot(false)["cursor"].get("style").is_none());
    let effects = screen.process(b"\x1b]2;Build\x07\x1b[6 q\x1b[?25l");
    assert_eq!(effects.title.as_deref(), Some("Build"));
    assert_eq!(
        screen.snapshot(false)["cursor"],
        json!({"row":0,"col":0,"hidden":true,"style":"bar","blink":false})
    );
    screen.process(b"\x1b[?12h");
    assert_eq!(screen.snapshot(false)["cursor"]["blink"], true);
    assert_eq!(screen.process(b"\x1b]2;\x07").title.as_deref(), Some(""));
    assert!(screen.snapshot(false).get("title").is_none());
    screen.process(b"\x1bc");
    assert_eq!(screen.snapshot(false)["cursor"], json!({"row":0,"col":0}));
}

#[test]
fn editing_and_scroll_regions_operate_on_cells_not_append_only_lines() {
    let mut screen = Screen::new(2, 10);
    screen.process(b"abcdef\x1b[1;3H\x1b[2P\x1b[2@");
    assert_eq!(lines(&screen), ["ab  ef", ""]);
    screen.process(b"\x1b[1;5H\x1b[K");
    assert_eq!(lines(&screen), ["ab", ""]);

    let mut screen = Screen::new(4, 8);
    screen.process(b"a\r\nb\r\nc\r\nd\x1b[2;3r\x1b[3;1H\n");
    assert_eq!(lines(&screen), ["a", "c", "", "d"]);
    assert_eq!(screen.snapshot(true)["scrollback"], json!([]));
}

#[test]
fn restore_roundtrips_styles_wide_characters_wraps_and_cursor() {
    let mut source = Screen::new(3, 8);
    source.process("\x1b[2;3;9;38;5;42mabcdefghij\x1b[0m\r\n好e\u{301}\r\n\x1b[44m  \x1b[0m\x1b[4 q\x1b[?25l\x1b[?1;2004h".as_bytes());
    let mut restored = Screen::new(3, 8);
    restored.process(&source.restore("full-snapshot", 0));
    assert_eq!(restored.snapshot(true), source.snapshot(true));
    assert_eq!(restored.replay_preamble(), source.replay_preamble());
}

#[test]
fn visible_restore_limits_history_while_full_restore_ignores_limit() {
    let mut source = Screen::new(2, 8);
    source.process(b"a\r\nb\r\nc\r\nd");
    let mut visible = Screen::new(2, 8);
    visible.process(&source.restore("visible-snapshot", 1));
    assert_eq!(lines(&visible), ["b", "c", "d"]);
    let mut full = Screen::new(2, 8);
    full.process(&source.restore("full-snapshot", 0));
    assert_eq!(lines(&full), ["a", "b", "c", "d"]);
}

#[test]
fn resize_reflows_logical_lines_without_losing_wide_or_combining_characters() {
    let mut screen = Screen::new(3, 6);
    screen.process("ab好e\u{301}fg\r\nnext".as_bytes());
    screen.resize(3, 12);
    let state = screen.snapshot(true);
    assert_eq!(state["cols"], 12);
    assert_eq!(state["rows"], 3);
    let captured = lines(&screen);
    assert!(captured.iter().any(|line| line == "ab好e\u{301}fg"));
    assert!(captured.iter().any(|line| line == "next"));
    screen.resize(4, 4);
    assert_eq!(screen.snapshot(true)["grid"].as_array().unwrap().len(), 4);
    let text = lines(&screen).concat();
    assert!(text.contains("ab好e\u{301}fg"));
    assert!(text.contains("next"));
}
