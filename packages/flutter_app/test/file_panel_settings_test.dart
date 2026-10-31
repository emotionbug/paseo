import 'package:flutter_test/flutter_test.dart';

import 'package:paseo_sessions/features/settings/app_preferences.dart';

void main() {
  group('Settings › Files (file-panels/settings/rows.ts)', () {
    test('a key offers only the panels claiming it; Source only when no built-in does', () {
      expect(offeredFilePanels('ext:md').map((p) => p.id), ['builtin:markdown']);
      // Keys hold lowercased extensions; an uppercase key matches no built-in.
      expect(offeredFilePanels('ext:MD').map((p) => p.id), ['builtin:source']);
      expect(offeredFilePanels('ext:png').map((p) => p.id), ['builtin:image']);
      expect(offeredFilePanels('ext:ts').map((p) => p.id), ['builtin:source']);
      expect(offeredFilePanels('name:README.html').map((p) => p.id), ['builtin:html']);
    });

    test('remembered keys are rows, extensions before filenames, with full panel ids', () {
      final rows = filePanelSettingsRows({
        'name:Dockerfile': 'builtin:source',
        'ext:md': 'builtin:markdown',
        'ext:html': 'plugin:gone',
      });
      expect(rows.map((r) => r.key), ['ext:html', 'ext:md', 'name:Dockerfile']);
      expect(rows[1].associatedPanelId, 'builtin:markdown');
      expect(rows[0].candidates.map((p) => p.id), ['builtin:html']);
      expect(filePanelSelectorLabel('ext:md'), '.md');
      expect(filePanelSelectorLabel('name:Dockerfile'), 'Dockerfile');
    });
  });
}
