import 'package:flutter/material.dart';

import '../../../core/json.dart';
import '../../../core/scope.dart';
import '../../../ui/command_center.dart' show ShortcutBadge;
import '../../../ui/common.dart';
import '../../../ui/shortcuts.dart';
import '../../../ui/theme.dart';

/// hooks/use-shortcut-keys.ts + keyboard-shortcuts.ts
/// resolveShortcutKeysForAction: the platform binding for a help id
/// ("agent-interrupt", "cycle-agent-mode", "focus-message-input"), replaced
/// by the user's override for that binding id; an unassigned (null/empty)
/// override hides the hint.
String? resolveShortcutCombo(List<ShortcutBinding> bindings, String helpId, JsonMap overrides) {
  for (final binding in bindings) {
    if (binding.data.object('help').text('id') != helpId) continue;
    if (!overrides.containsKey(binding.id)) return binding.combo;
    final override = overrides[binding.id];
    if (override == null || override == '') return null;
    return override is String ? override : binding.combo;
  }
  return null;
}

/// The composer's shortcut overrides (preferences.keyboardShortcuts).
JsonMap shortcutOverrides(BuildContext context) =>
    jsonMap(PaseoScope.of(context).preferences['keyboardShortcuts']);

/// components/ui/tooltip.tsx on the composer: desktop only (enabledOnMobile
/// false), no delay, above the trigger with an 8px offset; popover surface
/// (surface0 light / surface2 dark), 1px borderAccent, 12px radius, 4/8
/// insets, md shadow, base foreground label. A [combo] (or the binding for
/// [helpId]) follows the label as the original `Shortcut` chips, 8px apart.
class ComposerTooltip extends StatelessWidget {
  const ComposerTooltip({
    super.key,
    required this.label,
    required this.child,
    this.helpId,
    this.combo,
    this.height = 28,
  });

  final String label;
  final String? helpId, combo;
  final Widget child;

  /// The trigger height, to place the tooltip 8px above it.
  final double height;

  @override
  Widget build(BuildContext context) {
    if (isCompactLayout(context)) return child;
    if (helpId == null) return _tooltip(context, combo);
    return FutureBuilder<List<ShortcutBinding>>(
      future: ShortcutCatalog.load(),
      builder: (context, snapshot) => _tooltip(
        context,
        snapshot.data == null
            ? null
            : resolveShortcutCombo(snapshot.data!, helpId!, shortcutOverrides(context)),
      ),
    );
  }

  Widget _tooltip(BuildContext context, String? keys) {
    final palette = PaseoColors.of(context);
    final colors = Theme.of(context).colorScheme;
    final dark = Theme.of(context).brightness == Brightness.dark;
    final text = Theme.of(context).textTheme.bodyMedium!.copyWith(color: colors.onSurface);
    return Tooltip(
      waitDuration: Duration.zero,
      preferBelow: false,
      verticalOffset: height / 2 + 8,
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
      decoration: BoxDecoration(
        color: dark ? palette.surface2 : palette.surface0,
        border: Border.all(color: palette.borderAccent),
        borderRadius: BorderRadius.circular(12),
        boxShadow: [
          BoxShadow(
            color: dark ? const Color(0x33000000) : const Color(0x0a000000),
            offset: const Offset(0, 4),
            blurRadius: dark ? 8 : 16,
          ),
        ],
      ),
      textStyle: text,
      richMessage: TextSpan(
        style: text,
        children: [
          TextSpan(text: label),
          if (keys != null && keys.isNotEmpty)
            WidgetSpan(
              alignment: PlaceholderAlignment.middle,
              child: Padding(
                padding: const EdgeInsets.only(left: 8),
                child: ShortcutBadge(keys),
              ),
            ),
        ],
      ),
      child: child,
    );
  }
}
