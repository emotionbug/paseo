import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:paseo_sessions/features/chat/attachment_pill.dart';
import 'package:paseo_sessions/ui/theme.dart';

const _png =
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==';

void main() {
  testWidgets('a rebuilt image thumbnail keeps showing its picture', (tester) async {
    Widget thumbnail(Map<String, dynamic> image) => MaterialApp(
      theme: paseoTheme(brightness: Brightness.dark, preferences: const {}),
      home: Builder(builder: (context) => AttachmentPill.thumbnail(context, image)),
    );
    await tester.pumpWidget(thumbnail({'data': _png, 'mimeType': 'image/png'}));
    await tester.runAsync(() => precacheImage(
      AttachmentPill.provider({'data': _png})!,
      tester.element(find.byType(Image)),
    ));
    await tester.pump();
    expect(tester.widget<RawImage>(find.byType(RawImage)).image, isNotNull);

    // The composer rebuilds with a fresh attachment map carrying the same payload.
    await tester.pumpWidget(thumbnail({'data': _png, 'mimeType': 'image/png'}));
    expect(tester.widget<RawImage>(find.byType(RawImage)).image, isNotNull);
  });
}
