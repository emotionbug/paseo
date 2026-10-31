import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/app_store.dart';
import '../../core/json.dart';
import '../../ui/command_center.dart' show ShortcutBadge;
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/localization.dart';
import '../../ui/shortcuts.dart';
import '../../ui/theme.dart';
import 'settings_forms.dart';
import 'settings_host_widgets.dart';

/// Original `buildKeyboardShortcutHelpSections` order and title keys.
const _shortcutSections = [
  ('general', 'general'),
  ('workspaces', 'workspaces'),
  ('tabs-panes', 'tabsPanes'),
  ('layout', 'layout'),
  ('agent-input', 'agentInput'),
];

class KeyboardShortcutSettings extends StatefulWidget {
  final AppStore store;
  const KeyboardShortcutSettings({super.key, required this.store});
  @override
  State<KeyboardShortcutSettings> createState() =>
      _KeyboardShortcutSettingsState();
}

/// Original `screens/settings/keyboard-shortcuts-section.tsx`: one settings
/// section per help section, rows of label + shortcut keys + a 32px actions
/// menu (Bind/Rebind, Clear, Reset to default); capture happens inline on the
/// row (surface2) with ghost Done / Cancel buttons. Mobile shows the
/// desktop-only notice card.
class _KeyboardShortcutSettingsState extends State<KeyboardShortcutSettings> {
  List<ShortcutBinding> bindings = [];
  Object? error;
  bool loading = true;
  final captureFocus = FocusNode(debugLabel: 'shortcut capture');
  String? capturing;
  final steps = <String>[];

  @override
  void initState() {
    super.initState();
    load();
  }

  @override
  void dispose() {
    captureFocus.dispose();
    super.dispose();
  }

  Future<void> load() async {
    try {
      final catalog = await ShortcutCatalog.load();
      if (mounted) {
        setState(() {
          bindings = catalog;
          loading = false;
          error = null;
        });
      }
    } catch (e) {
      if (mounted) {
        setState(() {
          error = e;
          loading = false;
        });
      }
    }
  }

  String effectiveCombo(ShortcutBinding binding) => stringValue(
    jsonMap(widget.store.preferences['keyboardShortcuts'])[binding.id],
    binding.combo,
  );

  /// Original `createShortcutOverrideStore`: override writes run one after
  /// another, so an earlier failure cannot roll back a later change and a
  /// Reset all cannot erase a rebind made after it.
  Future<void> _tail = Future<void>.value();
  Future<void> _enqueue(Future<void> Function() operation) {
    final run = _tail.then((_) => operation());
    _tail = run.then((_) {}, onError: (Object _) {});
    return run;
  }

  /// A null [combo] drops the override (default restored); `''` unassigns.
  Future<void> persist(ShortcutBinding binding, String? combo) => _enqueue(() async {
    final before = widget.store.preferences['keyboardShortcuts'];
    final next = <String, dynamic>{...jsonMap(before)};
    if (combo == null) {
      next.remove(binding.id);
    } else {
      next[binding.id] = combo;
    }
    widget.store.preferences['keyboardShortcuts'] = next;
    try {
      await widget.store.savePreferences();
    } catch (_) {
      widget.store.preferences['keyboardShortcuts'] = before;
      rethrow;
    } finally {
      if (mounted) setState(() {});
    }
  });

  /// Original `isShortcutCaptureComplete`: at least one combo; indexed
  /// navigation must end on a number. Any other chord, even one another
  /// action already uses, can be saved.
  bool captureComplete(ShortcutBinding binding) =>
      steps.isNotEmpty &&
      (binding.payload['type'] != 'index' || RegExp(r'(^|\+)(Digit|[1-9])$').hasMatch(steps.last));

  static final _keyNames = <LogicalKeyboardKey, String>{
    LogicalKeyboardKey.space: 'Space',
    LogicalKeyboardKey.enter: 'Enter',
    LogicalKeyboardKey.escape: 'Escape',
    LogicalKeyboardKey.tab: 'Tab',
    LogicalKeyboardKey.arrowLeft: 'ArrowLeft',
    LogicalKeyboardKey.arrowRight: 'ArrowRight',
    LogicalKeyboardKey.arrowUp: 'ArrowUp',
    LogicalKeyboardKey.arrowDown: 'ArrowDown',
    LogicalKeyboardKey.pageUp: 'PageUp',
    LogicalKeyboardKey.pageDown: 'PageDown',
  };
  String keyName(LogicalKeyboardKey key) =>
      _keyNames[key] ?? key.keyLabel.replaceAll(' ', '');

