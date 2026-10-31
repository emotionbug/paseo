import 'dart:async';

import 'package:flutter_test/flutter_test.dart';

import 'package:paseo_sessions/core/json.dart';
import 'package:paseo_sessions/features/chat/chat_markdown.dart';
import 'package:paseo_sessions/features/chat/chrome/chat_find.dart';

JsonMap _row(int start, int end, String type, String text) => {
  'seqStart': start,
  'seqEnd': end,
  'item': {'type': type, 'text': text},
};

JsonMap _hit(int seq, String role, [int? count]) => {'seq': seq, 'role': role, 'count': ?count};

class _Ops implements ChatFindOperations {
  _Ops(this.rows, this.onSearch);

  @override
  String? epoch = 'e1';
  @override
  List<JsonMap> rows;
  Future<JsonMap> Function(String query, int? cursor) onSearch;
  Future<void> Function(String epoch, int seq)? onLoad;
  final List<int?> cursors = [];
  final List<int> loads = [];
  final List<int> scrolled = [];

  @override
  Future<JsonMap> search(String query, int? cursor) {
    cursors.add(cursor);
    return onSearch(query, cursor);
  }

  @override
  Future<void> load(String epoch, int seq) {
    loads.add(seq);
    return onLoad?.call(epoch, seq) ?? Future.value();
  }

  @override
  void scrollTo(int rowSeqStart) => scrolled.add(rowSeqStart);
}

Future<JsonMap> _page(List<JsonMap> locations, {int? next, String epoch = 'e1'}) =>
    Future.value({'epoch': epoch, 'locations': locations, 'nextCursor': next});

Future<void> _settle(WidgetTester tester) async {
  await tester.pump(const Duration(milliseconds: 120));
  await tester.pump();
}

