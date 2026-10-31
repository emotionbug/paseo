import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../../ui/indicator_clock.dart';
import '../../../ui/theme.dart';

/// utils/sidebar-agent-state.ts SidebarStateBucket.
enum StatusBucket { needsInput, failed, running, attention, done }

/// The order states are listed in when shown side by side (sidebar groups,
/// subagent pill segments): STATUS_BUCKET_ORDER.
const statusBucketOrder = [
  StatusBucket.needsInput,
  StatusBucket.failed,
  StatusBucket.attention,
  StatusBucket.running,
  StatusBucket.done,
];

/// protocol agent-state-bucket.ts deriveAgentStateBucket.
StatusBucket deriveStatusBucket({
  required String? status,
  int pendingPermissionCount = 0,
  bool requiresAttention = false,
  String? attentionReason,
}) {
  if (pendingPermissionCount > 0 || attentionReason == 'permission') {
    return StatusBucket.needsInput;
  }
  if (status == 'error' || attentionReason == 'error') return StatusBucket.failed;
  if (status == 'running') return StatusBucket.running;
  if (requiresAttention) return StatusBucket.attention;
  return StatusBucket.done;
}

/// utils/status-dot-color.ts over the theme's statusDot* band, which sits
/// louder than the status* family used for icons and text.
Color? statusDotColor(
  BuildContext context,
  StatusBucket bucket, {
  bool showDoneAsInactive = false,
}) {
  final dark = Theme.of(context).brightness == Brightness.dark;
  return switch (bucket) {
    StatusBucket.needsInput => dark ? const Color(0xffdb932e) : const Color(0xffb37824),
    StatusBucket.failed => dark ? const Color(0xfff7796d) : const Color(0xfff12e2f),
    StatusBucket.running => PaseoColors.of(context).running,
    StatusBucket.attention => dark ? const Color(0xff35c264) : const Color(0xff299f51),
    StatusBucket.done =>
      showDoneAsInactive ? Theme.of(context).colorScheme.outline : null,
  };
}

/// status-indicator-geometry.ts / status-ring/geometry.ts.
const statusFilledDotSize = 6.0;
const statusDotSize = statusFilledDotSize + 2;
const statusAlertSize = 12.0;
const statusRingGap = 1.5;
const statusRingStroke = 1.5;
const statusRingSize = statusFilledDotSize + (statusRingGap + statusRingStroke) * 2;
const statusRingHaloInset = 1.0;
const statusRingFrameSize = statusRingSize + statusRingHaloInset * 2;
const _ringPeriodMs = 900;

/// components/status-ring: the running indicator — a 6px dot inside a 1.5px
/// ring whose quarter turns once every 900ms, stepped on [IndicatorClock].
/// Every ring reads one wall clock, so a ring that mounts mid-flight is
/// already in phase with the others.
/// [backdrop] fills the 1px knockout halo; null leaves it transparent.
class StatusRing extends StatefulWidget {
  const StatusRing({super.key, this.backdrop, this.halo = true});
  final Color? backdrop;

  /// False trims the halo so the visible ring is the box (composer track pill).
  final bool halo;

  @override
  State<StatusRing> createState() => _StatusRingState();
}

class _StatusRingState extends State<StatusRing> with IndicatorClockState<StatusRing> {
  static double _phase() =>
      (DateTime.now().millisecondsSinceEpoch % _ringPeriodMs) / _ringPeriodMs;

  @override
  void onIndicatorTick() => setState(() {});

  @override
  Widget build(BuildContext context) {
    final size = widget.halo ? statusRingFrameSize : statusRingSize;
    return Semantics(
      label: 'Agent running',
      child: SizedBox.square(
        dimension: size,
        child: RepaintBoundary(
          child: CustomPaint(
            painter: _RingPainter(
              _phase(),
              PaseoColors.of(context).running,
              widget.halo ? widget.backdrop : null,
            ),
          ),
        ),
      ),
    );
  }
}

class _RingPainter extends CustomPainter {
  _RingPainter(this.rotation, this.color, this.backdrop);
  final double rotation;
  final Color color;
  final Color? backdrop;

  @override
  void paint(Canvas canvas, Size size) {
    final center = size.center(Offset.zero);
    if (backdrop != null) {
      canvas.drawCircle(center, size.width / 2, Paint()..color = backdrop!);
    }
    final radius = (statusRingSize - statusRingStroke) / 2;
    final stroke = Paint()
      ..style = PaintingStyle.stroke
      ..strokeWidth = statusRingStroke;
    canvas.drawCircle(center, radius, stroke..color = color.withValues(alpha: .3));
    canvas.drawArc(
      Rect.fromCircle(center: center, radius: radius),
      -math.pi / 2 + rotation * 2 * math.pi,
      math.pi / 2,
      false,
      stroke..color = color.withValues(alpha: .9),
    );
    canvas.drawCircle(center, statusFilledDotSize / 2, Paint()..color = color);
  }

  @override
  bool shouldRepaint(_RingPainter old) =>
      old.rotation != rotation || old.color != color || old.backdrop != backdrop;
}

/// components/status-icon.tsx: an icon with its state pinned to the bottom-right
/// corner — the running ring, a failed/attention dot with a [backdrop]-coloured
/// 1px border, or the needs-input alert glyph.
class StatusIcon extends StatelessWidget {
  const StatusIcon({
    super.key,
    required this.bucket,
    required this.backdrop,
    required this.child,
    this.size = 14,
  });
  final StatusBucket? bucket;
  final Color backdrop;
  final Widget child;
  final double size;

  @override
  Widget build(BuildContext context) {
    const dotOffset = -2.0;
    const alertOffset = -3.0;
    const ringOffset = dotOffset - (statusRingFrameSize - statusDotSize) / 2;
    final bucket = this.bucket;
    final dot = bucket == StatusBucket.failed || bucket == StatusBucket.attention
        ? statusDotColor(context, bucket!)
        : null;
    return SizedBox.square(
      dimension: size,
      child: Stack(
        clipBehavior: Clip.none,
        alignment: Alignment.center,
        children: [
          child,
          if (bucket == StatusBucket.running)
            Positioned(
              right: ringOffset,
              bottom: ringOffset,
              child: StatusRing(backdrop: backdrop),
            ),
          if (dot != null)
            Positioned(
              right: dotOffset,
              bottom: dotOffset,
              child: Container(
                width: statusDotSize,
                height: statusDotSize,
                decoration: BoxDecoration(
                  color: dot,
                  shape: BoxShape.circle,
                  border: Border.all(color: backdrop),
                ),
              ),
            ),
          if (bucket == StatusBucket.needsInput)
            Positioned(
              right: alertOffset,
              bottom: alertOffset,
              child: _NeedsInputAlert(
                fill: statusDotColor(context, StatusBucket.needsInput)!,
                stroke: PaseoColors.of(context).surface0,
              ),
            ),
        ],
      ),
    );
  }
}

/// Lucide CircleAlert filled with the needs-input colour and stroked in surface0.
class _NeedsInputAlert extends StatelessWidget {
  const _NeedsInputAlert({required this.fill, required this.stroke});
  final Color fill, stroke;

  @override
  Widget build(BuildContext context) => SizedBox.square(
    dimension: statusAlertSize,
    child: Stack(
      alignment: Alignment.center,
      children: [
        Container(
          width: statusAlertSize - 2,
          height: statusAlertSize - 2,
          decoration: BoxDecoration(color: fill, shape: BoxShape.circle),
        ),
        Icon(LucideIcons.circleAlert300, size: statusAlertSize, color: stroke),
      ],
    ),
  );
}
