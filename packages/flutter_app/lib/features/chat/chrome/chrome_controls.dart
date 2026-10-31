import 'dart:async';

import 'package:flutter/cupertino.dart';
import 'package:flutter/foundation.dart' show defaultTargetPlatform;
import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../../ui/common.dart';
import '../../../ui/theme.dart';

/// components/ui/loading-spinner.tsx: React Native's ActivityIndicator — the
/// UIKit spinner on Apple platforms, the Material ring elsewhere; "small" is
/// 20px and "large" 36px.
class ChromeSpinner extends StatelessWidget {
  const ChromeSpinner({super.key, this.large = false, this.size, this.color});
  final bool large;

  /// Explicit diameter (the sync toast's 18px spinner).
  final double? size;
  final Color? color;

  @override
  Widget build(BuildContext context) {
    final color = this.color ?? PaseoColors.of(context).muted;
    final diameter = size ?? (large ? 36.0 : 20.0);
    final apple = defaultTargetPlatform == TargetPlatform.iOS ||
        defaultTargetPlatform == TargetPlatform.macOS;
    if (apple) {
      return SizedBox.square(
        dimension: diameter,
        child: CupertinoActivityIndicator(color: color, radius: diameter / 2.2),
      );
    }
    return SizedBox.square(
      dimension: diameter,
      child: Padding(
        padding: EdgeInsets.all(diameter / 12),
        child: CircularProgressIndicator(
          strokeWidth: diameter / 10,
          color: color,
        ),
      ),
    );
  }
}

/// components/ui/pane-content-toolbar.tsx ToolbarButton (icon-button chrome,
/// size small): a 20px (32px compact) frame with a 14px (18px compact) muted
/// glyph, 6px radius, the interaction highlight on hover/press, half opacity
/// when disabled, tooltip on pointer layouts.
class ChromeToolbarButton extends StatefulWidget {
  const ChromeToolbarButton({
    super.key,
    required this.label,
    required this.icon,
    required this.onPressed,
    this.tooltip,
    this.tooltipBelow = false,
  });
  final String label;
  final IconData icon;
  final VoidCallback? onPressed;

  /// Tooltip text when it differs from the accessibility [label].
  final String? tooltip;
  final bool tooltipBelow;

  @override
  State<ChromeToolbarButton> createState() => _ChromeToolbarButtonState();
}

class _ChromeToolbarButtonState extends State<ChromeToolbarButton> {
  bool _hovered = false, _pressed = false;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final compact = isCompactLayout(context);
    final enabled = widget.onPressed != null;
    final highlighted = enabled && (_hovered || _pressed);
    final button = Semantics(
      button: true,
      enabled: enabled,
      label: widget.label,
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
              width: compact ? 32 : 20,
              height: compact ? 32 : 20,
              alignment: Alignment.center,
              decoration: BoxDecoration(
                color: highlighted ? paseoInteractionHighlight(context) : null,
                borderRadius: BorderRadius.circular(6),
              ),
              child: Icon(widget.icon, size: compact ? 18 : 14, color: colors.muted),
            ),
          ),
        ),
      ),
    );
    if (compact) return button;
    return Tooltip(
      message: widget.tooltip ?? widget.label,
      preferBelow: widget.tooltipBelow,
      child: button,
    );
  }
}

enum PanelToastVariant { normal, success, info, warning, error }

/// One toast in a panel's ToastViewport (components/toast-host.tsx).
@immutable
class PanelToastData {
  const PanelToastData({
    required this.id,
    required this.message,
    this.variant = PanelToastVariant.normal,
    this.icon,
    this.duration = const Duration(milliseconds: 2200),
  });
  final Object id;
  final String message;
  final PanelToastVariant variant;
  final Widget? icon;

  /// Null keeps the toast until it is replaced (the sync status toasts).
  final Duration? duration;
}

/// ToastViewport placement="panel": pinned 12px under the pane's top edge,
/// 16px side insets, 92% wide up to 480; the surface0 card has a 1px border,
/// 16px radius, 8/12 padding, an 18px icon 8px from base-size text, the md
/// shadow, and fades in and out over 140ms. Hover pauses the dismiss timer.
class PanelToastViewport extends StatefulWidget {
  const PanelToastViewport({super.key, required this.toast, required this.onDismiss});
  final PanelToastData? toast;
  final VoidCallback onDismiss;

  @override
  State<PanelToastViewport> createState() => _PanelToastViewportState();
}

