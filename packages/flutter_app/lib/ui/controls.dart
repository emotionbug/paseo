import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import 'common.dart';
import 'theme.dart';

/// Original `control-geometry.ts` sizes shared by buttons, fields and
/// segmented controls: tight 28, compact 32, field 44.
enum PaseoControlSize { xs, sm, md, lg }

enum PaseoButtonVariant { primary, secondary, outline, ghost, destructive }

double paseoButtonHeight(PaseoControlSize size) => switch (size) {
  PaseoControlSize.xs => 28,
  PaseoControlSize.sm => 32,
  _ => 44,
};

double paseoButtonIconSize(PaseoControlSize size) => switch (size) {
  PaseoControlSize.xs => 12,
  PaseoControlSize.sm => 14,
  PaseoControlSize.md => 16,
  PaseoControlSize.lg => 20,
};

/// The original `accentForeground`: white, except the near-white zinc accent.
Color paseoAccentForeground(BuildContext context) =>
    Theme.of(context).colorScheme.onTertiary;

/// Original `components/ui/button.tsx`: 1px bordered pill with an 8px icon
/// gap, 0.85 pressed and 0.5 disabled opacity, normal-weight base text.
class PaseoButton extends StatefulWidget {
  const PaseoButton({
    super.key,
    this.label,
    this.icon,
    this.leading,
    this.trailing,
    this.onPressed,
    this.variant = PaseoButtonVariant.secondary,
    this.size = PaseoControlSize.md,
    this.loading = false,
    this.tooltip,
    this.semanticLabel,
    this.expand = false,
    this.foreground,
  });

  final String? label;
  final IconData? icon;
  final Widget? leading;
  final Widget? trailing;
  final VoidCallback? onPressed;
  final PaseoButtonVariant variant;
  final PaseoControlSize size;
  final bool loading;
  final String? tooltip;
  final String? semanticLabel;

  /// Fill the available width (the original `style={{ flex: 1 }}` buttons).
  final bool expand;

  /// Overrides the text/icon colour (original `textStyle` color overrides).
  final Color? foreground;

  @override
  State<PaseoButton> createState() => _PaseoButtonState();
}

class _PaseoButtonState extends State<PaseoButton> {
  bool hovered = false, pressed = false;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final disabled = widget.onPressed == null || widget.loading;
    final (Color background, Color border, Color foreground) = switch (widget.variant) {
      PaseoButtonVariant.primary => (
        colors.accent, colors.accent, paseoAccentForeground(context),
      ),
      PaseoButtonVariant.secondary => (
        colors.surface3, colors.surface3, theme.colorScheme.onSurface,
      ),
      PaseoButtonVariant.outline => (
        Colors.transparent, colors.borderAccent, theme.colorScheme.onSurface,
      ),
      PaseoButtonVariant.ghost => (
        Colors.transparent,
        Colors.transparent,
        hovered && !disabled ? theme.colorScheme.onSurface : colors.muted,
      ),
      PaseoButtonVariant.destructive => (
        theme.colorScheme.error, theme.colorScheme.error,
        theme.brightness == Brightness.dark ? Colors.white : colors.surface0,
      ),
    };
    final fg = widget.foreground ?? foreground;
    final (double padding, double radius) = switch (widget.size) {
      PaseoControlSize.xs || PaseoControlSize.sm => (12.0, 12.0),
      PaseoControlSize.md => (16.0, 16.0),
      PaseoControlSize.lg => (24.0, 16.0),
    };
    final iconSize = paseoButtonIconSize(widget.size);
    final Widget? lead = widget.loading
        ? SizedBox.square(
            dimension: iconSize,
            child: CircularProgressIndicator(strokeWidth: 1.5, color: fg),
          )
        : widget.leading ??
              (widget.icon == null ? null : Icon(widget.icon, size: iconSize, color: fg));
    final textStyle = theme.textTheme.bodyMedium!.copyWith(
      color: fg,
      fontWeight: FontWeight.normal,
      fontSize: type.uiSize(widget.size == PaseoControlSize.xs ? 12 : 14),
      height: 1.25,
    );
    Widget content = Row(
      mainAxisSize: widget.expand ? MainAxisSize.max : MainAxisSize.min,
      mainAxisAlignment: MainAxisAlignment.center,
      spacing: 8,
      children: [
        ?lead,
        if (widget.label != null)
          Flexible(
            child: Text(
              widget.label!,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: textStyle,
            ),
          ),
        ?widget.trailing,
      ],
    );
    content = IconTheme.merge(
      data: IconThemeData(color: fg, size: iconSize),
      child: DefaultTextStyle.merge(style: textStyle, child: content),
    );
    Widget button = MouseRegion(
      cursor: disabled ? SystemMouseCursors.basic : SystemMouseCursors.click,
      onEnter: (_) => setState(() => hovered = true),
      onExit: (_) => setState(() => hovered = false),
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTapDown: disabled ? null : (_) => setState(() => pressed = true),
        onTapCancel: () => setState(() => pressed = false),
        onTapUp: disabled ? null : (_) => setState(() => pressed = false),
        onTap: disabled ? null : widget.onPressed,
        child: Opacity(
          opacity: disabled ? .5 : pressed ? .85 : 1,
          child: Container(
            constraints: BoxConstraints(minHeight: paseoButtonHeight(widget.size)),
            padding: EdgeInsets.symmetric(horizontal: padding),
            alignment: widget.expand ? Alignment.center : null,
            decoration: BoxDecoration(
              color: background,
              border: Border.all(color: border),
              borderRadius: BorderRadius.circular(radius),
            ),
            child: content,
          ),
        ),
      ),
    );
    button = Semantics(
      button: true,
      enabled: !disabled,
      label: widget.semanticLabel ?? widget.tooltip,
      child: button,
    );
    if (widget.tooltip != null) {
      button = Tooltip(message: widget.tooltip!, child: button);
    }
    return button;
  }
}

