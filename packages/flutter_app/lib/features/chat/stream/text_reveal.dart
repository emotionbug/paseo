import 'dart:async';

import 'package:flutter/widgets.dart';

// Port of agent-stream/text-reveal.ts + hooks/use-revealed-text.ts: streamed
// text is painted at a paced rate derived from the backlog, so the daemon's
// 60ms coalesced deltas do not show up as jagged jumps.

/// Backlog is drained over this horizon.
const textRevealHorizonMs = 150.0;

/// Reveal steps run on a 30Hz timer. Each step relays out the growing
/// markdown and redraws the window, and a vsync Ticker would also render the
/// frames between steps.
const textRevealFrameIntervalMs = 1000 / 30;

const _maxElapsedMs = 250.0;
const _zeroWidthJoiner = 0x200d;

/// Characters to reveal on this frame: proportional to the backlog, at least one.
int computeRevealStep({required int backlog, required double elapsedMs, double horizonMs = textRevealHorizonMs}) {
  if (backlog <= 0) return 0;
  if (horizonMs <= 0) return backlog;
  final elapsed = elapsedMs.clamp(0.0, _maxElapsedMs);
  if (elapsed <= 0) return 0;
  if (elapsed >= horizonMs) return backlog;
  final step = (backlog * elapsed / horizonMs).ceil();
  return step.clamp(1, backlog);
}

bool _isHighSurrogate(int unit) => unit >= 0xd800 && unit <= 0xdbff;
bool _isLowSurrogate(int unit) => unit >= 0xdc00 && unit <= 0xdfff;

int _codePointAt(String text, int index) {
  final unit = text.codeUnitAt(index);
  if (_isHighSurrogate(unit) && index + 1 < text.length && _isLowSurrogate(text.codeUnitAt(index + 1))) {
    return 0x10000 + ((unit - 0xd800) << 10) + (text.codeUnitAt(index + 1) - 0xdc00);
  }
  return unit;
}

/// text-reveal.ts clampToSafeRevealBoundary: pull a cut back to the start of
/// the extended grapheme cluster containing [index] (Intl.Segmenter
/// `containing(index).index`), so a visible glyph is never painted in parts.
int clampToSafeRevealBoundary(String text, int index) {
  if (index <= 0) return 0;
  if (index >= text.length) return text.length;
  return CharacterRange.at(text, index).stringBeforeLength;
}

int _trailingRegionalIndicators(String text) {
  var count = 0;
  var cursor = text.length;
  while (cursor >= 2) {
    final codePoint = _codePointAt(text, cursor - 2);
    if (codePoint < 0x1f1e6 || codePoint > 0x1f1ff) break;
    count++;
    cursor -= 2;
  }
  return count;
}

/// Holds back a trailing fragment that is certainly waiting on more input.
String _trimIncompleteTrailingCluster(String text) {
  if (text.isEmpty) return text;
  final last = text.codeUnitAt(text.length - 1);
  if (_isHighSurrogate(last) || last == _zeroWidthJoiner) return text.substring(0, text.length - 1);
  if (_trailingRegionalIndicators(text).isOdd) return text.substring(0, text.length - 2);
  return text;
}

/// use-revealed-text.ts useRevealedText: builds [builder] with the paced slice
/// of [text]. First sight of a text is shown whole; only growth while
/// [streaming] is paced, and a completed phase releases everything.
class RevealedText extends StatefulWidget {
  const RevealedText({super.key, required this.text, required this.streaming, required this.builder});
  final String text;
  final bool streaming;
  final Widget Function(BuildContext context, String visible, bool pacing) builder;

  @override
  State<RevealedText> createState() => _RevealedTextState();
}

class _RevealedTextState extends State<RevealedText> {
  late String _target = widget.text;
  late int _revealed = widget.text.length;
  Timer? _timer;
  final Stopwatch _sinceStep = Stopwatch();

  bool get _settled => _revealed >= _target.length;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    _sync();
  }

  @override
  void didUpdateWidget(covariant RevealedText oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (_target != widget.text) {
      // A shorter string means this slot shows a different message.
      _target = widget.text;
      if (_revealed > _target.length) _revealed = _target.length;
    }
    _sync();
  }

  void _sync() {
    // Pacing is only watched in a focused window's shown tab. A hidden tab
    // (TickerMode off) or a window behind others shows the text whole, one
    // frame per delta instead of 30.
    final lifecycle = WidgetsBinding.instance.lifecycleState;
    final paced = TickerMode.valuesOf(context).enabled &&
        (lifecycle == null || lifecycle == AppLifecycleState.resumed);
    if (!widget.streaming || !paced) {
      _stop();
      _revealed = _target.length;
      return;
    }
    if (_settled) {
      _stop();
      return;
    }
    if (_timer != null) return;
    _sinceStep
      ..reset()
      ..start();
    _timer = Timer.periodic(
      Duration(microseconds: (textRevealFrameIntervalMs * 1000).round()),
      (_) => _step(),
    );
  }

  void _stop() {
    _timer?.cancel();
    _timer = null;
    _sinceStep.stop();
  }

  void _step() {
    final elapsedMs = _sinceStep.elapsedMicroseconds / 1000;
    _sinceStep
      ..reset()
      ..start();
    final step = computeRevealStep(backlog: _target.length - _revealed, elapsedMs: elapsedMs);
    if (step > 0) setState(() => _revealed = (_revealed + step).clamp(0, _target.length));
    if (_settled) _stop();
  }

  @override
  void dispose() {
    _stop();
    super.dispose();
  }

  String get _visible {
    if (_settled) return widget.streaming ? _trimIncompleteTrailingCluster(_target) : _target;
    return _target.substring(0, clampToSafeRevealBoundary(_target, _revealed));
  }

  @override
  Widget build(BuildContext context) {
    final visible = _visible;
    return widget.builder(context, visible, widget.streaming || visible.length < _target.length);
  }
}
