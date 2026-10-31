import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../ui/common.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';

/// Original `WORKSPACE_SECONDARY_HEADER_HEIGHT` (constants/layout.ts).
const paneToolbarHeight = 36.0;

/// Original `WORKSPACE_PANE_TRAILING_GLYPH_RAIL` (components/tree-primitives.tsx).
const paneTrailingGlyphRail = 8.0;

/// Original `TREE_INDENT_PER_LEVEL` / `treeRowPaddingLeft`.
const treeIndentPerLevel = 12.0;
double treeRowPaddingLeft(int depth) => 12 + depth * treeIndentPerLevel;

/// Original `surfaceSidebarHover`: dark surface1, light surface3.
Color sidebarHoverColor(BuildContext context) {
  final colors = PaseoColors.of(context);
  return Theme.of(context).brightness == Brightness.dark ? colors.surface1 : colors.surface3;
}

/// Original small `iconButtonChromeFrameSize`: 20 on desktop, 32 on compact layouts.
double paneToolbarButtonSize(BuildContext context) => isCompactLayout(context) ? 32 : 20;

/// Original `paneContentToolbarIconSize`: 14 on desktop, 18 on compact layouts.
double paneToolbarIconSize(BuildContext context) => isCompactLayout(context) ? 18 : 14;

/// Original `paneContentToolbarTrailingPadding`: a trailing bare glyph's hitbox overhangs
/// the glyph rail by half its padding; a framed control pads by the full rail.
double paneToolbarTrailingPadding(BuildContext context, {bool framed = false}) => framed
    ? paneTrailingGlyphRail
    : paneTrailingGlyphRail - (paneToolbarButtonSize(context) - paneToolbarIconSize(context)) / 2;

/// Original `PaneContentToolbar` (components/ui/pane-content-toolbar.tsx): a 36px row with a
/// 1px border under it, surface0 in panes and surfaceSidebar in the explorer sidebar.
class FilePaneToolbar extends StatelessWidget {
  const FilePaneToolbar({
    super.key,
    required this.children,
    this.sidebar = false,
    this.padding = const EdgeInsets.symmetric(horizontal: 8),
  });
  final List<Widget> children;
  final bool sidebar;
  final EdgeInsetsGeometry padding;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final frame = paneToolbarButtonSize(context);
    return Container(
      height: paneToolbarHeight,
      padding: padding,
      decoration: BoxDecoration(
        color: sidebar ? colors.sidebar : colors.surface0,
        border: Border(bottom: BorderSide(color: theme.dividerColor)),
      ),
      child: IconButtonTheme(
        // Plain IconButtons inside a pane toolbar take the original ToolbarButton chrome.
        data: IconButtonThemeData(style: IconButton.styleFrom(
          minimumSize: Size.square(frame),
          maximumSize: Size.square(frame),
          fixedSize: Size.square(frame),
          padding: EdgeInsets.zero,
          iconSize: paneToolbarIconSize(context),
          foregroundColor: colors.extraMuted,
          disabledForegroundColor: colors.extraMuted.withValues(alpha: .5),
          hoverColor: paseoInteractionHighlight(context),
          highlightColor: paseoInteractionHighlight(context),
          tapTargetSize: MaterialTapTargetSize.shrinkWrap,
          shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(6)),
        )),
        child: DefaultTextStyle.merge(
          style: theme.textTheme.bodySmall!.copyWith(color: colors.muted),
          child: Row(children: children),
        ),
      ),
    );
  }
}

/// Original `ToolbarButton`: the icon action of pane toolbars. A 20px (desktop) / 32px
/// (compact) hitbox with a 14/18px extraMuted glyph, 6px radius, the interaction highlight
/// while hovered, pressed or [selected], 50% opacity when disabled, and a desktop-only tooltip.
class PaneToolbarButton extends StatefulWidget {
  const PaneToolbarButton({
    super.key,
    required this.tooltip,
    this.icon,
    this.child,
    this.onPressed,
    this.selected = false,
  }) : assert(icon != null || child != null);
  final String tooltip;
  final IconData? icon;
  final Widget? child;
  final VoidCallback? onPressed;
  final bool selected;

