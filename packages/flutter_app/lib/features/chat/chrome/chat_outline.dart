import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/foundation.dart' show defaultTargetPlatform;
import 'package:flutter/material.dart';

import '../../../core/json.dart';
import '../../../ui/theme.dart';

const _railWidth = 36.0;
const _slotHeight = 8.0;
const _minPanelWidth = 918.0;
const _restingPillHeight = 2.0;
const _magnifiedPillHeight = 4.0;
const _restingPillWidth = 10.0;
const _activePillWidth = 18.0;
const _magnifiedPillWidth = 26.0;
const _previewWidth = 260.0;
const _previewHeight = 48.0;
const _previewGap = 4.0;
const _magnifyRadius = 3;

/// chat-outline/model.ts promptTickMagnification: a raised cosine, 1 under the
/// pointer easing to 0 three slots away.
double promptTickMagnification(int slotDistance) {
  final distance = slotDistance.abs();
  if (distance >= _magnifyRadius) return 0;
  return (1 + math.cos(math.pi * distance / _magnifyRadius)) / 2;
}

/// resolveActivePromptSeq: the last indexed prompt at or before the timeline
/// position under the top of the viewport.
int? resolveActivePromptSeq(List<JsonMap> prompts, int? anchorSeq) {
  if (anchorSeq == null) return null;
  int? active;
  for (final prompt in prompts) {
    final seq = intValue(prompt['seq']);
    if (seq > anchorSeq) break;
    active = seq;
  }
  return active;
}

/// The outline is a wide-layout pointer affordance (rail.web.tsx); native
/// navigates the transcript by scrolling, so rail.tsx renders nothing there.
bool chatOutlineSupported() =>
    defaultTargetPlatform != TargetPlatform.android &&
    defaultTargetPlatform != TargetPlatform.iOS;

/// agent-stream/chat-outline/rail.web.tsx ChatOutlineRail: a 36px column 8px
/// from the transcript's left edge spanning its middle 80%, one tick per user
/// prompt (2px pills on the borderAccent rail, 18px wide for the prompt being
/// read, growing to 26x4 under the pointer with a dock-style falloff over
/// 140ms). After a 150ms hover intent the hovered tick turns foreground and
/// shows a 260x48 surface2 preview of its prompt. Hidden for fewer than two
/// prompts and in panels narrower than 918px.
class ChatOutlineRail extends StatefulWidget {
  const ChatOutlineRail({
    super.key,
    required this.prompts,
    required this.activeSeq,
    required this.onJumpToPrompt,
  });
  final List<JsonMap> prompts;
  final int? activeSeq;
  final ValueChanged<int> onJumpToPrompt;

  @override
  State<ChatOutlineRail> createState() => _ChatOutlineRailState();
}

class _ChatOutlineRailState extends State<ChatOutlineRail> {
  int? _hovered;
  int? _candidate;
  bool _active = false;
  Timer? _timer;
  Offset? _motionAnchor;

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  void _scheduleActivation() {
    if (_candidate == null) return;
    _timer?.cancel();
    _timer = Timer(const Duration(milliseconds: 150), () {
      _timer = null;
      _active = true;
      if (mounted && _candidate != null) setState(() => _hovered = _candidate);
    });
  }

  void _pointAt(int index) {
    _candidate = index;
    if (_active) {
      setState(() => _hovered = index);
      return;
    }
    if (_timer == null) _scheduleActivation();
  }

  // hover-intent.ts: a pointer crossing the rail horizontally restarts the
  // delay instead of lighting every tick it passes.
  void _move(Offset point) {
    if (_active || _motionAnchor == null) return;
    final dx = point.dx - _motionAnchor!.dx, dy = point.dy - _motionAnchor!.dy;
    if (dy.abs() >= dx.abs()) {
      _motionAnchor = point;
      return;
    }
    if (dx.abs() < 4) return;
    _motionAnchor = point;
    _scheduleActivation();
  }

  void _leave() {
    _timer?.cancel();
    _timer = null;
    _candidate = null;
    _motionAnchor = null;
    _active = false;
    setState(() => _hovered = null);
  }

