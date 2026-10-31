import 'dart:async';

import 'package:flutter/widgets.dart';

/// One wall-clock cadence for the indicators that stay animated while agents
/// run (status ring, tool shimmer, working dots, elapsed time).
///
/// An active Ticker renders a frame on every vsync, and each frame redraws the
/// whole window. Indicators on this shared 10Hz timer change in the same frame.
/// A tick renders nothing unless a listener calls setState.
abstract final class IndicatorClock {
  static const interval = Duration(milliseconds: 100);
  static final Set<VoidCallback> _listeners = {};
  static Timer? _timer;

  static void _add(VoidCallback listener) {
    _listeners.add(listener);
    _timer ??= Timer.periodic(interval, (_) {
      for (final listener in List.of(_listeners)) {
        listener();
      }
    });
  }

  static void _remove(VoidCallback listener) {
    _listeners.remove(listener);
    if (_listeners.isEmpty) {
      _timer?.cancel();
      _timer = null;
    }
  }
}

/// Listens to [IndicatorClock] while the indicator can be seen moving. That
/// excludes hidden tabs (TickerMode, as a Ticker would be muted) and, with
/// [pausesForReducedMotion], disabled animations.
mixin IndicatorClockState<T extends StatefulWidget> on State<T> {
  bool _listening = false;

  bool get pausesForReducedMotion => false;

  /// False while the indicator has nothing to animate (e.g. a finished turn).
  bool get indicatorActive => true;

  /// Runs on every tick; call setState only when what is shown changes.
  void onIndicatorTick();

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    syncIndicatorClock();
  }

  /// Re-evaluates [indicatorActive]; call when a widget update can change it.
  void syncIndicatorClock() {
    final listen = indicatorActive &&
        TickerMode.valuesOf(context).enabled &&
        !(pausesForReducedMotion && MediaQuery.disableAnimationsOf(context));
    if (listen == _listening) return;
    _listening = listen;
    listen ? IndicatorClock._add(onIndicatorTick) : IndicatorClock._remove(onIndicatorTick);
  }

  @override
  void dispose() {
    if (_listening) IndicatorClock._remove(onIndicatorTick);
    super.dispose();
  }
}
