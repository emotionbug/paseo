import 'dart:async';
import 'dart:convert';

import 'package:flutter/cupertino.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import 'localization.dart';
import 'theme.dart';

/// The original `AdaptiveModalSheet`: a bottom sheet (surface0, 16px top radius, drag
/// handle) on compact layouts and a centered 520px card (surface1, 12px radius, surface2
/// border, 85% max height, 55% black scrim) on wider layouts.
Future<T?> showAdaptiveSheet<T>({
  required BuildContext context,
  required WidgetBuilder builder,
  double maxWidth = 520,
  bool topAligned = false,
}) {
  final colors = PaseoColors.of(context);
  if (isCompactLayout(context)) {
    return showModalBottomSheet<T>(
      context: context,
      isScrollControlled: true,
      useSafeArea: true,
      showDragHandle: true,
      backgroundColor: colors.surface0,
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.vertical(top: Radius.circular(16)),
      ),
      builder: (context) => Padding(
        padding: EdgeInsets.only(bottom: MediaQuery.viewInsetsOf(context).bottom),
        child: builder(context),
      ),
    );
  }
  return showDialog<T>(
    context: context,
    barrierColor: const Color(0x8c000000),
    builder: (context) => Dialog(
      alignment: topAligned ? Alignment.topCenter : Alignment.center,
      backgroundColor: colors.surface1,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(12),
        side: BorderSide(color: colors.surface2),
      ),
      clipBehavior: Clip.antiAlias,
      insetPadding: EdgeInsets.symmetric(
        horizontal: 24, vertical: topAligned ? 48 : 24,
      ),
      child: ConstrainedBox(
        constraints: BoxConstraints(
          maxWidth: maxWidth,
          maxHeight: MediaQuery.sizeOf(context).height * .85,
        ),
        child: builder(context),
      ),
    ),
  );
}

/// The original sheet header/content/footer: 24px insets, base-size medium title,
/// close (or back) glyph, a surface2 rule under the header and above the footer.
class SheetContents extends StatelessWidget {
  const SheetContents({
    super.key,
    required this.title,
    required this.child,
    this.actions = const [],
    this.subtitle,
    this.onBack,
    this.headerActions = const [],
    this.header,
    this.stretchActions = false,
    this.scroll = true,
    this.leading,
    this.maxHeightFactor = .9,
  });
  final String title;
  final Widget child;
  final List<Widget> actions;
  final Widget? subtitle;
  final VoidCallback? onBack;
  final List<Widget> headerActions;

  /// Extra fixed content under the title row, such as the original header search row.
  final Widget? header;

  /// Footer buttons share the row equally, as in the original two-button sheets.
  final bool stretchActions;

  /// False when [child] scrolls on its own (lists); it is then given the bounded height.
  final bool scroll;

  /// Original SheetHeader `leading`: a glyph before the title (headerLeadingSlot).
  final Widget? leading;

  /// Height cap as a fraction of the window (the workspace sheet uses .92).
  final double maxHeightFactor;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final rule = BorderSide(color: colors.surface2);
    final glyph = IconButton.styleFrom(
      padding: const EdgeInsets.all(8),
      minimumSize: Size.zero,
      tapTargetSize: MaterialTapTargetSize.shrinkWrap,
      foregroundColor: colors.muted,
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(8)),
    );
    return ConstrainedBox(
      constraints: BoxConstraints(
        maxHeight: MediaQuery.sizeOf(context).height * maxHeightFactor,
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          DecoratedBox(
            decoration: BoxDecoration(border: Border(bottom: rule)),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Padding(
                  padding: const EdgeInsets.fromLTRB(24, 16, 16, 16),
                  child: Row(
                    spacing: 8,
                    children: [
                      if (onBack != null)
                        IconButton(
                          tooltip: 'Back',
                          style: glyph,
                          onPressed: onBack,
                          icon: const Icon(LucideIcons.arrowLeft300, size: 16),
                        ),
                      if (leading != null)
                        Center(widthFactor: 1, heightFactor: 1, child: leading),
                      Expanded(
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          spacing: 4,
                          children: [
                            Text(
                              title,
                              maxLines: 2,
                              overflow: TextOverflow.ellipsis,
                              style: Theme.of(context).textTheme.bodyMedium!.copyWith(
                                fontWeight: FontWeight.w500,
                              ),
                            ),
                            if (subtitle != null)
                              DefaultTextStyle(
                                style: Theme.of(context).textTheme.bodySmall!.copyWith(
                                  color: colors.muted,
                                ),
                                child: subtitle!,
                              ),
                          ],
                        ),
                      ),
                      ...headerActions,
                      IconButton(
                        tooltip: 'Close',
                        style: glyph,
                        onPressed: () => Navigator.pop(context),
                        icon: const Icon(LucideIcons.x300, size: 16),
                      ),
                    ],
                  ),
                ),
                ?header,
              ],
            ),
          ),
          Flexible(
            child: scroll
                ? SingleChildScrollView(
                    padding: const EdgeInsets.all(24),
                    child: child,
                  )
                : child,
          ),
          if (actions.isNotEmpty)
            DecoratedBox(
              decoration: BoxDecoration(border: Border(top: rule)),
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: 24, vertical: 12),
                child: Row(
                  mainAxisAlignment: MainAxisAlignment.end,
                  spacing: 8,
                  children: [
                    for (final action in actions)
                      stretchActions ? Expanded(child: action) : action,
                  ],
                ),
              ),
            ),
        ],
      ),
    );
  }
}

