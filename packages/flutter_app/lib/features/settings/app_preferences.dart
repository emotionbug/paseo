import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/app_store.dart';
import '../../core/json.dart';
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import '../chat/chat_code.dart';
import '../files/file_panel_resolve.dart';
import 'keyboard_shortcut_settings.dart';
import 'settings_forms.dart';

final bool _desktop = Platform.isLinux || Platform.isMacOS || Platform.isWindows;
final bool _mobile = Platform.isAndroid || Platform.isIOS;

/// Original `hooks/use-settings/storage.ts` bounds.
const minUiBaseFontSize = 10, maxUiBaseFontSize = 21;
const minContentFontSize = 10, maxContentFontSize = 21;
const minCodeFontSize = 9, maxCodeFontSize = 22;
const minContentMaxWidth = 600, maxContentMaxWidth = 4000;
const defaultContentMaxWidth = 820;
const _maxFontFamilyLength = 200;

int? parseClampedSize(Object? value, int min, int max) {
  final number = value is num
      ? value
      : value is String && value.trim().isNotEmpty
      ? num.tryParse(value.trim())
      : null;
  if (number == null || !number.isFinite) return null;
  return number.floor().clamp(min, max);
}

/// Mirrors original `sanitizeFontFamily`: empty means default, invalid is null.
/// Unfinished strings or escapes and unquoted `()[]/*` are rejected too.
String? sanitizeFontFamily(String value) {
  final trimmed = value.trim();
  if (trimmed.isEmpty) return '';
  if (trimmed.length > _maxFontFamilyLength) return null;
  if (RegExp(r'[;{}<>]').hasMatch(trimmed)) return null;
  if (trimmed.codeUnits.any((c) => c <= 0x1f)) return null;
  String? quote;
  var escaped = false;
  for (final char in trimmed.split('')) {
    if (escaped) {
      escaped = false;
    } else if (char == r'\') {
      escaped = true;
    } else if (quote != null) {
      if (char == quote) quote = null;
    } else if (char == "'" || char == '"') {
      quote = char;
    } else if ('()[]/*'.contains(char)) {
      return null;
    }
  }
  return quote == null && !escaped ? trimmed : null;
}

/// Original `sidebar-nav/model.ts` `toPreferences`: [items] (the resolved
/// rows, in display order) lead in the slots of the stored keys they occupy;
/// stored entries for keys that are not currently available (a plugin row)
/// keep their place so an unrelated edit does not erase them.
List<JsonMap> sidebarNavPreferences(List<JsonMap> items, List<JsonMap> previous) {
  final remaining = [
    for (final item in items) <String, dynamic>{'key': item['key'], 'visible': item['visible'] != false},
  ];
  final available = {for (final item in remaining) item['key']};
  final preferences = <JsonMap>[];
  final seen = <Object?>{};
  for (final preference in previous) {
    final key = preference['key'];
    if (!seen.add(key)) continue;
    if (available.contains(key)) {
      if (remaining.isNotEmpty) preferences.add(remaining.removeAt(0));
      continue;
    }
    preferences.add({'key': key, 'visible': preference['visible'] == true});
  }
  return [...preferences, ...remaining];
}

/// Original `parseFilePanelSelectorKey`: `name:<file>` or `ext:<extension>`.
({String kind, String value}) _parseFilePanelSelectorKey(String key) {
  final separator = key.indexOf(':');
  return (
    kind: separator > 0 && key.substring(0, separator) == 'name' ? 'name' : 'ext',
    value: key.substring(separator + 1),
  );
}

/// Original `filePanelSelectorLabel`: a filename as is, an extension dotted.
String filePanelSelectorLabel(String key) {
  final selector = _parseFilePanelSelectorKey(key);
  return selector.kind == 'name' ? selector.value : '.${selector.value}';
}

/// Original `claimsFilePanelSelectorKey`, by the resolver's matching rule.
bool _claimsFilePanelSelectorKey(FilePanelCandidate candidate, String key) {
  if (candidate.claimsAll) return false;
  final selector = _parseFilePanelSelectorKey(key);
  final name = selector.kind == 'name' ? selector.value : '';
  final extension = selector.kind == 'name' ? filePanelExtension(selector.value) : selector.value;
  return candidate.filenames.contains(name) ||
      candidate.extensions!.any((entry) => entry.toLowerCase() == extension);
}

/// Original `offeredCandidates`: the panels claiming the key; the built-in
/// floor (Source) only when no built-in panel claims it.
List<FilePanelCandidate> offeredFilePanels(String key, [List<FilePanelCandidate> candidates = builtinFilePanels]) {
  final claiming = candidates.where((c) => _claimsFilePanelSelectorKey(c, key)).toList();
  return [
    for (final candidate in candidates)
      if (candidate.claimsAll ? claiming.isEmpty : claiming.contains(candidate)) candidate,
  ];
}

