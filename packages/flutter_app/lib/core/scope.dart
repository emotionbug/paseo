import 'dart:async';

import 'package:flutter/widgets.dart';

import 'app_store.dart';

/// The app store for the tree below. [of] rebuilds its caller on every store
/// change; [select] only when the value it read changes; [read] never.
class PaseoScope extends InheritedWidget {
  const PaseoScope({super.key, required this.store, required super.child});

  final AppStore store;

  static AppStore of(BuildContext context) {
    final scope = context.dependOnInheritedWidgetOfExactType<PaseoScope>();
    assert(scope != null, 'PaseoScope is required above this widget');
    return scope!.store;
  }

  /// [read] applied to the store; [context] rebuilds only when a later store
  /// change makes it return a different value (`==`). Return immutable
  /// values (strings, numbers, records), never a collection the store
  /// mutates in place.
  static T select<T>(BuildContext context, T Function(AppStore store) read) {
    final element = context.getElementForInheritedWidgetOfExactType<PaseoScope>();
    assert(element != null, 'PaseoScope is required above this widget');
    final value = read((element!.widget as PaseoScope).store);
    context.dependOnInheritedElement(element, aspect: _Selection(read, value));
    return value;
  }

  /// The store without a dependency, for event handlers and callbacks.
  static AppStore read(BuildContext context) {
    final scope = context.getInheritedWidgetOfExactType<PaseoScope>();
    assert(scope != null, 'PaseoScope is required above this widget');
    return scope!.store;
  }

  @override
  bool updateShouldNotify(PaseoScope oldWidget) => oldWidget.store != store;

  @override
  InheritedElement createElement() => _PaseoScopeElement(this);
}

class _Selection {
  _Selection(this.read, this.value);
  final Object? Function(AppStore store) read;
  final Object? value;
}

/// One dependent's selections from its latest build: a new build replaces
/// them rather than adding to them.
class _Selections {
  final list = <_Selection>[];
  bool stale = false, clearScheduled = false;
}

/// A dependent that called [PaseoScope.of]: every change rebuilds it.
const _everything = Object();

class _PaseoScopeElement extends InheritedElement {
  _PaseoScopeElement(PaseoScope widget) : super(widget) {
    widget.store.addListener(_storeChanged);
  }

  bool _dirty = false;

  AppStore get _store => (widget as PaseoScope).store;

  void _storeChanged() {
    _dirty = true;
    markNeedsBuild();
  }

  @override
  void update(PaseoScope newWidget) {
    final old = _store;
    if (old != newWidget.store) {
      old.removeListener(_storeChanged);
      newWidget.store.addListener(_storeChanged);
    }
    super.update(newWidget);
  }

  @override
  Widget build() {
    if (_dirty) {
      _dirty = false;
      notifyClients(widget as PaseoScope);
    }
    return super.build();
  }

  @override
  void unmount() {
    _store.removeListener(_storeChanged);
    super.unmount();
  }

  @override
  void updateDependencies(Element dependent, Object? aspect) {
    final current = getDependencies(dependent);
    if (identical(current, _everything)) return;
    if (aspect is! _Selection) {
      setDependencies(dependent, _everything);
      return;
    }
    final selections = current is _Selections ? current : _Selections();
    if (selections.stale) {
      selections
        ..stale = false
        ..list.clear();
    }
    // Everything one build selects registers synchronously; the next build's
    // selections start a fresh list.
    if (!selections.clearScheduled) {
      selections.clearScheduled = true;
      scheduleMicrotask(() {
        selections
          ..clearScheduled = false
          ..stale = true;
      });
    }
    selections.list.add(aspect);
    setDependencies(dependent, selections);
  }

  @override
  void notifyDependent(InheritedWidget oldWidget, Element dependent) {
    final current = getDependencies(dependent);
    if (current is _Selections &&
        !current.list.any((selection) => selection.read(_store) != selection.value)) {
      return;
    }
    dependent.didChangeDependencies();
  }
}
