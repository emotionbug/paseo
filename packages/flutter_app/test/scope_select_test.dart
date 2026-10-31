import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:paseo_sessions/core/app_store.dart';
import 'package:paseo_sessions/core/scope.dart';

void main() {
  testWidgets('select rebuilds on its own value only; of on every store change', (tester) async {
    final store = AppStore(connectHosts: false, discoverLocalHosts: false);
    var selected = 0, everything = 0;
    try {
      await tester.pumpWidget(
        PaseoScope(
          store: store,
          child: Directionality(
            textDirection: TextDirection.ltr,
            child: Column(
              children: [
                Builder(builder: (context) {
                  selected++;
                  final theme = PaseoScope.select(
                    context,
                    (store) => '${store.preferences['syntaxTheme'] ?? 'one'}',
                  );
                  return Text(theme);
                }),
                Builder(builder: (context) {
                  everything++;
                  PaseoScope.of(context);
                  return const SizedBox();
                }),
              ],
            ),
          ),
        ),
      );
      expect((selected, everything), (1, 1));

      store.preferences['unrelated'] = 1;
      await store.savePreferences();
      await tester.pump();
      expect((selected, everything), (1, 2), reason: 'an unrelated change skips the selector');

      store.preferences['syntaxTheme'] = 'github';
      await store.savePreferences();
      await tester.pump();
      expect(selected, 2);
      expect(find.text('github'), findsOneWidget);

      // The earlier build's selection ('one') is gone: an unrelated change
      // after the rebuild still skips it.
      store.preferences['unrelated'] = 2;
      await store.savePreferences();
      await tester.pump();
      expect((selected, everything), (2, 4));
    } finally {
      await tester.pumpWidget(const SizedBox.shrink());
      await store.disposeAsync();
    }
  });
}
