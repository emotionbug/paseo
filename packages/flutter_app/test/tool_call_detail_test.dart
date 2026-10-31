import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:paseo_sessions/core/app_store.dart';
import 'package:paseo_sessions/core/json.dart';
import 'package:paseo_sessions/core/scope.dart';
import 'package:paseo_sessions/core/target.dart';
import 'package:paseo_sessions/features/chat/timeline_item.dart';
import 'package:paseo_sessions/features/chat/tool_call_details.dart';
import 'package:paseo_sessions/ui/theme.dart';

const _target = PanelTarget(
  kind: 'agent',
  serverId: 'host',
  workspaceId: 'workspace',
  cwd: '/home/user/project',
  id: 'agent',
  title: 'Agent',
);

JsonMap _tool(String callId, String name, JsonMap detail, {String status = 'completed', Object? error}) => {
  'seqStart': callId.hashCode,
  'turnId': 'turn',
  'timestamp': '2026-10-30T07:35:00.000Z',
  'item': {
    'type': 'tool_call',
    'callId': callId,
    'name': name,
    'status': status,
    'error': error,
    'detail': detail,
  },
};

final _payloads = <JsonMap>[
  _tool('search', 'grep', {
    'type': 'search',
    'query': '오류|error',
    'toolName': 'grep',
    'content': 'lib/a.dart:12: error\nlib/b.dart:4: 오류',
    'filePaths': ['lib/a.dart', 'lib/b.dart'],
    'numFiles': 2,
    'numMatches': 2,
    'mode': 'content',
  }),
  _tool('web', 'web_search', {
    'type': 'search',
    'query': 'flutter',
    'toolName': 'web_search',
    'webResults': [
      {'title': 'Flutter', 'url': 'https://flutter.dev'},
    ],
  }),
  _tool('read', 'read', {
    'type': 'read',
    'filePath': '/home/user/project/lib/main.dart',
    'content': 'void main() {\n  runApp(const App());\n}\n',
    'offset': 1,
    'limit': 40,
  }),
  _tool('shell', 'shell', {
    'type': 'shell',
    'command': 'ls -la',
    'cwd': '/home/user/project',
    'output': 'total 0\ndrwxr-xr-x  2 user user 40 Oct 30 07:35 .',
    'exitCode': 0,
  }),
  _tool('shell-failed', 'shell', {
    'type': 'shell',
    'command': 'false',
    'output': '',
    'exitCode': 1,
  }, status: 'failed', error: {'message': 'exit 1'}),
  _tool('edit', 'edit', {
    'type': 'edit',
    'filePath': '/home/user/project/lib/main.dart',
    'unifiedDiff': '@@ -1 +1 @@\n-a\n+b\n',
  }),
  _tool('write', 'write', {
    'type': 'write',
    'filePath': '/home/user/project/notes.txt',
    'content': 'hello',
  }),
  _tool('fetch', 'fetch', {
    'type': 'fetch',
    'url': 'https://example.com',
    'result': '# Example\n\nBody with `code`.',
    'code': 200,
    'codeText': 'OK',
  }),
  _tool('task', 'task', {
    'type': 'sub_agent',
    'subAgentType': 'Explore',
    'description': 'Find files',
    'log': 'done',
    'actions': [],
  }),
  _tool('plain', 'notes', {'type': 'plain_text', 'label': 'Note', 'text': 'Hello', 'icon': 'sparkles'}),
  _tool('unknown', 'mcp__x__y', {
    'type': 'unknown',
    'input': {'a': 1},
    'output': [
      {'type': 'text', 'text': 'ok'},
    ],
  }),
  _tool('worktree', 'worktree_setup', {
    'type': 'worktree_setup',
    'worktreePath': '/tmp/wt',
    'branchName': 'feature',
    'log': 'ok',
    'commands': [
      {'index': 1, 'command': 'npm i', 'cwd': '/tmp/wt', 'log': 'ok', 'status': 'completed', 'exitCode': 0},
    ],
  }),
];

Future<void> _pump(WidgetTester tester, AppStore store, List<JsonMap> entries, {bool expanded = false}) =>
    tester.pumpWidget(
      PaseoScope(
        store: store,
        child: MaterialApp(
          theme: paseoTheme(brightness: Brightness.dark, preferences: {}),
          home: Scaffold(
            body: SingleChildScrollView(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  for (final entry in entries)
                    TimelineItem(entry: entry, target: _target, expandedTools: expanded),
                ],
              ),
            ),
          ),
        ),
      ),
    );