/// Original `density` tokens: compact (<600) layouts use touch sizes.
bool isCompactLayout(BuildContext context) => MediaQuery.sizeOf(context).width < 600;

/// Original DensityIcon sizes: xs/sm/md/lg = 14/18/20/24 compact, 12/14/16/20 desktop.
double densityIcon(BuildContext context, String size) {
  final compact = isCompactLayout(context);
  return switch (size) {
    'xs' => compact ? 14 : 12,
    'sm' => compact ? 18 : 14,
    'md' => compact ? 20 : 16,
    _ => compact ? 24 : 20,
  };
}

/// Original `surfaceSidebarHover`: surface1 on dark palettes, surface3 on light.
Color paseoSidebarHover(BuildContext context) {
  final colors = PaseoColors.of(context);
  return Theme.of(context).brightness == Brightness.dark ? colors.surface1 : colors.surface3;
}

/// The original sidebar navigation row (`SidebarHeaderRow`, compact variant): an 8px-inset,
/// 36/28px row with an 8px radius whose sm muted icon and base-size muted label turn
/// foreground while hovered or active (`surfaceSidebarHover` fill) and fill surface2 while
/// pressed.
class SidebarHeaderRow extends StatefulWidget {
  const SidebarHeaderRow({
    super.key,
    required this.icon,
    required this.label,
    required this.onTap,
    this.active = false,
  });
  final IconData icon;
  final String label;
  final VoidCallback onTap;
  final bool active;

  @override
  State<SidebarHeaderRow> createState() => _SidebarHeaderRowState();
}

