import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:intl/intl.dart' show DateFormat, Intl;

import '../../../core/json.dart';
import '../../../ui/common.dart';
import '../../../ui/localization.dart';
import '../../../ui/theme.dart';

/// message.tsx STREAM_METADATA_FONT_SIZE: timestamps, durations, elapsed timer.
const streamMetadataFontSize = 13.0;

/// utils/time.ts formatMessageTimestamp: the time today ("오후 3:00"),
/// "`<weekday> <time>`" for the previous 6 calendar days ("목요일 오후 3:00"),
/// "`<d MMM yyyy>, <time>`" otherwise; locale-formatted like Intl
/// toLocaleDateString(undefined, {weekday: long} / {day, month: short, year}).
String formatMessageTimestamp(BuildContext context, DateTime value) {
  final local = value.toLocal();
  final localizations = MaterialLocalizations.of(context);
  final time = localizations.formatTimeOfDay(
    TimeOfDay.fromDateTime(local),
    alwaysUse24HourFormat: MediaQuery.alwaysUse24HourFormatOf(context),
  );
  final now = DateTime.now();
  // Calendar days, immune to DST-shortened days.
  final days = DateTime.utc(now.year, now.month, now.day)
      .difference(DateTime.utc(local.year, local.month, local.day))
      .inDays;
  if (days == 0) return time;
  final locale = Intl.canonicalizedLocale(Localizations.localeOf(context).toString());
  if (days > 0 && days < 7) {
    return '${DateFormat.EEEE(locale).format(local)} $time';
  }
  return '${DateFormat.yMMMd(locale).format(local)}, $time';
}

/// A bare glyph with 4px padding: muted, foreground while hovered or open;
/// 50% opacity while [busy] (original trigger `opacity[50]` when locked).
class GlyphButton extends StatefulWidget {
  const GlyphButton({
    super.key,
    required this.tooltip,
    required this.icon,
    required this.size,
    required this.onTap,
    this.active = false,
    this.busy = false,
  });
  final String tooltip;
  final IconData icon;
  final double size;
  final void Function(BuildContext anchor)? onTap;
  final bool active, busy;

  @override
  State<GlyphButton> createState() => _GlyphButtonState();
}

