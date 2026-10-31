import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_svg/flutter_svg.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:package_info_plus/package_info_plus.dart';
import 'package:url_launcher/url_launcher.dart';

import '../../core/app_store.dart';
import '../../core/client.dart';
import '../../platform/mobile/mobile_qr_scanner.dart';
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import 'host_dialog.dart';

/// Original `components/welcome-screen.tsx`, shown while no host is connected: the Paseo
/// logo, the onboarding title/subtitle (plus the paseo.sh link on mobile), the connection
/// method buttons, a ghost Settings button and the app version.
///
/// Mobile offers Scan QR code (primary), Direct connection and Paste pairing link; desktop
/// offers Direct connection (primary), Remote SSH and Paste pairing link. Each opens the
/// matching connection flow; [onFinished] runs once a connection was saved or any saved host
/// comes online (the original `finishOnboarding` redirect).
class WelcomeScreen extends StatefulWidget {
  const WelcomeScreen({
    super.key,
    required this.store,
    required this.onOpenSettings,
    this.onFinished,
  });

  final AppStore store;
  final VoidCallback onOpenSettings;
  final VoidCallback? onFinished;

  @override
  State<WelcomeScreen> createState() => _WelcomeScreenState();
}

class _WelcomeAction {
  const _WelcomeAction({
    required this.label,
    required this.icon,
    required this.primary,
    required this.onPressed,
  });
  final String label;
  final IconData icon;
  final bool primary;
  final VoidCallback onPressed;
}

class _WelcomeScreenState extends State<WelcomeScreen> {
  static const _buildVersion = String.fromEnvironment('PASEO_VERSION');

  String? _version = _buildVersion.trim().isEmpty ? null : _buildVersion.trim();
  bool _busy = false;
  bool _finished = false;

  @override
  void initState() {
    super.initState();
    widget.store.addListener(_storeChanged);
    if (_version == null) unawaited(_loadVersion());
    WidgetsBinding.instance.addPostFrameCallback((_) => _storeChanged());
  }

  @override
  void didUpdateWidget(WelcomeScreen oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (!identical(oldWidget.store, widget.store)) {
      oldWidget.store.removeListener(_storeChanged);
      widget.store.addListener(_storeChanged);
    }
  }

  @override
  void dispose() {
    widget.store.removeListener(_storeChanged);
    super.dispose();
  }

  Future<void> _loadVersion() async {
    try {
      final version = (await PackageInfo.fromPlatform()).version.trim();
      if (mounted && version.isNotEmpty) setState(() => _version = version);
    } catch (_) {
      // Original `resolveAppVersion()` → null renders "—".
    }
  }

  /// Original `useAnyHostOnline`: the first saved host to come online ends onboarding.
  void _storeChanged() {
    if (!mounted) return;
    final online = widget.store.hosts.any(
      (host) => host.client.status == ConnectionStatus.connected && host.error == null,
    );
    if (online) _finish();
  }

  void _finish() {
    if (_finished) return;
    _finished = true;
    widget.onFinished?.call();
  }

  Future<void> _open(String method) async {
    if (_busy) return;
    _busy = true;
    try {
      final saved = await showHostConnectionFlow(
        context,
        method: method,
        store: widget.store,
      );
      if (saved && mounted) _finish();
    } catch (error) {
      if (mounted) showError(context, error);
    } finally {
      _busy = false;
    }
  }

