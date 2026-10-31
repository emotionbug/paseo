import 'dart:async';

import 'package:flutter/widgets.dart';

/// One wall-clock cadence for the indicators that stay animated while agents
/// run (status ring, tool shimmer, working dots, elapsed time).
///
/// An active Ticker renders a frame on every vsync, and each frame redraws the
/// whole window. Indicators on this shared timer change in the same frame. A
/// tick renders nothing unless a listener calls setState.
///
/// The cadence follows the window: 10Hz while focused, 4Hz while visible
/// behind another window, and stopped while minimized or hidden.
abstract final class IndicatorClock {
  static const focusedInterval = Duration(milliseconds: 100);
  static const unfocusedInterval = Duration(milliseconds: 250);
  static final Set<VoidCallback> _listeners = {};
  static Timer? _timer;
  static Duration? _interval;
  static final _lifecycle = _LifecycleObserver();

  static Duration? _intervalFor(AppLifecycleState? state) => switch (state) {
    null || AppLifecycleState.resumed => focusedInterval,
    AppLifecycleState.inactive => unfocusedInterval,
    _ => null,
  };

  static void _retime() {
    final interval = _listeners.isEmpty
        ? null
        : _intervalFor(WidgetsBinding.instance.lifecycleState);
    if (interval == _interval && (_timer != null) == (interval != null)) return;
    _timer?.cancel();
    _interval = interval;
    _timer = interval == null
        ? null
        : Timer.periodic(interval, (_) {
            for (final listener in List.of(_listeners)) {
              listener();
            }
          });
  }

  static void _add(VoidCallback listener) {
    if (_listeners.isEmpty) WidgetsBinding.instance.addObserver(_lifecycle);
    _listeners.add(listener);
    _retime();
  }

  static void _remove(VoidCallback listener) {
    _listeners.remove(listener);
    if (_listeners.isEmpty) WidgetsBinding.instance.removeObserver(_lifecycle);
    _retime();
  }
}

class _LifecycleObserver with WidgetsBindingObserver {
  @override
  void didChangeAppLifecycleState(AppLifecycleState state) => IndicatorClock._retime();
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