class _SidebarHeaderRowState extends State<SidebarHeaderRow> {
  bool _hovered = false, _pressed = false;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final foreground = Theme.of(context).colorScheme.onSurface;
    final highlighted = _hovered || widget.active;
    final color = highlighted ? foreground : colors.muted;
    final rowHeight = isCompactLayout(context) ? 36.0 : 28.0;
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 8),
      child: Semantics(
        button: true,
        selected: widget.active,
        child: MouseRegion(
          cursor: SystemMouseCursors.click,
          onEnter: (_) => setState(() => _hovered = true),
          onExit: (_) => setState(() => _hovered = false),
          child: GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTapDown: (_) => setState(() => _pressed = true),
            onTapCancel: () => setState(() => _pressed = false),
            onTapUp: (_) => setState(() => _pressed = false),
            onTap: widget.onTap,
            child: Container(
              constraints: BoxConstraints(minHeight: rowHeight),
              padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
              decoration: BoxDecoration(
                color: _pressed
                    ? colors.surface2
                    : highlighted
                    ? paseoSidebarHover(context)
                    : null,
                borderRadius: BorderRadius.circular(8),
              ),
              child: Row(
                spacing: 8,
                children: [
                  Icon(widget.icon, size: densityIcon(context, 'sm'), color: color),
                  Flexible(
                    child: Text(
                      widget.label,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: Theme.of(context).textTheme.bodyMedium!.copyWith(color: color),
                    ),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// Icon-only chrome of the original sidebar and header controls: a fixed [extent] hitbox
/// (circle or [radius] corners) that fills [highlight] while pressed — and while hovered
/// unless [pressOnly] — with an [iconSize] glyph in [color] ([hoverColor] while hovered).
/// Holding or right-clicking runs [onLongPress]; tooltips are desktop-only.
class PaseoChromeButton extends StatefulWidget {
  const PaseoChromeButton({
    super.key,
    required this.tooltip,
    required this.icon,
    required this.extent,
    required this.iconSize,
    required this.onPressed,
    this.onLongPress,
    this.color,
    this.hoverColor,
    this.highlight,
    this.radius,
    this.pressOnly = false,
    this.hoverOnly = false,
  });
  final String tooltip;
  final IconData icon;
  final double extent, iconSize;
  final void Function(BuildContext anchor)? onPressed;
  final void Function(BuildContext anchor)? onLongPress;
  /// Glyph colour; defaults to the muted foreground.
  final Color? color, hoverColor;
  /// Fill while active; defaults to the original `interactionHighlight`.
  final Color? highlight;
  /// Corner radius; null draws a circle.
  final double? radius;
  /// Highlight only while pressed (original sidebar footer icons).
  final bool pressOnly;
  /// Highlight only while hovered (original display-preferences trigger).
  final bool hoverOnly;

  @override
  State<PaseoChromeButton> createState() => _PaseoChromeButtonState();
}

class _PaseoChromeButtonState extends State<PaseoChromeButton> {
  bool _hovered = false, _pressed = false;

  @override
  Widget build(BuildContext context) {
    final enabled = widget.onPressed != null;
    final highlighted = enabled &&
        (widget.pressOnly
            ? _pressed
            : widget.hoverOnly
            ? _hovered
            : _hovered || _pressed);
    final muted = widget.color ?? PaseoColors.of(context).muted;
    final long = widget.onLongPress;
    Widget button = Semantics(
      button: true,
      enabled: enabled,
      label: widget.tooltip,
      excludeSemantics: true,
      child: MouseRegion(
        cursor: enabled ? SystemMouseCursors.click : MouseCursor.defer,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: enabled ? (_) => setState(() => _pressed = true) : null,
          onTapCancel: () => setState(() => _pressed = false),
          onTapUp: (_) => setState(() => _pressed = false),
          onTap: enabled ? () => widget.onPressed!(context) : null,
          onLongPress: long == null ? null : () => long(context),
          onSecondaryTap: long == null ? null : () => long(context),
          child: Opacity(
            opacity: enabled ? 1 : .5,
            child: Container(
              width: widget.extent,
              height: widget.extent,
              alignment: Alignment.center,
              decoration: BoxDecoration(
                shape: widget.radius == null ? BoxShape.circle : BoxShape.rectangle,
                borderRadius:
                    widget.radius == null ? null : BorderRadius.circular(widget.radius!),
                color: highlighted
                    ? widget.highlight ?? paseoInteractionHighlight(context)
                    : null,
              ),
              child: Icon(
                widget.icon,
                size: widget.iconSize,
                color: _hovered && widget.hoverColor != null ? widget.hoverColor : muted,
              ),
            ),
          ),
        ),
      ),
    );
    if (!isCompactLayout(context)) {
      button = Tooltip(
        message: widget.tooltip,
        waitDuration: const Duration(milliseconds: 300),
        child: button,
      );
    }
    return button;
  }
}

/// Dense tree/navigation row, without Material's large list-tile gutters.
class NavigationRow extends StatelessWidget {
  const NavigationRow({
    super.key,
    required this.title,
    this.leading,
    this.trailing,
    this.subtitle,
    this.selected = false,
    this.onTap,
  });
  final Widget title;
  final Widget? leading;
  final Widget? trailing;
  final Widget? subtitle;
  final bool selected;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) => Material(
    color: selected ? PaseoColors.of(context).selected : Colors.transparent,
    child: InkWell(
      onTap: onTap,
      child: ConstrainedBox(
        constraints: BoxConstraints(
          minHeight: isCompactLayout(context) ? 44 : 36,
        ),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 3),
          child: Row(
            children: [
              if (leading != null) ...[
                IconTheme(
                  data: IconThemeData(
                    size: densityIcon(context, 'sm'),
                    color: PaseoColors.of(context).muted,
                  ),
                  child: leading!,
                ),
                const SizedBox(width: 8),
              ],
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    DefaultTextStyle(
                      style: Theme.of(context).textTheme.bodyMedium!,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      child: title,
                    ),
                    if (subtitle != null)
                      DefaultTextStyle(
                        style: Theme.of(context).textTheme.bodySmall!.copyWith(
                          color: PaseoColors.of(context).muted,
                        ),
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        child: subtitle!,
                      ),
                  ],
                ),
              ),
              ?trailing,
            ],
          ),
        ),
      ),
    ),
  );
}

/// Original `confirmDialog`: the platform's own alert (`Alert.alert` on native, the desktop
/// host's ask dialog elsewhere) — title, plain message, Cancel then the confirm action, and
/// dismissal counts as cancel. Android draws both as text buttons (React Native's
/// `destructive` style only colours iOS); Apple platforms get the Cupertino alert.
Future<bool> confirmAction(
  BuildContext context, {
  required String title,
  required String message,
  String confirmLabel = 'Confirm',
  bool destructive = false,
}) async {
  final cancelLabel = tr(context, 'common.actions.cancel');
  final apple = const {TargetPlatform.iOS, TargetPlatform.macOS}
      .contains(Theme.of(context).platform);
  final result = await showAdaptiveDialog<bool>(
    context: context,
    barrierDismissible: true,
    builder: (dialog) => AlertDialog.adaptive(
      title: Text(title),
      content: Text(message),
      actions: apple
          ? [
              CupertinoDialogAction(
                onPressed: () => Navigator.pop(dialog, false),
                child: Text(cancelLabel),
              ),
              CupertinoDialogAction(
                isDestructiveAction: destructive,
                onPressed: () => Navigator.pop(dialog, true),
                child: Text(confirmLabel),
              ),
            ]
          : [
              TextButton(
                onPressed: () => Navigator.pop(dialog, false),
                child: Text(cancelLabel),
              ),
              TextButton(
                onPressed: () => Navigator.pop(dialog, true),
                child: Text(confirmLabel),
              ),
            ],
    ),
  );
  return result ?? false;
}