/// Original `buildFilePanelSettingsRows` over the built-in catalog: every key
/// where Paseo would ask (never, with built-ins only) plus every remembered
/// key, extensions before filenames, each alphabetically.
List<({String key, List<FilePanelCandidate> candidates, String? associatedPanelId})>
filePanelSettingsRows(JsonMap associations) {
  int compare(String left, String right) {
    final a = _parseFilePanelSelectorKey(left);
    final b = _parseFilePanelSelectorKey(right);
    if (a.kind != b.kind) return a.kind == 'ext' ? -1 : 1;
    return a.value.toLowerCase().compareTo(b.value.toLowerCase());
  }

  return [
    for (final key in associations.keys.toList()..sort(compare))
      (
        key: key,
        candidates: offeredFilePanels(key),
        associatedPanelId: associations[key] is String ? associations[key] as String : null,
      ),
  ];
}

/// Original THEME_OPTIONS order, groups and swatches; values are the literals
/// written to the `theme` setting.
const themeOptions = <(String, String, int?)>[
  ('light', 'primary', 0xffffffff),
  ('dark', 'primary', 0xff2d8b62),
  ('auto', 'primary', null),
  ('zinc', 'variant', 0xff808080),
  ('midnight', 'variant', 0xff4a6ba8),
  ('claude', 'variant', 0xffd97757),
  ('ghostty', 'variant', 0xff8caaee),
  ('pureBlack', 'variant', 0xff000000),
];

const syntaxThemeOptions = <(String, String)>[
  ('github', 'GitHub'),
  ('catppuccin', 'Catppuccin'),
  ('dracula', 'Dracula'),
  ('tokyo-night', 'Tokyo Night'),
  ('one', 'One'),
  ('nord', 'Nord'),
  ('gruvbox', 'Gruvbox'),
  ('solarized', 'Solarized'),
];

const _languages = [
  'system', 'ar', 'en', 'es', 'fr', 'ja', 'ko', 'pt-BR', 'ru', 'zh-CN',
];

String _languageKey(String value) => switch (value) {
  'pt-BR' => 'ptBR',
  'zh-CN' => 'zhCN',
  _ => value,
};

String _normalizedTheme(Object? value) {
  final theme = switch (value) {
    'dark-zinc' => 'zinc',
    'dark-midnight' => 'midnight',
    'dark-claude' => 'claude',
    'dark-ghostty' => 'ghostty',
    'dark-pure-black' => 'pureBlack',
    'system' => 'auto',
    final String name => name,
    _ => 'auto',
  };
  return themeOptions.any((o) => o.$1 == theme) ? theme : 'auto';
}

class AppPreferenceSettings extends StatefulWidget {
  final AppStore store;
  final String section;
  const AppPreferenceSettings({
    super.key,
    required this.store,
    this.section = 'general',
  });
  @override
  State<AppPreferenceSettings> createState() => _AppPreferenceSettingsState();
}

class _AppPreferenceSettingsState extends State<AppPreferenceSettings> {
  bool saving = false;
  dynamic value(String key, dynamic fallback) =>
      widget.store.preferences[key] ?? fallback;
  String t(String key, [Map<String, Object?> args = const {}]) =>
      tr(context, key, args: args);

  Future<void> save(String key, dynamic next) async {
    final before = widget.store.preferences[key];
    setState(() => saving = true);
    if (next == null) {
      widget.store.preferences.values.remove(key);
    } else {
      widget.store.preferences[key] = next;
    }
    try {
      await widget.store.savePreferences();
    } catch (e) {
      if (before == null) {
        widget.store.preferences.values.remove(key);
      } else {
        widget.store.preferences[key] = before;
      }
      if (mounted) showError(context, e);
    } finally {
      if (mounted) setState(() => saving = false);
    }
  }

  @override
  Widget build(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: switch (widget.section) {
      'general' => general(),
      'appearance' => [
        AppearanceSettings(store: widget.store, save: save),
      ],
      'sidebar' => sidebar(),
      'chat' => chat(),
      'editor' => [
        SettingsSection(
          title: t('settings.editor.title'),
          children: [
            SettingsSwitchRow(
              label: t('settings.editor.vimKeybindings'),
              hint: t('settings.editor.vimHint'),
              value: value('vimKeybindings', false) == true,
              onChanged: saving ? null : (v) => save('vimKeybindings', v),
            ),
          ],
        ),
      ],
      'files' => files(),
      // Original keyboard-shortcuts-section renders its own per-group sections.
      'shortcuts' => [KeyboardShortcutSettings(store: widget.store)],
      _ => const [],
    },
  );