  @override
  State<PaneToolbarButton> createState() => _PaneToolbarButtonState();
}

class _PaneToolbarButtonState extends State<PaneToolbarButton> {
  bool _hovered = false, _pressed = false;

  @override
  Widget build(BuildContext context) {
    final compact = isCompactLayout(context);
    final enabled = widget.onPressed != null;
    final size = paneToolbarButtonSize(context);
    final highlighted = widget.selected || (enabled && (_hovered || _pressed));
    Widget button = Semantics(
      button: true,
      enabled: enabled,
      selected: widget.selected,
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
          onTap: widget.onPressed,
          child: Opacity(
            opacity: enabled ? 1 : .5,
            child: Container(
              width: size,
              height: size,
              alignment: Alignment.center,
              decoration: BoxDecoration(
                color: highlighted ? paseoInteractionHighlight(context) : null,
                borderRadius: BorderRadius.circular(6),
              ),
              child: IconTheme(
                data: IconThemeData(
                  size: paneToolbarIconSize(context),
                  color: PaseoColors.of(context).extraMuted,
                ),
                child: widget.child ?? Icon(widget.icon),
              ),
            ),
          ),
        ),
      ),
    );
    if (!compact) {
      button = Tooltip(
        message: widget.tooltip,
        waitDuration: const Duration(milliseconds: 300),
        child: button,
      );
    }
    return button;
  }
}

/// Original `ToolbarControls`: a row of toolbar buttons 1px apart.
class PaneToolbarControls extends StatelessWidget {
  const PaneToolbarControls({super.key, required this.children});
  final List<Widget> children;

  @override
  Widget build(BuildContext context) =>
      Row(mainAxisSize: MainAxisSize.min, spacing: 1, children: children);
}

/// Original workspace tree row (components/tree-primitives.tsx `workspaceTreeRowStyles` plus
/// the explorer entry): `treeRowPaddingLeft(depth)`, 6px vertical padding, the 8px trailing
/// glyph rail, a 16px icon slot 8px before a base-size foreground name at 76% opacity (100%
/// while hovered), and the surfaceSidebarHover fill when hovered, pressed or selected.
class FileTreeRow extends StatefulWidget {
  const FileTreeRow({
    super.key,
    required this.name,
    required this.icon,
    required this.onTap,
    this.depth = 0,
    this.selected = false,
    this.trailing,
    this.onLongPress,
    this.onSecondaryTapDown,
    this.tooltip,
  });
  final String name;
  final Widget icon;
  final VoidCallback onTap;
  final VoidCallback? onLongPress;

  /// Desktop context menu (right click) at the pointer's global position.
  final ValueChanged<Offset>? onSecondaryTapDown;
  final int depth;
  final bool selected;
  final Widget? trailing;
  final String? tooltip;

  @override
  State<FileTreeRow> createState() => _FileTreeRowState();
}

class _FileTreeRowState extends State<FileTreeRow> {
  bool _hovered = false, _pressed = false;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final active = _hovered || _pressed || widget.selected;
    Widget name = Text(
      widget.name,
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
      style: theme.textTheme.bodyMedium!.copyWith(
        color: theme.colorScheme.onSurface.withValues(alpha: _hovered ? 1 : .76),
      ),
    );
    if (widget.tooltip != null && !isCompactLayout(context)) {
      name = Tooltip(
        message: widget.tooltip,
        waitDuration: const Duration(milliseconds: 600),
        child: name,
      );
    }
    return Semantics(
      button: true,
      selected: widget.selected,
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
          onLongPress: widget.onLongPress == null
              ? null
              : () {
                  setState(() => _pressed = false);
                  widget.onLongPress!();
                },
          onSecondaryTapDown: widget.onSecondaryTapDown == null
              ? null
              : (details) => widget.onSecondaryTapDown!(details.globalPosition),
          child: Container(
            color: active ? sidebarHoverColor(context) : null,
            padding: EdgeInsets.fromLTRB(
              treeRowPaddingLeft(widget.depth),
              6,
              paneTrailingGlyphRail,
              6,
            ),
            constraints: const BoxConstraints(minHeight: 28),
            child: Row(children: [
              IconTheme(
                data: IconThemeData(size: 16, color: colors.muted),
                child: SizedBox.square(dimension: 16, child: Center(child: widget.icon)),
              ),
              const SizedBox(width: 8),
              Expanded(child: name),
              if (widget.trailing != null) ...[const SizedBox(width: 4), widget.trailing!],
            ]),
          ),
        ),
      ),
    );
  }
}