class _GlyphButtonState extends State<GlyphButton> {
  bool hovered = false;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final enabled = widget.onTap != null && !widget.busy;
    return Tooltip(
      message: widget.tooltip,
      // Original tooltips are desktop-only (enabledOnMobile={false}).
      triggerMode: isCompactLayout(context) ? TooltipTriggerMode.manual : null,
      // container: its own node sized to the glyph, never merged into the
      // enclosing row's node.
      child: Semantics(
        container: true,
        button: true,
        enabled: enabled,
        label: widget.tooltip,
        excludeSemantics: true,
        child: MouseRegion(
          cursor: enabled ? SystemMouseCursors.click : MouseCursor.defer,
          onEnter: (_) => setState(() => hovered = true),
          onExit: (_) => setState(() => hovered = false),
          child: GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTap: enabled ? () => widget.onTap!(context) : null,
            child: Opacity(
              opacity: widget.busy ? .5 : 1,
              child: Padding(
                padding: const EdgeInsets.all(4),
                child: Icon(
                  widget.icon,
                  size: widget.size,
                  color: hovered || widget.active ? colors.onSurface : colors.onSurfaceVariant,
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// message.tsx TurnCopyButton: 14px copy glyph that shows a check for 1.5s.
class TurnCopyButton extends StatefulWidget {
  const TurnCopyButton({super.key, required this.getContent, this.labelKey = 'message.actions.copyTurn'});
  final String Function() getContent;
  final String labelKey;

  @override
  State<TurnCopyButton> createState() => _TurnCopyButtonState();
}

class _TurnCopyButtonState extends State<TurnCopyButton> {
  bool copied = false;
  Timer? _reset;

  @override
  void dispose() {
    _reset?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => GlyphButton(
    tooltip: tr(context, copied ? 'message.actions.copied' : widget.labelKey),
    icon: copied ? LucideIcons.check300 : LucideIcons.copy300,
    size: 14,
    onTap: (_) async {
      final content = widget.getContent();
      if (content.isEmpty) return;
      await Clipboard.setData(ClipboardData(text: content));
      if (!mounted) return;
      setState(() => copied = true);
      _reset?.cancel();
      _reset = Timer(const Duration(milliseconds: 1500), () {
        if (mounted) setState(() => copied = false);
      });
    },
  );
}

/// One row of an [showActionMenu] page.
typedef ActionMenuItem = ({String value, String label, IconData icon});

/// The original DropdownMenu (MenuSurface) for actions whose items use
/// `closeOnSelect={false}` + `status="pending"`: picking an item keeps the menu
/// open, swaps that row's leading icon for a 16px muted spinner, disables the
/// other rows and refuses to close until [onSelect] settles; then it closes.
/// Compact layouts get the bottom-sheet surface unless [popover] (menus the
/// original anchors on every platform), others a 220px popover under
/// [anchor] (its end edge with [alignEnd]). Visuals mirror ui/common.dart
/// showPaseoMenu.
Future<void> showActionMenu(
  BuildContext context, {
  required Rect anchor,
  required List<ActionMenuItem> items,
  required Future<void> Function(String value) onSelect,
  String? header,
  bool alignEnd = false,
  bool popover = false,
}) {
  final colors = PaseoColors.of(context);
  Widget page() => _ActionMenuPage(items: items, onSelect: onSelect, header: header);
  if (!popover && isCompactLayout(context)) {
    return showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      backgroundColor: colors.surface0,
      barrierColor: const Color(0x73000000),
      shape: RoundedRectangleBorder(
        borderRadius: const BorderRadius.vertical(top: Radius.circular(12)),
        side: BorderSide(color: Theme.of(context).dividerColor),
      ),
      builder: (sheet) => ConstrainedBox(
        constraints: BoxConstraints(maxHeight: MediaQuery.sizeOf(sheet).height * .8),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Center(
              child: Container(
                margin: const EdgeInsets.symmetric(vertical: 10),
                width: 36,
                height: 4,
                decoration: BoxDecoration(
                  color: colors.surface2,
                  borderRadius: BorderRadius.circular(2),
                ),
              ),
            ),
            Flexible(
              child: SingleChildScrollView(
                padding: EdgeInsets.only(bottom: MediaQuery.paddingOf(sheet).bottom + 4),
                child: page(),
              ),
            ),
          ],
        ),
      ),
    );
  }
  final dark = Theme.of(context).brightness == Brightness.dark;
  return showGeneralDialog<void>(
    context: context,
    barrierDismissible: true,
    barrierLabel: MaterialLocalizations.of(context).modalBarrierDismissLabel,
    barrierColor: Colors.transparent,
    transitionDuration: const Duration(milliseconds: 120),
    transitionBuilder: (_, animation, _, child) => FadeTransition(opacity: animation, child: child),
    pageBuilder: (popover, _, _) => CustomSingleChildLayout(
      delegate: _PopoverLayout(anchor, alignEnd, MediaQuery.paddingOf(popover)),
      child: SizedBox(
        width: 220,
        child: Material(
          color: colors.surface1,
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(8),
            side: BorderSide(color: colors.borderAccent),
          ),
          clipBehavior: Clip.antiAlias,
          child: DecoratedBox(
            decoration: BoxDecoration(
              boxShadow: [
                BoxShadow(
                  color: dark ? const Color(0x33000000) : const Color(0x0a000000),
                  offset: const Offset(0, 4),
                  blurRadius: dark ? 8 : 16,
                ),
              ],
            ),
            child: SingleChildScrollView(child: page()),
          ),
        ),
      ),
    ),
  );
}

class _PopoverLayout extends SingleChildLayoutDelegate {
  const _PopoverLayout(this.anchor, this.alignEnd, this.padding);
  final Rect anchor;
  final bool alignEnd;
  final EdgeInsets padding;

  @override
  BoxConstraints getConstraintsForChild(BoxConstraints constraints) => BoxConstraints.loose(
    Size(constraints.maxWidth - 16, constraints.maxHeight - padding.vertical - 16),
  );

  @override
  Offset getPositionForChild(Size size, Size child) {
    final x = alignEnd ? anchor.right - child.width : anchor.left;
    final below = anchor.bottom + 4;
    final y = below + child.height <= size.height - padding.bottom - 8
        ? below
        : anchor.top - 4 - child.height;
    return Offset(
      x.clamp(8, size.width - child.width - 8),
      y.clamp(padding.top + 8, size.height - child.height - 8),
    );
  }

  @override
  bool shouldRelayout(_PopoverLayout old) =>
      anchor != old.anchor || alignEnd != old.alignEnd || padding != old.padding;
}

class _ActionMenuPage extends StatefulWidget {
  const _ActionMenuPage({required this.items, required this.onSelect, this.header});
  final List<ActionMenuItem> items;
  final Future<void> Function(String value) onSelect;
  final String? header;

  @override
  State<_ActionMenuPage> createState() => _ActionMenuPageState();
}

class _ActionMenuPageState extends State<_ActionMenuPage> {
  String? pending;

  Future<void> _select(String value) async {
    if (pending != null) return;
    setState(() => pending = value);
    try {
      await widget.onSelect(value);
    } catch (_) {
      // Callers own failure toasts; the menu only owns flow state.
    } finally {
      if (mounted) {
        setState(() => pending = null);
        Navigator.of(context).pop();
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final theme = Theme.of(context);
    return PopScope(
      canPop: pending == null,
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 4),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            if (widget.header != null) ...[
              // rewind-menu.tsx warningHeader: 12 sides, 8 vertical, sm muted.
              Padding(
                padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
                child: Text(
                  widget.header!,
                  style: theme.textTheme.bodySmall!.copyWith(color: colors.muted),
                ),
              ),
              Container(
                height: 1,
                margin: const EdgeInsets.symmetric(vertical: 4),
                color: colors.borderAccent,
              ),
            ],
            for (final item in widget.items)
              _ActionMenuRow(
                item: item,
                pending: pending == item.value,
                enabled: pending == null,
                onTap: () => _select(item.value),
              ),
          ],
        ),
      ),
    );
  }
}

/// Original MenuItem: 40/28 row inset 4px, 6px radius, 16px leading slot
/// (spinner while pending), base text on an 18px line, surface2 hover/press,
/// 50% opacity when disabled.
class _ActionMenuRow extends StatefulWidget {
  const _ActionMenuRow({required this.item, required this.pending, required this.enabled, required this.onTap});
  final ActionMenuItem item;
  final bool pending, enabled;
  final VoidCallback onTap;

  @override
  State<_ActionMenuRow> createState() => _ActionMenuRowState();
}

class _ActionMenuRowState extends State<_ActionMenuRow> {
  bool hovered = false, pressed = false;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final text = theme.textTheme.bodyMedium!;
    final enabled = widget.enabled;
    return Opacity(
      opacity: enabled || widget.pending ? 1 : .5,
      child: MouseRegion(
        cursor: enabled ? SystemMouseCursors.click : MouseCursor.defer,
        onEnter: (_) => setState(() => hovered = true),
        onExit: (_) => setState(() => hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: enabled ? (_) => setState(() => pressed = true) : null,
          onTapCancel: () => setState(() => pressed = false),
          onTap: enabled
              ? () {
                  setState(() => pressed = false);
                  widget.onTap();
                }
              : null,
          child: Container(
            constraints: BoxConstraints(minHeight: isCompactLayout(context) ? 40 : 28),
            margin: const EdgeInsets.symmetric(horizontal: 4),
            padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
            decoration: BoxDecoration(
              color: enabled && (hovered || pressed) ? colors.surface2 : null,
              border: Border.all(color: Colors.transparent),
              borderRadius: BorderRadius.circular(6),
            ),
            child: Row(
              spacing: 8,
              children: [
                SizedBox.square(
                  dimension: 16,
                  child: Center(
                    child: widget.pending
                        ? SizedBox.square(
                            dimension: 14,
                            child: CircularProgressIndicator(strokeWidth: 1.5, color: colors.muted),
                          )
                        : Icon(widget.item.icon, size: 16, color: theme.colorScheme.onSurface),
                  ),
                ),
                Expanded(
                  child: Text(
                    widget.item.label,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: text.copyWith(
                      color: theme.colorScheme.onSurface,
                      height: 18 / (text.fontSize ?? 14),
                    ),
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// components/assistant-fork-menu.tsx: a 14px split glyph opening
/// "Fork in a new tab" / "Fork in a new workspace" in a popover anchored to
/// the glyph on every platform; the picked row spins and the glyph dims at
/// 50% until the fork settles.
class AssistantForkMenuButton extends StatefulWidget {
  const AssistantForkMenuButton({super.key, required this.onFork});

  /// Receives `tab` or `workspace`.
  final Future<void> Function(String target) onFork;

  @override
  State<AssistantForkMenuButton> createState() => _AssistantForkMenuButtonState();
}

class _AssistantForkMenuButtonState extends State<AssistantForkMenuButton> {
  bool open = false, pending = false;

  Future<void> _open(BuildContext anchor) async {
    setState(() => open = true);
    await showActionMenu(
      context,
      anchor: paseoAnchorRect(anchor),
      popover: true,
      items: [
        (value: 'tab', label: tr(context, 'message.actions.forkInNewTab'), icon: LucideIcons.split300),
        (
          value: 'workspace',
          label: tr(context, 'message.actions.forkInNewWorkspace'),
          icon: LucideIcons.split300,
        ),
      ],
      onSelect: (target) async {
        if (mounted) setState(() => pending = true);
        try {
          await widget.onFork(target);
        } finally {
          if (mounted) setState(() => pending = false);
        }
      },
    );
    if (mounted) setState(() => open = false);
  }

  @override
  Widget build(BuildContext context) => GlyphButton(
    tooltip: tr(context, 'message.actions.forkMenu'),
    icon: LucideIcons.split300,
    size: 14,
    active: open,
    busy: pending,
    onTap: _open,
  );
}

/// components/rewind/rewind-menu.tsx: a 16px undo glyph whose menu starts with
/// the "cannot be undone" warning, then one item per supported rewind mode
/// (rewind-mode.ts resolveRewindMenuItems). Hidden without supported modes.
class RewindMenuButton extends StatefulWidget {
  const RewindMenuButton({super.key, required this.capabilities, required this.onRewind});
  final JsonMap capabilities;
  final Future<void> Function(String mode) onRewind;

  static List<String> modes(JsonMap capabilities) => [
    if (capabilities['supportsRewindConversation'] == true) 'conversation',
    if (capabilities['supportsRewindFiles'] == true) 'files',
    if (capabilities['supportsRewindBoth'] == true) 'both',
  ];

  @override
  State<RewindMenuButton> createState() => _RewindMenuButtonState();
}

class _RewindMenuButtonState extends State<RewindMenuButton> {
  bool open = false, pending = false;

  Future<void> _open(BuildContext anchor) async {
    setState(() => open = true);
    await showActionMenu(
      context,
      anchor: paseoAnchorRect(anchor),
      alignEnd: true,
      header: tr(context, 'rewind.warning'),
      items: [
        for (final mode in RewindMenuButton.modes(widget.capabilities))
          (
            value: mode,
            label: tr(context, 'rewind.actions.$mode'),
            icon: switch (mode) {
              'conversation' => LucideIcons.messageSquare300,
              'files' => LucideIcons.fileText300,
              _ => LucideIcons.layers300,
            },
          ),
      ],
      onSelect: (mode) async {
        if (mounted) setState(() => pending = true);
        try {
          await widget.onRewind(mode);
        } finally {
          if (mounted) setState(() => pending = false);
        }
      },
    );
    if (mounted) setState(() => open = false);
  }

  @override
  Widget build(BuildContext context) {
    if (RewindMenuButton.modes(widget.capabilities).isEmpty) return const SizedBox.shrink();
    return GlyphButton(
      tooltip: tr(context, 'rewind.tooltip'),
      icon: LucideIcons.undo2300,
      size: 16,
      active: open,
      busy: pending,
      onTap: _open,
    );
  }
}