  List<Widget> general() {
    final language = value('language', 'system').toString();
    final send = value('sendBehavior', 'steer').toString();
    final openInSidePane = jsonMap(value('openInSidePane', {}));
    final destinations = [
      ('main', t('settings.layout.openInSidePane.destinations.main')),
      ('side', t('settings.layout.openInSidePane.destinations.side')),
    ];
    return [
      SettingsSection(
        title: t('settings.general.title'),
        children: [
          SettingsSelectRow<String>(
            label: t('settings.general.language.label'),
            hint: t('settings.general.language.description'),
            value: _languages.contains(language) ? language : 'system',
            options: [
              for (final option in _languages)
                (
                  option,
                  t('settings.general.language.options.${_languageKey(option)}'),
                ),
            ],
            menuWidth: 300,
            onChanged: saving ? null : (v) => save('language', v),
          ),
        ],
      ),
      SettingsSection(
        title: t('settings.general.sending'),
        children: [
          SettingsSelectRow<String>(
            label: t('settings.general.defaultSend.label'),
            hint: t('settings.general.defaultSend.descriptions.$send'),
            value: send,
            options: [
              for (final option in const ['interrupt', 'steer', 'queue'])
                (option, t('settings.general.defaultSend.options.$option')),
            ],
            onChanged: saving ? null : (v) => save('sendBehavior', v),
          ),
        ],
      ),
      if (_desktop)
        SettingsSection(
          title: t('settings.layout.openInSidePane.title'),
          children: [
            for (final source in const [
              'explorerFiles',
              'diffs',
              'chatFiles',
              'diffFiles',
              'subagents',
            ])
              SettingsSelectRow<String>(
                label: t(
                  'settings.layout.openInSidePane.sources.$source.label',
                ),
                value: openInSidePane[source] == true ? 'side' : 'main',
                options: destinations,
                onChanged: saving
                    ? null
                    : (v) => save('openInSidePane', {
                        ...openInSidePane,
                        source: v == 'side',
                      }),
              ),
            SettingsSelectRow<String>(
              label: t(
                'settings.layout.openInSidePane.sources.pullRequests.label',
              ),
              value: value('pullRequestOpenLocation', 'explorer').toString(),
              options: [
                ...destinations,
                (
                  'explorer',
                  t('settings.layout.openInSidePane.destinations.explorer'),
                ),
              ],
              onChanged: saving
                  ? null
                  : (v) => save('pullRequestOpenLocation', v),
            ),
          ],
        ),
    ];
  }

  List<Widget> chat() => [
    SettingsSection(
      title: t('settings.appearance.detailLevel.title'),
      children: [
        SettingsSwitchRow(
          label: t('settings.general.autoExpandReasoning.label'),
          hint: t('settings.general.autoExpandReasoning.description'),
          value: value('autoExpandReasoning', false) == true,
          onChanged: saving ? null : (v) => save('autoExpandReasoning', v),
        ),
        SettingsSelectRow<String>(
          label: t('settings.general.toolCallDetail.label'),
          hint: t('settings.general.toolCallDetail.description'),
          value: value('toolCallDetailLevel', 'detailed') == 'overview'
              ? 'overview'
              : 'detailed',
          options: [
            for (final option in const ['detailed', 'overview'])
              (option, t('settings.general.toolCallDetail.options.$option')),
          ],
          onChanged: saving ? null : (v) => save('toolCallDetailLevel', v),
        ),
        if (!_mobile)
          SettingsSwitchRow(
            label: t('settings.appearance.chatOutline.title'),
            hint: t('settings.appearance.chatOutline.description'),
            value: value('chatOutlineEnabled', true) != false,
            onChanged: saving ? null : (v) => save('chatOutlineEnabled', v),
          ),
      ],
    ),
  ];

