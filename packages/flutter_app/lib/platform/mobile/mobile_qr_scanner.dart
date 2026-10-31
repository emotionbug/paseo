import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:mobile_scanner/mobile_scanner.dart';
import 'package:permission_handler/permission_handler.dart';

import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';

/// Pairs one scanned pairing link. Resolves true once the host is connected and false when
/// the attempt ended without an error (a cancelled confirmation or password prompt); throws
/// with the message the scan screen shows when pairing failed.
typedef QrPairing = Future<bool> Function(String link);

/// Original `hosts/pair-scan-model.ts` states.
enum _ScanStatus { scanning, pairing, stopped }

/// Original `app/pair-scan.tsx`: a back header, a rounded camera surface with the accent scan
/// frame, "Pairing..." / error copy and "Scan QR code" again over the camera, and a
/// permission card when the camera is not allowed.
class MobileQrScanner extends StatefulWidget {
  const MobileQrScanner({super.key, this.pair});

  /// When set, the scan screen pairs the link itself and pops it only once connected.
  /// Without it the first pairing link seen is popped for the caller to pair.
  final QrPairing? pair;

  static bool get supported =>
      !kIsWeb && (Platform.isAndroid || Platform.isIOS);

  static Future<String?> scan(BuildContext context, {QrPairing? pair}) =>
      Navigator.of(context).push<String>(
        MaterialPageRoute(builder: (_) => MobileQrScanner(pair: pair)),
      );

  @override
  State<MobileQrScanner> createState() => _MobileQrScannerState();
}

/// Original `pairingLinkFromScan`: only pairing links start an attempt.
String? _pairingLinkFromScan(String data) {
  final raw = data.trim();
  if (raw.contains('#offer=') ||
      raw.contains('#connect=') ||
      raw.startsWith('relay://')) {
    return raw;
  }
  return null;
}

