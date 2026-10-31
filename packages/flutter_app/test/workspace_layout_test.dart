import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:paseo_sessions/core/app_store.dart';
import 'package:paseo_sessions/core/target.dart';

PanelTarget _file(String id) => PanelTarget(
  kind: 'file',
  serverId: 'host',
  workspaceId: 'workspace',
  cwd: '/project',
  id: id,
);

/// Original workspace-layout-actions.ts: `closeTabInLayout` and
/// `moveTabToPaneInLayout` drop a pane once it has no tabs (`detachTabFromTree`),
/// keeping only the default `main` pane and the last pane.
void main() {
  late AppStore store;
  setUp(() => store = AppStore(connectHosts: false, discoverLocalHosts: false));
  tearDown(() => store.disposeAsync());

  test('closing the last tab of a split pane removes the pane', () async {
    store.open(_file('a'));
    final right = store.splitPane('main', Axis.horizontal);
    store.open(_file('b'));
    expect(store.layout.panes.map((pane) => pane.id), ['main', right]);

    expect(await store.closeTab(_file('b').key), isTrue);

    expect(store.layout.panes.map((pane) => pane.id), ['main']);
    expect(store.focusedPaneId, 'main');
    expect(store.selected?.key, _file('a').key);
  });

  test('the main pane stays when its last tab closes beside another pane', () async {
    store.open(_file('a'));
    final right = store.splitPane('main', Axis.horizontal);
    store.open(_file('b'));
    store.focusPane('main');

    expect(await store.closeTab(_file('a').key), isTrue);

    expect(store.layout.panes.map((pane) => pane.id), ['main', right]);
    expect(store.layout.panes.first.tabKeys, isEmpty);
  });

  test('closing the focused tab focuses the next tab, else the previous one', () async {
    for (final id in ['a', 'b', 'c']) {
      store.open(_file(id));
    }
    store.selectTab(_file('b').key);

    await store.closeTab(_file('b').key);
    expect(store.selected?.key, _file('c').key);

    await store.closeTab(_file('c').key);
    expect(store.selected?.key, _file('a').key);
  });

  test('moving the only tab out of a pane removes that pane', () {
    store.open(_file('a'));
    final right = store.splitPane('main', Axis.horizontal);
    store.open(_file('b'));
    final down = store.splitPane(right, Axis.vertical);
    store.open(_file('c'));

    store.moveTab(_file('c').key, 'main');

    expect(store.layout.panes.map((pane) => pane.id), ['main', right]);
    expect(store.layout.panes.first.tabKeys, [_file('a').key, _file('c').key]);
    expect(store.focusedPaneId, 'main');
    expect(store.layout.panes.any((pane) => pane.id == down), isFalse);
  });

  test('a closed pane hands focus to its nearest sibling', () async {
    store.open(_file('a'));
    final right = store.splitPane('main', Axis.horizontal);
    store.open(_file('b'));
    final down = store.splitPane(right, Axis.vertical);
    store.open(_file('c'));

    expect(await store.closePane(down), isTrue);

    expect(store.layout.panes.map((pane) => pane.id), ['main', right]);
    expect(store.focusedPaneId, right);
  });
}
