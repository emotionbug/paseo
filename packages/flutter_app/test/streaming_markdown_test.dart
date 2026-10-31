import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:paseo_sessions/core/app_store.dart';
import 'package:paseo_sessions/core/scope.dart';
import 'package:paseo_sessions/features/chat/chat_markdown.dart';
import 'package:paseo_sessions/ui/theme.dart';

/// Answers as they stream: every revealed slice renders block-split exactly
/// as the whole document does, so finishing the message never moves a line.
const _documents = [
  '''
## Findings

The **hold** now runs during layout. Key points:

- `_applyReadingHold` measured stale geometry
- the second call doubled the jump

1. first
2. second

3. third after a blank line

Closing paragraph after the list.

```dart
final shift = 1;

if (shift < 2) return;
```

| File | Change |
|---|---|
| `a.dart` | hook |

> [!NOTE]
> An alert.

> A quote.

---

Last line with `code` and a [link](https://example.com).
''',
  '''
- loose item one

- loose item two

  indented continuation

Paragraph.

    indented code

    more code

Done.
''',
  '''
Intro paragraph

~~~
fenced with tildes

still code
~~~

* star list
* second

Tail text
''',
];

Future<double> _height(WidgetTester tester, AppStore store, Widget child) async {
  await tester.pumpWidget(
    PaseoScope(
      store: store,
      child: MaterialApp(
        theme: paseoTheme(brightness: Brightness.dark, preferences: const {}),
        home: Scaffold(
          body: SingleChildScrollView(
            child: SizedBox(width: 380, child: KeyedSubtree(key: const ValueKey('md'), child: child)),
          ),
        ),
      ),
    ),
  );
  return tester.getSize(find.byKey(const ValueKey('md'))).height;
}

void main() {
  test('splits only before an unindented non-list block outside fences', () {
    expect(markdownStableSplit('One\n\nTwo'), 5);
    expect(markdownStableSplit('- a\n\n- b'), isNull, reason: 'a list never splits');
    expect(markdownStableSplit('```\na\n\nb\n```'), isNull, reason: 'inside a fence');
    expect(markdownStableSplit('- a\n\n  more'), isNull, reason: 'indented continuation');
    expect(markdownStableSplit('A\n\n<details>\n\nB'), isNull, reason: 'HTML spans blocks');
    expect(markdownStableSplit('A [x]\n\n[x]: https://e.com\n\nB'), isNull);
    for (final document in _documents) {
      expect(markdownStableSplit(document), isNotNull);
    }
  });

  for (final platform in [TargetPlatform.android, TargetPlatform.linux]) {
    testWidgets('a split streaming slice lays out like the whole text ($platform)', (tester) async {
      debugDefaultTargetPlatformOverride = platform;
      final store = AppStore(connectHosts: false, discoverLocalHosts: false);
      try {
        for (final document in _documents) {
          final cuts = <int>{document.length};
          for (var i = 0; i < document.length; i++) {
            if (document[i] == '\n') cuts.addAll([i, i + 1]);
          }
          for (final cut in cuts) {
            final slice = document.substring(0, cut);
            if (markdownStableSplit(slice) == null) continue;
            final whole = await _height(tester, store, PaseoMarkdown(data: slice));
            final split = await _height(
              tester,
              store,
              StreamingMarkdown(
                data: slice,
                streaming: true,
                markdown: (part) => PaseoMarkdown(data: part),
              ),
            );
            expect(split, whole, reason: 'slice ending at $cut:\n$slice');
          }
        }
      } finally {
        debugDefaultTargetPlatformOverride = null;
        await tester.pumpWidget(const SizedBox.shrink());
        await store.disposeAsync();
      }
    });
  }
}