Future<String?> promptText(
  BuildContext context, {
  required String title,
  String initialValue = '',
  String? hint,
  bool multiline = false,
}) => showDialog<String>(
  context: context,
  builder: (context) => _TextPromptDialog(
    title: title,
    initialValue: initialValue,
    hint: hint,
    multiline: multiline,
  ),
);

class _TextPromptDialog extends StatefulWidget {
  const _TextPromptDialog({
    required this.title,
    required this.initialValue,
    this.hint,
    required this.multiline,
  });
  final String title;
  final String initialValue;
  final String? hint;
  final bool multiline;
  @override
  State<_TextPromptDialog> createState() => _TextPromptDialogState();
}

class _TextPromptDialogState extends State<_TextPromptDialog> {
  late final _controller = TextEditingController(text: widget.initialValue);
  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: Text(widget.title),
    content: SizedBox(
      width: 480,
      child: TextField(
        controller: _controller,
        autofocus: true,
        minLines: widget.multiline ? 4 : 1,
        maxLines: widget.multiline ? 12 : 1,
        decoration: InputDecoration(hintText: widget.hint),
        onSubmitted: widget.multiline
            ? null
            : (value) => Navigator.pop(context, value),
      ),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.pop(context),
        child: const Text('Cancel'),
      ),
      FilledButton(
        onPressed: () => Navigator.pop(context, _controller.text),
        child: const Text('Save'),
      ),
    ],
  );
}

/// Original `ToastVariant`: the border and default glyph of a toast.
enum ToastVariant { normal, info, success, warning, error }

OverlayEntry? _toastEntry;

/// Original `toast-host` toast: one compact surface0 pill centred under the app header
/// (92% wide, at most 480), replaced by the next toast, dismissed after [duration]
/// (null keeps it until replaced). Hovering pauses the countdown. Returns a dismisser that
/// becomes a no-op once another toast replaced this one.
VoidCallback showToast(
  BuildContext context,
  String message, {
  ToastVariant variant = ToastVariant.normal,
  IconData? icon,
  Duration? duration = const Duration(milliseconds: 2200),
}) {
  final text = message.trim();
  final overlay = context.mounted ? Overlay.maybeOf(context, rootOverlay: true) : null;
  if (text.isEmpty || overlay == null) return () {};
  _toastEntry?.remove();
  late final OverlayEntry entry;
  void remove() {
    if (!identical(_toastEntry, entry)) return;
    _toastEntry = null;
    entry.remove();
  }
  final view = GlobalKey<_ToastViewState>();
  entry = OverlayEntry(
    builder: (_) => _ToastView(
      key: view,
      message: text,
      variant: variant,
      icon: icon,
      duration: duration,
      onDismissed: remove,
    ),
  );
  _toastEntry = entry;
  overlay.insert(entry);
  return () {
    if (identical(_toastEntry, entry)) view.currentState?.dismiss();
  };
}

/// Original `toast.copied`: a success toast with the foreground check glyph.
void showCopiedToast(BuildContext context, [String? label]) => showToast(
  context,
  label == null ? tr(context, 'common.states.copied')
      : tr(context, 'common.states.copiedLabel', args: {'label': label}),
  variant: ToastVariant.success,
  icon: LucideIcons.circleCheck300,
);

/// The one-line message of an error, as the original `toErrorMessage` shows `error.message`:
/// no exception type prefix, platform-channel tuple or trailing stack/details.
String errorMessage(Object error) {
  final raw = switch (error) {
    PlatformException(:final message, :final code) =>
      message?.trim().isNotEmpty == true ? message! : code,
    StateError(:final message) => message,
    FormatException(:final message) => message,
    ArgumentError(:final message) when message != null => '$message',
    _ => error.toString(),
  };
  final line = raw.trim().split('\n').first.trim();
  final unwrapped = RegExp(r'^PlatformException\([^,]*,\s*(.*?)(?:,\s*(?:null|\{.*\}|\[.*\]))*\)$')
      .firstMatch(line)?.group(1) ?? line;
  final message = unwrapped.replaceFirst(
    RegExp(r'^(?:Exception|Bad state|FormatException|Invalid argument\(s\)|[A-Z]\w*(?:Exception|Error)):\s*'),
    '',
  );
  return message.isEmpty ? line : message;
}

/// Original `toast.error`: the error's message in an error toast for 3.2 seconds.
void showError(BuildContext context, Object error) {
  if (!context.mounted) return;
  showToast(
    context,
    errorMessage(error),
    variant: ToastVariant.error,
    duration: const Duration(milliseconds: 3200),
  );
}

/// Original `interactionHighlight`: the hover/press fill of icon-only chrome.
Color paseoInteractionHighlight(BuildContext context) =>
    Theme.of(context).brightness == Brightness.dark
        ? const Color(0x14ffffff)
        : const Color(0x0f000000);

/// The global rect of [anchor], for anchoring a [showPaseoMenu] popover.
Rect paseoAnchorRect(BuildContext anchor) {
  final box = anchor.findRenderObject()! as RenderBox;
  return box.localToGlobal(Offset.zero) & box.size;
}

