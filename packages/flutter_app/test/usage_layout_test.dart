import 'package:flutter/cupertino.dart' show CupertinoLocalizations;
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter_localizations/flutter_localizations.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:paseo_sessions/features/settings/settings_forms.dart';
import 'package:paseo_sessions/features/settings/usage_settings.dart';
import 'package:paseo_sessions/ui/localization.dart';
import 'package:paseo_sessions/ui/theme.dart';

const _icon = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 14 14">'
    '<circle cx="7" cy="7" r="5" fill="currentColor"/></svg>';

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

Future<void> _pump(WidgetTester tester, Widget child, {double width = 424}) async {
  tester.view.physicalSize = Size(width, 924);
  tester.view.devicePixelRatio = 1;
  late PaseoLocalizations strings;
  late MaterialLocalizations material;
  late CupertinoLocalizations cupertino;
  await tester.runAsync(() async {
    strings = await PaseoLocalizations.load(const Locale('ko'));
    material = await GlobalMaterialLocalizations.delegate.load(const Locale('ko'));
    cupertino = await GlobalCupertinoLocalizations.delegate.load(const Locale('ko'));
  });
  await tester.pumpWidget(MaterialApp(
    locale: const Locale('ko'),
    supportedLocales: supportedPaseoLocales,
    localizationsDelegates: [
      _Loaded<PaseoLocalizations>(strings),
      _Loaded<MaterialLocalizations>(material),
      GlobalWidgetsLocalizations.delegate,
      _Loaded<CupertinoLocalizations>(cupertino),
    ],
    theme: paseoTheme(brightness: Brightness.dark, preferences: const {}),
    home: Scaffold(
      body: SingleChildScrollView(
        child: Padding(padding: const EdgeInsets.all(16), child: child),
      ),
    ),
  ));
  await tester.pumpAndSettle();
}

void main() {

  testWidgets('section actions stay on the right rail and info stays beside its title', (tester) async {
    addTearDown(tester.view.reset);
    const sectionKey = ValueKey('section');
    const actionsKey = ValueKey('actions');
    for (final width in [360.0, 424.0, 900.0]) {
      await _pump(tester, SettingsSection(
        key: sectionKey,
        title: '플랜 사용량',
        info: '사용량 정보',
        card: false,
        trailing: const SizedBox(key: actionsKey, width: 72, height: 32),
        children: const [Text('reports')],
      ), width: width);
      final section = tester.getRect(find.byKey(sectionKey));
      final actions = tester.getRect(find.byKey(actionsKey));
      expect(actions.right, closeTo(section.right, .01), reason: 'actions must not drift toward the title');
      final title = tester.getRect(find.text('플랜 사용량'));
      final info = tester.getRect(find.byIcon(LucideIcons.info300));
      expect(info.left - title.right, closeTo(8, .01));
      expect(tester.takeException(), isNull);
    }
  }, variant: TargetPlatformVariant.only(TargetPlatform.android));

  testWidgets('usage names use available width and refresh stays at the card edge', (tester) async {
    addTearDown(tester.view.reset);
    const cardKey = ValueKey('usage-card');
    for (final status in ['available', 'error', 'unavailable']) {
      for (final width in [424.0, 900.0]) {
        await _pump(tester, UsageCard(
          key: cardKey,
          title: 'Kiro',
          icon: _icon,
          planLabel: 'Kiro Pro+',
          status: status,
          windows: const [],
          balances: const [],
          details: const [],
          messages: const [],
          retained: false,
          displayRemaining: false,
          onRefresh: () {},
        ), width: width);
        final title = tester.renderObject<RenderParagraph>(find.descendant(
          of: find.text('Kiro'),
          matching: find.byType(RichText),
        ));
        expect(title.didExceedMaxLines, isFalse, reason: 'the name fits beside the plan and status at $width');
        final card = tester.getRect(find.byKey(cardKey));
        final refresh = tester.getRect(find.byIcon(LucideIcons.rotateCw300));
        expect(refresh.right, closeTo(card.right - 16, .01), reason: 'the refresh glyph belongs on the content rail');
        expect(tester.takeException(), isNull);
      }
    }
  }, variant: TargetPlatformVariant.only(TargetPlatform.android));

  testWidgets('usage values reach the right rail without a half-width text cap', (tester) async {
    addTearDown(tester.view.reset);
    const cardKey = ValueKey('usage-card');
    for (final width in [424.0, 900.0]) {
      await _pump(tester, UsageCard(
        key: cardKey,
        title: 'Claude',
        status: 'available',
        windows: [
          {
            'id': 'session',
            'label': 'Session',
            'usedPct': 94,
            'resetsAt': DateTime.now().add(const Duration(hours: 2, minutes: 5)).toIso8601String(),
          },
        ],
        balances: [
          {
            'id': 'credits',
            'label': 'Credits',
            'remaining': 0,
            'unit': 'credits',
            'resetsAt': DateTime.now().add(const Duration(days: 4, hours: 1)).toIso8601String(),
          },
        ],
        details: const [],
        messages: const [],
        retained: false,
        displayRemaining: false,
      ), width: width);
      final card = tester.getRect(find.byKey(cardKey));
      for (final prefix in ['94%', '0 credits']) {
        final value = find.byWidgetPredicate(
          (widget) => widget is RichText && widget.text.toPlainText().startsWith(prefix),
        );
        expect(tester.renderObject<RenderParagraph>(value).didExceedMaxLines, isFalse);
        expect(tester.getRect(value).right, closeTo(card.right - 16, .01));
      }
      final amount = tester.getRect(find.text('0 credits 남음'));
      final reset = tester.getRect(find.text('4일 후 재설정'));
      expect(reset.top, greaterThanOrEqualTo(amount.bottom));
      expect(reset.right, closeTo(amount.right, .01));
      expect(tester.takeException(), isNull);
    }
  }, variant: TargetPlatformVariant.only(TargetPlatform.android));
}