  List<Widget> sidebar() {
    final rowItems = jsonMap(value('sidebarRowItems', {}));
    return [
      navCard(
        'sidebarNavItems',
        const ['new-workspace', 'history', 'search', 'schedules'],
        t('settings.appearance.sidebar.header.title'),
        t('settings.appearance.sidebar.header.description'),
      ),
      navCard(
        'sidebarFooterItems',
        const ['usage'],
        t('settings.appearance.sidebar.footer.title'),
        t('settings.appearance.sidebar.footer.description'),
      ),
      // Original kept these in the sidebar's display menu; the Flutter
      // sidebar has no such menu, so they stay reachable here.
      SettingsSection(
        title: t('sidebar.display.heading'),
        children: [
          SettingsSelectRow<String>(
            label: t('sidebar.display.titleSource.label'),
            value: value('workspaceTitleSource', 'title') == 'branch'
                ? 'branch'
                : 'title',
            options: [
              ('title', t('sidebar.display.titleSource.title')),
              ('branch', t('sidebar.display.titleSource.branch')),
            ],
            onChanged: saving ? null : (v) => save('workspaceTitleSource', v),
          ),
          for (final item in const [
            'branch',
            'project',
            'host',
            'changeRequest',
            'services',
            'labels',
          ])
            SettingsSwitchRow(
              label: t('sidebar.display.show.$item'),
              value: rowItems[item] != false,
              onChanged: saving
                  ? null
                  : (v) => save('sidebarRowItems', {...rowItems, item: v}),
            ),
          SettingsSelectRow<String>(
            label: t('sidebar.display.show.checks'),
            value: value('sidebarChecksDisplay', 'iconAndText').toString(),
            options: [
              for (final option in const ['iconAndText', 'icon', 'none'])
                (option, t('sidebar.display.checks.$option')),
            ],
            onChanged: saving ? null : (v) => save('sidebarChecksDisplay', v),
          ),
          SettingsSelectRow<String>(
            label: t('sidebar.display.show.timestamp'),
            value: value('sidebarWorkspaceTrailing', 'timestamp').toString(),
            options: [
              ('timestamp', t('sidebar.display.show.timestamp')),
              ('diff', t('sidebar.display.show.diff')),
              ('none', t('sidebar.display.checks.none')),
            ],
            onChanged: saving
                ? null
                : (v) => save('sidebarWorkspaceTrailing', v),
          ),
        ],
      ),
    ];
  }

  Widget navCard(
    String key,
    List<String> defaults,
    String title,
    String info,
  ) {
    final stored = jsonList(value(key, [])).map(jsonMap).toList();
    final rows = <JsonMap>[];
    final placed = <String>{};
    for (final row in stored) {
      final id = stringValue(row['key']);
      if (defaults.contains(id) && placed.add(id)) {
        rows.add({'key': id, 'visible': row['visible'] != false});
      }
    }
    for (final id in defaults) {
      if (placed.add(id)) rows.add({'key': id, 'visible': id != 'usage'});
    }
    Future<void> persist() => save(key, sidebarNavPreferences(rows, stored));
    final muted = PaseoColors.of(context).muted;
    return SettingsSection(
      title: title,
      info: info,
      children: [
        for (var i = 0; i < rows.length; i++)
          SettingsRow(
            // Original SidebarNavRow: 16px muted icon, one-line base title,
            // ghost sm move buttons (14px arrows) and the switch, 8px gaps.
            leading: Icon(switch (rows[i]['key']) {
              'new-workspace' => LucideIcons.plus300,
              'history' => LucideIcons.history300,
              'search' => LucideIcons.search300,
              'schedules' => LucideIcons.calendarClock300,
              _ => LucideIcons.gauge300,
            }, size: 16, color: muted),
            labelMaxLines: 1,
            label: switch (rows[i]['key']) {
              'new-workspace' => t('sessionNavigation.newSession'),
              'history' => t('sidebar.sections.sessions'),
              'search' => t('sidebar.sections.search'),
              'schedules' => t('sidebar.sections.schedules'),
              _ => t('sidebar.footer.usage'),
            },
            control: Row(
              mainAxisSize: MainAxisSize.min,
              spacing: 8,
              children: [
                PaseoButton(
                  icon: LucideIcons.arrowUp300,
                  variant: PaseoButtonVariant.ghost,
                  size: PaseoControlSize.sm,
                  semanticLabel: t('settings.appearance.sidebar.moveUp'),
                  onPressed: saving || i == 0
                      ? null
                      : () {
                          rows.insert(i - 1, rows.removeAt(i));
                          persist();
                        },
                ),
                PaseoButton(
                  icon: LucideIcons.arrowDown300,
                  variant: PaseoButtonVariant.ghost,
                  size: PaseoControlSize.sm,
                  semanticLabel: t('settings.appearance.sidebar.moveDown'),
                  onPressed: saving || i == rows.length - 1
                      ? null
                      : () {
                          rows.insert(i + 1, rows.removeAt(i));
                          persist();
                        },
                ),
                PaseoSwitch(
                  value: rows[i]['visible'] != false,
                  semanticLabel: switch (rows[i]['key']) {
                    'new-workspace' => t('sessionNavigation.newSession'),
                    'history' => t('sidebar.sections.sessions'),
                    'search' => t('sidebar.sections.search'),
                    'schedules' => t('sidebar.sections.schedules'),
                    _ => t('sidebar.footer.usage'),
                  },
                  onChanged: saving
                      ? null
                      : (v) {
                          rows[i]['visible'] = v;
                          persist();
                        },
                ),
              ],
            ),
          ),
      ],
    );
  }