Future<List<FlutterErrorDetails>> _expand(
  WidgetTester tester,
  JsonMap payload, {
  required Size size,
  String? expectText,
  void Function(Size detailSize)? measure,
}) async {
  tester.view.physicalSize = size;
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  final store = AppStore();
  final errors = <FlutterErrorDetails>[];
  final previous = FlutterError.onError;
  FlutterError.onError = errors.add;
  try {
    await _pump(tester, store, [payload]);
    await tester.tap(find.byType(ToolBadge).first);
    await tester.pumpAndSettle();
    expect(find.byType(ErrorWidget), findsNothing);
    if (jsonMap(jsonMap(payload['item'])['detail'])['type'] != 'worktree_setup') {
      expect(find.byType(ToolCallDetailsContent), findsOneWidget);
    }
    if (expectText != null) expect(find.text(expectText), findsOneWidget);
    if (measure != null) measure(tester.getSize(find.byType(ToolCallDetailsContent)));
    return errors;
  } finally {
    FlutterError.onError = previous;
    await tester.pumpWidget(const SizedBox.shrink());
    await store.disposeAsync();
  }
}

void main() {
  testWidgets('tool rows start collapsed, including failed calls', (tester) async {
    final store = AppStore();
    try {
      await _pump(tester, store, _payloads);
      expect(tester.takeException(), isNull);
      expect(find.byType(ToolBadge), findsNWidgets(_payloads.length));
      expect(find.byType(ToolCallDetailsContent), findsNothing);
    } finally {
      await tester.pumpWidget(const SizedBox.shrink());
      await store.disposeAsync();
    }
  });

  testWidgets('expanded search detail shows its output instead of an error box', (tester) async {
    final errors = await _expand(
      tester,
      _payloads.first,
      size: const Size(1200, 900),
      expectText: 'lib/a.dart:12: error\nlib/b.dart:4: 오류',
    );
    expect(errors.map((e) => e.exceptionAsString()).toList(), isEmpty);
  });

  for (final payload in _payloads) {
    final item = jsonMap(payload['item']);
    testWidgets('inline ${item['callId']} tool detail builds without errors', (tester) async {
      final errors = await _expand(tester, payload, size: const Size(1200, 900));
      expect(errors.map((e) => e.exceptionAsString()).toList(), isEmpty);
    });
    testWidgets('compact ${item['callId']} tool sheet builds without errors', (tester) async {
      final errors = await _expand(tester, payload, size: const Size(412, 915));
      expect(errors.map((e) => e.exceptionAsString()).toList(), isEmpty);
    });
  }

  testWidgets('expanded long read output stays bounded inline', (tester) async {
    final long = _tool('long-read', 'read', {
      'type': 'read',
      'filePath': '/home/user/project/lib/long.dart',
      'content': List.generate(600, (i) => 'line $i ${'x' * 80}').join('\n'),
    });
    final errors = await _expand(tester, long, size: const Size(1200, 900), measure: (size) {
      expect(size.height.isFinite, isTrue);
      expect(size.height, lessThanOrEqualTo(480));
    });
    expect(errors.map((e) => e.exceptionAsString()).toList(), isEmpty);
  });

  testWidgets('user message images render thumbnails and open safely', (tester) async {
    tester.view.physicalSize = const Size(412, 915);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    const png =
        'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==';
    final store = AppStore();
    final semantics = tester.ensureSemantics();
    final errors = <FlutterErrorDetails>[];
    final previous = FlutterError.onError;
    FlutterError.onError = errors.add;
    try {
      await _pump(tester, store, [
        {
          'seqStart': 1,
          'timestamp': '2026-10-30T07:35:00.000Z',
          'item': {
            'type': 'user_message',
            'text': 'look',
            'images': [
              {'data': png, 'mimeType': 'image/png'},
              {'data': 'data:image/png;base64,$png', 'mimeType': 'image/png'},
              {'data': 'not base64 !!', 'mimeType': 'image/png'},
              {'data': base64Encode(utf8.encode('not an image')), 'mimeType': 'image/png'},
            ],
          },
        },
      ]);
      await tester.pumpAndSettle();
      expect(find.byType(ErrorWidget), findsNothing);
      for (var i = 0; i < 4; i++) {
        await tester.tap(find.byTooltip('Image').at(i));
        await tester.pumpAndSettle();
        expect(find.byType(ErrorWidget), findsNothing);
        expect(find.byType(InteractiveViewer), findsOneWidget);
        await tester.tap(find.byIcon(LucideIcons.x300).last);
        await tester.pumpAndSettle();
      }
      expect(errors.map((e) => e.exceptionAsString()).toList(), isEmpty);
    } finally {
      FlutterError.onError = previous;
      semantics.dispose();
      await tester.pumpWidget(const SizedBox.shrink());
      await store.disposeAsync();
    }
  });
}
