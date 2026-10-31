import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:paseo_sessions/core/app_store.dart';
import 'package:paseo_sessions/core/client.dart';
import 'package:paseo_sessions/core/host_config.dart';
import 'package:paseo_sessions/core/scope.dart';
import 'package:paseo_sessions/features/navigation/session_sidebar.dart';
import 'package:paseo_sessions/ui/theme.dart';

void main() {
  testWidgets('host and directory toggles do not wait for storage', (tester) async {
    final store = _SlowPreferencesStore();
    final config = HostConfig(
      id: 'host',
      label: 'Offline host',
      endpoint: 'ws://127.0.0.1:1/ws',
    );
    store.hosts.add(
      HostState(config: config, client: PaseoClient(host: config))
        ..projects = [
          {
            'projectId': 'project',
            'projectDisplayName': 'Project',
            'projectRootPath': '/project',
          },
        ]
        ..workspaces = [
          {'id': 'workspace', 'projectId': 'project'},
        ]
        ..agents = [
          {
            'id': 'agent',
            'workspaceId': 'workspace',
            'title': 'Saved session',
            'provider': 'codex',
            'status': 'idle',
          },
        ],
    );
    final host = find.byKey(const ValueKey('host:host'));
    final directory = find.byKey(const ValueKey('project:host:project'));
    final session = find.byKey(const ValueKey('session:host:agent'));
    try {
      await tester.pumpWidget(
        PaseoScope(
          store: store,
          child: MaterialApp(
            theme: paseoTheme(brightness: Brightness.dark, preferences: {}),
            home: const Scaffold(
              body: SizedBox(width: 320, child: SessionSidebar()),
            ),
          ),
        ),
      );
      expect(session, findsOneWidget);
      await tester.tap(host);
      await tester.pump();
      expect(directory, findsNothing);
      await tester.tap(host);
      await tester.pump();
      expect(directory, findsOneWidget);
      expect(session, findsOneWidget);
      await tester.tap(directory);
      await tester.pump();
      expect(session, findsNothing);
      await tester.tap(directory);
      await tester.pump();
      expect(session, findsOneWidget);
    } finally {
      store.saved.complete();
      await tester.pumpWidget(const SizedBox.shrink());
      await store.disposeAsync();
    }
  });
}

class _SlowPreferencesStore extends AppStore {
  final saved = Completer<void>();

  @override
  Future<void> savePreferences() => saved.future;
}