  /// Original `screens/settings/files-section.tsx`: per remembered key, a
  /// menu of the panels that claim it and Ask again (forgets the key).
  List<Widget> files() {
    final associations = jsonMap(value('filePanelAssociations', {}));
    final rows = filePanelSettingsRows(associations);
    String panelLabel(FilePanelCandidate panel) => t('panels.file.filePanels.option', {
      'title': t('panels.file.filePanels.builtin.${panel.id.substring('builtin:'.length)}'),
      'owner': t('panels.file.filePanels.builtin.owner'),
    });
    return [
      SettingsSection(
        title: t('settings.files.title'),
        children: [
          if (rows.isEmpty)
            // Original emptyCard: 16px padding, centered muted base text.
            Padding(
              padding: const EdgeInsets.all(16),
              child: Center(
                child: Text(
                  t('settings.files.empty'),
                  textAlign: TextAlign.center,
                  style: Theme.of(context).textTheme.bodyMedium?.copyWith(
                    color: PaseoColors.of(context).muted,
                  ),
                ),
              ),
            ),
          for (final row in rows)
            Builder(builder: (context) {
              final label = filePanelSelectorLabel(row.key);
              final associated = row.candidates.where((c) => c.id == row.associatedPanelId).firstOrNull;
              // Original `associatedValueLabel`.
              final valueLabel = row.associatedPanelId == null
                  ? t('settings.files.askEveryTime')
                  : associated == null
                  ? t('settings.files.unavailable')
                  : panelLabel(associated);
              return SettingsRow(
                label: label,
                control: Wrap(
                  spacing: 8,
                  runSpacing: 8,
                  crossAxisAlignment: WrapCrossAlignment.center,
                  children: [
                    PaseoDropdownTrigger<String>(
                      value: associated?.id ?? '',
                      title: '$label: $valueLabel',
                      menuWidth: 260,
                      options: [
                        if (associated == null) PaseoSelectOption('', valueLabel, enabled: false),
                        for (final panel in row.candidates) PaseoSelectOption(panel.id, panelLabel(panel)),
                      ],
                      onChanged: saving || row.candidates.isEmpty
                          ? null
                          : (id) => save('filePanelAssociations', {...associations, row.key: id}),
                    ),
                    settingsButton(
                      context,
                      t('settings.files.askAgain'),
                      saving || row.associatedPanelId == null
                          ? null
                          : () => save('filePanelAssociations', {...associations}..remove(row.key)),
                    ),
                  ],
                ),
              );
            }),
        ],
      ),
    ];
  }
}

/// Original `screens/settings/appearance/appearance-section.tsx`: Theme, Fonts
/// (commit on blur/submit with the original clamps), Layout and Syntax with a
/// live preview driven by the in-progress drafts.
class AppearanceSettings extends StatefulWidget {
  final AppStore store;
  final Future<void> Function(String key, dynamic value) save;
  const AppearanceSettings({
    super.key,
    required this.store,
    required this.save,
  });
  @override
  State<AppearanceSettings> createState() => _AppearanceSettingsState();
}

class _AppearanceSettingsState extends State<AppearanceSettings> {
  late final uiFont = TextEditingController(text: text('uiFontFamily'));
  late final monoFont = TextEditingController(text: text('monoFontFamily'));
  late final uiSize = TextEditingController(text: '${uiBase()}');
  late final contentSize = TextEditingController(text: '${content()}');
  late final codeSize = TextEditingController(text: '${code()}');
  late final width = TextEditingController(text: '${contentWidth()}');
  late final _controllers = [
    uiFont, monoFont, uiSize, contentSize, codeSize, width,
  ];
  late final Listenable drafts = Listenable.merge(_controllers);

  Map<String, dynamic> get prefs => widget.store.preferences.values;
  String text(String key) => prefs[key] is String ? prefs[key] as String : '';
  int uiBase() =>
      parseClampedSize(prefs['uiBaseFontSize'], minUiBaseFontSize,
          maxUiBaseFontSize) ??
      (_mobile ? 15 : 14);
  int content() =>
      parseClampedSize(prefs['contentFontSize'], minContentFontSize,
          maxContentFontSize) ??
      (_mobile ? 16 : 15);
  int code() =>
      parseClampedSize(prefs['codeFontSize'], minCodeFontSize,
          maxCodeFontSize) ??
      12;
  int? widthOverride() => parseClampedSize(
    prefs['contentMaxWidth'], minContentMaxWidth, maxContentMaxWidth,
  );
  int contentWidth() => widthOverride() ?? defaultContentMaxWidth;