class _MobileQrScannerState extends State<MobileQrScanner>
    with WidgetsBindingObserver {
  late final MobileScannerController? _controller = MobileQrScanner.supported
      ? MobileScannerController(formats: const [BarcodeFormat.qrCode])
      : null;
  _ScanStatus _status = _ScanStatus.scanning;
  String? _scanError;
  Object? _cameraError;
  bool _closed = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
  }

  void _capture(BarcodeCapture capture) {
    if (_status != _ScanStatus.scanning || _closed) return;
    for (final barcode in capture.barcodes) {
      final link = _pairingLinkFromScan(barcode.rawValue ?? '');
      if (link == null) continue;
      unawaited(_pairLink(link));
      return;
    }
  }

  Future<void> _pairLink(String link) async {
    final pair = widget.pair;
    if (pair == null) {
      // The caller's pairing dialog validates and confirms the scanned payload.
      _closed = true;
      Navigator.of(context).pop(link);
      return;
    }
    setState(() => _status = _ScanStatus.pairing);
    try {
      final connected = await pair(link);
      if (!mounted) return;
      if (connected) {
        _closed = true;
        Navigator.of(context).pop(link);
        return;
      }
      setState(() {
        _status = _ScanStatus.stopped;
        _scanError = null;
      });
    } catch (error) {
      if (!mounted) return;
      setState(() {
        _status = _ScanStatus.stopped;
        _scanError = errorMessage(error);
      });
    }
  }

  void _scanAgain() {
    if (_status != _ScanStatus.stopped) return;
    setState(() {
      _status = _ScanStatus.scanning;
      _scanError = null;
    });
  }

  Future<void> _start() async {
    final controller = _controller;
    if (controller == null) return;
    try {
      await controller.start();
      if (mounted) setState(() => _cameraError = null);
    } catch (error) {
      if (mounted) setState(() => _cameraError = error);
    }
  }

  /// Original "Grant permission": asks again, or opens the app settings once the OS stops
  /// showing the prompt.
  Future<void> _requestPermission() async {
    final status = await Permission.camera.request();
    if (status.isGranted) {
      await _start();
    } else if (status.isPermanentlyDenied || status.isRestricted) {
      await openAppSettings();
    }
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    final controller = _controller;
    if (controller == null || !controller.value.hasCameraPermission || _closed) {
      return;
    }
    if (state == AppLifecycleState.resumed) {
      unawaited(_start());
    } else if (state == AppLifecycleState.inactive ||
        state == AppLifecycleState.paused) {
      unawaited(
        controller.stop().catchError((Object error) {
          if (mounted) setState(() => _cameraError = error);
        }),
      );
    }
  }

  bool _isPermissionError(Object error) =>
      error is MobileScannerException &&
      error.errorCode == MobileScannerErrorCode.permissionDenied;

  /// Original `permissionCard`: 24px top margin and padding, xl radius, surface2, 16px gap.
  Widget _card({
    required String title,
    required String body,
    required String action,
    required VoidCallback onPressed,
  }) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    return Padding(
      padding: const EdgeInsets.only(top: 24),
      child: Container(
        padding: const EdgeInsets.all(24),
        decoration: BoxDecoration(
          color: colors.surface2,
          borderRadius: BorderRadius.circular(12),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          mainAxisSize: MainAxisSize.min,
          spacing: 16,
          children: [
            Text(
              title,
              style: theme.textTheme.bodyMedium!.copyWith(
                color: theme.colorScheme.onSurface,
                fontWeight: FontWeight.w600,
              ),
            ),
            Text(
              body,
              style: theme.textTheme.bodyMedium!.copyWith(color: colors.muted),
            ),
            _PermissionButton(label: action, onPressed: onPressed),
          ],
        ),
      ),
    );
  }

  Widget _cameraFailure(Object error) => _isPermissionError(error)
      ? _card(
          title: tr(context, 'pairing.scan.cameraPermissionTitle'),
          body: tr(context, 'pairing.scan.cameraPermissionBody'),
          action: tr(context, 'pairing.scan.grantPermission'),
          onPressed: () => unawaited(_requestPermission()),
        )
      : _card(
          title: tr(context, 'pairing.scan.errorTitle'),
          body: errorMessage(error),
          action: tr(context, 'common.actions.retry'),
          onPressed: () => unawaited(_start()),
        );

  Widget _camera(MobileScannerController controller) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final helper = theme.textTheme.bodyMedium!.copyWith(
      color: theme.colorScheme.onSurface,
    );
    return ClipRRect(
      borderRadius: BorderRadius.circular(12),
      child: ColoredBox(
        color: colors.surface2,
        child: Stack(
          fit: StackFit.expand,
          children: [
            MobileScanner(
              controller: controller,
              onDetect: _capture,
              errorBuilder: (context, error) => ColoredBox(
                color: colors.surface0,
                child: Align(
                  alignment: Alignment.topCenter,
                  child: _cameraFailure(error),
                ),
              ),
            ),
            ValueListenableBuilder<MobileScannerState>(
              valueListenable: controller,
              builder: (context, state, _) => state.error != null
                  ? const SizedBox.shrink()
                  : Center(
                      child: Column(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          const SizedBox.square(
                            dimension: 260,
                            child: _ScanFrame(),
                          ),
                          if (_status == _ScanStatus.pairing)
                            Padding(
                              padding: const EdgeInsets.only(top: 24),
                              child: Text(
                                tr(context, 'pairing.scan.pairing'),
                                textAlign: TextAlign.center,
                                style: helper,
                              ),
                            ),
                          if (_status == _ScanStatus.stopped && _scanError != null)
                            Padding(
                              padding: const EdgeInsets.only(top: 24),
                              child: Text(
                                _scanError!,
                                textAlign: TextAlign.center,
                                style: helper,
                              ),
                            ),
                          if (_status == _ScanStatus.stopped)
                            Padding(
                              padding: const EdgeInsets.only(top: 24),
                              child: PaseoButton(
                                label: tr(context, 'pairing.connectionMethods.scanQr.title'),
                                onPressed: _scanAgain,
                              ),
                            ),
                        ],
                      ),
                    ),
            ),
          ],
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final controller = _controller;
    final bottom = MediaQuery.paddingOf(context).bottom;
    final Widget body;
    if (controller == null) {
      // Original web branch: scanning is unavailable, so offer the way back.
      body = Align(
        alignment: Alignment.topCenter,
        child: _card(
          title: tr(context, 'pairing.scan.webUnavailableTitle'),
          body: tr(context, 'pairing.scan.webUnavailableBody'),
          action: tr(context, 'pairing.scan.backToSettings'),
          onPressed: () => Navigator.of(context).maybePop(),
        ),
      );
    } else if (_cameraError != null) {
      body = Align(
        alignment: Alignment.topCenter,
        child: _cameraFailure(_cameraError!),
      );
    } else {
      body = _camera(controller);
    }
    return Scaffold(
      backgroundColor: colors.surface0,
      body: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          _ScanBackHeader(
            title: tr(context, 'pairing.scan.title'),
            onBack: () => Navigator.of(context).maybePop(),
          ),
          Expanded(
            child: Padding(
              padding: EdgeInsets.fromLTRB(24, 0, 24, bottom + 24),
              child: body,
            ),
          ),
        ],
      ),
    );
  }

  @override
  void dispose() {
    _closed = true;
    WidgetsBinding.instance.removeObserver(this);
    final controller = _controller;
    if (controller != null) unawaited(controller.dispose());
    super.dispose();
  }
}