  @override
  Widget build(BuildContext context) {
    final prompts = widget.prompts;
    if (prompts.length < 2 || !chatOutlineSupported()) return const SizedBox.shrink();
    return LayoutBuilder(
      builder: (context, constraints) {
        if (constraints.maxWidth < _minPanelWidth) return const SizedBox.shrink();
        final top = constraints.maxHeight * .1;
        final height = constraints.maxHeight * .8;
        final slot = math.min(_slotHeight, height / prompts.length);
        final railHeight = slot * prompts.length;
        final railTop = top + (height - railHeight) / 2;
        final attention = _hovered;
        return Stack(
          clipBehavior: Clip.none,
          children: [
            Positioned(
              left: 8,
              top: railTop,
              width: _railWidth,
              height: railHeight,
              child: MouseRegion(
                onEnter: (event) => _motionAnchor = event.position,
                onHover: (event) => _move(event.position),
                onExit: (_) => _leave(),
                child: Column(
                  children: [
                    for (var i = 0; i < prompts.length; i++)
                      _Tick(
                        height: slot,
                        label: '${i + 1} of ${prompts.length}: ${stringValue(prompts[i]['preview'])}',
                        active: intValue(prompts[i]['seq']) == widget.activeSeq,
                        attention: i == attention,
                        magnification:
                            attention == null ? 0 : promptTickMagnification(i - attention),
                        onHover: () => _pointAt(i),
                        onPressed: () => widget.onJumpToPrompt(intValue(prompts[i]['seq'])),
                      ),
                  ],
                ),
              ),
            ),
            if (attention != null && attention < prompts.length)
              Positioned(
                left: 8 + _railWidth + _previewGap,
                top: railTop + slot * attention + slot / 2 - _previewHeight / 2,
                child: IgnorePointer(child: _Preview(text: stringValue(prompts[attention]['preview']))),
              ),
          ],
        );
      },
    );
  }
}

class _Tick extends StatelessWidget {
  const _Tick({
    required this.height,
    required this.label,
    required this.active,
    required this.attention,
    required this.magnification,
    required this.onHover,
    required this.onPressed,
  });
  final double height, magnification;
  final String label;
  final bool active, attention;
  final VoidCallback onHover, onPressed;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final resting = active ? _activePillWidth : _restingPillWidth;
    final width = resting + magnification * (_magnifiedPillWidth - resting);
    final pillHeight =
        _restingPillHeight + magnification * (_magnifiedPillHeight - _restingPillHeight);
    return Semantics(
      button: true,
      selected: active,
      label: label,
      excludeSemantics: true,
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        onEnter: (_) => onHover(),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: onPressed,
          child: SizedBox(
            width: _railWidth,
            height: height,
            child: Padding(
              padding: const EdgeInsets.only(left: 4),
              child: Align(
                alignment: Alignment.centerLeft,
                child: AnimatedContainer(
                  duration: const Duration(milliseconds: 140),
                  curve: Curves.easeOut,
                  width: width,
                  height: pillHeight,
                  decoration: BoxDecoration(
                    borderRadius: BorderRadius.circular(999),
                    color: attention
                        ? Theme.of(context).colorScheme.onSurface
                        : active
                        ? colors.extraMuted
                        : colors.borderAccent,
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

class _Preview extends StatelessWidget {
  const _Preview({required this.text});
  final String text;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final dark = theme.brightness == Brightness.dark;
    return Container(
      width: _previewWidth,
      height: _previewHeight,
      alignment: Alignment.centerLeft,
      padding: const EdgeInsets.symmetric(horizontal: 12),
      decoration: BoxDecoration(
        color: PaseoColors.of(context).surface2,
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: theme.dividerColor),
        boxShadow: [
          BoxShadow(
            color: dark ? const Color(0x33000000) : const Color(0x0a000000),
            offset: const Offset(0, 4),
            blurRadius: dark ? 8 : 16,
          ),
        ],
      ),
      child: Text(
        text,
        maxLines: 2,
        overflow: TextOverflow.ellipsis,
        style: theme.textTheme.bodySmall!.copyWith(
          fontSize: PaseoTypography.of(context).uiSize(12),
          color: theme.colorScheme.onSurface,
        ),
      ),
    );
  }
}