  void startCapture(ShortcutBinding binding) {
    setState(() {
      capturing = binding.id;
      steps.clear();
    });
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted && capturing == binding.id) captureFocus.requestFocus();
    });
  }

  void cancelCapture() => setState(() {
    capturing = null;
    steps.clear();
  });

  /// Original `saveCapture`: write the override and leave capture at once;
  /// a failed write restores the previous value.
  void saveCapture(ShortcutBinding binding) {
    if (!captureComplete(binding)) return;
    unawaited(update(binding, steps.join(' ')));
    cancelCapture();
  }

  Future<void> update(ShortcutBinding binding, String? combo) async {
    try {
      await persist(binding, combo);
    } catch (e) {
      if (mounted) showError(context, e);
    }
  }

  Future<void> resetAll() => _enqueue(() async {
    final before = widget.store.preferences['keyboardShortcuts'];
    widget.store.preferences['keyboardShortcuts'] = <String, dynamic>{};
    try {
      await widget.store.savePreferences();
    } catch (e) {
      widget.store.preferences['keyboardShortcuts'] = before;
      if (mounted) showError(context, e);
    }
    if (mounted) setState(() {});
  });

  KeyEventResult onCaptureKey(ShortcutBinding binding, KeyEvent event) {
    if (event is! KeyDownEvent) return KeyEventResult.handled;
    if ({
      LogicalKeyboardKey.controlLeft,
      LogicalKeyboardKey.controlRight,
      LogicalKeyboardKey.metaLeft,
      LogicalKeyboardKey.metaRight,
      LogicalKeyboardKey.altLeft,
      LogicalKeyboardKey.altRight,
      LogicalKeyboardKey.shiftLeft,
      LogicalKeyboardKey.shiftRight,
    }.contains(event.logicalKey)) {
      return KeyEventResult.handled;
    }
    final keyboard = HardwareKeyboard.instance;
    final modified = keyboard.isControlPressed ||
        keyboard.isMetaPressed ||
        keyboard.isAltPressed ||
        keyboard.isShiftPressed;
    // Original: an unmodified Backspace removes the last recorded step.
    if (event.logicalKey == LogicalKeyboardKey.backspace && !modified) {
      setState(() {
        if (steps.isNotEmpty) steps.removeLast();
      });
      return KeyEventResult.handled;
    }
    final hid = event.physicalKey.usbHidUsage;
    final digit =
        hid >= 0x7001e && hid <= 0x70026 ||
        RegExp(r'^[1-9]$').hasMatch(event.logicalKey.keyLabel);
    final key = binding.payload['type'] == 'index' && digit
        ? 'Digit'
        : keyName(event.logicalKey);
    if (key.isEmpty) return KeyEventResult.handled;
    // Original `keyboardEventToComboString` modifier order and names.
    setState(() {
      steps.add(
        [
          if (keyboard.isControlPressed) 'Ctrl',
          if (keyboard.isAltPressed) 'Alt',
          if (keyboard.isShiftPressed) 'Shift',
          if (keyboard.isMetaPressed) 'Cmd',
          key,
        ].join('+'),
      );
    });
    return KeyEventResult.handled;
  }

  Widget row(ShortcutBinding binding) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    String t(String key) => tr(context, key);
    final base = theme.textTheme.bodyMedium!.copyWith(fontSize: type.uiSize(14));
    final mutedBase = base.copyWith(color: colors.muted);
    final isCapturing = capturing == binding.id;
    final overrides = jsonMap(widget.store.preferences['keyboardShortcuts']);
    final combo = effectiveCombo(binding);
    final hasOverride = overrides.containsKey(binding.id);
    final hasDefault = binding.combo.isNotEmpty;
    final complete = captureComplete(binding);

    final Widget keys;
    if (isCapturing) {
      keys = steps.isEmpty
          ? Text(t('settings.shortcuts.capturePrompt'), style: mutedBase)
          : ShortcutBadge(steps.join(' '));
    } else if (combo.isEmpty) {
      keys = Text(t('settings.shortcuts.unassigned'), style: mutedBase);
    } else {
      keys = ShortcutBadge(combo);
    }
    final keyColumn = Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.end,
      children: [
        keys,
        if (isCapturing && steps.isNotEmpty && !complete)
          Text(t('settings.shortcuts.captureIndexPrompt'), style: mutedBase),
      ],
    );

    final actions = isCapturing
        ? [
            if (steps.isNotEmpty)
              PaseoButton(
                label: t('settings.shortcuts.actions.done'),
                size: PaseoControlSize.sm,
                variant: PaseoButtonVariant.ghost,
                onPressed: complete ? () => saveCapture(binding) : null,
              ),
            PaseoButton(
              label: t('settings.shortcuts.actions.cancel'),
              size: PaseoControlSize.sm,
              variant: PaseoButtonVariant.ghost,
              onPressed: cancelCapture,
            ),
          ]
        : [
            SettingsMenuButton(
              label: tr(
                context,
                'settings.shortcuts.actions.menu',
                args: {'name': binding.label},
              ),
              options: [
                PaseoMenuEntry.item(
                  value: 'bind',
                  label: t(combo.isEmpty
                      ? 'settings.shortcuts.actions.bind'
                      : 'settings.shortcuts.actions.rebind'),
                  leading: Icon(LucideIcons.pencil300, size: 14, color: colors.muted),
                ),
                if (combo.isNotEmpty)
                  PaseoMenuEntry.item(
                    value: 'clear',
                    label: t('settings.shortcuts.actions.clear'),
                    leading: Icon(LucideIcons.x300, size: 14, color: colors.muted),
                  ),
                if (hasOverride && hasDefault)
                  PaseoMenuEntry.item(
                    value: 'reset',
                    label: t('settings.shortcuts.actions.reset'),
                    leading: Icon(LucideIcons.undo2300, size: 14, color: colors.muted),
                  ),
              ],
              onSelected: (action) => switch (action) {
                'bind' => startCapture(binding),
                'clear' => update(binding, ''),
                _ => update(binding, null),
              },
            ),
          ];

    Widget content = Container(
      color: isCapturing ? colors.surface2 : null,
      padding: const EdgeInsets.all(12),
      child: Row(
        spacing: 8,
        children: [
          Expanded(child: Text(binding.label, style: base)),
          Flexible(child: keyColumn),
          ...actions,
        ],
      ),
    );
    if (isCapturing) {
      content = Focus(
        focusNode: captureFocus,
        autofocus: true,
        onKeyEvent: (_, event) => onCaptureKey(binding, event),
        child: content,
      );
    }
    return content;
  }

  @override
  Widget build(BuildContext context) {
    String t(String key) => tr(context, key);
    if (Platform.isAndroid || Platform.isIOS) {
      return SettingsSection(
        title: t('settings.sections.shortcuts'),
        card: false,
        children: [SettingsEmptyCard(t('settings.shortcuts.unavailableOnMobile'))],
      );
    }
    if (loading) return const LoadingState();
    if (error != null) return ErrorState(error: error!, onRetry: load);
    final overrides = jsonMap(widget.store.preferences['keyboardShortcuts']);
    final sections = <Widget>[];
    for (final (id, key) in _shortcutSections) {
      final seen = <String>{};
      final rows = bindings.where((binding) {
        final help = binding.data.object('help');
        final section = help.text('section');
        // A binding without a help section stays rebindable under General.
        if (section != id && !(section.isEmpty && id == 'general')) return false;
        return seen.add(help.text('id', binding.id));
      }).toList();
      if (rows.isEmpty) continue;
      sections.add(
        SettingsSection(
          title: t('settings.shortcuts.sections.$key'),
          card: false,
          trailing: sections.isEmpty && overrides.isNotEmpty
              ? PaseoButton(
                  label: t('settings.shortcuts.actions.resetAll'),
                  size: PaseoControlSize.sm,
                  variant: PaseoButtonVariant.ghost,
                  onPressed: resetAll,
                )
              : null,
          children: [
            SettingsRowsCard(children: [for (final binding in rows) row(binding)]),
          ],
        ),
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: sections,
    );
  }
}
