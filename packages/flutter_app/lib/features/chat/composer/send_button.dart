import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../../ui/theme.dart';
import 'composer_tooltip.dart';

/// input.tsx sendButton: a 28px accent circle with the accent-foreground
/// ArrowUp (glyph 20 native, 16 web), a spinner while the submit loads, 0.5
/// opacity when disabled; desktop shows its tooltip above.
class ComposerSendButton extends StatelessWidget {
  const ComposerSendButton({
    super.key,
    required this.glyph,
    required this.label,
    required this.onPressed,
    this.tooltip,
    this.loading = false,
  });

  final double glyph;
  final String label;
  final String? tooltip;
  final VoidCallback? onPressed;
  final bool loading;

  @override
  Widget build(BuildContext context) {
    final foreground = Theme.of(context).colorScheme.onTertiary;
    return _CircleAction(
      color: PaseoColors.of(context).accent,
      label: label,
      tooltip: tooltip,
      // input.tsx DEFAULT_SEND_KEYS: the send tooltip shows Enter.
      tooltipCombo: 'Enter',
      onPressed: onPressed,
      child: loading
          ? SizedBox.square(
              dimension: 14,
              child: CircularProgressIndicator(strokeWidth: 2, color: foreground),
            )
          : Icon(LucideIcons.arrowUp300, size: glyph, color: foreground),
    );
  }
}

/// composer/index.tsx ComposerCancelButton: a 28px red-600 circle with a
/// white filled Square (the lucide square: an 18-unit 2-radius rect with a
/// 1.5 stroke), a white spinner while cancelling; disabled at 0.5 opacity
/// while disconnected or cancelling. Tooltip "Interrupt".
class ComposerCancelButton extends StatelessWidget {
  const ComposerCancelButton({
    super.key,
    required this.glyph,
    required this.label,
    required this.tooltip,
    required this.onPressed,
    this.cancelling = false,
  });

  final double glyph;
  final String label, tooltip;
  final VoidCallback? onPressed;
  final bool cancelling;

  @override
  Widget build(BuildContext context) => _CircleAction(
    color: const Color(0xffdc2626),
    label: label,
    tooltip: tooltip,
    tooltipHelpId: 'agent-interrupt',
    onPressed: cancelling ? null : onPressed,
    child: cancelling
        ? const SizedBox.square(
            dimension: 14,
            child: CircularProgressIndicator(strokeWidth: 2, color: Colors.white),
          )
        : Container(
            width: glyph * 19.5 / 24,
            height: glyph * 19.5 / 24,
            decoration: BoxDecoration(
              color: Colors.white,
              borderRadius: BorderRadius.circular(glyph * 2.75 / 24),
            ),
          ),
  );
}

class _CircleAction extends StatelessWidget {
  const _CircleAction({
    required this.color,
    required this.label,
    required this.onPressed,
    required this.child,
    this.tooltip,
    this.tooltipCombo,
    this.tooltipHelpId,
  });

  final Color color;
  final String label;
  final String? tooltip, tooltipCombo, tooltipHelpId;
  final VoidCallback? onPressed;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    final button = Semantics(
      button: true,
      enabled: onPressed != null,
      label: label,
      excludeSemantics: true,
      child: Opacity(
        opacity: onPressed == null ? .5 : 1,
        child: Material(
          color: color,
          shape: const CircleBorder(),
          clipBehavior: Clip.antiAlias,
          child: InkWell(
            onTap: onPressed,
            child: SizedBox.square(dimension: 28, child: Center(child: child)),
          ),
        ),
      ),
    );
    if (tooltip == null) return button;
    return ComposerTooltip(
      label: tooltip!,
      combo: tooltipCombo,
      helpId: tooltipHelpId,
      child: button,
    );
  }
}

/// input.tsx attachButton: a 28px round glyph button, transparent until
/// hovered (surface2), 0.5 opacity when disabled; [builder] gets the hover
/// state so the glyph can switch from muted to foreground.
class ComposerHoverGlyph extends StatefulWidget {
  const ComposerHoverGlyph({
    super.key,
    required this.builder,
    required this.onPressed,
    required this.hoverColor,
    required this.semanticsLabel,
    this.disabled = false,
  });

  final Widget Function(bool hovered) builder;
  final VoidCallback onPressed;
  final Color hoverColor;
  final String semanticsLabel;
  final bool disabled;

  @override
  State<ComposerHoverGlyph> createState() => _ComposerHoverGlyphState();
}

class _ComposerHoverGlyphState extends State<ComposerHoverGlyph> {
  bool hovered = false;

  @override
  Widget build(BuildContext context) => Semantics(
    button: true,
    enabled: !widget.disabled,
    label: widget.semanticsLabel,
    excludeSemantics: true,
    child: Opacity(
      opacity: widget.disabled ? .5 : 1,
      child: MouseRegion(
        cursor: widget.disabled ? SystemMouseCursors.basic : SystemMouseCursors.click,
        onEnter: (_) => setState(() => hovered = true),
        onExit: (_) => setState(() => hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: widget.disabled ? null : widget.onPressed,
          child: Container(
            width: 28,
            height: 28,
            decoration: BoxDecoration(
              shape: BoxShape.circle,
              color: hovered && !widget.disabled ? widget.hoverColor : null,
            ),
            alignment: Alignment.center,
            child: widget.builder(hovered && !widget.disabled),
          ),
        ),
      ),
    ),
  );
}