  String t(String key, [Map<String, Object?> args = const {}]) =>
      tr(context, key, args: args);

  @override
  void initState() {
    super.initState();
    widget.store.addListener(_resync);
  }

  @override
  void dispose() {
    widget.store.removeListener(_resync);
    for (final controller in _controllers) {
      controller.dispose();
    }
    super.dispose();
  }

  /// Resync drafts when the committed value changes elsewhere.
  void _resync() {
    void sync(TextEditingController controller, String value) {
      if (controller.text != value) controller.text = value;
    }

    sync(uiSize, '${uiBase()}');
    sync(contentSize, '${content()}');
    sync(codeSize, '${code()}');
    sync(width, '${contentWidth()}');
    if (mounted) setState(() {});
  }

  void commitSize(
    TextEditingController controller,
    String key,
    int current,
    int min,
    int max,
  ) {
    final next = parseClampedSize(controller.text, min, max) ?? current;
    controller.text = '$next';
    if (next != current) widget.save(key, next);
  }

  void commitFamily(TextEditingController controller, String key) {
    final sanitized = sanitizeFontFamily(controller.text);
    if (sanitized == null) {
      controller.text = text(key);
      return;
    }
    controller.text = sanitized;
    if (sanitized != text(key)) widget.save(key, sanitized);
  }

  void commitWidth() {
    final current = contentWidth();
    final next =
        parseClampedSize(width.text, minContentMaxWidth, maxContentMaxWidth) ??
        current;
    width.text = '$next';
    // Typing the width already in effect keeps following the default.
    if (next != current) widget.save('contentMaxWidth', next);
  }