enum _MenuEntryKind { item, separator, label }

/// One row of an original `MenuSurface` page: an item, a separator or a section label.
class PaseoMenuEntry<T> {
  const PaseoMenuEntry.item({
    required T this.value,
    required String this.label,
    this.icon,
    this.leading,
    this.hint,
    this.enabled = true,
    this.muted = false,
    this.destructive = false,
    this.selected = false,
  }) : _kind = _MenuEntryKind.item;
  const PaseoMenuEntry.separator()
    : _kind = _MenuEntryKind.separator,
      value = null,
      label = null,
      icon = null,
      leading = null,
      hint = null,
      enabled = false,
      muted = false,
      destructive = false,
      selected = false;
  const PaseoMenuEntry.label(String this.label)
    : _kind = _MenuEntryKind.label,
      value = null,
      icon = null,
      leading = null,
      hint = null,
      enabled = false,
      muted = false,
      destructive = false,
      selected = false;

  final _MenuEntryKind _kind;
  final T? value;
  final String? label;
  final IconData? icon;
  final Widget? leading;
  final String? hint;
  final bool enabled;
  /// Original `DropdownMenuItem muted`: still selectable, drawn at 72% opacity with a
  /// muted label. Ignored on disabled rows.
  final bool muted;
  final bool destructive;
  /// Original `DropdownMenuItem selected`: a trailing 16px muted check.
  final bool selected;
}

/// Original `DropdownMenu` / `MenuSurface`: a bottom sheet on compact layouts (surface0,
/// 12px top radius, 1px border, 36x4 surface2 handle, 45% scrim, base-size medium title,
/// content-sized up to 80% of the window) and a 220px popover elsewhere (surface1, 1px
/// borderAccent, 8px radius, md shadow) under [anchor], or its end edge with [alignEnd].
/// [compactSheet] false is the original menu's default `compactMode: "popover"`: the
/// anchored popover on every layout.
Future<T?> showPaseoMenu<T>(
  BuildContext context, {
  required List<PaseoMenuEntry<T>> entries,
  String? title,
  Rect? anchor,
  double width = 220,
  bool alignEnd = false,
  bool compactSheet = true,
}) => showPaseoMenuSurface<T>(
  context,
  anchor: anchor,
  width: width,
  alignEnd: alignEnd,
  compactSheet: compactSheet,
  // Sheets always have a header; popovers never do.
  builder: (surface) => Column(
    mainAxisSize: MainAxisSize.min,
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      if (title != null && compactSheet && isCompactLayout(surface))
        PaseoMenuSheetHeader(title: title),
      _PaseoMenuPage<T>(entries: entries),
    ],
  ),
);

/// The original `MenuSurface` chrome around caller-built content: the compact bottom
/// sheet or the anchored popover of [showPaseoMenu]. [builder] owns the page(s) inside,
/// so a stateful builder can push sub pages (`MenuSubTrigger`) without closing the menu.
Future<T?> showPaseoMenuSurface<T>(
  BuildContext context, {
  required WidgetBuilder builder,
  Rect? anchor,
  double width = 220,
  bool alignEnd = false,
  bool compactSheet = true,
}) {
  final colors = PaseoColors.of(context);
  if (compactSheet && isCompactLayout(context)) {
    return showModalBottomSheet<T>(
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
                child: Builder(builder: builder),
              ),
            ),
          ],
        ),
      ),
    );
  }
  final dark = Theme.of(context).brightness == Brightness.dark;
  return showGeneralDialog<T>(
    context: context,
    barrierDismissible: true,
    barrierLabel: MaterialLocalizations.of(context).modalBarrierDismissLabel,
    barrierColor: Colors.transparent,
    transitionDuration: const Duration(milliseconds: 120),
    transitionBuilder: (_, animation, _, child) =>
        FadeTransition(opacity: animation, child: child),
    pageBuilder: (popover, _, _) => CustomSingleChildLayout(
      delegate: _PopoverLayout(
        anchor ?? Rect.fromCenter(
          center: MediaQuery.sizeOf(popover).center(Offset.zero),
          width: 0,
          height: 0,
        ),
        alignEnd,
        MediaQuery.paddingOf(popover),
      ),
      child: SizedBox(
        width: width,
        child: Material(
          color: colors.surface1,
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(8),
            side: BorderSide(color: colors.borderAccent),
          ),
          clipBehavior: Clip.antiAlias,
          elevation: 0,
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
            child: SingleChildScrollView(child: Builder(builder: builder)),
          ),
        ),
      ),
    ),
  );
}

