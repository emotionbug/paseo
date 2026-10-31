import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:paseo_sessions/features/chat/chat_markdown.dart';
import 'package:paseo_sessions/features/chat/markdown_html.dart';
import 'package:paseo_sessions/ui/theme.dart';

void main() {
  // Original components/markdown/renderer.test.ts
  // "GitHub comment HTML normalization".
  const cases = <(String, String)>[
    (
      '<img alt="P1" src="https://example.com/p1.svg" align="top">',
      '![P1](<https://example.com/p1.svg>)',
    ),
    ('<a href="https://example.com/review">▶</a>', '[▶](<https://example.com/review>)'),
    ('Before<br>After<br/>End', 'Before  \nAfter  \nEnd'),
    ('<b>bold</b> and <strong>strong</strong>', '**bold** and **strong**'),
    ('<i>italic</i> and <em>emphasis</em>', '*italic* and *emphasis*'),
    ('Use <code>const n = 1</code>.', 'Use `const n = 1`.'),
    ('H<sub>2</sub>O and x<sup>2</sup>', 'H2O and x2'),
    (
      '<details><summary>Review details</summary>Body **stays**.</details>',
      '**Review details**\n\nBody **stays**.',
    ),
    ('Before<!-- private -->After', 'BeforeAfter'),
    (
      '<details><summary><h3>Important Files Changed</h3></summary>Body</details>',
      '**Important Files Changed**\n\nBody',
    ),
    (
      '<details><summary><p>First</p><p>Second</p></summary>Body</details>',
      '**First Second**\n\nBody',
    ),
    ('<span>file.</span>ts', 'file.ts'),
    ('<a href="https://example.com">Git</a>Hub', '[Git](<https://example.com>)Hub'),
    ('<span>Git</span><span>Hub</span>', 'GitHub'),
    ('<span>Git</span> Hub', 'Git Hub'),
    ('<div><span>Keep the content</span></div>', 'Keep the content'),
    (
      '<a href="https://example.com"><strong>Review</strong></a>',
      '[**Review**](<https://example.com>)',
    ),
    ('`<img src="literal.png">` and <b>bold</b>', '`<img src="literal.png">` and **bold**'),
    (
      '```html\n<img src="literal.png">\n```\n\n<b>bold</b>',
      '```html\n<img src="literal.png">\n```\n\n**bold**',
    ),
    ('~~~html\n<a href="literal">link</a>\n~~~', '~~~html\n<a href="literal">link</a>\n~~~'),
    (
      '<details><summary>Code</summary>\n\n```html\n<img src="literal.png">\n```\n</details>',
      '**Code**\n\n\n\n```html\n<img src="literal.png">\n```\n',
    ),
    (
      '<https://example.com> and <user@example.com>',
      '<https://example.com> and <user@example.com>',
    ),
    (
      '<div>First</div><p>Second<br>line</p><h3>Heading</h3><table><tr><td>A</td><td>B</td></tr><tr><td>C</td><td>D</td></tr></table>',
      'First\n\nSecond  \nline\n\nHeading\n\nA B\n\nC D',
    ),
    (
      '[open](<my file.png> "<b>literal title</b>")',
      '[open](<my file.png> "<b>literal title</b>")',
    ),
  ];
  for (final (source, expected) in cases) {
    test('normalizes $source without changing code', () {
      expect(normalizeMarkdownHtml(source), expected);
    });
  }

  test('preserves backslashes and decodes entities inside HTML code', () {
    expect(
      normalizeMarkdownHtml(r'<code>\*literal\* &lt;img&gt;</code>'),
      '`' r'\*literal\* <img>' '`',
    );
  });

  testWidgets('details content is rendered and footnotes stay literal', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        theme: paseoTheme(brightness: Brightness.dark, preferences: const {}),
        home: const Scaffold(
          body: PaseoMarkdown(
            data:
                '<details><summary>HTML details</summary>hidden body</details>\n\n'
                'Footnote reference[^1].\n\n[^1]: The footnote text.',
          ),
        ),
      ),
    );
    expect(find.textContaining('HTML details', findRichText: true), findsOneWidget);
    expect(find.textContaining('hidden body', findRichText: true), findsOneWidget);
    expect(find.textContaining('Footnote reference[^1].', findRichText: true), findsOneWidget);
    expect(find.textContaining('[^1]: The footnote text.', findRichText: true), findsOneWidget);
  });

  testWidgets('inline code text paints no background over its chip', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        theme: paseoTheme(brightness: Brightness.dark, preferences: const {}),
        home: const Scaffold(
          body: PaseoMarkdown(data: 'Installed on (`192.168.0.151:33411`) with `adb install -r`.'),
        ),
      ),
    );
    final code = <String, Color?>{};
    for (final text in tester.widgetList<RichText>(find.byType(RichText))) {
      text.text.visitChildren((span) {
        if (span is TextSpan && (span.text == '192.168.0.151:33411' || span.text == 'adb install -r')) {
          code[span.text!] = span.style?.backgroundColor;
        }
        return true;
      });
    }
    expect(code.keys, unorderedEquals(['192.168.0.151:33411', 'adb install -r']));
    // The surface2 chip behind each line is the only background.
    for (final background in code.values) {
      expect(background == null || background.a == 0, isTrue, reason: 'text background $background');
    }
  });

  testWidgets('table headers without a column alignment start like body cells', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        theme: paseoTheme(brightness: Brightness.dark, preferences: const {}),
        home: const Scaffold(
          body: PaseoMarkdown(data: '| Name | Value |\n|---|:---:|\n| a | b |'),
        ),
      ),
    );
    TextAlign alignOf(String text) => tester
        .widget<RichText>(find.textContaining(text, findRichText: true))
        .textAlign;
    expect(alignOf('Name'), isNot(TextAlign.center));
    expect(alignOf('Value'), TextAlign.center, reason: 'an explicit column alignment still applies');
  });
}
