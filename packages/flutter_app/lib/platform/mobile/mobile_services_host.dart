import 'dart:async';

import 'package:flutter/material.dart';

import '../../features/navigation/host_dialog.dart';
import '../../ui/common.dart';
import 'mobile_services.dart';

/// Place inside the application's Navigator, around the main shell, so links
/// always use the existing review/password dialog rather than silently pairing.
class MobileServicesHost extends StatefulWidget {
  const MobileServicesHost({super.key, required this.child});
  final Widget child;
  @override
  State<MobileServicesHost> createState() => _MobileServicesHostState();
}

class _MobileServicesHostState extends State<MobileServicesHost> {
  final _service = MobileServices.instance;
  Object? _shownError;
  bool _showingLink = false;

  @override
  void initState() {
    super.initState();
    if (!MobileServices.supported) return;
    _service.addListener(_changed);
    _service.pendingConnectionLink.addListener(_changed);
    _changed();
  }

  void _changed() {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      final error = _service.error;
      if (error != null && !identical(error, _shownError)) {
        _shownError = error;
        showError(context, error);
      }
      if (!_showingLink && _service.pendingConnectionLink.value != null) {
        unawaited(_openLinks());
      }
    });
  }

  Future<void> _openLinks() async {
    _showingLink = true;
    try {
      while (mounted && _service.pendingConnectionLink.value != null) {
        final link = _service.takeConnectionLink();
        if (link == null) break;
        await showHostDialog(context, initialLink: link);
      }
    } catch (error) {
      if (mounted) showError(context, error);
    } finally {
      _showingLink = false;
    }
  }

  @override
  Widget build(BuildContext context) => widget.child;

  @override
  void dispose() {
    if (MobileServices.supported) {
      _service.removeListener(_changed);
      _service.pendingConnectionLink.removeListener(_changed);
    }
    super.dispose();
  }
}