/// Original `MenuSheetHeader`: 12px insets (4px top, 12px bottom), an optional 24px
/// back button with a muted 18px ChevronLeft, then the base-size medium title.
class PaseoMenuSheetHeader extends StatelessWidget {
  const PaseoMenuSheetHeader({super.key, required this.title, this.onBack});
  final String title;
  final VoidCallback? onBack;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 4, 12, 12),
      child: Row(
        spacing: 8,
        children: [
          if (onBack != null)
            Semantics(
              button: true,
              label: tr(context, 'common.back'),
              child: GestureDetector(
                behavior: HitTestBehavior.opaque,
                onTap: onBack,
                child: SizedBox.square(
                  dimension: 24,
                  child: Icon(
                    LucideIcons.chevronLeft300,
                    size: 18,
                    color: PaseoColors.of(context).muted,
                  ),
                ),
              ),
            ),
          Flexible(
            child: Text(
              title,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: theme.textTheme.bodyMedium!.copyWith(fontWeight: FontWeight.w500),
            ),
          ),
        ],
      ),
    );
  }
}

/// Original `MenuSeparator`: a 1px borderAccent rule with 4px above and below.
class PaseoMenuSeparator extends StatelessWidget {
  const PaseoMenuSeparator({super.key});

  @override
  Widget build(BuildContext context) => Container(
    height: 1,
    margin: const EdgeInsets.symmetric(vertical: 4),
    color: PaseoColors.of(context).borderAccent,
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

/// Original `MenuPage`: 4px vertical inset, rows without their own vertical spacing.
class _PaseoMenuPage<T> extends StatelessWidget {
  const _PaseoMenuPage({required this.entries});
  final List<PaseoMenuEntry<T>> entries;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 4),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          for (final entry in entries)
            switch (entry._kind) {
              _MenuEntryKind.separator => const PaseoMenuSeparator(),
              _MenuEntryKind.label => Padding(
                padding: const EdgeInsets.fromLTRB(12, 8, 12, 4),
                child: Text(
                  entry.label!,
                  style: Theme.of(context).textTheme.bodySmall!.copyWith(color: colors.muted),
                ),
              ),
              _MenuEntryKind.item => PaseoMenuItem(
                label: entry.label!,
                icon: entry.icon,
                leading: entry.leading,
                hint: entry.hint,
                enabled: entry.enabled,
                muted: entry.muted,
                destructive: entry.destructive,
                selected: entry.selected,
                onTap: () => Navigator.of(context).pop(entry.value),
              ),
            },
        ],
      ),
    );
  }
}

/// Original `MenuItem`: a 40/28 row inset 4px from the surface edges, 6px radius, 16px
/// leading slot, base text on an 18px line, surface2 fill when hovered, pressed or
/// [active] (its submenu is open), 50% opacity when disabled and, when [muted] but
/// enabled, 72% opacity with a muted label. [trailing] replaces the
/// selected check (a `MenuSubTrigger` puts its value, indicator and chevron there).
class PaseoMenuItem extends StatefulWidget {
  const PaseoMenuItem({
    super.key,
    required this.label,
    required this.onTap,
    this.icon,
    this.leading,
    this.hint,
    this.trailing,
    this.enabled = true,
    this.muted = false,
    this.destructive = false,
    this.selected = false,
    this.active = false,
  });
  final String label;
  final VoidCallback onTap;
  final IconData? icon;
  final Widget? leading;
  final String? hint;
  final Widget? trailing;
  final bool enabled;
  final bool muted;
  final bool destructive;
  final bool selected;
  final bool active;
  @override
  State<PaseoMenuItem> createState() => _PaseoMenuItemState();
}

class _PaseoMenuItemState extends State<PaseoMenuItem> {
  bool _hovered = false, _pressed = false;

