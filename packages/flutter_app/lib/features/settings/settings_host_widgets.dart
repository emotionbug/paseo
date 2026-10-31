import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:url_launcher/url_launcher.dart';

import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/theme.dart';

enum StatusBadgeVariant { muted, success, warning, error }

/// Original `components/ui/status-badge.tsx`: full-radius 1px bordered surface3
/// pill (8/3 padding, xs 6/1), 6px leading gap, normal 12px muted label;
/// status variants drop the border onto a 12% (light) / 16% (dark) tint.
class StatusBadge extends StatelessWidget {
  const StatusBadge({
    super.key,
    required this.label,
    this.variant = StatusBadgeVariant.muted,
    this.leading,
    this.xs = false,
  });
  final String label;
  final StatusBadgeVariant variant;
  final Widget? leading;
  final bool xs;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final tint = theme.brightness == Brightness.dark ? .16 : .12;
    final Color? tone = switch (variant) {
      StatusBadgeVariant.success => colors.success,
      StatusBadgeVariant.warning => colors.warning,
      StatusBadgeVariant.error => colors.danger,
      StatusBadgeVariant.muted => null,
    };
    return Container(
      padding: EdgeInsets.symmetric(horizontal: xs ? 6 : 8, vertical: xs ? 1 : 3),
      decoration: BoxDecoration(
        color: tone?.withValues(alpha: tint) ?? colors.surface3,
        border: Border.all(color: tone == null ? theme.dividerColor : Colors.transparent),
        borderRadius: BorderRadius.circular(999),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        spacing: 6,
        children: [
          ?leading,
          Flexible(
            child: Text(
              label,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: theme.textTheme.bodySmall!.copyWith(
                fontSize: type.uiSize(12),
                fontWeight: FontWeight.normal,
                color: tone ?? colors.muted,
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// The original settings row actions trigger (`menuButton`): a 32px square,
/// 8px radius, surface2 on hover/open and surface3 pressed, with a muted
/// `MoreHorizontal` glyph that turns foreground on hover. Opens the original
/// end-aligned 220px `DropdownMenu` (a bottom sheet on compact layouts).
class SettingsMenuButton extends StatefulWidget {
  const SettingsMenuButton({
    super.key,
    required this.label,
    required this.options,
    required this.onSelected,
    this.iconSize = 14,
    this.enabled = true,
  });
  final String label;
  final List<PaseoMenuEntry<String>> options;
  final ValueChanged<String> onSelected;
  final double iconSize;
  final bool enabled;

  @override
  State<SettingsMenuButton> createState() => _SettingsMenuButtonState();
}

class _SettingsMenuButtonState extends State<SettingsMenuButton> {
  bool hovered = false, pressed = false, open = false;

  Future<void> show() async {
    setState(() => open = true);
    final value = await showPaseoMenu<String>(
      context,
      title: widget.label,
      entries: widget.options,
      anchor: paseoAnchorRect(context),
      alignEnd: true,
    );
    if (!mounted) return;
    setState(() => open = false);
    if (value != null) widget.onSelected(value);
  }

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final active = widget.enabled && (hovered || open);
    return Semantics(
      button: true,
      enabled: widget.enabled,
      label: widget.label,
      child: MouseRegion(
        cursor: widget.enabled ? SystemMouseCursors.click : SystemMouseCursors.basic,
        onEnter: (_) => setState(() => hovered = true),
        onExit: (_) => setState(() => hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: widget.enabled ? (_) => setState(() => pressed = true) : null,
          onTapCancel: () => setState(() => pressed = false),
          onTapUp: (_) => setState(() => pressed = false),
          onTap: widget.enabled ? show : null,
          child: Opacity(
            opacity: widget.enabled ? 1 : .5,
            child: Container(
              width: 32,
              height: 32,
              alignment: Alignment.center,
              decoration: BoxDecoration(
                color: pressed
                    ? colors.surface3
                    : active
                    ? colors.surface2
                    : Colors.transparent,
                borderRadius: BorderRadius.circular(8),
              ),
              child: Icon(
                LucideIcons.moreHorizontal300,
                size: widget.iconSize,
                color: active
                    ? Theme.of(context).colorScheme.onSurface
                    : colors.muted,
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// Original ghost `size="sm"` icon button (`leftIcon` at ICON_SIZE.sm, muted;
/// destructive glyphs keep the destructive colour).
Widget settingsIconButton(
  BuildContext context,
  IconData icon,
  VoidCallback? onPressed, {
  required String label,
  bool destructive = false,
}) => PaseoButton(
  icon: icon,
  tooltip: label,
  semanticLabel: label,
  size: PaseoControlSize.sm,
  variant: PaseoButtonVariant.ghost,
  foreground: destructive ? Theme.of(context).colorScheme.error : null,
  onPressed: onPressed,
);

/// Original `emptyCard`: a bordered settings card with centred muted base
/// text (16px padding), optionally followed by an action 12px below.
class SettingsEmptyCard extends StatelessWidget {
  const SettingsEmptyCard(this.text, {super.key, this.action, this.roomy = false});
  final String text;
  final Widget? action;

  /// The agent-profiles variant: 24px vertical padding.
  final bool roomy;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    return DecoratedBox(
      decoration: BoxDecoration(
        color: colors.surface1,
        border: Border.all(color: theme.dividerColor),
        borderRadius: BorderRadius.circular(8),
      ),
      child: Padding(
        padding: EdgeInsets.symmetric(horizontal: 16, vertical: roomy ? 24 : 16),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          spacing: 12,
          children: [
            Text(
              text,
              textAlign: TextAlign.center,
              style: theme.textTheme.bodyMedium!.copyWith(
                color: colors.muted,
                fontSize: type.uiSize(14),
              ),
            ),
            ?action,
          ],
        ),
      ),
    );
  }
}

/// Original `SkillCheckboxRow`: settings row with a 22px, 2px-radius bordered
/// box (accent fill and accentForeground 14px check when checked), 12px gap,
/// base title; disabled rows drop to 50% opacity.
class SettingsCheckboxRow extends StatelessWidget {
  const SettingsCheckboxRow({
    super.key,
    required this.label,
    required this.checked,
    required this.onChanged,
    this.hint,
  });
  final String label;
  final String? hint;
  final bool checked;
  final ValueChanged<bool>? onChanged;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final enabled = onChanged != null;
    return Semantics(
      checked: checked,
      enabled: enabled,
      label: label,
      child: MouseRegion(
        cursor: enabled ? SystemMouseCursors.click : SystemMouseCursors.basic,
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: enabled ? () => onChanged!(!checked) : null,
          child: Opacity(
            opacity: enabled ? 1 : .5,
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Row(
                spacing: 12,
                children: [
                  Container(
                    width: 22,
                    height: 22,
                    alignment: Alignment.center,
                    decoration: BoxDecoration(
                      color: checked ? colors.accent : null,
                      border: Border.all(color: checked ? colors.accent : theme.dividerColor),
                      borderRadius: BorderRadius.circular(2),
                    ),
                    child: checked
                        ? Icon(
                            LucideIcons.check300,
                            size: 14,
                            color: paseoAccentForeground(context),
                          )
                        : null,
                  ),
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          label,
                          style: theme.textTheme.bodyMedium!.copyWith(fontSize: type.uiSize(14)),
                        ),
                        if (hint != null && hint!.isNotEmpty)
                          Padding(
                            padding: const EdgeInsets.only(top: 4),
                            child: Text(
                              hint!,
                              style: theme.textTheme.bodySmall!.copyWith(
                                color: colors.muted,
                                fontSize: type.uiSize(12),
                              ),
                            ),
                          ),
                      ],
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

/// Original `settingsStyles.card` holding custom rows: each row after the
/// first gets the 1px `rowBorder`; rows keep their own padding.
class SettingsRowsCard extends StatelessWidget {
  const SettingsRowsCard({super.key, required this.children});
  final List<Widget> children;
  @override
  Widget build(BuildContext context) {
    final border = Theme.of(context).dividerColor;
    return DecoratedBox(
      decoration: BoxDecoration(
        color: PaseoColors.of(context).surface1,
        border: Border.all(color: border),
        borderRadius: BorderRadius.circular(8),
      ),
      child: ClipRRect(
        borderRadius: BorderRadius.circular(7),
        child: Material(
          type: MaterialType.transparency,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              for (var i = 0; i < children.length; i++) ...[
                if (i > 0) Divider(height: 1, thickness: 1, color: border),
                children[i],
              ],
            ],
          ),
        ),
      ),
    );
  }
}

/// Original `LoadingSpinner` at a small glyph size.
class SettingsSpinner extends StatelessWidget {
  const SettingsSpinner({super.key, this.size = 14, this.color});
  final double size;
  final Color? color;
  @override
  Widget build(BuildContext context) => SizedBox.square(
    dimension: size,
    child: CircularProgressIndicator(
      strokeWidth: 1.5,
      color: color ?? PaseoColors.of(context).muted,
    ),
  );
}

/// Original `components/ui/search-field.tsx`: grows to 420px, 6/12 padding,
/// 6px radius, surface1 with a 1px border (borderAccent on surface2 while
/// focused), muted 14px Search glyph, base 20px-tall input and a 14px clear X.
class SettingsSearchField extends StatefulWidget {
  const SettingsSearchField({
    super.key,
    required this.onChanged,
    required this.placeholder,
    required this.clearLabel,
  });
  final ValueChanged<String> onChanged;
  final String placeholder, clearLabel;
  @override
  State<SettingsSearchField> createState() => _SettingsSearchFieldState();
}

class _SettingsSearchFieldState extends State<SettingsSearchField> {
  final controller = TextEditingController();
  final focus = FocusNode();

  @override
  void initState() {
    super.initState();
    focus.addListener(() => setState(() {}));
  }

  @override
  void dispose() {
    controller.dispose();
    focus.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final size = type.uiSize(14);
    final focused = focus.hasFocus;
    return Align(
      alignment: Alignment.centerLeft,
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 420),
        child: Container(
          width: double.infinity,
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
          decoration: BoxDecoration(
            color: focused ? colors.surface2 : colors.surface1,
            border: Border.all(color: focused ? colors.borderAccent : theme.dividerColor),
            borderRadius: BorderRadius.circular(6),
          ),
          child: Row(
            spacing: 8,
            children: [
              Icon(LucideIcons.search300, size: 14, color: colors.muted),
              Expanded(
                child: SizedBox(
                  height: 20,
                  child: TextField(
                    controller: controller,
                    focusNode: focus,
                    autocorrect: false,
                    style: theme.textTheme.bodyMedium!.copyWith(fontSize: size, height: 20 / size),
                    decoration: InputDecoration.collapsed(
                      hintText: widget.placeholder,
                      hintStyle: TextStyle(color: colors.muted, fontSize: size),
                    ),
                    onChanged: (value) {
                      setState(() {});
                      widget.onChanged(value);
                    },
                  ),
                ),
              ),
              if (controller.text.isNotEmpty)
                Semantics(
                  button: true,
                  label: widget.clearLabel,
                  child: GestureDetector(
                    behavior: HitTestBehavior.opaque,
                    onTap: () {
                      controller.clear();
                      setState(() {});
                      widget.onChanged('');
                    },
                    child: Icon(LucideIcons.x300, size: 14, color: colors.muted),
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }
}

/// Original `components/ui/alert.tsx` at size sm: 12/16 padding, 16px radius,
/// half-alpha error border (plain border for success), 14px status glyph,
/// medium accent title over a muted description indented past the glyph.
class SettingsInlineAlert extends StatelessWidget {
  const SettingsInlineAlert({
    super.key,
    required this.title,
    this.description,
    this.success = false,
  });
  final String title;
  final String? description;
  final bool success;
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final accent = success ? colors.success : colors.danger;
    final text = theme.textTheme.bodyMedium!.copyWith(fontSize: type.uiSize(14));
    return Semantics(
      liveRegion: true,
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
        decoration: BoxDecoration(
          border: Border.all(
            color: success ? theme.dividerColor : accent.withValues(alpha: .5),
          ),
          borderRadius: BorderRadius.circular(16),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          spacing: 2,
          children: [
            Row(
              spacing: 12,
              children: [
                Icon(
                  success ? LucideIcons.circleCheck300 : LucideIcons.circleX300,
                  size: 14,
                  color: accent,
                ),
                Expanded(
                  child: SelectableText(
                    title,
                    style: text.copyWith(color: accent, fontWeight: FontWeight.w500),
                  ),
                ),
              ],
            ),
            if (description != null && description!.isNotEmpty)
              Padding(
                padding: const EdgeInsets.only(left: 26),
                child: Text(description!, style: text.copyWith(color: colors.muted)),
              ),
          ],
        ),
      ),
    );
  }
}

/// Original `ExternalLink`: muted 12px label with a 12px ArrowUpRight, 4px gap.
class SettingsExternalLink extends StatelessWidget {
  const SettingsExternalLink({
    super.key,
    required this.label,
    required this.url,
    this.semanticLabel,
    this.leadingIcon = false,
  });
  final String label, url;
  final String? semanticLabel;

  /// The section-header link variant puts the arrow first (`leftIcon`, 14px).
  final bool leadingIcon;
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final style = theme.textTheme.bodySmall!.copyWith(
      color: colors.muted,
      fontSize: type.uiSize(12),
    );
    final text = Text(label, style: style);
    final icon = Icon(
      LucideIcons.arrowUpRight300,
      size: leadingIcon ? 14 : 12,
      color: colors.muted,
    );
    return Semantics(
      link: true,
      label: semanticLabel ?? label,
      excludeSemantics: true,
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: () => launchUrl(Uri.parse(url)),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            spacing: 4,
            children: leadingIcon ? [icon, text] : [text, icon],
          ),
        ),
      ),
    );
  }
}

/// Original `SettingsTextAreaCard`: a settings card holding a borderless
/// multiline base input (12/16 padding, 96px minimum height).
class SettingsTextAreaCard extends StatelessWidget {
  const SettingsTextAreaCard({
    super.key,
    required this.controller,
    required this.semanticLabel,
    this.placeholder,
    this.enabled = true,
    this.onChanged,
  });
  final TextEditingController controller;
  final String semanticLabel;
  final String? placeholder;
  final bool enabled;
  final ValueChanged<String>? onChanged;
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    return SettingsRowsCard(
      children: [
        ConstrainedBox(
          constraints: const BoxConstraints(minHeight: 96),
          child: Semantics(
            label: semanticLabel,
            textField: true,
            child: TextField(
              controller: controller,
              enabled: enabled,
              minLines: 4,
              maxLines: 16,
              keyboardType: TextInputType.multiline,
              style: theme.textTheme.bodyMedium!.copyWith(fontSize: type.uiSize(14)),
              decoration: InputDecoration(
                isDense: true,
                filled: false,
                hintText: placeholder,
                hintStyle: TextStyle(color: colors.muted),
                border: InputBorder.none,
                enabledBorder: InputBorder.none,
                focusedBorder: InputBorder.none,
                disabledBorder: InputBorder.none,
                contentPadding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
              ),
              onChanged: onChanged,
            ),
          ),
        ),
      ],
    );
  }
}
