import 'package:flutter/cupertino.dart' show CupertinoLocalizations;
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_localizations/flutter_localizations.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:paseo_sessions/features/chat/chrome/composer_tracks.dart';
import 'package:paseo_sessions/ui/localization.dart';
import 'package:paseo_sessions/ui/theme.dart';

class _Loaded<T> extends LocalizationsDelegate<T> {
  const _Loaded(this.value);
  final T value;
  @override
  bool isSupported(Locale locale) => true;
  @override
  Future<T> load(Locale locale) => SynchronousFuture(value);
  @override
  bool shouldReload(_Loaded<T> old) => false;
}

SubagentRowData _row(String id) => SubagentRowData(
  kind: 'provider',
  id: id,
  serverId: 'srv',
  provider: 'claude',
  status: 'completed',
  createdAt: DateTime(2026),
  description: 'Task $id',
);

void main() {
  testWidgets('the open subagent sheet follows archive-finished and closes with an emptied track', (tester) async {
    tester.view.physicalSize = const Size(424, 924);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    late PaseoLocalizations strings;
    late MaterialLocalizations material;
    late CupertinoLocalizations cupertino;
    await tester.runAsync(() async {
      strings = await PaseoLocalizations.load(const Locale('en'));
      material = await GlobalMaterialLocalizations.delegate.load(const Locale('en'));
      cupertino = await GlobalCupertinoLocalizations.delegate.load(const Locale('en'));
    });
    var rows = [_row('a'), _row('b'), _row('c')];
    var status = const ArchiveFinishedStatus.idle();
    late StateSetter update;
    await tester.pumpWidget(MaterialApp(
      locale: const Locale('en'),
      supportedLocales: supportedPaseoLocales,
      localizationsDelegates: [
        _Loaded<PaseoLocalizations>(strings),
        _Loaded<MaterialLocalizations>(material),
        GlobalWidgetsLocalizations.delegate,
        _Loaded<CupertinoLocalizations>(cupertino),
      ],
      theme: paseoTheme(brightness: Brightness.dark, preferences: const {}),
      home: Scaffold(
        body: StatefulBuilder(builder: (context, setState) {
          update = setState;
          return Align(
            alignment: Alignment.bottomCenter,
            child: AgentTracks(
              serverId: 'srv',
              tasks: const [],
              rows: rows,
              archiveStatus: status,
              // Like the panel: progress first, then each archived row leaves.
              onArchiveFinished: () => update(() => status = ArchiveFinishedStatus.archiving(0, rows.length)),
              onOpenRow: (_) {},
              onArchiveRow: (_) {},
              onDetachRow: null,
              additions: 0,
              deletions: 0,
              onOpenChanges: () {},
              factsFor: (_) => null,
              hostLabelFor: (id) => id,
              providerSvgFor: (_) => null,
              opensInSheet: (_) => false,
            ),
          );
        }),
      ),
    ));
    await tester.pumpAndSettle();
    await tester.tap(find.text(strings.text('subagents.pillLabelMany', {'count': 3})));
    await tester.pumpAndSettle();
    expect(find.text('Task a'), findsOneWidget);

    await tester.tap(find.text(strings.text('subagents.archiveFinishedAction', const {})));
    await tester.pumpAndSettle();
    expect(find.text('0/3'), findsOneWidget);

    update(() {
      rows = rows.sublist(1);
      status = const ArchiveFinishedStatus.archiving(1, 3);
    });
    await tester.pumpAndSettle();
    expect(find.text('Task a'), findsNothing);
    expect(find.text('1/3'), findsOneWidget);

    // Provider rows are dismissed in one synchronous pass: the track empties
    // and its pill leaves while the sheet is still open.
    update(() {
      rows = [];
      status = const ArchiveFinishedStatus.idle();
    });
    await tester.pumpAndSettle();
    expect(find.text(strings.text('subagents.archiveFinishedAction', const {})), findsNothing);
    expect(find.text('Task b'), findsNothing);
  }, variant: TargetPlatformVariant.only(TargetPlatform.android));
}