class _PanelToastViewportState extends State<PanelToastViewport> {
  Timer? _timer;
  DateTime? _deadline;
  Duration _remaining = Duration.zero;

  @override
  void initState() {
    super.initState();
    _schedule(widget.toast?.duration);
  }

  @override
  void didUpdateWidget(PanelToastViewport old) {
    super.didUpdateWidget(old);
    if (old.toast?.id != widget.toast?.id ||
        old.toast?.duration != widget.toast?.duration) {
      _schedule(widget.toast?.duration);
    }
  }

  void _schedule(Duration? duration) {
    _timer?.cancel();
    _timer = null;
    _deadline = null;
    if (duration == null) {
      _remaining = Duration.zero;
      return;
    }
    _remaining = duration;
    _deadline = DateTime.now().add(duration);
    _timer = Timer(duration, widget.onDismiss);
  }

  void _pause() {
    if (_deadline != null) {
      final left = _deadline!.difference(DateTime.now());
      _remaining = left.isNegative ? Duration.zero : left;
    }
    _deadline = null;
    _timer?.cancel();
  }

  void _resume() {
    final duration = widget.toast?.duration;
    if (duration == null) return;
    _schedule(_remaining == Duration.zero ? duration : _remaining);
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final toast = widget.toast;
    return Positioned(
      left: 16,
      right: 16,
      top: 12,
      child: IgnorePointer(
        ignoring: toast == null,
        child: LayoutBuilder(
          builder: (context, constraints) => Center(
            child: ConstrainedBox(
              constraints: BoxConstraints(
                maxWidth: (constraints.maxWidth * .92).clamp(0.0, 480.0),
              ),
              child: AnimatedSwitcher(
                duration: const Duration(milliseconds: 140),
                child: toast == null
                    ? const SizedBox.shrink()
                    : MouseRegion(
                        key: ValueKey(toast.id),
                        onEnter: (_) => _pause(),
                        onExit: (_) => _resume(),
                        child: _ToastCard(toast: toast),
                      ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

class _ToastCard extends StatelessWidget {
  const _ToastCard({required this.toast});
  final PanelToastData toast;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final dark = theme.brightness == Brightness.dark;
    const info = Color(0xff93c5fd);
    const amber = Color(0xfff59e0b);
    final destructive = theme.colorScheme.error;
    final border = switch (toast.variant) {
      PanelToastVariant.info => info,
      PanelToastVariant.warning => amber,
      PanelToastVariant.error => destructive,
      _ => theme.dividerColor,
    };
    final icon = toast.icon ??
        switch (toast.variant) {
          PanelToastVariant.info => Icon(LucideIcons.info300, size: 18, color: info),
          PanelToastVariant.success =>
            Icon(LucideIcons.circleCheck300, size: 18, color: colors.accent),
          PanelToastVariant.warning =>
            Icon(LucideIcons.triangleAlert300, size: 18, color: amber),
          PanelToastVariant.error =>
            Icon(LucideIcons.triangleAlert300, size: 18, color: destructive),
          PanelToastVariant.normal => null,
        };
    return Semantics(
      liveRegion: true,
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
        decoration: BoxDecoration(
          color: colors.surface0,
          borderRadius: BorderRadius.circular(16),
          border: Border.all(color: border),
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
            ?icon,
            Flexible(
              child: Text(
                toast.message,
                style: theme.textTheme.bodyMedium!.copyWith(
                  color: theme.colorScheme.onSurface,
                  fontWeight: FontWeight.normal,
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// components/ui/alert.tsx variant="error" size="sm": transparent, 1px
/// destructive@50% border, 16px radius, 12/16 padding, a 14px XCircle 12px
/// from base-size muted text.
class ErrorAlert extends StatelessWidget {
  const ErrorAlert({super.key, required this.message});
  final String message;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final destructive = theme.colorScheme.error;
    return Semantics(
      liveRegion: true,
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
        decoration: BoxDecoration(
          border: Border.all(color: destructive.withValues(alpha: .5)),
          borderRadius: BorderRadius.circular(16),
        ),
        child: Row(
          spacing: 12,
          children: [
            SizedBox(
              width: 14,
              child: Icon(LucideIcons.circleX300, size: 14, color: destructive),
            ),
            Expanded(
              child: Text(
                message,
                style: theme.textTheme.bodyMedium!.copyWith(
                  color: PaseoColors.of(context).muted,
                  fontWeight: FontWeight.normal,
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}