  /// Original `formatVersionWithPrefix`.
  String get _versionText {
    final value = _version?.trim();
    if (value == null || value.isEmpty) return '\u2014';
    return value.startsWith('v') ? value : 'v$value';
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final base = theme.textTheme.bodyMedium!;
    final mobile = Platform.isAndroid || Platform.isIOS;
    final desktop = Platform.isLinux || Platform.isMacOS || Platform.isWindows;
    final actions = <_WelcomeAction>[
      if (mobile && MobileQrScanner.supported)
        _WelcomeAction(
          label: tr(context, 'pairing.connectionMethods.scanQr.title'),
          icon: LucideIcons.qrCode300,
          primary: true,
          onPressed: () => unawaited(_open('scan')),
        ),
      _WelcomeAction(
        label: tr(context, 'pairing.connectionMethods.direct.title'),
        icon: LucideIcons.link2300,
        primary: !(mobile && MobileQrScanner.supported),
        onPressed: () => unawaited(_open('direct')),
      ),
      if (desktop)
        _WelcomeAction(
          label: tr(context, 'pairing.connectionMethods.remoteSsh.title'),
          icon: LucideIcons.terminal300,
          primary: false,
          onPressed: () => unawaited(_open('ssh')),
        ),
      _WelcomeAction(
        label: tr(context, 'pairing.connectionMethods.pasteLink.title'),
        icon: LucideIcons.clipboardPaste300,
        primary: false,
        onPressed: () => unawaited(_open('link')),
      ),
    ];
    final content = Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        _PaseoLogo(size: 96, color: theme.colorScheme.onSurface),
        Padding(
          padding: const EdgeInsets.only(bottom: 48),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            spacing: 8,
            children: [
              Text(
                tr(context, 'onboarding.title'),
                textAlign: TextAlign.center,
                style: base.copyWith(
                  color: theme.colorScheme.onSurface,
                  fontWeight: FontWeight.w500,
                ),
              ),
              Text(
                tr(context, 'onboarding.subtitle'),
                textAlign: TextAlign.center,
                style: base.copyWith(color: colors.muted),
              ),
              if (mobile)
                Semantics(
                  link: true,
                  child: MouseRegion(
                    cursor: SystemMouseCursors.click,
                    child: GestureDetector(
                      behavior: HitTestBehavior.opaque,
                      onTap: () => unawaited(
                        launchUrl(
                          Uri.parse('https://paseo.sh'),
                          mode: LaunchMode.externalApplication,
                        ),
                      ),
                      child: Row(
                        mainAxisSize: MainAxisSize.min,
                        spacing: 6,
                        children: [
                          Text(
                            'paseo.sh',
                            style: base.copyWith(
                              color: colors.accent,
                              fontWeight: FontWeight.w500,
                            ),
                          ),
                          Icon(LucideIcons.externalLink300, size: 14, color: colors.accent),
                        ],
                      ),
                    ),
                  ),
                ),
            ],
          ),
        ),
        ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 420),
          child: SizedBox(
            width: double.infinity,
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              spacing: 12,
              children: [
                for (final action in actions) _WelcomeActionButton(action: action),
              ],
            ),
          ),
        ),
        Padding(
          padding: const EdgeInsets.only(top: 24),
          child: PaseoButton(
            variant: PaseoButtonVariant.ghost,
            size: PaseoControlSize.sm,
            icon: LucideIcons.settings300,
            label: tr(context, 'onboarding.actions.settings'),
            onPressed: widget.onOpenSettings,
          ),
        ),
      ],
    );
    return ColoredBox(
      color: colors.surface0,
      child: ScrollConfiguration(
        behavior: ScrollConfiguration.of(context).copyWith(scrollbars: false),
        child: CustomScrollView(
          slivers: [
            SliverPadding(
              padding: EdgeInsets.fromLTRB(
                24,
                24,
                24,
                24 + MediaQuery.paddingOf(context).bottom,
              ),
              sliver: SliverFillRemaining(
                hasScrollBody: false,
                child: Column(
                  children: [
                    Expanded(child: Center(child: content)),
                    Padding(
                      padding: const EdgeInsets.only(top: 24),
                      child: Text(
                        _versionText,
                        textAlign: TextAlign.center,
                        style: theme.textTheme.bodySmall!.copyWith(color: colors.muted),
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
}

/// Original `WelcomeActionButton`: a centred 12px-gap row (18px glyph, base medium label)
/// with 16px vertical padding, xl radius and a 1px border; surface2 / border colours, or
/// accent with accent-foreground content when primary.
class _WelcomeActionButton extends StatelessWidget {
  const _WelcomeActionButton({required this.action});
  final _WelcomeAction action;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final foreground = action.primary
        ? paseoAccentForeground(context)
        : theme.colorScheme.onSurface;
    return Semantics(
      button: true,
      label: action.label,
      excludeSemantics: true,
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: action.onPressed,
          child: Container(
            padding: const EdgeInsets.symmetric(vertical: 16),
            decoration: BoxDecoration(
              color: action.primary ? colors.accent : colors.surface2,
              borderRadius: BorderRadius.circular(12),
              border: Border.all(
                color: action.primary ? colors.accent : theme.dividerColor,
              ),
            ),
            child: Row(
              mainAxisAlignment: MainAxisAlignment.center,
              spacing: 12,
              children: [
                Icon(action.icon, size: 18, color: foreground),
                Flexible(
                  child: Text(
                    action.label,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: theme.textTheme.bodyMedium!.copyWith(
                      color: foreground,
                      fontWeight: FontWeight.w500,
                    ),
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// Original `components/icons/paseo-logo.tsx`: the 700×700 Paseo mark in the foreground
/// colour.
class _PaseoLogo extends StatelessWidget {
  const _PaseoLogo({required this.size, required this.color});
  final double size;
  final Color color;

  static const _svg =
      '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 700 700" fill="none"><path fill="#000000" d="'
      'M291.495 91.399C333.897 104.892 379.155 135.075 416.229 173.191C453.389 211.394 484.429 259.725 495.708 311.251C497.555 319.693 498.865 328.216 499.586 336.776C509.755 326.554 519.867 317.815 529.89 311.547C540.647 304.821 553.808 299.297 568.641 299.785C584.29 300.299 597.395 307.326 607.747 317.632C632.173 341.947 629.612 372.898 619.872 397.936C610.185 422.833 591.557 447.826 572.732 469.124C553.591 490.78 532.713 510.308 516.779 524.318C508.775 531.355 501.936 537.073 497.07 541.052C494.635 543.043 492.689 544.603 491.334 545.679C490.657 546.217 490.126 546.635 489.756 546.926C489.571 547.071 489.425 547.184 489.321 547.265C489.269 547.305 489.227 547.338 489.196 547.362C489.181 547.374 489.168 547.385 489.157 547.393C489.153 547.397 489.147 547.401 489.144 547.403C489.134 547.4 488.837 547.06 473.001 528.499L489.135 547.411C478.157 555.911 462.033 554.334 453.122 543.89C444.213 533.448 445.887 518.094 456.861 509.592C456.863 509.591 456.865 509.588 456.869 509.586C456.88 509.577 456.902 509.561 456.933 509.536C456.997 509.487 457.101 509.404 457.245 509.292C457.533 509.066 457.979 508.715 458.569 508.247C459.749 507.31 461.506 505.901 463.742 504.073C468.216 500.414 474.589 495.088 482.073 488.508C497.114 475.284 516.315 457.282 533.578 437.75C551.157 417.862 565.26 398.01 571.859 381.048C578.403 364.227 575.681 356.302 570.724 351.367C568.928 349.579 567.744 348.902 567.267 348.676C566.888 348.496 566.811 348.52 566.804 348.52C566.605 348.513 563.971 348.537 557.953 352.3C545.161 360.299 528.815 377.492 506.807 403.867C494.927 418.106 481.871 434.435 467.547 451.957C463.709 457.28 459.503 462.538 454.91 467.717L454.702 467.549C420.808 508.347 380.37 553.856 332.335 593.848C301.853 619.226 262.656 622.597 228.642 614.743C194.834 606.936 162.658 587.448 142.217 561.686C108.054 518.631 100.57 469.801 108.223 427.836C115.56 387.606 137.391 351.005 166.502 331.557C161.248 315.813 156.813 299.49 153.519 283.013C142.593 228.368 143.239 167.031 174.28 119.619C186.922 100.31 205.846 89.1535 227.387 85.2773C248.1 81.5504 270.278 84.648 291.495 91.399Z'
      'M378.642 206.356C345.773 172.563 307.463 147.917 275.208 137.654C259.096 132.527 246.171 131.514 236.828 133.195C228.314 134.727 222.227 138.497 217.721 145.38C196.712 177.468 193.858 224.004 203.82 273.827C206.532 287.394 210.127 300.834 214.345 313.817C236.45 310.276 260.156 311.463 281.22 317.11C319.621 327.403 357.501 355.419 357.501 405.654C357.501 435.255 339.111 465.136 307.278 473.815C273.211 483.103 238.854 464.822 213.105 427.541C203.716 413.947 194.443 397.766 185.947 379.89C174.028 392.223 163.08 411.953 158.673 436.118C153.128 466.518 158.514 501.286 183.085 532.253C195.993 548.522 217.742 562.031 240.771 567.349C263.594 572.619 284.147 569.24 298.664 557.154C349.383 514.927 390.709 466.547 426.366 422.952C448.879 390.86 453.195 356.06 445.578 321.265C436.703 280.718 411.425 240.06 378.642 206.356Z'
      'M306.296 405.722C306.296 384.769 292.223 370.736 267.284 364.051C256.012 361.03 244.156 360.087 233.095 360.771C240.361 375.935 248.168 389.513 255.897 400.704C275.647 429.298 289.989 427.822 293.247 426.934C298.737 425.437 306.296 418.161 306.296 405.722Z'
      '"/></svg>';

  @override
  Widget build(BuildContext context) => SvgPicture.string(
    _svg,
    width: size,
    height: size,
    colorFilter: ColorFilter.mode(color, BlendMode.srcIn),
  );
}