  /// Original appearance `sizeInput`/`widthInput`/`fontFamilyInput`: 36px
  /// min height, 8/12 padding, 6px radius, 1px border on surface2, base text;
  /// numeric fields are right-aligned and select their text on focus.
  Widget field(
    TextEditingController controller,
    VoidCallback commit, {
    double width = 64,
    bool numeric = true,
    String? hint,
    String? semantics,
  }) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final border = OutlineInputBorder(
      borderRadius: BorderRadius.circular(6),
      borderSide: BorderSide(color: theme.colorScheme.outline),
    );
    final input = Focus(
      onFocusChange: (focused) {
        if (!focused) {
          commit();
        } else if (numeric) {
          controller.selection = TextSelection(
            baseOffset: 0,
            extentOffset: controller.text.length,
          );
        }
      },
      child: Semantics(
        label: semantics,
        child: TextField(
          controller: controller,
          onSubmitted: (_) => commit(),
          textAlign: numeric ? TextAlign.right : TextAlign.left,
          keyboardType: numeric ? TextInputType.number : TextInputType.text,
          inputFormatters: numeric
              ? [FilteringTextInputFormatter.digitsOnly]
              : null,
          autocorrect: false,
          enableSuggestions: false,
          style: theme.textTheme.bodyMedium?.copyWith(
            fontSize: type.uiSize(14),
            color: theme.colorScheme.onSurface,
          ),
          decoration: paseoInputDecoration(
            context,
            hintText: hint,
            size: PaseoControlSize.sm,
          ).copyWith(
            constraints: const BoxConstraints(minHeight: 36),
            contentPadding: const EdgeInsets.symmetric(
              horizontal: 12,
              vertical: 8,
            ),
            border: border,
            enabledBorder: border,
            focusedBorder: border,
            disabledBorder: border,
          ),
        ),
      ),
    );
    return numeric
        ? Row(
            mainAxisSize: MainAxisSize.min,
            spacing: 8,
            children: [
              SizedBox(width: width, child: input),
              Text(
                'px',
                style: theme.textTheme.bodyMedium?.copyWith(
                  color: colors.muted,
                  fontSize: type.uiSize(14),
                ),
              ),
            ],
          )
        : ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 280),
            child: input,
          );
  }

  Widget themeLeading(String value) {
    final colors = PaseoColors.of(context);
    final swatch = themeOptions.where((o) => o.$1 == value).first.$3;
    return switch (value) {
      'light' => Icon(LucideIcons.sun300, size: 16, color: colors.muted),
      'dark' => Icon(LucideIcons.moon300, size: 16, color: colors.muted),
      'auto' => Icon(LucideIcons.monitor300, size: 16,
          color: colors.muted),
      _ => Container(
        width: 16,
        height: 16,
        decoration: BoxDecoration(
          color: Color(swatch!),
          shape: BoxShape.circle,
          border: Border.all(color: Theme.of(context).dividerColor),
        ),
      ),
    };
  }

  @override
  Widget build(BuildContext context) {
    final theme = _normalizedTheme(prefs['theme']);
    final syntax = syntaxThemeOptions.any((o) => o.$1 == prefs['syntaxTheme'])
        ? prefs['syntaxTheme'] as String
        : 'one';
    final systemDefault = t('settings.appearance.fonts.systemDefault');
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        SettingsSection(
          title: t('settings.appearance.theme.title'),
          children: [
            SettingsRow(
              label: t('settings.appearance.theme.title'),
              control: SettingsDropdown<String>(
                value: theme,
                menuWidth: 200,
                semanticLabel: t(
                  'settings.appearance.theme.accessibilityLabel',
                  {'value': t('settings.appearance.theme.options.$theme')},
                ),
                leading: themeLeading,
                separatorsBefore: const {'zinc'},
                options: [
                  for (final option in themeOptions)
                    (
                      option.$1,
                      t('settings.appearance.theme.options.${option.$1}'),
                    ),
                ],
                onChanged: (v) => widget.save('theme', v),
              ),
            ),
          ],
        ),
        SettingsSection(
          title: t('settings.appearance.fonts.title'),
          children: [
            if (!_mobile)
              SettingsRow(
                label: t('settings.appearance.fonts.interfaceFont'),
                hint: t('settings.appearance.fonts.interfaceFontHint'),
                inline: true,
                control: field(
                  uiFont,
                  () => commitFamily(uiFont, 'uiFontFamily'),
                  numeric: false,
                  hint: systemDefault,
                  semantics: t(
                    'settings.appearance.fonts.interfaceFontAccessibility',
                  ),
                ),
              ),
            SettingsRow(
              label: t('settings.appearance.fonts.interfaceSize'),
              hint: t('settings.appearance.fonts.interfaceSizeHint'),
              control: field(
                uiSize,
                () => commitSize(uiSize, 'uiBaseFontSize', uiBase(),
                    minUiBaseFontSize, maxUiBaseFontSize),
                semantics: t(
                  'settings.appearance.fonts.interfaceSizeAccessibility',
                ),
              ),
            ),
            SettingsRow(
              label: t('settings.appearance.fonts.contentSize'),
              hint: t('settings.appearance.fonts.contentSizeHint'),
              control: field(
                contentSize,
                () => commitSize(contentSize, 'contentFontSize', content(),
                    minContentFontSize, maxContentFontSize),
                semantics: t(
                  'settings.appearance.fonts.contentSizeAccessibility',
                ),
              ),
            ),
            SettingsRow(
              label: t('settings.appearance.fonts.codeFont'),
              hint: t('settings.appearance.fonts.codeFontHint'),
              inline: true,
              control: field(
                monoFont,
                () => commitFamily(monoFont, 'monoFontFamily'),
                numeric: false,
                hint: systemDefault,
                semantics: t('settings.appearance.fonts.codeFontAccessibility'),
              ),
            ),
            SettingsRow(
              label: t('settings.appearance.fonts.codeSize'),
              hint: t('settings.appearance.fonts.codeSizeHint'),
              control: field(
                codeSize,
                () => commitSize(codeSize, 'codeFontSize', code(),
                    minCodeFontSize, maxCodeFontSize),
                semantics: t('settings.appearance.fonts.codeSizeAccessibility'),
              ),
            ),
          ],
        ),
        SettingsSection(
          title: t('settings.appearance.layout.title'),
          children: [
            SettingsRow(
              label: t('settings.appearance.layout.contentWidth'),
              hint: t('settings.appearance.layout.contentWidthHint'),
              control: Row(
                mainAxisSize: MainAxisSize.min,
                spacing: 8,
                children: [
                  if (widthOverride() != null)
                    PaseoButton(
                      label: t('settings.appearance.layout.reset'),
                      semanticLabel: t(
                        'settings.appearance.layout.resetAccessibility',
                      ),
                      variant: PaseoButtonVariant.ghost,
                      size: PaseoControlSize.sm,
                      onPressed: () => widget.save('contentMaxWidth', null),
                    ),
                  field(
                    width,
                    commitWidth,
                    width: 80,
                    semantics: t(
                      'settings.appearance.layout.contentWidthAccessibility',
                    ),
                  ),
                ],
              ),
            ),
          ],
        ),
        SettingsSection(
          title: t('settings.appearance.syntax.title'),
          card: false,
          children: [
            SettingsCard(
              children: [
                SettingsRow(
                  label: t('settings.appearance.syntax.highlightTheme'),
                  hint: t('settings.appearance.syntax.highlightThemeHint'),
                  control: SettingsDropdown<String>(
                    value: syntax,
                    menuWidth: 200,
                    semanticLabel: t(
                      'settings.appearance.syntax.highlightThemeAccessibility',
                      {
                        'value': syntaxThemeOptions
                            .firstWhere((o) => o.$1 == syntax)
                            .$2,
                      },
                    ),
                    options: syntaxThemeOptions,
                    onChanged: (v) => widget.save('syntaxTheme', v),
                  ),
                ),
              ],
            ),
            Padding(
              // Original `styles.preview`: 16px above the syntax card, on top
              // of the section's 12px content gap.
              padding: const EdgeInsets.only(top: 16),
              child: ListenableBuilder(
                listenable: drafts,
                builder: (context, _) => _AppearancePreview(
                  syntaxTheme: syntax,
                  contentFontSize: int.tryParse(contentSize.text),
                  codeFontSize: int.tryParse(codeSize.text),
                  monoFontFamily: monoFont.text,
                ),
              ),
            ),
          ],
        ),
      ],
    );
  }
}

