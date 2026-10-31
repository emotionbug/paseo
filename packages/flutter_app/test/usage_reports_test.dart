import 'package:flutter_test/flutter_test.dart';

import 'package:paseo_sessions/core/json.dart';
import 'package:paseo_sessions/features/settings/usage_settings.dart';

JsonMap _entry(String id, String fetchedAt, {String sourceId = 'claude', List<JsonMap> windows = const []}) => {
  'id': id,
  'sourceId': sourceId,
  'sourceLabel': id,
  'account': const <String, dynamic>{},
  'fetchedAt': fetchedAt,
  'report': {'status': 'available', 'windows': windows},
};

void main() {
  group('usage reports (usage/model.ts)', () {
    test('a streamed report never replaces a copy fetched after it', () {
      final newer = _entry('a', '2026-01-01T00:10:00Z');
      final older = _entry('a', '2026-01-01T00:05:00Z');
      expect(upsertUsageReport([newer], older).single, same(newer));
      expect(upsertUsageReport([older], newer).single, same(newer));
      expect(upsertUsageReport(null, older).single, same(older));
    });

    test('a finished list keeps shown order, drops missing and appends new', () {
      final a = _entry('a', '2026-01-01T00:00:00Z');
      final b = _entry('b', '2026-01-01T00:00:00Z');
      final c = _entry('c', '2026-01-01T00:00:00Z');
      final settled = settleUsageReports([b, a], [c, a]);
      expect(settled.map((r) => r['id']), ['a', 'c']);
      expect(replaceUsageReport([a, b], 'a', null).map((r) => r['id']), ['b']);
    });

    test('a refresh failure keeps data; the view only errors without data', () {
      expect(
        resolveUsageView(connected: true, supported: true, data: const [], error: StateError('x')),
        UsageViewKind.ready,
      );
      expect(
        resolveUsageView(connected: true, supported: true, data: null, error: StateError('x')),
        UsageViewKind.error,
      );
      expect(
        resolveUsageView(connected: false, supported: true, data: const [], error: null),
        UsageViewKind.unavailable,
      );
      expect(supportsUsageReports({'providerUsageList': true}), isTrue);
      expect(supportsUsageReports({}), isFalse);
    });
  });

  group('usage pins (usage/preferences.ts)', () {
    final reports = [
      _entry('claude', '2026-01-01T00:00:00Z', windows: [
        {'id': 'five_hour', 'label': 'Session', 'usedPct': 10, 'summary': true},
        {'id': 'weekly', 'label': 'Weekly', 'usedPct': 20},
      ]),
      _entry('codex', '2026-01-01T00:00:00Z', sourceId: 'codex', windows: [
        {'id': 'empty', 'label': 'Empty'},
        {'id': 'weekly', 'label': 'Weekly', 'remainingPct': 70},
      ]),
    ];

    test('defaults are summary windows, else the first window with a percent', () {
      expect(effectiveUsagePins({}, reports), [
        {'sourceId': 'claude', 'windowId': 'five_hour'},
        {'sourceId': 'codex', 'windowId': 'weekly'},
      ]);
      expect(effectiveUsagePins({'pins': []}, reports), isEmpty);
    });

    test('the first toggle snapshots the defaults and keeps other preferences', () {
      final next = toggleUsagePin(
        {'displayAs': 'remaining'},
        {'sourceId': 'claude', 'windowId': 'weekly'},
        reports,
      );
      expect(next, {
        'displayAs': 'remaining',
        'pins': [
          {'sourceId': 'claude', 'windowId': 'five_hour'},
          {'sourceId': 'codex', 'windowId': 'weekly'},
          {'sourceId': 'claude', 'windowId': 'weekly'},
        ],
      });
      final unpinned = toggleUsagePin(next, {'sourceId': 'codex', 'windowId': 'weekly'}, reports);
      expect(jsonList(unpinned['pins']).length, 2);
    });

    test('a pre-usageSources provider list becomes report entries', () {
      final entries = legacyUsageReports({
        'fetchedAt': '2026-01-01T00:00:00Z',
        'providers': [
          {'providerId': 'claude', 'displayName': 'Claude', 'status': 'error', 'error': 'boom', 'windows': []},
          {'providerId': 'codex', 'displayName': 'Codex', 'status': 'unavailable', 'windows': []},
        ],
      });
      expect(entries.first['report'], {'status': 'error', 'error': 'boom'});
      expect(entries.first['fetchedAt'], '2026-01-01T00:00:00Z');
      expect(jsonMap(entries.last['report'])['problem'], {'kind': 'no_quota', 'detail': ''});
    });
  });
}