  @override
  Widget build(BuildContext context) {
    final entry = widget;
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final text = theme.textTheme.bodyMedium!;
    final muted = entry.muted && entry.enabled;
    final foreground = muted
        ? colors.muted
        : entry.destructive
        ? theme.colorScheme.error
        : theme.colorScheme.onSurface;
    return Opacity(
      opacity: !entry.enabled ? .5 : muted ? .72 : 1,
      child: MouseRegion(
        cursor: entry.enabled ? SystemMouseCursors.click : MouseCursor.defer,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: entry.enabled ? (_) => setState(() => _pressed = true) : null,
          onTapCancel: () => setState(() => _pressed = false),
          onTap: entry.enabled
              ? () {
                  setState(() => _pressed = false);
                  entry.onTap();
                }
              : null,
          child: Container(
            constraints: BoxConstraints(minHeight: isCompactLayout(context) ? 40 : 28),
            margin: const EdgeInsets.symmetric(horizontal: 4),
            padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
            decoration: BoxDecoration(
              color: entry.enabled && (_hovered || _pressed || entry.active)
                  ? colors.surface2
                  : null,
              border: Border.all(color: Colors.transparent),
              borderRadius: BorderRadius.circular(6),
            ),
            child: Row(
              spacing: 8,
              children: [
                if (entry.leading != null || entry.icon != null)
                  SizedBox(
                    width: 16,
                    child: Center(
                      child: entry.leading ??
                          Icon(
                            entry.icon,
                            size: 16,
                            color: entry.destructive ? theme.colorScheme.error : colors.muted,
                          ),
                    ),
                  ),
                Expanded(
                  child: Text(
                    entry.label,
                    maxLines: 2,
                    overflow: TextOverflow.ellipsis,
                    style: text.copyWith(
                      color: foreground,
                      height: 18 / (text.fontSize ?? 14),
                    ),
                  ),
                ),
                if (entry.hint != null)
                  Text(
                    entry.hint!,
                    style: theme.textTheme.bodySmall!.copyWith(color: colors.muted),
                  ),
                if (entry.trailing != null)
                  entry.trailing!
                else if (entry.selected)
                  Icon(LucideIcons.check300, size: 16, color: colors.muted),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// Original `MenuSubTrigger`: a row that opens a sub page instead of acting, showing the
/// decision's current [value], an accent dot while it holds a non-default setting
/// ([indicator]) and a muted 14px ChevronRight.
class PaseoMenuSubTrigger extends StatelessWidget {
  const PaseoMenuSubTrigger({
    super.key,
    required this.label,
    required this.onOpen,
    this.value,
    this.indicator = false,
    this.leading,
    this.active = false,
  });
  final String label;
  final VoidCallback onOpen;
  final String? value;
  final bool indicator;
  final Widget? leading;
  final bool active;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    return PaseoMenuItem(
      label: label,
      leading: leading,
      active: active,
      onTap: onOpen,
      trailing: Row(
        mainAxisSize: MainAxisSize.min,
        spacing: 6,
        children: [
          if (value != null)
            Text(
              value!,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: theme.textTheme.bodyMedium!.copyWith(color: colors.muted),
            ),
          if (indicator)
            Container(
              width: 6,
              height: 6,
              decoration: BoxDecoration(color: colors.accent, shape: BoxShape.circle),
            ),
          Icon(LucideIcons.chevronRight300, size: 14, color: colors.muted),
        ],
      ),
    );
  }
}

class _ToastView extends StatefulWidget {
  const _ToastView({
    super.key,
    required this.message,
    required this.variant,
    required this.icon,
    required this.duration,
    required this.onDismissed,
  });
  final String message;
  final ToastVariant variant;
  final IconData? icon;
  final Duration? duration;
  final VoidCallback onDismissed;
  @override
  State<_ToastView> createState() => _ToastViewState();
}

class _ToastViewState extends State<_ToastView> with SingleTickerProviderStateMixin {
  // Original FadeIn/FadeOut.duration(140).
  late final _fade = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 140),
  )..forward();
  Timer? _timer;
  DateTime? _deadline;
  Duration _remaining = Duration.zero;

  @override
  void initState() {
    super.initState();
    _schedule(widget.duration);
  }

  void _schedule(Duration? duration) {
    _timer?.cancel();
    if (duration == null) return;
    _remaining = duration;
    _deadline = DateTime.now().add(duration);
    _timer = Timer(duration, dismiss);
  }

  void _pause() {
    final deadline = _deadline;
    if (deadline != null) {
      final left = deadline.difference(DateTime.now());
      _remaining = left.isNegative ? Duration.zero : left;
    }
    _deadline = null;
    _timer?.cancel();
  }

  void _resume() {
    if (widget.duration == null) return;
    _schedule(_remaining == Duration.zero ? widget.duration : _remaining);
  }

  void dismiss() {
    _timer?.cancel();
    if (!mounted) return;
    _fade.reverse().whenComplete(widget.onDismissed);
  }

  @override
  void dispose() {
    _timer?.cancel();
    _fade.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final dark = theme.brightness == Brightness.dark;
    final compact = isCompactLayout(context);
    // Original app-shell placement: safe area + mobile header top padding + header height
    // (56 compact / 36 wide), then spacing[2]; spacing[4] side insets.
    final top = MediaQuery.paddingOf(context).top + (compact ? 8 + 56 : 36) + 8;
    const info = Color(0xff93c5fd);
    const amber = Color(0xfff59e0b);
    final border = switch (widget.variant) {
      ToastVariant.info => info,
      ToastVariant.warning => amber,
      ToastVariant.error => theme.colorScheme.error,
      _ => theme.dividerColor,
    };
    final (IconData? glyph, Color glyphColor) = switch (widget.variant) {
      ToastVariant.info => (LucideIcons.info300, info),
      ToastVariant.success => (LucideIcons.circleCheck300, theme.colorScheme.primary),
      ToastVariant.warning => (LucideIcons.triangleAlert300, amber),
      ToastVariant.error => (LucideIcons.triangleAlert300, theme.colorScheme.error),
      ToastVariant.normal => (null, theme.colorScheme.onSurface),
    };
    final shownIcon = widget.icon ?? glyph;
    return Positioned(
      top: top,
      left: 16,
      right: 16,
      child: FractionallySizedBox(
        widthFactor: .92,
        child: Center(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 480),
            child: FadeTransition(
              opacity: _fade,
              child: Semantics(
                liveRegion: true,
                container: true,
                child: MouseRegion(
                  onEnter: (_) => _pause(),
                  onExit: (_) => _resume(),
                  child: Material(
                    type: MaterialType.transparency,
                    child: Container(
                      key: const ValueKey('app-toast'),
                      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
                      decoration: BoxDecoration(
                        color: colors.surface0,
                        borderRadius: BorderRadius.circular(16),
                        border: Border.all(color: border),
                        // Original theme.shadow.md.
                        boxShadow: [
                          BoxShadow(
                            color: dark ? const Color(0x33000000) : const Color(0x0a000000),
                            offset: const Offset(0, 4),
                            blurRadius: dark ? 8 : 16,
                          ),
                        ],
                      ),
                      child: Row(
                        mainAxisSize: MainAxisSize.min,
                        spacing: 8,
                        children: [
                          if (shownIcon != null)
                            Icon(
                              shownIcon,
                              size: 18,
                              // A caller's glyph (original `copied`) draws in foreground.
                              color: widget.icon != null
                                  ? theme.colorScheme.onSurface
                                  : glyphColor,
                            ),
                          Flexible(
                            child: Text(
                              widget.message,
                              key: const ValueKey('app-toast-message'),
                              style: theme.textTheme.bodyMedium!.copyWith(
                                color: theme.colorScheme.onSurface,
                              ),
                            ),
                          ),
                        ],
                      ),
                    ),
                  ),
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

class ErrorState extends StatelessWidget {
  const ErrorState({super.key, required this.error, this.onRetry});
  final Object error;
  final VoidCallback? onRetry;
  @override
  Widget build(BuildContext context) => Center(
    child: Padding(
      padding: const EdgeInsets.all(24),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 640),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(
              LucideIcons.circleAlert300,
              color: Theme.of(context).colorScheme.error,
            ),
            const SizedBox(height: 12),
            SelectableText(error.toString(), textAlign: TextAlign.center),
            if (onRetry != null)
              Padding(
                padding: const EdgeInsets.only(top: 16),
                child: OutlinedButton.icon(
                  onPressed: onRetry,
                  icon: const Icon(LucideIcons.rotateCw300),
                  label: const Text('Retry'),
                ),
              ),
          ],
        ),
      ),
    ),
  );
}

class LoadingState extends StatelessWidget {
  const LoadingState({super.key, this.label});
  final String? label;
  @override
  Widget build(BuildContext context) => Center(
    child: Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        const SizedBox(
          width: 24,
          height: 24,
          child: CircularProgressIndicator(strokeWidth: 2),
        ),
        if (label != null)
          Padding(padding: const EdgeInsets.only(top: 12), child: Text(label!)),
      ],
    ),
  );
}

class EmptyState extends StatelessWidget {
  const EmptyState({super.key, required this.title, this.message, this.action});
  final String title;
  final String? message;
  final Widget? action;
  @override
  Widget build(BuildContext context) => Center(
    child: Padding(
      padding: const EdgeInsets.all(24),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(
            title,
            style: Theme.of(context).textTheme.titleMedium,
            textAlign: TextAlign.center,
          ),
          if (message != null)
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Text(message!, textAlign: TextAlign.center),
            ),
          if (action != null)
            Padding(padding: const EdgeInsets.only(top: 16), child: action!),
        ],
      ),
    ),
  );
}

class JsonDetails extends StatelessWidget {
  const JsonDetails({super.key, required this.value});
  final Object? value;
  @override
  Widget build(BuildContext context) {
    final text = value is String
        ? value as String
        : const JsonEncoder.withIndent('  ').convert(value);
    return Stack(
      children: [
        SingleChildScrollView(
          padding: const EdgeInsets.all(12),
          child: SelectableText(
            text,
            style: Theme.of(context).textTheme.bodySmall
                ?.copyWith(fontFamily: 'monospace'),
          ),
        ),
        Positioned(
          top: 0,
          right: 0,
          child: IconButton(
            tooltip: 'Copy',
            icon: const Icon(LucideIcons.copy300, size: 16),
            onPressed: () => Clipboard.setData(ClipboardData(text: text)),
          ),
        ),
      ],
    );
  }
}

class AsyncButton extends StatefulWidget {
  const AsyncButton({
    super.key,
    required this.onPressed,
    required this.child,
    this.icon,
    this.destructive = false,
  });
  final Future<void> Function() onPressed;
  final Widget child;
  final IconData? icon;
  final bool destructive;
  @override
  State<AsyncButton> createState() => _AsyncButtonState();
}

class _AsyncButtonState extends State<AsyncButton> {
  bool _busy = false;
  Future<void> _run() async {
    if (_busy) return;
    setState(() => _busy = true);
    try {
      await widget.onPressed();
    } catch (error) {
      if (mounted) showError(context, error);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) => FilledButton(
    onPressed: _busy ? null : _run,
    style: widget.destructive
        ? FilledButton.styleFrom(
            backgroundColor: Theme.of(context).colorScheme.error,
          )
        : null,
    child: Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        if (_busy)
          const SizedBox(
            width: 16,
            height: 16,
            child: CircularProgressIndicator(strokeWidth: 2),
          )
        else if (widget.icon != null)
          Icon(widget.icon, size: 18),
        if (_busy || widget.icon != null) const SizedBox(width: 8),
        widget.child,
      ],
    ),
  );
}