/// Original scan frame: a 260px square marked by four 36px accent corners, 4px strokes and
/// 12px outer radii.
class _ScanFrame extends StatelessWidget {
  const _ScanFrame();

  @override
  Widget build(BuildContext context) {
    final accent = PaseoColors.of(context).accent;
    final side = BorderSide(color: accent, width: 4);
    const radius = Radius.circular(12);
    Widget corner({
      required Alignment alignment,
      required Border border,
      required BorderRadius borderRadius,
    }) => Align(
      alignment: alignment,
      child: Container(
        width: 36,
        height: 36,
        decoration: BoxDecoration(border: border, borderRadius: borderRadius),
      ),
    );
    return Stack(
      children: [
        corner(
          alignment: Alignment.topLeft,
          border: Border(left: side, top: side),
          borderRadius: const BorderRadius.only(topLeft: radius),
        ),
        corner(
          alignment: Alignment.topRight,
          border: Border(right: side, top: side),
          borderRadius: const BorderRadius.only(topRight: radius),
        ),
        corner(
          alignment: Alignment.bottomLeft,
          border: Border(left: side, bottom: side),
          borderRadius: const BorderRadius.only(bottomLeft: radius),
        ),
        corner(
          alignment: Alignment.bottomRight,
          border: Border(right: side, bottom: side),
          borderRadius: const BorderRadius.only(bottomRight: radius),
        ),
      ],
    );
  }
}

/// Original `permissionButton`: a start-aligned blue-500 pill (24/12 padding, lg radius) with
/// white semibold text.
class _PermissionButton extends StatelessWidget {
  const _PermissionButton({required this.label, required this.onPressed});
  final String label;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) => Semantics(
    button: true,
    child: MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: onPressed,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 24, vertical: 12),
          decoration: BoxDecoration(
            color: const Color(0xff3b82f6),
            borderRadius: BorderRadius.circular(8),
          ),
          child: Text(
            label,
            style: Theme.of(context).textTheme.bodyMedium!.copyWith(
              color: Colors.white,
              fontWeight: FontWeight.w600,
            ),
          ),
        ),
      ),
    ),
  );
}

/// Original `BackHeader` → `CompactHeader navigation="back"`: safe-area top plus 8px, 12px
/// sides, a 56px row with the 32px round back island (16px muted arrow) and the lg medium
/// title 8px after it.
class _ScanBackHeader extends StatefulWidget {
  const _ScanBackHeader({required this.title, required this.onBack});
  final String title;
  final VoidCallback onBack;

  @override
  State<_ScanBackHeader> createState() => _ScanBackHeaderState();
}

class _ScanBackHeaderState extends State<_ScanBackHeader> {
  bool _hovered = false, _pressed = false;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final type = PaseoTypography.of(context);
    final insets = MediaQuery.paddingOf(context);
    return Padding(
      padding: EdgeInsets.fromLTRB(12 + insets.left, insets.top + 8, 12 + insets.right, 0),
      child: SizedBox(
        height: 56,
        child: Row(
          spacing: 8,
          children: [
            Semantics(
              button: true,
              label: tr(context, 'common.actions.back'),
              child: MouseRegion(
                cursor: SystemMouseCursors.click,
                onEnter: (_) => setState(() => _hovered = true),
                onExit: (_) => setState(() => _hovered = false),
                child: GestureDetector(
                  behavior: HitTestBehavior.opaque,
                  onTapDown: (_) => setState(() => _pressed = true),
                  onTapCancel: () => setState(() => _pressed = false),
                  onTapUp: (_) => setState(() => _pressed = false),
                  onTap: widget.onBack,
                  child: Padding(
                    padding: const EdgeInsets.symmetric(vertical: 8),
                    child: Container(
                      width: 32,
                      height: 32,
                      alignment: Alignment.center,
                      decoration: BoxDecoration(
                        shape: BoxShape.circle,
                        color: _hovered || _pressed
                            ? paseoInteractionHighlight(context)
                            : null,
                      ),
                      child: Icon(
                        LucideIcons.arrowLeft300,
                        size: 16,
                        color: PaseoColors.of(context).muted,
                      ),
                    ),
                  ),
                ),
              ),
            ),
            Expanded(
              child: Text(
                widget.title,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: theme.textTheme.bodyMedium!.copyWith(
                  fontSize: type.uiSize(16),
                  fontWeight: FontWeight.w500,
                  color: theme.colorScheme.onSurface,
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}