const _previewBefore = [
  '// Format a price for display',
  'export function formatPrice(cents: number) {',
  '  const amount = cents / 100;',
  '  return "\$" + amount;',
  '}',
];
const _previewAfter = [
  '// Format a price for display',
  'export function formatPrice(cents: number): string {',
  '  const amount = cents / 100;',
  r'  return `$${amount.toFixed(2)}`;',
  '}',
];
const _previewChanged = {1, 3};

/// Original `appearance-preview.tsx`: content sample plus a unified diff of a
/// fixed snippet in the code font and selected syntax theme.
class _AppearancePreview extends StatelessWidget {
  final String syntaxTheme;
  final int? contentFontSize, codeFontSize;
  final String monoFontFamily;
  const _AppearancePreview({
    required this.syntaxTheme,
    required this.contentFontSize,
    required this.codeFontSize,
    required this.monoFontFamily,
  });
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final dark = theme.brightness == Brightness.dark;
    final highlighter = ChatCodeHighlighter(dark, theme: syntaxTheme);
    final family = monoFontFamily.trim();
    final codeSize = codeFontSize?.toDouble() ?? type.codeFontSize;
    final code = type.codeStyle.copyWith(
      fontFamily: family.isEmpty ? null : family.split(',').first.trim(),
      fontSize: codeSize,
      height: (codeSize * 1.5).roundToDouble() / codeSize,
      color: theme.colorScheme.onSurface,
    );
    final contentSize = contentFontSize?.toDouble() ?? type.contentFontSize;
    Widget row(String marker, String line, Color? tint, Color markerColor) =>
        Container(
          color: tint,
          padding: const EdgeInsets.symmetric(horizontal: 12),
          child: Text.rich(
            TextSpan(
              style: code,
              children: [
                TextSpan(text: marker, style: TextStyle(color: markerColor)),
                if (line.isEmpty)
                  const TextSpan(text: '\u200b')
                else
                  highlighter.format(line),
              ],
            ),
            softWrap: false,
            overflow: TextOverflow.fade,
          ),
        );
    return Semantics(
      label: tr(context, 'settings.appearance.syntax.previewAccessibility'),
      child: Container(
        decoration: BoxDecoration(
          color: colors.surface1,
          border: Border.all(color: theme.dividerColor),
          borderRadius: BorderRadius.circular(8),
        ),
        clipBehavior: Clip.antiAlias,
        // Original preview card: 8px vertical padding, content sample with
        // 12px sides and 8px below, then the diff rows (no separator).
        padding: const EdgeInsets.symmetric(vertical: 8),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(12, 0, 12, 8),
              child: Text(
                tr(context, 'settings.appearance.syntax.previewContent'),
                style: theme.textTheme.bodyLarge?.copyWith(
                  fontSize: contentSize,
                  height: (contentSize * 1.4).roundToDouble() / contentSize,
                  color: theme.colorScheme.onSurface,
                ),
              ),
            ),
            for (var i = 0; i < _previewBefore.length; i++)
              if (_previewChanged.contains(i)) ...[
                row('- ', _previewBefore[i],
                    const Color(0x1af85149), colors.removed),
                row('+ ', _previewAfter[i],
                    const Color(0x262ea043), colors.added),
              ] else
                row('  ', _previewBefore[i], null, colors.muted),
          ],
        ),
      ),
    );
  }
}