void main() {
  testWidgets('pages every result, then steps through each message occurrence before wrapping', (tester) async {
    final ops = _Ops(
      [
        _row(1, 1, 'user_message', 'please find me'),
        _row(2, 2, 'assistant_message', 'I will find it, then find more'),
        _row(3, 3, 'user_message', 'thanks'),
        _row(4, 4, 'assistant_message', 'find'),
      ],
      (query, cursor) => cursor == null
          ? _page([_hit(1, 'user', 1), _hit(2, 'assistant', 2)], next: 2)
          : _page([_hit(4, 'assistant', 1)]),
    );
    final find = ChatFindController(ops)..show();
    find.setQuery('find');
    expect(find.phase, 'searching');
    await tester.pump(const Duration(milliseconds: 119));
    expect(ops.cursors, isEmpty);
    await _settle(tester);

    expect(ops.cursors, [null, 2]);
    expect((find.phase, find.selectedRow, find.occurrence, find.count), ('ready', 1, 0, 4));
    expect(find.highlight, (query: 'find', occurrence: 0));

    final visited = <(int?, int, int?)>[];
    for (var i = 0; i < 4; i++) {
      find.next();
      await tester.pump();
      visited.add((find.selectedRow, find.occurrence, find.highlight?.occurrence));
    }
    expect(visited, [(2, 1, 0), (2, 2, 1), (4, 3, 0), (1, 0, 0)]);

    find.previous();
    await tester.pump();
    expect((find.selectedRow, find.occurrence), (4, 3));
    find.previous();
    await tester.pump();
    // Entering a message backwards lands on its last occurrence.
    expect((find.selectedRow, find.occurrence, find.highlight?.occurrence), (2, 2, 1));
    find.dispose();
  });

  testWidgets('hits inside one displayed message collapse into it with the rendered count', (tester) async {
    final ops = _Ops(
      [
        _row(1, 1, 'user_message', 'go'),
        _row(2, 5, 'assistant_message', 'alpha beta\n\nalpha gamma alpha'),
      ],
      (query, cursor) => _page([_hit(3, 'assistant', 1), _hit(5, 'assistant', 1)]),
    );
    final find = ChatFindController(ops)..show();
    find.setQuery('alpha');
    await _settle(tester);
    expect((find.selectedRow, find.occurrence, find.count), (2, 0, 3));

    final steps = <int>[];
    for (var i = 0; i < 3; i++) {
      find.next();
      await tester.pump();
      steps.add(find.occurrence);
    }
    expect(steps, [1, 2, 0]);
    expect(find.selectedRow, 2);
    find.dispose();
  });

  testWidgets('a hit outside the loaded history loads it instead of landing on a later row', (tester) async {
    final ops = _Ops(
      [_row(10, 10, 'assistant_message', 'later alpha')],
      (query, cursor) => _page([_hit(4, 'assistant', 2), _hit(10, 'assistant', 1)]),
    );
    late ChatFindController find;
    ops.onLoad = (epoch, seq) async {
      ops.rows = [
        _row(3, 3, 'user_message', 'go'),
        _row(4, 4, 'assistant_message', 'alpha and alpha'),
        _row(10, 10, 'assistant_message', 'later alpha'),
      ];
      find.historyChanged(epoch);
    };
    find = ChatFindController(ops)..show();
    find.setQuery('alpha');
    await _settle(tester);
    expect(ops.loads, [4]);
    expect((find.selectedRow, find.occurrence, find.count), (4, 0, 3));
    find.dispose();
  });

  testWidgets('a located message without a rendered match is skipped and dropped from the total', (tester) async {
    final ops = _Ops(
      [
        _row(1, 1, 'assistant_message', 'see [docs](https://alpha.dev)'),
        _row(2, 2, 'user_message', 'alpha'),
      ],
      (query, cursor) => _page([_hit(1, 'assistant', 1), _hit(2, 'user', 1)]),
    );
    final find = ChatFindController(ops)..show();
    find.setQuery('alpha');
    await _settle(tester);
    expect((find.selectedRow, find.occurrence, find.count), (2, 0, 1));

    ops.rows = [_row(1, 1, 'assistant_message', 'nothing here')];
    final empty = ChatFindController(_Ops(ops.rows, (query, cursor) => _page([_hit(1, 'assistant', 1)])))..show();
    empty.setQuery('alpha');
    await _settle(tester);
    expect((empty.phase, empty.selectedRow, empty.count), ('ready', null, 0));
    find.dispose();
    empty.dispose();
  });

  testWidgets('a superseded query discards its late response', (tester) async {
    final pending = <String, Completer<JsonMap>>{};
    final ops = _Ops(
      [_row(1, 1, 'user_message', 'ab ab')],
      (query, cursor) => (pending[query] = Completer<JsonMap>()).future,
    );
    final find = ChatFindController(ops)..show();
    find.setQuery('a');
    await _settle(tester);
    find.setQuery('ab');
    await _settle(tester);
    pending['a']!.complete({'epoch': 'e1', 'locations': [_hit(1, 'user', 2)], 'nextCursor': null});
    await tester.pump();
    expect(find.phase, 'searching');
    pending['ab']!.complete({'epoch': 'e1', 'locations': [_hit(1, 'user', 2)], 'nextCursor': null});
    await tester.pump();
    expect((find.phase, find.query, find.count), ('ready', 'ab', 2));
    find.dispose();
  });

  testWidgets('history moving under the results fails with historyChanged', (tester) async {
    final ops = _Ops(
      [_row(1, 1, 'user_message', 'alpha')],
      (query, cursor) => _page([_hit(1, 'user', 1)]),
    );
    final find = ChatFindController(ops)..show();
    find.setQuery('alpha');
    await _settle(tester);
    expect(find.phase, 'ready');
    find.historyChanged('e2');
    expect((find.phase, find.failure, find.selectedRow, find.highlight), ('error', ChatFindFailure.historyChanged, null, null));

    // A result from another epoch than the loaded timeline fails the same way.
    ops.onSearch = (query, cursor) => _page([_hit(1, 'user', 1)], epoch: 'e0');
    find.retry();
    await _settle(tester);
    expect(find.failure, ChatFindFailure.historyChanged);
    find.dispose();
  });

  testWidgets('host and load failures read connection; a hit that never loads reads reveal', (tester) async {
    final ops = _Ops([], (query, cursor) => Future.error(StateError('offline')));
    final find = ChatFindController(ops)..show();
    find.setQuery('alpha');
    await _settle(tester);
    expect((find.phase, find.failure), ('error', ChatFindFailure.connection));

    ops.onSearch = (query, cursor) => _page([_hit(7, 'assistant', 1)]);
    ops.onLoad = (epoch, seq) => Future.error(StateError('offline'));
    find.retry();
    await _settle(tester);
    expect(find.failure, ChatFindFailure.connection);

    ops.onLoad = null;
    find.retry();
    await _settle(tester);
    expect((find.phase, find.failure), ('loading', null));
    await tester.pump(const Duration(seconds: 5));
    expect((find.phase, find.failure), ('error', ChatFindFailure.reveal));
    find.dispose();
  });

  testWidgets('close keeps the query, show searches it again and asks for focus', (tester) async {
    var searches = 0;
    final ops = _Ops([_row(1, 1, 'user_message', 'alpha')], (query, cursor) {
      searches++;
      return _page([_hit(1, 'user', 1)]);
    });
    final find = ChatFindController(ops)..show();
    final focus = find.focusRequests;
    find.setQuery('alpha');
    await _settle(tester);
    find.close();
    expect((find.open, find.phase, find.selectedRow, find.count, find.query), (false, 'idle', null, 0, 'alpha'));
    find.show();
    expect(find.focusRequests, focus + 1);
    await _settle(tester);
    expect((searches, find.phase, find.count), (2, 'ready', 1));
    // Find while open refocuses the widget without searching again.
    find.show();
    await _settle(tester);
    expect((searches, find.focusRequests), (2, focus + 2));
    find.dispose();
  });

  test('rendered markdown matches are counted per block, never in hidden syntax', () {
    expect(chatFindMarkdownMatchCount('alpha **alpha** [alpha](https://alpha.dev)', 'alpha'), 3);
    expect(chatFindMarkdownMatchCount('one\n\ntwo', 'one two'), 0);
    expect(chatFindMarkdownMatchCount('one\ntwo', 'one two'), 1);
    expect(chatFindMarkdownMatchCount('```\nalpha\n```\n- alpha\n  - alpha', 'ALPHA'), 3);
    expect(chatFindMarkdownMatchCount('![alpha](a.png) `alpha`', 'alpha'), 1);
    expect(chatFindMarkdownMatchCount('> [!NOTE]\n> alpha', 'note'), 0);
    expect(chatFindMarkdownMatchCount('alpha', '   '), 0);
  });
}