/// Rotating disclosure chevron shared by the Files and Changes trees: a 16px extraMuted
/// chevron-right turned 90° when expanded (original `TreeChevron`).
class TreeChevron extends StatelessWidget {
  const TreeChevron({super.key, required this.expanded});
  final bool expanded;

  @override
  Widget build(BuildContext context) => RotatedBox(
    quarterTurns: expanded ? 1 : 0,
    child: Icon(LucideIcons.chevronRight300, size: 16, color: PaseoColors.of(context).extraMuted),
  );
}

/// Original `WORKSPACE_TREE_LOADING_ICON_SIZE` spinner in a directory row's icon slot.
class TreeLoadingIcon extends StatelessWidget {
  const TreeLoadingIcon({super.key});

  @override
  Widget build(BuildContext context) => SizedBox.square(
    dimension: 14,
    child: CircularProgressIndicator(strokeWidth: 1.5, color: PaseoColors.of(context).muted),
  );
}

/// git/file-header-presentation.ts formatDiffCount: en-US
/// `Intl.NumberFormat({notation: "compact", maximumFractionDigits: 1})`,
/// lower-cased — one fraction digit at every magnitude (156200 → "156.2k").
String formatDiffCount(int value) {
  const suffixes = ['', 'k', 'm', 'b', 't'];
  var magnitude = 0;
  var scaled = value.abs().toDouble();
  while (scaled >= 1000 && magnitude < suffixes.length - 1) {
    scaled /= 1000;
    magnitude++;
  }
  var rounded = (scaled * 10).round() / 10;
  // Rounding can carry into the next unit (999950 → "1m").
  if (rounded >= 1000 && magnitude < suffixes.length - 1) {
    rounded = (rounded / 100).round() / 10;
    magnitude++;
  }
  final text = rounded == rounded.truncateToDouble()
      ? '${rounded.toInt()}'
      : rounded.toStringAsFixed(1);
  return '${value < 0 ? '-' : ''}$text${suffixes[magnitude]}';
}

String fileChangeKind(Map<String, dynamic> file) {
  if (file['isNew'] == true || file['status'] == 'added') return 'added';
  if (file['isDeleted'] == true || file['status'] == 'deleted') return 'deleted';
  return 'modified';
}

/// Original `FileChangeIcon`: Lucide square-plus / square-minus / square-dot at 14px in the
/// status success / danger / warning colours.
class FileChangeIcon extends StatelessWidget {
  const FileChangeIcon({super.key, required this.change, this.size = 14});
  final String change;
  final double size;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final (icon, color, label) = switch (change) {
      'added' => (LucideIcons.squarePlus300, colors.success, 'workspace.git.diff.newFile'),
      'deleted' => (LucideIcons.squareMinus300, colors.danger, 'workspace.git.diff.deletedFile'),
      _ => (LucideIcons.squareDot300, colors.warning, 'workspace.git.diff.modifiedFile'),
    };
    return Icon(icon, size: size, color: color, semanticLabel: tr(context, label));
  }
}

/// Original `DiffStat`: a 20px row, 4px apart, small normal-weight +/- counts in the status
/// success / danger colours.
class FileDiffStat extends StatelessWidget {
  const FileDiffStat({super.key, required this.additions, required this.deletions});
  final int additions, deletions;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final style = Theme.of(context).textTheme.bodySmall!.copyWith(fontWeight: FontWeight.normal);
    return SizedBox(
      height: 20,
      child: Row(mainAxisSize: MainAxisSize.min, spacing: 4, children: [
        Text('+${formatDiffCount(additions)}', style: style.copyWith(color: colors.success)),
        Text('-${formatDiffCount(deletions)}', style: style.copyWith(color: colors.danger)),
      ]),
    );
  }
}
