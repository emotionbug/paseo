import 'package:flutter/material.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:highlight/highlight_core.dart';
import 'package:highlight/languages/dart.dart';
import 'package:highlight/languages/javascript.dart';
import 'package:highlight/languages/python.dart';
import 'package:highlight/languages/rust.dart';
import 'package:highlight/languages/bash.dart';
import 'package:highlight/languages/json.dart';

class ChatCodeHighlighter extends SyntaxHighlighter {
  ChatCodeHighlighter(this.dark, {this.theme = 'one'});
  final bool dark;
  final String theme;
  List<int> get palette => switch ((theme, dark)) {
    ('github', true) => const [
      0xff8b949e,
      0xffa5d6ff,
      0xffff7b72,
      0xff79c0ff,
      0xffd2a8ff,
    ],
    ('github', false) => const [
      0xff6e7781,
      0xff0a3069,
      0xffcf222e,
      0xff0550ae,
      0xff8250df,
    ],
    ('catppuccin', true) => const [
      0xff9399b2,
      0xffa6e3a1,
      0xffcba6f7,
      0xfffab387,
      0xff89b4fa,
    ],
    ('catppuccin', false) => const [
      0xff6c6f85,
      0xff40a02b,
      0xff8839ef,
      0xfffe640b,
      0xff1e66f5,
    ],
    ('dracula', true) => const [
      0xff8292c0,
      0xfff1fa8c,
      0xffff79c6,
      0xffbd93f9,
      0xff50fa7b,
    ],
    ('dracula', false) => const [
      0xff6272a4,
      0xff746500,
      0xffa51862,
      0xff6e36ad,
      0xff167b35,
    ],
    ('tokyo-night', true) => const [
      0xff8b92b3,
      0xff9ece6a,
      0xffbb9af7,
      0xffff9e64,
      0xff7aa2f7,
    ],
    ('tokyo-night', false) => const [
      0xff6c6e75,
      0xff485e30,
      0xff7847bd,
      0xff965027,
      0xff34548a,
    ],
    ('nord', true) => const [
      0xff9099ab,
      0xffa3be8c,
      0xff81a1c1,
      0xffb48ead,
      0xff88c0d0,
    ],
    ('nord', false) => const [
      0xff667080,
      0xff416534,
      0xff365e84,
      0xff7b4773,
      0xff286578,
    ],
    ('gruvbox', true) => const [
      0xffa89984,
      0xffb8bb26,
      0xfffb4934,
      0xffd3869b,
      0xff8ec07c,
    ],
    ('gruvbox', false) => const [
      0xff7c6f64,
      0xff79740e,
      0xff9d0006,
      0xff8f3f71,
      0xff427b58,
    ],
    ('solarized', true) => const [
      0xff93a1a1,
      0xff859900,
      0xff268bd2,
      0xffd33682,
      0xff2aa198,
    ],
    ('solarized', false) => const [
      0xff657b83,
      0xff718400,
      0xff006da8,
      0xffb31a63,
      0xff157b74,
    ],
    (_, true) => const [
      0xff9aa89c,
      0xff9bd5a2,
      0xffc4a7ef,
      0xffedc28a,
      0xff8cc8ed,
    ],
    (_, false) => const [
      0xff58645c,
      0xff246329,
      0xff7543a8,
      0xff92550e,
      0xff175f8a,
    ],
  };
  static final _engine = Highlight()
    ..registerLanguages({
      'dart': dart,
      'javascript': javascript,
      'python': python,
      'rust': rust,
      'bash': bash,
      'json': json,
    });
  static final _cache = <String, List<Node>>{};
  @override
  TextSpan format(String source) {
    if (source.length > 32768) {
      return TextSpan(
        text: source,
        style: const TextStyle(fontFamily: 'monospace'),
      );
    }
    var nodes = _cache[source];
    if (nodes == null) {
      nodes =
          _engine.parse(source, autoDetection: true).nodes ??
          [Node(value: source)];
      // Bound retained source bytes; long tool output remains selectable plain text.
      if (source.length <= 32768) {
        if (_cache.length >= 48) _cache.remove(_cache.keys.first);
        _cache[source] = nodes;
      }
    }
    return TextSpan(
      style: const TextStyle(fontFamily: 'monospace'),
      children: _spans(nodes),
    );
  }

  List<TextSpan> _spans(List<Node> nodes) => nodes.map((node) {
    final name = node.className ?? '';
    Color? color;
    final colors = palette;
    if (name.contains('comment')) {
      color = Color(colors[0]);
    } else if (name.contains('string') || name.contains('regexp')) {
      color = Color(colors[1]);
    } else if (name.contains('keyword') || name.contains('literal')) {
      color = Color(colors[2]);
    } else if (name.contains('number')) {
      color = Color(colors[3]);
    } else if (name.contains('title') || name.contains('built_in')) {
      color = Color(colors[4]);
    }
    return TextSpan(
      text: node.value,
      style: color == null ? null : TextStyle(color: color),
      children: node.children == null ? null : _spans(node.children!),
    );
  }).toList();
}