/// Original `components/ui/switch.tsx`: 34x20 track, 16px thumb, 180ms ease,
/// surface3 → accent track, white → accentForeground thumb.
class PaseoSwitch extends StatelessWidget {
  const PaseoSwitch({
    super.key,
    required this.value,
    this.onChanged,
    this.semanticLabel,
  });
  final bool value;
  final ValueChanged<bool>? onChanged;
  final String? semanticLabel;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final disabled = onChanged == null;
    const duration = Duration(milliseconds: 180);
    return Semantics(
      toggled: value,
      enabled: !disabled,
      label: semanticLabel,
      child: MouseRegion(
        cursor: disabled ? SystemMouseCursors.basic : SystemMouseCursors.click,
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: disabled ? null : () => onChanged!(!value),
          child: Padding(
            // Original hitSlop 8 around a 32px-tall control.
            padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 6),
            child: Opacity(
              opacity: disabled ? .5 : 1,
              child: AnimatedContainer(
                duration: duration,
                curve: Curves.easeInOut,
                width: 34,
                height: 20,
                padding: const EdgeInsets.all(2),
                decoration: BoxDecoration(
                  color: value ? colors.accent : colors.surface3,
                  borderRadius: BorderRadius.circular(10),
                ),
                child: AnimatedAlign(
                  duration: duration,
                  curve: Curves.easeInOut,
                  alignment: value ? Alignment.centerRight : Alignment.centerLeft,
                  child: AnimatedContainer(
                    duration: duration,
                    width: 16,
                    height: 16,
                    decoration: BoxDecoration(
                      shape: BoxShape.circle,
                      color: value ? paseoAccentForeground(context) : Colors.white,
                      boxShadow: const [
                        BoxShadow(
                          color: Color(0x40000000),
                          offset: Offset(0, 1),
                          blurRadius: 2,
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
    );
  }
}

class PaseoSegment<T> {
  const PaseoSegment(this.value, this.label, {this.icon, this.enabled = true});
  final T value;
  final String label;
  final IconData? icon;
  final bool enabled;
}

/// Original `components/ui/segmented-control.tsx`: transparent row, 4px gap,
/// selected segment surface3, hover surface2, muted → foreground label.
class PaseoSegmentedControl<T> extends StatelessWidget {
  const PaseoSegmentedControl({
    super.key,
    required this.segments,
    required this.value,
    required this.onChanged,
    this.size = PaseoControlSize.md,
    this.hideLabels = false,
    this.expand = false,
  });
  final List<PaseoSegment<T>> segments;
  final T value;
  final ValueChanged<T>? onChanged;
  final PaseoControlSize size;
  final bool hideLabels;
  final bool expand;

  @override
  Widget build(BuildContext context) {
    final children = [
      for (final segment in segments)
        _SegmentItem<T>(
          segment: segment,
          selected: segment.value == value,
          size: size,
          hideLabels: hideLabels,
          onTap: onChanged == null || !segment.enabled || segment.value == value
              ? null
              : () => onChanged!(segment.value),
        ),
    ];
    // Original container: transparent, 4px gap, the control height (28/32/44)
    // with each segment inset 2/2/3px inside it.
    return ConstrainedBox(
      constraints: BoxConstraints(minHeight: paseoButtonHeight(size)),
      child: Row(
        mainAxisSize: expand ? MainAxisSize.max : MainAxisSize.min,
        spacing: 4,
        children: expand
            ? [for (final child in children) Expanded(child: child)]
            : children,
      ),
    );
  }
}

class _SegmentItem<T> extends StatefulWidget {
  const _SegmentItem({
    required this.segment,
    required this.selected,
    required this.size,
    required this.hideLabels,
    required this.onTap,
  });
  final PaseoSegment<T> segment;
  final bool selected, hideLabels;
  final PaseoControlSize size;
  final VoidCallback? onTap;
  @override
  State<_SegmentItem<T>> createState() => _SegmentItemState<T>();
}

class _SegmentItemState<T> extends State<_SegmentItem<T>> {
  bool hovered = false, pressed = false;
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final (double height, double padding, double radius, double font, double icon) =
        switch (widget.size) {
          PaseoControlSize.xs => (24.0, 8.0, 12.0, 12.0, 12.0),
          PaseoControlSize.sm => (28.0, 8.0, 12.0, 14.0, 14.0),
          _ => (38.0, 12.0, 16.0, 14.0, 16.0),
        };
    final fg = widget.selected ? theme.colorScheme.onSurface : colors.muted;
    final background = widget.selected || pressed
        ? colors.surface3
        : hovered && widget.onTap != null
        ? colors.surface2
        : Colors.transparent;
    return Semantics(
      button: true,
      selected: widget.selected,
      label: widget.segment.label,
      child: MouseRegion(
        cursor: widget.onTap == null ? SystemMouseCursors.basic : SystemMouseCursors.click,
        onEnter: (_) => setState(() => hovered = true),
        onExit: (_) => setState(() => hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: widget.onTap == null ? null : (_) => setState(() => pressed = true),
          onTapCancel: () => setState(() => pressed = false),
          onTapUp: (_) => setState(() => pressed = false),
          onTap: widget.onTap,
          child: Opacity(
            opacity: widget.segment.enabled ? 1 : .5,
            child: Container(
              constraints: BoxConstraints(minHeight: height),
              padding: EdgeInsets.symmetric(horizontal: padding),
              decoration: BoxDecoration(
                color: background,
                borderRadius: BorderRadius.circular(radius),
              ),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                mainAxisAlignment: MainAxisAlignment.center,
                spacing: 4,
                children: [
                  if (widget.segment.icon != null)
                    Icon(widget.segment.icon, size: icon, color: fg),
                  if (!widget.hideLabels)
                    Flexible(
                      child: Text(
                        widget.segment.label,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: theme.textTheme.bodyMedium!.copyWith(
                          color: fg,
                          fontSize: type.uiSize(font),
                          fontWeight: FontWeight.normal,
                        ),
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

/// One choice in a [PaseoSelectField] / [showPaseoSelect].
class PaseoSelectOption<T> {
  const PaseoSelectOption(
    this.value,
    this.label, {
    this.description,
    this.leading,
    this.trailing,
    this.semanticLabel,
    this.enabled = true,
    this.separatorBefore = false,
  });
  final T value;
  final String label;
  final String? description;
  final Widget? leading;

  /// Original `ComboboxItem` `trailingSlot` (e.g. a branch's ahead/behind).
  final Widget? trailing;

  /// Original `accessibilityLabel`; defaults to [label].
  final String? semanticLabel;
  final bool enabled;
  final bool separatorBefore;
}

/// Original `Combobox`: a bottom sheet (surface0, 16px radius, base medium
/// title, 36px rows inset 4px) on compact layouts; an anchored surface0
/// popover (8px radius, border, md shadow, 400px max) on wide layouts.
Future<T?> showPaseoSelect<T>({
  required BuildContext context,
  required List<PaseoSelectOption<T>> options,
  required String title,
  T? value,
  bool searchable = false,
  String? searchPlaceholder,
  String emptyText = 'No options',
  double desktopMinWidth = 220,
}) {
  final colors = PaseoColors.of(context);
  if (isCompactLayout(context)) {
    return showModalBottomSheet<T>(
      context: context,
      isScrollControlled: true,
      useSafeArea: true,
      showDragHandle: true,
      backgroundColor: colors.surface0,
      barrierColor: const Color(0x73000000),
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.vertical(top: Radius.circular(16)),
      ),
      builder: (context) => Padding(
        padding: EdgeInsets.only(bottom: MediaQuery.viewInsetsOf(context).bottom),
        child: _SelectList<T>(
          options: options,
          value: value,
          title: title,
          searchable: searchable,
          searchPlaceholder: searchPlaceholder,
          emptyText: emptyText,
          sheet: true,
        ),
      ),
    );
  }
  final box = context.findRenderObject() as RenderBox?;
  final overlay = Overlay.of(context).context.findRenderObject() as RenderBox;
  final anchor = box == null
      ? Rect.fromCenter(center: overlay.size.center(Offset.zero), width: 0, height: 0)
      : box.localToGlobal(Offset.zero, ancestor: overlay) & box.size;
  return showGeneralDialog<T>(
    context: context,
    barrierDismissible: true,
    barrierLabel: 'Dismiss',
    barrierColor: Colors.transparent,
    transitionDuration: Duration.zero,
    pageBuilder: (context, _, _) {
      final width = anchor.width < desktopMinWidth ? desktopMinWidth : anchor.width;
      final screen = MediaQuery.sizeOf(context);
      final below = screen.height - anchor.bottom - 8;
      final openUp = below < 240 && anchor.top > below;
      final left = (anchor.left + width > screen.width - 8)
          ? (screen.width - 8 - width).clamp(8.0, double.infinity)
          : anchor.left;
      return Stack(
        children: [
          Positioned(
            left: left,
            width: width,
            top: openUp ? null : anchor.bottom + 4,
            bottom: openUp ? screen.height - anchor.top + 4 : null,
            child: ConstrainedBox(
              constraints: BoxConstraints(
                maxHeight: (openUp ? anchor.top - 12 : below - 4).clamp(120.0, 400.0),
              ),
              child: Material(
                color: colors.surface0,
                elevation: 0,
                shape: RoundedRectangleBorder(
                  borderRadius: BorderRadius.circular(8),
                  side: BorderSide(color: Theme.of(context).dividerColor),
                ),
                shadowColor: Colors.transparent,
                clipBehavior: Clip.antiAlias,
                child: DecoratedBox(
                  decoration: const BoxDecoration(
                    boxShadow: [
                      BoxShadow(color: Color(0x33000000), offset: Offset(0, 4), blurRadius: 8),
                    ],
                  ),
                  child: _SelectList<T>(
                    options: options,
                    value: value,
                    title: title,
                    searchable: searchable,
                    searchPlaceholder: searchPlaceholder,
                    emptyText: emptyText,
                    sheet: false,
                  ),
                ),
              ),
            ),
          ),
        ],
      );
    },
  );
}

class _SelectList<T> extends StatefulWidget {
  const _SelectList({
    required this.options,
    required this.value,
    required this.title,
    required this.searchable,
    required this.searchPlaceholder,
    required this.emptyText,
    required this.sheet,
  });
  final List<PaseoSelectOption<T>> options;
  final T? value;
  final String title, emptyText;
  final String? searchPlaceholder;
  final bool searchable, sheet;
  @override
  State<_SelectList<T>> createState() => _SelectListState<T>();
}

class _SelectListState<T> extends State<_SelectList<T>> {
  String query = '';
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final needle = query.trim().toLowerCase();
    final visible = needle.isEmpty
        ? widget.options
        : widget.options
              .where((o) =>
                  o.label.toLowerCase().contains(needle) ||
                  (o.description?.toLowerCase().contains(needle) ?? false))
              .toList();
    final rows = <Widget>[
      for (final option in visible) ...[
        if (option.separatorBefore && needle.isEmpty)
          Divider(height: 9, thickness: 1, color: theme.dividerColor),
        _SelectItem<T>(
          option: option,
          selected: option.value == widget.value,
          sheet: widget.sheet,
          onTap: () => Navigator.pop(context, option.value),
        ),
      ],
    ];
    final search = widget.searchable
        ? Container(
            margin: widget.sheet ? const EdgeInsets.symmetric(horizontal: 4) : null,
            padding: const EdgeInsets.symmetric(horizontal: 12),
            decoration: BoxDecoration(
              color: colors.surface1,
              border: Border(bottom: BorderSide(color: theme.dividerColor)),
            ),
            child: Row(
              spacing: 8,
              children: [
                Icon(LucideIcons.search300, size: 16, color: colors.muted),
                Expanded(
                  child: TextField(
                    autofocus: !widget.sheet,
                    onChanged: (value) => setState(() => query = value),
                    style: theme.textTheme.bodyMedium!.copyWith(fontSize: type.uiSize(14)),
                    decoration: InputDecoration(
                      isCollapsed: true,
                      filled: false,
                      border: InputBorder.none,
                      enabledBorder: InputBorder.none,
                      focusedBorder: InputBorder.none,
                      contentPadding: const EdgeInsets.symmetric(vertical: 12),
                      hintText: widget.searchPlaceholder ?? 'Search…',
                    ),
                  ),
                ),
              ],
            ),
          )
        : null;
    final list = rows.isEmpty
        ? Padding(
            padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
            child: Text(
              widget.emptyText,
              style: theme.textTheme.bodyMedium!.copyWith(color: colors.muted),
            ),
          )
        : ListView(
            shrinkWrap: true,
            padding: widget.sheet
                ? const EdgeInsets.fromLTRB(8, 4, 8, 32)
                : const EdgeInsets.symmetric(vertical: 4),
            children: rows,
          );
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        if (widget.sheet)
          Padding(
            padding: const EdgeInsets.fromLTRB(24, 0, 24, 8),
            child: Text(
              widget.title,
              style: theme.textTheme.bodyMedium!.copyWith(
                fontWeight: FontWeight.w500,
                fontSize: type.uiSize(14),
              ),
            ),
          ),
        ?search,
        Flexible(child: list),
      ],
    );
  }
}

class _SelectItem<T> extends StatefulWidget {
  const _SelectItem({
    required this.option,
    required this.selected,
    required this.sheet,
    required this.onTap,
  });
  final PaseoSelectOption<T> option;
  final bool selected, sheet;
  final VoidCallback onTap;
  @override
  State<_SelectItem<T>> createState() => _SelectItemState<T>();
}

class _SelectItemState<T> extends State<_SelectItem<T>> {
  bool hovered = false, pressed = false;
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final option = widget.option;
    final enabled = option.enabled;
    final Widget row = MouseRegion(
      cursor: enabled ? SystemMouseCursors.click : SystemMouseCursors.basic,
      onEnter: (_) => setState(() => hovered = true),
      onExit: (_) => setState(() => hovered = false),
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTapDown: enabled ? (_) => setState(() => pressed = true) : null,
        onTapCancel: () => setState(() => pressed = false),
        onTapUp: (_) => setState(() => pressed = false),
        onTap: enabled ? widget.onTap : null,
        child: Opacity(
          opacity: enabled ? 1 : .55,
          child: Container(
            margin: widget.sheet ? const EdgeInsets.fromLTRB(4, 0, 4, 4) : null,
            constraints: const BoxConstraints(minHeight: 36),
            padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
            color: hovered || pressed ? colors.surface1 : Colors.transparent,
            child: Row(
              spacing: 8,
              children: [
                if (option.leading != null)
                  SizedBox(width: 16, child: Center(child: option.leading)),
                Expanded(
                  child: Row(
                    crossAxisAlignment: CrossAxisAlignment.baseline,
                    textBaseline: TextBaseline.alphabetic,
                    spacing: 8,
                    children: [
                      Flexible(
                        flex: 0,
                        child: Text(
                          option.label,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: theme.textTheme.bodyMedium!.copyWith(
                            fontSize: type.uiSize(14),
                          ),
                        ),
                      ),
                      if (option.description != null)
                        Flexible(
                          child: Text(
                            option.description!,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: theme.textTheme.bodySmall!.copyWith(
                              color: colors.muted,
                              fontSize: type.uiSize(12),
                            ),
                          ),
                        ),
                    ],
                  ),
                ),
                ?option.trailing,
                if (widget.selected)
                  Icon(LucideIcons.check300, size: 16, color: colors.muted),
              ],
            ),
          ),
        ),
      ),
    );
    if (option.semanticLabel == null) return row;
    return Semantics(
      button: true,
      enabled: enabled,
      selected: widget.selected,
      label: option.semanticLabel,
      onTap: enabled ? widget.onTap : null,
      excludeSemantics: true,
      child: row,
    );
  }
}

/// Original `SelectFieldTrigger`: surface2 field (sm 32/6px, md 44/8px),
/// transparent border that turns borderAccent on hover/open, base text,
/// muted 16px chevron.
class PaseoSelectField<T> extends StatefulWidget {
  const PaseoSelectField({
    super.key,
    required this.value,
    required this.options,
    required this.onChanged,
    required this.title,
    this.placeholder = 'Select…',
    this.size = PaseoControlSize.md,
    this.leading,
    this.loading = false,
    this.searchable = false,
    this.searchPlaceholder,
    this.emptyText = 'No options',
  });
  final T? value;
  final List<PaseoSelectOption<T>> options;
  final ValueChanged<T>? onChanged;
  final String title, placeholder, emptyText;
  final String? searchPlaceholder;
  final PaseoControlSize size;
  final Widget? leading;
  final bool loading, searchable;
  @override
  State<PaseoSelectField<T>> createState() => _PaseoSelectFieldState<T>();
}

class _PaseoSelectFieldState<T> extends State<PaseoSelectField<T>> {
  bool hovered = false, open = false;

  Future<void> pick(BuildContext anchor) async {
    setState(() => open = true);
    final picked = await showPaseoSelect<T>(
      context: anchor,
      options: widget.options,
      title: widget.title,
      value: widget.value,
      searchable: widget.searchable,
      searchPlaceholder: widget.searchPlaceholder,
      emptyText: widget.emptyText,
    );
    if (!mounted) return;
    setState(() => open = false);
    if (picked is T && picked != widget.value) widget.onChanged?.call(picked);
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final selected = widget.options.where((o) => o.value == widget.value).firstOrNull;
    final disabled = widget.onChanged == null;
    final small = widget.size == PaseoControlSize.sm || widget.size == PaseoControlSize.xs;
    final active = open || hovered;
    return Semantics(
      button: true,
      label: '${widget.title} (${selected?.label ?? widget.placeholder})',
      child: MouseRegion(
        cursor: disabled ? SystemMouseCursors.basic : SystemMouseCursors.click,
        onEnter: (_) => setState(() => hovered = true),
        onExit: (_) => setState(() => hovered = false),
        child: Builder(
          builder: (anchor) => GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTap: disabled ? null : () => pick(anchor),
            child: Opacity(
              opacity: disabled && !widget.loading ? .5 : 1,
              child: Container(
                constraints: BoxConstraints(minHeight: small ? 32 : 44),
                padding: EdgeInsets.symmetric(horizontal: small ? 12 : 16),
                decoration: BoxDecoration(
                  color: colors.surface2,
                  border: Border.all(
                    color: active && !disabled ? colors.borderAccent : Colors.transparent,
                  ),
                  borderRadius: BorderRadius.circular(small ? 6 : 8),
                ),
                child: Row(
                  spacing: 8,
                  children: [
                    ?widget.leading ?? selected?.leading,
                    Expanded(
                      child: Text(
                        selected?.label ?? widget.placeholder,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: theme.textTheme.bodyMedium!.copyWith(
                          fontSize: type.uiSize(14),
                          height: 1.4,
                          color: selected == null ? colors.muted : null,
                        ),
                      ),
                    ),
                    if (widget.loading)
                      SizedBox.square(
                        dimension: 14,
                        child: CircularProgressIndicator(strokeWidth: 1.5, color: colors.muted),
                      ),
                    Icon(LucideIcons.chevronDown300, size: 16, color: colors.muted),
                  ],
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// Original `DropdownTrigger` inside a `DropdownMenu` (`components/settings`
/// `SettingsSelect`): an outline button (borderAccent, button geometry) with the
/// label and a chevron, opening the menu's default popover (`compactMode:
/// "popover"`, side bottom, align end) on every layout.
class PaseoDropdownTrigger<T> extends StatefulWidget {
  const PaseoDropdownTrigger({
    super.key,
    required this.value,
    required this.options,
    required this.onChanged,
    required this.title,
    this.size = PaseoControlSize.sm,
    this.leading,
    this.menuWidth = 220,
  });
  final T value;
  final List<PaseoSelectOption<T>> options;
  final ValueChanged<T>? onChanged;
  final String title;
  final PaseoControlSize size;
  final Widget? leading;
  final double menuWidth;
  @override
  State<PaseoDropdownTrigger<T>> createState() => _PaseoDropdownTriggerState<T>();
}

class _PaseoDropdownTriggerState<T> extends State<PaseoDropdownTrigger<T>> {
  bool pressed = false;
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final selected = widget.options.where((o) => o.value == widget.value).firstOrNull;
    final disabled = widget.onChanged == null;
    final (double padding, double radius) = switch (widget.size) {
      PaseoControlSize.xs || PaseoControlSize.sm => (12.0, 12.0),
      PaseoControlSize.md => (16.0, 16.0),
      PaseoControlSize.lg => (24.0, 16.0),
    };
    return Semantics(
      button: true,
      label: '${widget.title} (${selected?.label ?? ''})',
      child: Builder(
        builder: (anchor) => MouseRegion(
          cursor: disabled ? SystemMouseCursors.basic : SystemMouseCursors.click,
          child: GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTapDown: disabled ? null : (_) => setState(() => pressed = true),
            onTapCancel: () => setState(() => pressed = false),
            onTapUp: (_) => setState(() => pressed = false),
            onTap: disabled
                ? null
                : () async {
                    final picked = await showPaseoMenu<T>(
                      anchor,
                      anchor: paseoAnchorRect(anchor),
                      alignEnd: true,
                      width: widget.menuWidth,
                      compactSheet: false,
                      entries: [
                        for (final option in widget.options) ...[
                          if (option.separatorBefore) const PaseoMenuEntry.separator(),
                          PaseoMenuEntry.item(
                            value: option.value,
                            label: option.label,
                            leading: option.leading,
                            enabled: option.enabled,
                            selected: option.value == widget.value,
                          ),
                        ],
                      ],
                    );
                    if (picked is T && picked != widget.value) {
                      widget.onChanged?.call(picked);
                    }
                  },
            child: Opacity(
              opacity: disabled ? .5 : pressed ? .85 : 1,
              child: Container(
                constraints: BoxConstraints(minHeight: paseoButtonHeight(widget.size)),
                padding: EdgeInsets.symmetric(horizontal: padding),
                decoration: BoxDecoration(
                  border: Border.all(color: colors.borderAccent),
                  borderRadius: BorderRadius.circular(radius),
                ),
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  spacing: 8,
                  children: [
                    ?widget.leading ?? selected?.leading,
                    Flexible(
                      child: Text(
                        selected?.label ?? '${widget.value}',
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: theme.textTheme.bodyMedium!.copyWith(
                          fontSize: type.uiSize(widget.size == PaseoControlSize.xs ? 12 : 14),
                        ),
                      ),
                    ),
                    Transform.translate(
                      offset: const Offset(0, 1),
                      child: Icon(
                        LucideIcons.chevronDown300,
                        size: paseoButtonIconSize(widget.size),
                        color: colors.muted,
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// Original `FormTextInput` chrome: surface2 fill, transparent 1px border that
/// becomes borderAccent on hover/focus, sm 32/6px or md 44/8px geometry,
/// base text with a 1.4 line height. Errors never recolor the chrome; the
/// original `Field` shows them as one red-300 12px line under the control.
InputDecoration paseoInputDecoration(
  BuildContext context, {
  String? hintText,
  PaseoControlSize size = PaseoControlSize.md,
  Widget? prefixIcon,
  Widget? suffixIcon,
  String? errorText,
  bool multiline = false,
}) {
  final colors = PaseoColors.of(context);
  final small = size == PaseoControlSize.sm || size == PaseoControlSize.xs;
  final radius = BorderRadius.circular(small ? 6 : 8);
  final lineHeight = (14 * 1.4).roundToDouble();
  final vertical = multiline ? 12.0 : ((small ? 32 : 44) - lineHeight - 2) / 2;
  OutlineInputBorder border(Color color) =>
      OutlineInputBorder(borderRadius: radius, borderSide: BorderSide(color: color));
  return InputDecoration(
    isDense: true,
    filled: true,
    fillColor: colors.surface2,
    hoverColor: Colors.transparent,
    hintText: hintText,
    hintStyle: TextStyle(color: colors.muted),
    errorText: errorText,
    prefixIcon: prefixIcon,
    suffixIcon: suffixIcon,
    prefixIconConstraints: const BoxConstraints(minWidth: 36, minHeight: 32),
    suffixIconConstraints: const BoxConstraints(minWidth: 36, minHeight: 32),
    contentPadding: EdgeInsets.symmetric(horizontal: small ? 12 : 16, vertical: vertical),
    errorStyle: TextStyle(
      color: paseoFieldErrorColor,
      fontSize: PaseoTypography.of(context).uiSize(12),
      height: 1.4,
    ),
    errorMaxLines: 1,
    border: WidgetStateInputBorder.resolveWith((states) {
      if (states.contains(WidgetState.focused) || states.contains(WidgetState.hovered)) {
        return border(colors.borderAccent);
      }
      return border(Colors.transparent);
    }),
  );
}

/// Original `Field` error text: palette red-300 in every theme.
const paseoFieldErrorColor = Color(0xfffca5a5);

/// Original `FormField`: muted base label over the control (8px gap) with a
/// muted 12px hint or a single red-300 error line under it.
class PaseoFormField extends StatelessWidget {
  const PaseoFormField({
    super.key,
    required this.label,
    required this.child,
    this.hint,
    this.error,
    this.trailing,
  });
  final String label;
  final Widget child;
  final String? hint, error;
  final Widget? trailing;
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final small = theme.textTheme.bodySmall!.copyWith(
      fontSize: type.uiSize(12),
      height: 1.4,
    );
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      spacing: 8,
      children: [
        Row(
          spacing: 8,
          children: [
            Expanded(
              child: Text(
                label,
                style: theme.textTheme.bodyMedium!.copyWith(
                  color: colors.muted,
                  fontSize: type.uiSize(14),
                  fontWeight: FontWeight.normal,
                ),
              ),
            ),
            ?trailing,
          ],
        ),
        child,
        if (error != null && error!.isNotEmpty)
          Text(
            error!,
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: small.copyWith(color: paseoFieldErrorColor),
          )
        else if (hint != null && hint!.isNotEmpty)
          Text(hint!, style: small.copyWith(color: colors.muted)),
      ],
    );
  }
}
