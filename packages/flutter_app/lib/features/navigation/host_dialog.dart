import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:url_launcher/url_launcher.dart';
import 'package:uuid/uuid.dart';

import '../../core/app_store.dart';
import '../../core/client.dart';
import '../../core/host_config.dart';
import '../../core/json.dart';
import '../../core/scope.dart';
import '../../core/transport/connection_selection.dart' show probeConnectTimeout;
import '../../platform/mobile/mobile_qr_scanner.dart';
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import 'navigation_actions.dart';

/// Opens the add/edit connection flow (see [showHostConnectionFlow]).
Future<void> showHostDialog(
  BuildContext context, {
  HostConfig? config,
  String? initialLink,
  String? method,
  AppStore? store,
}) async {
  await showHostConnectionFlow(
    context,
    config: config,
    initialLink: initialLink,
    method: method,
    store: store,
  );
}

/// How a connection form sheet closed: [saved], or [back] from its Cancel button. Dismissing
/// the sheet (close glyph, scrim, drag) resolves null.
enum _FormResult { saved, back }

/// Original add-connection flow. Without a [method], [config] or [initialLink] it opens the
/// `AddHostMethodModal`; a form's Cancel returns to it, as in settings. With a [method]
/// (`direct`, `ssh`, `link` or `scan`) the welcome screen opens that form directly and Cancel
/// just closes it. Resolves true once a connection was saved.
Future<bool> showHostConnectionFlow(
  BuildContext context, {
  HostConfig? config,
  String? initialLink,
  String? method,
  AppStore? store,
}) async {
  final appStore = store ?? PaseoScope.of(context);
  if (config != null || initialLink != null || method != null) {
    if (method == 'scan' && config == null && initialLink == null) {
      return _scanAndPair(context, appStore);
    }
    final result = await showAdaptiveSheet<_FormResult>(
      context: context,
      builder: (_) => HostConnectionDialog(
        store: appStore,
        config: config,
        initialLink: initialLink,
        initialMethod: initialLink != null ? 'link' : method ?? 'direct',
      ),
    );
    return result == _FormResult.saved;
  }
  while (true) {
    if (!context.mounted) return false;
    final selected = await showAdaptiveSheet<String>(
      context: context,
      builder: (_) => const _HostMethodSheet(),
    );
    if (selected == null || !context.mounted) return false;
    if (selected == 'scan') return _scanAndPair(context, appStore);
    final result = await showAdaptiveSheet<_FormResult>(
      context: context,
      builder: (_) => HostConnectionDialog(store: appStore, initialMethod: selected),
    );
    if (result != _FormResult.back) return result == _FormResult.saved;
  }
}

/// Original `app/pair-scan.tsx` + `pair-scan-model.ts`: the scan screen pairs each scanned
/// link itself (host confirmation, then connect), stays open on failure with "Scan QR code"
/// again, and asks for the host password in the pairing-link form when the host requires one.
Future<bool> _scanAndPair(BuildContext context, AppStore store) async {
  final link = await MobileQrScanner.scan(
    context,
    pair: (link) => _pairScannedLink(context, store, link),
  );
  return link != null;
}

Future<bool> _pairScannedLink(BuildContext context, AppStore store, String link) async {
  // A saved connection for the same link (an earlier failed attempt) is reused, not duplicated.
  final existing = store.hosts
      .expand((host) => host.connections)
      .where((connection) => connection.endpoint.trim() == link)
      .firstOrNull;
  final config = HostConfig(
    id: existing?.id ?? const Uuid().v4(),
    label: existing?.label ?? '',
    endpoint: link,
    password: existing?.password,
    expectedServerId: existing?.expectedServerId,
    color: existing?.color,
  );
  final resolved = config.resolve();
  if (!context.mounted) return false;
  if (!await _confirmHost(context, store, config, resolved)) return false;
  try {
    await _probeConnection(config);
    if (store.hosts.any((host) => host.connections.any((c) => c.id == config.id))) {
      await store.updateHost(config);
    } else {
      await store.addHost(config);
    }
    final host = store.hostFor(config.id);
    if (host?.error != null) throw StateError(errorMessage(host!.error!));
    return true;
  } catch (error) {
    if (!_isAuthFailure(error) || !context.mounted) rethrow;
    final result = await showAdaptiveSheet<_FormResult>(
      context: context,
      builder: (_) => HostConnectionDialog(
        store: store,
        initialLink: link,
        initialMethod: 'link',
        needsPassword: true,
        preApproved: true,
        connectionId: config.id,
      ),
    );
    return result == _FormResult.saved;
  }
}

/// Original `connectToDaemon` / `connectAndProbe`: a throwaway client without
/// reconnects must reach server_info within the connection's probe timeout
/// (relay 10s, SSH 15s, otherwise 6s) before the connection is saved.
Future<void> _probeConnection(HostConfig config) async {
  final probe = PaseoClient(host: config, autoReconnect: false);
  try {
    await probe.connect().timeout(
      probeConnectTimeout(config),
      onTimeout: () => throw const DaemonException(
        'Connection timed out',
        code: 'DAEMON_REQUEST_TIMEOUT',
      ),
    );
  } finally {
    probe.dispose();
  }
}

/// Original `getConnectionAuthFailureReason`: the host refused the (missing) password.
bool _isAuthFailure(Object error) => RegExp(
  r'password|unauthori[sz]ed|authentication required|\b401\b',
  caseSensitive: false,
).hasMatch(error.toString());

/// Original `AddHostMethodModal`: one bordered surface2 option per method (18px foreground
/// glyph, base title, muted base description 4px below), 16px apart. Remote SSH is desktop
/// only; QR scanning needs a native camera.
class _HostMethodSheet extends StatelessWidget {
  const _HostMethodSheet();

  @override
  Widget build(BuildContext context) {
    final desktop = Platform.isLinux || Platform.isMacOS || Platform.isWindows;
    return SheetContents(
      title: tr(context, 'pairing.connectionMethods.title'),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 16,
        children: [
          _HostMethodOption(
            icon: LucideIcons.link2300,
            title: tr(context, 'pairing.connectionMethods.direct.title'),
            description: tr(context, 'pairing.connectionMethods.direct.description'),
            onTap: () => Navigator.pop(context, 'direct'),
          ),
          if (desktop)
            _HostMethodOption(
              icon: LucideIcons.terminal300,
              title: tr(context, 'pairing.connectionMethods.remoteSsh.title'),
              description: tr(context, 'pairing.connectionMethods.remoteSsh.description'),
              onTap: () => Navigator.pop(context, 'ssh'),
            ),
          if (MobileQrScanner.supported)
            _HostMethodOption(
              icon: LucideIcons.qrCode300,
              title: tr(context, 'pairing.connectionMethods.scanQr.title'),
              description: tr(context, 'pairing.connectionMethods.scanQr.description'),
              onTap: () => Navigator.pop(context, 'scan'),
            ),
          _HostMethodOption(
            icon: LucideIcons.clipboardPaste300,
            title: tr(context, 'pairing.connectionMethods.pasteLink.title'),
            description: tr(context, 'pairing.connectionMethods.pasteLink.description'),
            onTap: () => Navigator.pop(context, 'link'),
          ),
        ],
      ),
    );
  }
}

class _HostMethodOption extends StatelessWidget {
  const _HostMethodOption({
    required this.icon,
    required this.title,
    required this.description,
    required this.onTap,
  });
  final IconData icon;
  final String title;
  final String description;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final base = theme.textTheme.bodyMedium!;
    return Semantics(
      button: true,
      label: title,
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: onTap,
          child: Container(
            padding: const EdgeInsets.all(16),
            decoration: BoxDecoration(
              color: colors.surface2,
              borderRadius: BorderRadius.circular(12),
              border: Border.all(color: theme.dividerColor),
            ),
            child: Row(
              spacing: 16,
              children: [
                Icon(icon, size: 18, color: theme.colorScheme.onSurface),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        title,
                        style: base.copyWith(
                          color: theme.colorScheme.onSurface,
                          fontWeight: FontWeight.normal,
                        ),
                      ),
                      const SizedBox(height: 4),
                      Text(description, style: base.copyWith(color: colors.muted)),
                    ],
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

/// Original `HostConfirmation.confirmLink`: a link whose relay connection (endpoint, TLS and
/// daemon key, as `hostConnectionEquals`) a saved host of that server already has connects
/// without asking; otherwise the `HostConfirmationSheet` asks about a new host or a changed
/// connection of a saved one.
Future<bool> _confirmHost(
  BuildContext context,
  AppStore store,
  HostConfig config,
  ResolvedHost resolved,
) async {
  final serverId = resolved.expectedServerId ?? '';
  final saved = store.hosts.where((host) => host.serverId == serverId).firstOrNull;
  bool sameRelay(HostConfig connection) {
    try {
      final other = connection.resolve();
      return resolved.isRelay &&
          other.isRelay &&
          other.daemonPublicKey == resolved.daemonPublicKey &&
          other.uri.scheme == resolved.uri.scheme &&
          other.uri.host == resolved.uri.host &&
          other.uri.port == resolved.uri.port;
    } on FormatException {
      return false;
    }
  }

  if (saved != null && saved.connections.any(sameRelay)) return true;
  final approved = await showAdaptiveSheet<bool>(
    context: context,
    builder: (_) => _HostConfirmationSheet(
      serverId: serverId,
      keyFingerprint: _formatDaemonKeyFingerprint(resolved.daemonPublicKey ?? ''),
      relayEndpoint: '${resolved.uri.host}:${resolved.uri.port}',
      changedConnection: saved != null,
    ),
  );
  return approved == true;
}

/// Original `formatDaemonKeyFingerprint`: the key itself in groups of four; keys longer than
/// 16 characters show their first and last 8.
String _formatDaemonKeyFingerprint(String key) {
  final normalized = key.replaceAll(RegExp('[^A-Za-z0-9]'), '');
  String group(String value) => value
      .replaceAllMapped(RegExp('(.{4})'), (match) => '${match[1]} ')
      .trim();
  if (normalized.length <= 16) return group(normalized);
  return '${group(normalized.substring(0, 8))} … ${group(normalized.substring(normalized.length - 8))}';
}

/// Original `hosts/host-confirmation-sheet.tsx`: muted description, a settings card of
/// Host / Key fingerprint / Relay rows, then small Cancel / Connect buttons 16px below.
class _HostConfirmationSheet extends StatelessWidget {
  const _HostConfirmationSheet({
    required this.serverId,
    required this.keyFingerprint,
    required this.relayEndpoint,
    required this.changedConnection,
  });
  final String serverId, keyFingerprint, relayEndpoint;
  final bool changedConnection;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    return SheetContents(
      title: tr(context, 'pairing.hostConfirmation.title'),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 16,
        children: [
          Text(
            tr(
              context,
              changedConnection
                  ? 'pairing.hostConfirmation.descriptionChanged'
                  : 'pairing.hostConfirmation.description',
            ),
            style: theme.textTheme.bodyMedium!.copyWith(color: colors.muted),
          ),
          Container(
            clipBehavior: Clip.antiAlias,
            decoration: BoxDecoration(
              color: colors.surface1,
              borderRadius: BorderRadius.circular(8),
              border: Border.all(color: theme.dividerColor),
            ),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                _DetailRow(
                  label: tr(context, 'pairing.hostConfirmation.hostLabel'),
                  value: serverId,
                ),
                _DetailRow(
                  label: tr(context, 'pairing.hostConfirmation.fingerprintLabel'),
                  value: keyFingerprint,
                  showBorder: true,
                ),
                _DetailRow(
                  label: tr(context, 'pairing.hostConfirmation.relayLabel'),
                  value: relayEndpoint,
                  showBorder: true,
                ),
              ],
            ),
          ),
          Padding(
            padding: const EdgeInsets.only(top: 16),
            child: Row(
              spacing: 8,
              children: [
                Expanded(
                  child: PaseoButton(
                    expand: true,
                    size: PaseoControlSize.sm,
                    label: tr(context, 'common.actions.cancel'),
                    onPressed: () => Navigator.pop(context, false),
                  ),
                ),
                Expanded(
                  child: PaseoButton(
                    expand: true,
                    size: PaseoControlSize.sm,
                    variant: PaseoButtonVariant.primary,
                    label: tr(context, 'pairing.hostConfirmation.connect'),
                    onPressed: () => Navigator.pop(context, true),
                  ),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

/// Original settings `row` with a `rowTitle` and selectable `rowHint`.
class _DetailRow extends StatelessWidget {
  const _DetailRow({required this.label, required this.value, this.showBorder = false});
  final String label, value;
  final bool showBorder;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Container(
      padding: const EdgeInsets.all(16),
      decoration: BoxDecoration(
        border: showBorder ? Border(top: BorderSide(color: theme.dividerColor)) : null,
      ),
      child: Padding(
        padding: const EdgeInsets.only(right: 12),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              label,
              style: theme.textTheme.bodyMedium!.copyWith(color: theme.colorScheme.onSurface),
            ),
            const SizedBox(height: 4),
            SelectableText(
              value,
              style: theme.textTheme.bodySmall!.copyWith(color: PaseoColors.of(context).muted),
            ),
          ],
        ),
      ),
    );
  }
}

/// The original connection forms in one sheet: `AddHostModal` (`direct`),
/// `AddRemoteSshHostModal` (`ssh`) and `PairLinkModal` (`link`). An existing [config] with an
/// endpoint edits that connection.
class HostConnectionDialog extends StatefulWidget {
  const HostConnectionDialog({
    super.key,
    required this.store,
    this.config,
    this.initialLink,
    this.initialMethod = 'direct',
    this.needsPassword = false,
    this.preApproved = false,
    this.connectionId,
  });
  final AppStore store;
  final HostConfig? config;
  final String? initialLink;
  final String initialMethod;

  /// Original `PairLinkModal passwordRequired`: the host already asked for a password.
  final bool needsPassword;

  /// The [initialLink] pairing was already confirmed by the user.
  final bool preApproved;

  /// Reuses a connection saved by an earlier attempt instead of adding another.
  final String? connectionId;

  @override
  State<HostConnectionDialog> createState() => _HostConnectionDialogState();
}

class _HostConnectionDialogState extends State<HostConnectionDialog> {
  late final String _id =
      widget.config?.id ?? widget.connectionId ?? const Uuid().v4();
  late final _label = TextEditingController(text: widget.config?.label ?? '');
  late final _password = TextEditingController(
    text: widget.config?.password ?? '',
  );
  late final _endpoint = TextEditingController(
    text: widget.initialLink ?? widget.config?.endpoint ?? '',
  );
  late final _identity = TextEditingController(
    text: widget.config?.expectedServerId ?? '',
  );
  late final _color = TextEditingController(text: widget.config?.color ?? '');
  final _sshTarget = TextEditingController();
  final _directHost = TextEditingController();
  final _directPort = TextEditingController(text: '6767');
  final _portFocus = FocusNode();
  bool _useTls = false;
  bool _showPassword = false;
  late bool _advanced = widget.config?.endpoint.isNotEmpty == true;
  late bool _details = widget.config != null;
  bool _busy = false;
  late final String _method = widget.initialLink == null ? widget.initialMethod : 'link';
  late final bool _editing = widget.config?.endpoint.isNotEmpty == true;
  bool _saved = false;
  String? _error;
  late String? _approvedPairing =
      widget.preApproved && widget.initialLink != null ? '${widget.initialLink!.trim()}\n' : null;
  late bool _needsPassword = widget.needsPassword;
  late String _passwordLink = widget.initialLink?.trim() ?? '';

  @override
  void dispose() {
    for (final controller in [
      _label,
      _endpoint,
      _password,
      _identity,
      _color,
      _sshTarget,
      _directHost,
      _directPort,
    ]) {
      controller.dispose();
    }
    _portFocus.dispose();
    super.dispose();
  }

  /// Original `prepareDirectConnection` / `parseSshTransportUri` / pairing-link checks.
  String _resolveEndpoint() {
    switch (_method) {
      case 'ssh':
        final raw = _sshTarget.text.trim();
        if (raw.isEmpty) throw FormatException(tr(context, 'pairing.remoteSsh.errors.targetRequired'));
        final uri = Uri.tryParse(raw.contains('://') ? raw : 'ssh://$raw');
        if (uri == null ||
            uri.scheme != 'ssh' ||
            uri.host.isEmpty ||
            uri.host.startsWith('-') ||
            uri.fragment.isNotEmpty ||
            uri.queryParameters.keys.any((key) => key != 'daemonPort')) {
          throw FormatException(tr(context, 'pairing.remoteSsh.errors.invalidTarget'));
        }
        return uri.toString();
      case 'link':
        final link = _endpoint.text.trim();
        if (link.isEmpty) throw FormatException(tr(context, 'pairing.link.errors.required'));
        if (!link.contains('#offer=') &&
            !link.startsWith('relay://') &&
            !link.contains('#connect=')) {
          throw FormatException(tr(context, 'pairing.link.errors.missingOffer'));
        }
        return link;
      default:
        if (_advanced) {
          final uri = _endpoint.text.trim();
          if (uri.isEmpty) throw FormatException(tr(context, 'pairing.direct.errors.hostRequired'));
          return uri;
        }
        if (_directHost.text.trim().isEmpty) {
          throw FormatException(tr(context, 'pairing.direct.errors.hostRequired'));
        }
        final port = int.tryParse(_directPort.text.trim());
        if (port == null || port < 1 || port > 65535) {
          throw FormatException(tr(context, 'pairing.direct.errors.invalidPort'));
        }
        return _directUri();
    }
  }

  Future<void> _save() async {
    if (_busy) return;
    final String endpoint;
    try {
      endpoint = _resolveEndpoint();
      if (_color.text.trim().isNotEmpty &&
          !RegExp(r'^#[0-9a-fA-F]{6}$').hasMatch(_color.text.trim())) {
        throw const FormatException('Badge color must use #RRGGBB');
      }
    } on FormatException catch (error) {
      setState(() => _error = error.message);
      return;
    }
    setState(() {
      _busy = true;
      _error = null;
    });
    var relay = false;
    try {
      final config = HostConfig(
        id: _id,
        label: _label.text.trim(),
        endpoint: endpoint,
        password: _password.text.isEmpty ? null : _password.text,
        localCredential: endpoint == widget.config?.endpoint
            ? widget.config?.localCredential
            : null,
        localCredentialPath: endpoint == widget.config?.endpoint
            ? widget.config?.localCredentialPath
            : null,
        expectedServerId: _identity.text.trim().isEmpty
            ? null
            : _identity.text.trim(),
        color: _color.text.trim().isEmpty ? null : _color.text.trim(),
      );
      // Resolve before storing: pairing URLs retain their approved public-key/security floor.
      final resolved = config.resolve();
      relay = resolved.isRelay;
      final pairingIdentity = '$endpoint\n${config.expectedServerId ?? ''}';
      if (relay && _approvedPairing != pairingIdentity) {
        if (!await _confirmHost(context, widget.store, config, resolved)) return;
        _approvedPairing = pairingIdentity;
      }
      // Original `connectToDaemon` precedes every upsert: only a connection that
      // answers hello is saved; a failed attempt leaves the registry unchanged.
      await _probeConnection(config);
      if (widget.config != null ||
          _saved ||
          widget.store.hosts.any((host) => host.connections.any((c) => c.id == _id))) {
        await widget.store.updateHost(config);
      } else {
        await widget.store.addHost(config);
        _saved = true;
      }
      final matches = widget.store.hosts.where((host) => host.config.id == _id);
      if (matches.isNotEmpty && matches.first.error != null) {
        throw StateError(errorMessage(matches.first.error!));
      }
      if (mounted) Navigator.pop(context, _FormResult.saved);
    } catch (error) {
      if (!mounted) return;
      setState(() {
        // Unparseable input reads as the parse error itself, as `prepareDirectConnection`.
        _error = error is FormatException
            ? errorMessage(error)
            : _failureMessage(endpoint, error, relay: relay);
        if (_method == 'link' && _isAuthFailure(error)) {
          _needsPassword = true;
          _passwordLink = _endpoint.text.trim();
        }
      });
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  /// Original failure copy: SSH wraps the detail, pairing links show the error itself, and
  /// direct connections use `buildConnectionFailureCopy`.
  String _failureMessage(String endpoint, Object error, {required bool relay}) {
    final raw = errorMessage(error).trim();
    if (_method == 'ssh') {
      return tr(context, 'pairing.remoteSsh.errors.failedToConnect', args: {'detail': raw});
    }
    if (_method == 'link') {
      return raw.isEmpty ? tr(context, 'pairing.link.errors.unableToPair') : raw;
    }
    if (relay) {
      return raw.isEmpty ? tr(context, 'pairing.direct.errors.invalidConnection') : raw;
    }
    final title = tr(
      context,
      'pairing.direct.errors.failedToConnect',
      args: {'endpoint': _withoutPassword(endpoint)},
    );
    final detail = _directFailureDetail(error, raw);
    if (raw.isNotEmpty && raw != detail) {
      return '$title\n$detail\n${tr(context, 'pairing.direct.errors.details', args: {'detail': raw})}';
    }
    return '$title\n$detail';
  }

  String _withoutPassword(String endpoint) {
    final uri = Uri.tryParse(endpoint);
    if (uri == null || !uri.queryParameters.containsKey('password')) return endpoint;
    final query = Map<String, String>.of(uri.queryParameters)..remove('password');
    return uri.replace(queryParameters: query.isEmpty ? null : query).toString();
  }

  /// Original `buildConnectionFailureCopy` detail, with the Dart socket wording of each case.
  String _directFailureDetail(Object error, String raw) {
    final lower = '$raw ${error.runtimeType}'.toLowerCase();
    if (_isAuthFailure(error)) return raw;
    if (lower.contains('timed out')) return tr(context, 'pairing.direct.errors.timedOut');
    if (lower.contains('econnrefused') ||
        lower.contains('connection refused') ||
        lower.contains('err_connection_refused')) {
      return tr(context, 'pairing.direct.errors.refused');
    }
    if (lower.contains('unable to resolve host') ||
        lower.contains('enotfound') ||
        lower.contains('failed host lookup')) {
      return tr(context, 'pairing.direct.errors.hostNotFound');
    }
    if (lower.contains('certificate') ||
        lower.contains('certpathvalidator') ||
        lower.contains('trust anchor') ||
        lower.contains('handshake') ||
        lower.contains('tls') ||
        lower.contains('ssl')) {
      return tr(context, 'pairing.direct.errors.tlsError');
    }
    if (lower.contains('not found')) return tr(context, 'pairing.direct.errors.hostNotFound');
    if (lower.contains('ehostunreach') ||
        lower.contains('host is unreachable') ||
        lower.contains('no route to host') ||
        lower.contains('network is unreachable')) {
      return tr(context, 'pairing.direct.errors.hostUnreachable');
    }
    return tr(context, 'pairing.direct.errors.unableToConnect');
  }

  String _directUri() => Uri(
    scheme: 'tcp',
    host: _directHost.text.trim(),
    port: int.parse(_directPort.text.trim()),
    queryParameters: _useTls ? const {'ssl': 'true'} : null,
  ).toString();

  /// Original `handleToggleAdvanced`: the URI and the host/port/SSL draft convert both ways.
  void _toggleAdvanced() {
    if (_busy) return;
    if (!_advanced) {
      final port = int.tryParse(_directPort.text.trim());
      _endpoint.text = _directHost.text.trim().isNotEmpty &&
              port != null &&
              port >= 1 &&
              port <= 65535
          ? _directUri()
          : '';
    } else {
      final input = _endpoint.text.trim();
      final uri = Uri.tryParse(input.contains('://') ? input : 'tcp://$input');
      if (uri != null && const {'tcp', 'ws', 'wss'}.contains(uri.scheme) && uri.host.isNotEmpty) {
        _directHost.text = uri.host;
        _directPort.text = '${uri.hasPort ? uri.port : 6767}';
        _useTls = uri.scheme == 'wss' || uri.queryParameters['ssl'] == 'true';
        if (uri.queryParameters['password'] case final String password) {
          _password.text = password;
        }
      }
    }
    setState(() {
      _error = null;
      _advanced = !_advanced;
    });
  }

  /// Original pairing-link `PairingTargetTracker`: another link drops the password prompt.
  void _linkChanged(String value) {
    if (!_needsPassword || value.trim() == _passwordLink) return;
    setState(() {
      _password.clear();
      _needsPassword = false;
      _error = null;
    });
  }

  /// Original `AddHostModal` / `PairLinkModal` input: surface2, lg radius, 16/12 padding,
  /// a 1px border that stays on focus.
  InputDecoration _input(BuildContext context, String? hint) {
    final colors = PaseoColors.of(context);
    final border = OutlineInputBorder(
      borderRadius: BorderRadius.circular(8),
      borderSide: BorderSide(color: Theme.of(context).dividerColor),
    );
    return InputDecoration(
      hintText: hint,
      hintStyle: TextStyle(color: colors.muted),
      filled: true,
      fillColor: colors.surface2,
      hoverColor: Colors.transparent,
      isDense: true,
      contentPadding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
      border: border,
      enabledBorder: border,
      disabledBorder: border,
      focusedBorder: border,
    );
  }

  /// Original label: muted base, medium for the direct/link forms and normal in `Field`.
  TextStyle _labelStyle(BuildContext context, {bool medium = true}) =>
      Theme.of(context).textTheme.bodyMedium!.copyWith(
        color: PaseoColors.of(context).muted,
        fontWeight: medium ? FontWeight.w500 : FontWeight.normal,
      );

  Widget _field(BuildContext context, String label, Widget input, {bool medium = true}) =>
      Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 8,
        children: [Text(label, style: _labelStyle(context, medium: medium)), input],
      );

  Widget _text(
    BuildContext context,
    TextEditingController controller, {
    String? hint,
    bool url = false,
    bool number = false,
    bool secret = false,
    bool autofocus = false,
    bool form = false,
    bool? enabled,
    FocusNode? focusNode,
    TextInputAction? action,
    ValueChanged<String>? onSubmitted,
    ValueChanged<String>? onChanged,
  }) => TextField(
    controller: controller,
    focusNode: focusNode,
    enabled: enabled ?? !_busy,
    autofocus: autofocus,
    autocorrect: false,
    enableSuggestions: !secret,
    obscureText: secret,
    textCapitalization: TextCapitalization.none,
    textInputAction: action,
    keyboardType: number
        ? TextInputType.number
        : url
        ? TextInputType.url
        : null,
    style: Theme.of(context).textTheme.bodyMedium!.copyWith(
      color: Theme.of(context).colorScheme.onSurface,
    ),
    decoration: form ? InputDecoration(hintText: hint) : _input(context, hint),
    onChanged: onChanged,
    onSubmitted: onSubmitted,
  );

  /// Original password-visibility toggle: [form] uses the `FormTextInput` adornment (field
  /// height, transparent border turning `borderAccent` on hover/press); the direct form a
  /// 44px bordered surface2 square. Muted 18px eye glyph.
  Widget _eye(BuildContext context, String scope, {bool form = false}) => _EyeButton(
    visible: _showPassword,
    form: form,
    tooltip: tr(context, '$scope.passwordVisibility.${_showPassword ? 'hide' : 'show'}'),
    onPressed: _busy ? null : () => setState(() => _showPassword = !_showPassword),
  );

  Widget _toggle(BuildContext context, bool open, String label, String? semantics, VoidCallback onTap) =>
      Align(
        alignment: Alignment.centerLeft,
        child: Semantics(
          button: true,
          label: semantics,
          child: MouseRegion(
            cursor: _busy ? SystemMouseCursors.basic : SystemMouseCursors.click,
            child: GestureDetector(
              behavior: HitTestBehavior.opaque,
              onTap: _busy ? null : onTap,
              child: Padding(
                padding: const EdgeInsets.symmetric(vertical: 4),
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  spacing: 8,
                  children: [
                    Icon(
                      open ? LucideIcons.chevronDown300 : LucideIcons.chevronRight300,
                      size: 16,
                      color: PaseoColors.of(context).muted,
                    ),
                    Text(
                      label,
                      style: Theme.of(context).textTheme.bodyMedium!.copyWith(
                        color: Theme.of(context).colorScheme.onSurface,
                        fontWeight: FontWeight.w500,
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ),
        ),
      );

  Widget? _errorText(BuildContext context) => _error == null
      ? null
      : SelectableText(
          _error!,
          style: Theme.of(context).textTheme.bodyMedium!.copyWith(
            color: Theme.of(context).colorScheme.error,
          ),
        );

  List<Widget> _directBody(BuildContext context) {
    final colors = PaseoColors.of(context);
    final theme = Theme.of(context);
    return [
      if (!_advanced || !_editing)
        Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          spacing: 12,
          children: [
            Expanded(
              child: _field(
                context,
                tr(context, 'pairing.direct.fields.host'),
                _text(
                  context,
                  _directHost,
                  hint: 'localhost',
                  url: true,
                  autofocus: !_editing,
                  action: TextInputAction.next,
                  onSubmitted: (_) => _portFocus.requestFocus(),
                ),
              ),
            ),
            SizedBox(
              width: 112,
              child: _field(
                context,
                tr(context, 'pairing.direct.fields.port'),
                _text(
                  context,
                  _directPort,
                  hint: '6767',
                  number: true,
                  focusNode: _portFocus,
                  action: TextInputAction.done,
                  onSubmitted: (_) => _save(),
                ),
              ),
            ),
          ],
        ),
      if (!_advanced || !_editing)
        Semantics(
          checked: _useTls,
          enabled: !_busy,
          label: tr(context, 'pairing.direct.fields.useSsl'),
          child: MouseRegion(
            cursor: _busy ? SystemMouseCursors.basic : SystemMouseCursors.click,
            child: GestureDetector(
              behavior: HitTestBehavior.opaque,
              onTap: _busy ? null : () => setState(() => _useTls = !_useTls),
              child: Row(
                spacing: 12,
                children: [
                  Container(
                    width: 22,
                    height: 22,
                    alignment: Alignment.center,
                    decoration: BoxDecoration(
                      color: _useTls ? colors.accent : null,
                      borderRadius: BorderRadius.circular(2),
                      border: Border.all(
                        color: _useTls ? colors.accent : theme.dividerColor,
                      ),
                    ),
                    child: _useTls
                        ? Icon(
                            LucideIcons.check300,
                            size: 14,
                            color: paseoAccentForeground(context),
                          )
                        : null,
                  ),
                  Text(
                    tr(context, 'pairing.direct.fields.useSsl'),
                    style: _labelStyle(context),
                  ),
                ],
              ),
            ),
          ),
        ),
      _field(
        context,
        tr(context, 'pairing.direct.fields.password'),
        Row(
          spacing: 8,
          children: [
            Expanded(
              child: _text(
                context,
                _password,
                hint: tr(context, 'pairing.direct.fields.optional'),
                secret: !_showPassword,
                action: TextInputAction.done,
                onSubmitted: (_) => _save(),
              ),
            ),
            _eye(context, 'pairing.direct'),
          ],
        ),
      ),
      Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 8,
        children: [
          _toggle(
            context,
            _advanced,
            tr(context, 'pairing.direct.advanced.label'),
            tr(context, _advanced ? 'pairing.direct.advanced.hide' : 'pairing.direct.advanced.show'),
            _toggleAdvanced,
          ),
          if (_advanced)
            _text(
              context,
              _endpoint,
              hint: 'tcp://localhost:6767?ssl=true',
              url: true,
              action: TextInputAction.done,
              // Original: submitting the URI folds it back into the host/port draft.
              onSubmitted: (_) => _editing ? _save() : _toggleAdvanced(),
            ),
          ?_errorText(context),
        ],
      ),
    ];
  }

  List<Widget> _linkBody(BuildContext context) {
    final host = () {
      try {
        return HostConfig(id: '', label: '', endpoint: _endpoint.text.trim())
                .resolve()
                .expectedServerId ??
            'host';
      } catch (_) {
        return 'host';
      }
    }();
    return [
      Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 8,
        children: [
          Text(tr(context, 'pairing.link.label'), style: _labelStyle(context)),
          _text(
            context,
            _endpoint,
            hint: 'https://app.paseo.sh/#offer=...',
            url: true,
            autofocus: true,
            enabled: true,
            onChanged: _linkChanged,
          ),
          ?_errorText(context),
        ],
      ),
      if (_needsPassword)
        _field(
          context,
          tr(context, 'pairing.hostPassword.title', args: {'host': host}),
          _text(context, _password, secret: true, enabled: true),
        ),
    ];
  }

  List<Widget> _sshBody(BuildContext context) {
    final theme = Theme.of(context);
    final type = PaseoTypography.of(context);
    return [
      Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 8,
        children: [
          Text(tr(context, 'pairing.remoteSsh.fields.target'), style: _labelStyle(context, medium: false)),
          _text(
            context,
            _sshTarget,
            hint: 'ssh://user@host',
            form: true,
            autofocus: true,
            action: TextInputAction.done,
            onSubmitted: (_) => _save(),
          ),
          if (_error != null)
            SelectableText(
              _error!,
              style: theme.textTheme.bodySmall!.copyWith(
                color: const Color(0xfffca5a5),
                fontSize: type.uiSize(12),
                height: 1.4,
              ),
            ),
        ],
      ),
      _field(
        context,
        tr(context, 'pairing.remoteSsh.fields.password'),
        Row(
          spacing: 8,
          children: [
            Expanded(
              child: _text(
                context,
                _password,
                hint: tr(context, 'pairing.remoteSsh.fields.optional'),
                secret: !_showPassword,
                form: true,
                action: TextInputAction.done,
                onSubmitted: (_) => _save(),
              ),
            ),
            _eye(context, 'pairing.remoteSsh', form: true),
          ],
        ),
        medium: false,
      ),
    ];
  }

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final theme = Theme.of(context);
    final scope = switch (_method) {
      'ssh' => 'pairing.remoteSsh',
      'link' => 'pairing.link',
      _ => 'pairing.direct',
    };
    final helper = theme.textTheme.bodyMedium!.copyWith(color: colors.muted);
    return SheetContents(
      title: _editing ? 'Edit connection' : tr(context, '$scope.title'),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 16,
        children: [
          Text(
            tr(context, '$scope.helper'),
            // Original remote SSH helper is the sm size.
            style: _method == 'ssh' ? theme.textTheme.bodySmall!.copyWith(color: colors.muted) : helper,
          ),
          ...switch (_method) {
            'ssh' => _sshBody(context),
            'link' => _linkBody(context),
            _ => _directBody(context),
          },
          if (_editing)
            Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              spacing: 8,
              children: [
                _toggle(
                  context,
                  _details,
                  'Host details',
                  null,
                  () => setState(() => _details = !_details),
                ),
                if (_details) ...[
                  _field(context, 'Host name', _text(context, _label, hint: tr(context, 'pairing.direct.fields.optional'))),
                  _field(context, 'Expected server ID', _text(context, _identity, hint: tr(context, 'pairing.direct.fields.optional'))),
                  _field(context, 'Badge color', _text(context, _color, hint: '#4488CC')),
                ],
              ],
            ),
          Padding(
            padding: const EdgeInsets.only(top: 8),
            child: Row(
              spacing: 12,
              children: [
                Expanded(
                  child: PaseoButton(
                    expand: true,
                    label: tr(context, '$scope.actions.cancel'),
                    onPressed: _busy ? null : () => Navigator.pop(context, _FormResult.back),
                  ),
                ),
                Expanded(
                  child: PaseoButton(
                    expand: true,
                    variant: PaseoButtonVariant.primary,
                    icon: switch (_method) {
                      'ssh' => LucideIcons.terminal300,
                      'link' => LucideIcons.link300,
                      _ => LucideIcons.link2300,
                    },
                    label: _method == 'link'
                        ? tr(context, _busy ? 'pairing.link.actions.pairing' : 'pairing.link.actions.pair')
                        : tr(context, _busy ? '$scope.actions.connecting' : '$scope.actions.connect'),
                    onPressed: _busy ? null : _save,
                  ),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

class _EyeButton extends StatefulWidget {
  const _EyeButton({
    required this.visible,
    required this.form,
    required this.tooltip,
    required this.onPressed,
  });
  final bool visible, form;
  final String tooltip;
  final VoidCallback? onPressed;

  @override
  State<_EyeButton> createState() => _EyeButtonState();
}

class _EyeButtonState extends State<_EyeButton> {
  bool _hovered = false, _pressed = false;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final disabled = widget.onPressed == null;
    final mobile = Platform.isAndroid || Platform.isIOS;
    final double size = widget.form && !mobile ? 32 : 44;
    final double radius = widget.form && !mobile ? 6 : 8;
    final Color border = widget.form
        ? (!disabled && (_hovered || _pressed) ? colors.borderAccent : Colors.transparent)
        : theme.dividerColor;
    return Tooltip(
      message: widget.tooltip,
      child: Semantics(
        button: true,
        enabled: !disabled,
        label: widget.tooltip,
        child: MouseRegion(
          cursor: disabled ? SystemMouseCursors.basic : SystemMouseCursors.click,
          onEnter: (_) => setState(() => _hovered = true),
          onExit: (_) => setState(() => _hovered = false),
          child: GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTapDown: disabled ? null : (_) => setState(() => _pressed = true),
            onTapCancel: () => setState(() => _pressed = false),
            onTapUp: disabled ? null : (_) => setState(() => _pressed = false),
            onTap: widget.onPressed,
            child: Opacity(
              opacity: widget.form && disabled ? .5 : 1,
              child: Container(
                width: size,
                height: size,
                alignment: Alignment.center,
                decoration: BoxDecoration(
                  color: colors.surface2,
                  borderRadius: BorderRadius.circular(radius),
                  border: Border.all(color: border),
                ),
                child: Icon(
                  widget.visible ? LucideIcons.eyeOff300 : LucideIcons.eye300,
                  size: 18,
                  color: colors.muted,
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// Original `PairDeviceModal` (desktop/components/pair-device-modal.tsx): the
/// "Pair a device" sheet, 480px wide on desktop.
Future<void> showHostPairing(
  BuildContext context,
  AppStore store,
  String serverId,
) => showAdaptiveSheet<void>(
  context: context,
  maxWidth: 480,
  builder: (sheet) => SheetContents(
    title: tr(sheet, 'settings.host.pairDevices.rowTitle'),
    child: _PairDeviceSection(store: store, serverId: serverId),
  ),
);

/// Original `PairDeviceSection`: loads the host's pairing offer and shows, in order, the
/// disconnected / loading / error states, the relay consent when relay is off, or the QR
/// code with the copyable link and the security warning.
class _PairDeviceSection extends StatefulWidget {
  const _PairDeviceSection({required this.store, required this.serverId});
  final AppStore store;
  final String serverId;

  @override
  State<_PairDeviceSection> createState() => _PairDeviceSectionState();
}

class _PairDeviceSectionState extends State<_PairDeviceSection> {
  bool _pending = true;
  Object? _error;
  JsonMap? _offer;
  bool _enabling = false;
  Object? _enableError;
  bool _copied = false;
  Timer? _copiedTimer;
  List<List<bool>>? _qr;
  bool _qrError = false;

  PaseoClient? get _client => widget.store.hostFor(widget.serverId)?.client;

  @override
  void initState() {
    super.initState();
    widget.store.addListener(_storeChanged);
    // Deferred: the load sets state synchronously, which initState must not do.
    unawaited(Future.microtask(_load));
  }

  @override
  void dispose() {
    widget.store.removeListener(_storeChanged);
    _copiedTimer?.cancel();
    super.dispose();
  }

  void _storeChanged() {
    if (!mounted) return;
    // Original query runs once the host is online.
    if (_offer == null && _error == null && !_pending && _connected) unawaited(_load());
    setState(() {});
  }

  bool get _connected => _client?.status == ConnectionStatus.connected;

  bool get _disconnected {
    final status = _client?.status;
    return status == null ||
        status == ConnectionStatus.disconnected ||
        status == ConnectionStatus.error;
  }

  Future<void> _load() async {
    if (!mounted) return;
    if (!_connected) {
      setState(() => _pending = false);
      return;
    }
    setState(() {
      _pending = true;
      _error = null;
    });
    try {
      final response = await navigationRequest(
        widget.store,
        widget.serverId,
        'daemon.get_pairing_offer.request',
        {},
        'daemon.get_pairing_offer.response',
      );
      final url = response.text('url');
      List<List<bool>>? qr;
      var qrError = false;
      if (url.isNotEmpty) {
        try {
          qr = _QrCode.encode(url);
        } catch (_) {
          qrError = true;
        }
      }
      if (!mounted) return;
      setState(() {
        _offer = response;
        _qr = qr;
        _qrError = qrError;
        _pending = false;
      });
    } catch (error) {
      if (!mounted) return;
      setState(() {
        _error = error;
        _pending = false;
      });
    }
  }

  Future<void> _enableRelay() async {
    if (_enabling) return;
    setState(() {
      _enabling = true;
      _enableError = null;
    });
    try {
      if (_client?.supports('relayConfig') != true) {
        throw StateError(tr(context, 'pairing.device.updateRequired'));
      }
      await navigationRequest(
        widget.store,
        widget.serverId,
        'set_daemon_config_request',
        {
          'config': {
            'relay': {'enabled': true},
          },
        },
        'set_daemon_config_response',
      );
      await _load();
    } catch (error) {
      if (mounted) setState(() => _enableError = error);
    } finally {
      if (mounted) setState(() => _enabling = false);
    }
  }

  Future<void> _copy(String url) async {
    await Clipboard.setData(ClipboardData(text: url));
    if (!mounted) return;
    setState(() => _copied = true);
    _copiedTimer?.cancel();
    _copiedTimer = Timer(const Duration(seconds: 2), () {
      if (mounted) setState(() => _copied = false);
    });
  }

  Widget _stateLine(String text) => Padding(
    padding: const EdgeInsets.symmetric(vertical: 24),
    child: Text(
      text,
      textAlign: TextAlign.center,
      style: Theme.of(context).textTheme.bodyMedium!.copyWith(
        color: PaseoColors.of(context).muted,
      ),
    ),
  );

  Widget _loadError(String message) => _Alert(
    variant: _AlertVariant.error,
    description: message,
    action: Align(
      alignment: Alignment.centerLeft,
      child: PaseoButton(
        variant: PaseoButtonVariant.outline,
        size: PaseoControlSize.sm,
        icon: LucideIcons.rotateCw300,
        label: tr(context, 'pairing.device.retry'),
        onPressed: () => unawaited(_load()),
      ),
    ),
  );

  Widget _consent() {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final canConfigure =
        _client?.supports('daemonStatusRpc') == true && _client?.supports('relayConfig') == true;
    final enableLabel = _enabling
        ? tr(context, 'pairing.device.enablingRelay')
        : _enableError != null
        ? tr(context, 'pairing.device.retry')
        : tr(context, 'pairing.device.enableRelay');
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      spacing: 16,
      children: [
        Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          spacing: 8,
          children: [
            Padding(
              padding: const EdgeInsets.only(bottom: 4),
              child: Container(
                width: 48,
                height: 48,
                alignment: Alignment.center,
                decoration: BoxDecoration(
                  shape: BoxShape.circle,
                  color: colors.surface2,
                  border: Border.all(color: theme.dividerColor),
                ),
                child: Icon(LucideIcons.shieldCheck300, size: 20, color: colors.accentBright),
              ),
            ),
            Text(
              tr(context, 'pairing.device.enableTitle'),
              style: theme.textTheme.bodyMedium!.copyWith(
                color: theme.colorScheme.onSurface,
                fontWeight: FontWeight.w500,
              ),
            ),
            Text(
              tr(context, 'pairing.device.enableDescription'),
              style: theme.textTheme.bodyMedium!.copyWith(color: colors.muted, height: 1.5),
            ),
            Semantics(
              link: true,
              label: tr(context, 'pairing.device.relayDocsAccessibility'),
              child: MouseRegion(
                cursor: SystemMouseCursors.click,
                child: GestureDetector(
                  behavior: HitTestBehavior.opaque,
                  onTap: () => unawaited(
                    launchUrl(
                      Uri.parse('https://paseo.sh/docs/security'),
                      mode: LaunchMode.externalApplication,
                    ),
                  ),
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    spacing: 4,
                    children: [
                      Text(
                        tr(context, 'pairing.device.relayDocs'),
                        style: theme.textTheme.bodySmall!.copyWith(color: colors.muted),
                      ),
                      Icon(LucideIcons.arrowUpRight300, size: 12, color: colors.muted),
                    ],
                  ),
                ),
              ),
            ),
          ],
        ),
        if (_enableError != null)
          _Alert(variant: _AlertVariant.error, description: errorMessage(_enableError!)),
        if (!canConfigure)
          _Alert(
            variant: _AlertVariant.warning,
            description: tr(context, 'pairing.device.updateRequired'),
          ),
        Row(
          spacing: 12,
          children: [
            Expanded(
              child: PaseoButton(
                expand: true,
                label: tr(context, 'pairing.device.notNow'),
                onPressed: () => Navigator.pop(context),
              ),
            ),
            if (canConfigure)
              Expanded(
                child: PaseoButton(
                  expand: true,
                  variant: PaseoButtonVariant.primary,
                  loading: _enabling,
                  label: enableLabel,
                  onPressed: () => unawaited(_enableRelay()),
                ),
              ),
          ],
        ),
        Container(
          padding: const EdgeInsets.only(top: 16),
          decoration: BoxDecoration(
            border: Border(top: BorderSide(color: theme.dividerColor)),
          ),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            spacing: 8,
            children: [
              Padding(
                padding: const EdgeInsets.only(top: 1),
                child: Icon(LucideIcons.network300, size: 14, color: colors.muted),
              ),
              Expanded(
                child: Text(
                  tr(context, 'pairing.device.directConnectionHint'),
                  style: theme.textTheme.bodySmall!.copyWith(color: colors.muted, height: 1.5),
                ),
              ),
            ],
          ),
        ),
      ],
    );
  }

  Widget _pairingOffer(String url) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final Widget qr;
    if (_qr != null) {
      qr = Semantics(
        image: true,
        label: tr(context, 'pairing.device.qrAccessibility'),
        child: CustomPaint(painter: _QrPainter(_qr!), size: Size.infinite),
      );
    } else if (_qrError) {
      qr = Text(
        tr(context, 'pairing.device.qrUnavailable'),
        style: theme.textTheme.bodySmall!.copyWith(color: colors.muted),
      );
    } else {
      qr = SizedBox.square(
        dimension: 20,
        child: CircularProgressIndicator(strokeWidth: 2, color: colors.muted),
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      spacing: 16,
      children: [
        Text(
          tr(context, 'pairing.device.hint'),
          textAlign: TextAlign.center,
          style: theme.textTheme.bodyMedium!.copyWith(color: colors.muted),
        ),
        Center(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 304),
            child: AspectRatio(
              aspectRatio: 1,
              child: Container(
                padding: const EdgeInsets.all(12),
                alignment: Alignment.center,
                decoration: BoxDecoration(
                  color: Colors.white,
                  borderRadius: BorderRadius.circular(12),
                  border: Border.all(color: theme.dividerColor),
                ),
                child: qr,
              ),
            ),
          ),
        ),
        Row(
          spacing: 8,
          children: [
            Expanded(
              child: Container(
                clipBehavior: Clip.antiAlias,
                padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
                decoration: BoxDecoration(
                  color: colors.surface2,
                  borderRadius: BorderRadius.circular(8),
                  border: Border.all(color: theme.dividerColor),
                ),
                child: Semantics(
                  label: tr(context, 'pairing.link.label'),
                  child: SelectableText(
                    url,
                    maxLines: 1,
                    style: type.codeStyle.copyWith(
                      color: colors.muted,
                      fontSize: type.uiSize(12),
                      height: 1.4,
                    ),
                  ),
                ),
              ),
            ),
            PaseoButton(
              variant: PaseoButtonVariant.outline,
              size: PaseoControlSize.sm,
              icon: _copied ? LucideIcons.check300 : LucideIcons.copy300,
              label: tr(context, _copied ? 'pairing.device.copied' : 'pairing.device.copy'),
              onPressed: () => unawaited(_copy(url)),
            ),
          ],
        ),
        _Alert(
          variant: _AlertVariant.warning,
          description: tr(context, 'pairing.device.securityWarning'),
        ),
      ],
    );
  }

  @override
  Widget build(BuildContext context) {
    if (_disconnected) return _loadError(tr(context, 'workspace.terminal.hostDisconnected'));
    if (_pending) return _stateLine(tr(context, 'pairing.device.loadingOffer'));
    if (_error != null) return _loadError(errorMessage(_error!));
    final offer = _offer;
    if (offer == null || !offer.flag('relayEnabled')) return _consent();
    final url = offer.text('url');
    if (url.isEmpty) return _stateLine(tr(context, 'pairing.device.unavailable'));
    return _pairingOffer(url);
  }
}

enum _AlertVariant { warning, error }

/// Original `components/ui/alert.tsx` at size sm: transparent with a 1px border at half the
/// accent alpha, 2xl radius, 12/16 padding, a 14px variant glyph 12px before base muted text,
/// actions 8px below indented under the text.
class _Alert extends StatelessWidget {
  const _Alert({required this.variant, required this.description, this.action});
  final _AlertVariant variant;
  final String description;
  final Widget? action;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final accent = switch (variant) {
      _AlertVariant.warning => const Color(0xfff59e0b),
      _AlertVariant.error => theme.colorScheme.error,
    };
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
      decoration: BoxDecoration(
        borderRadius: BorderRadius.circular(16),
        border: Border.all(color: accent.withValues(alpha: .5)),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 2,
        children: [
          Row(
            spacing: 12,
            children: [
              SizedBox(
                width: 14,
                child: Icon(
                  variant == _AlertVariant.warning
                      ? LucideIcons.triangleAlert300
                      : LucideIcons.circleX300,
                  size: 14,
                  color: accent,
                ),
              ),
              Expanded(
                child: Text(
                  description,
                  style: theme.textTheme.bodyMedium!.copyWith(
                    color: PaseoColors.of(context).muted,
                  ),
                ),
              ),
            ],
          ),
          if (action != null)
            Padding(
              padding: const EdgeInsets.only(left: 14 + 12.0, top: 8),
              child: action,
            ),
        ],
      ),
    );
  }
}

/// Draws [modules] dark on the white tile with the one-module quiet zone of the original
/// `QRCode.toString(..., { margin: 1 })`.
class _QrPainter extends CustomPainter {
  _QrPainter(this.modules);
  final List<List<bool>> modules;

  @override
  void paint(Canvas canvas, Size size) {
    final count = modules.length + 2;
    final side = math.min(size.width, size.height);
    final cell = side / count;
    final origin = Offset((size.width - side) / 2 + cell, (size.height - side) / 2 + cell);
    final paint = Paint()
      ..color = Colors.black
      ..isAntiAlias = false;
    final path = Path();
    for (var y = 0; y < modules.length; y++) {
      for (var x = 0; x < modules.length; x++) {
        if (!modules[y][x]) continue;
        path.addRect(Rect.fromLTWH(origin.dx + x * cell, origin.dy + y * cell, cell + .5, cell + .5));
      }
    }
    canvas.drawPath(path, paint);
  }

  @override
  bool shouldRepaint(_QrPainter oldDelegate) => !identical(oldDelegate.modules, modules);
}

/// QR Code Model 2 encoder (byte mode, error correction level M — the original
/// `errorCorrectionLevel: "M"`), following ISO/IEC 18004.
abstract final class _QrCode {
  static const _eccCodewordsPerBlock = [
    -1, 10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26, 26, //
    26, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
  ];
  static const _numErrorCorrectionBlocks = [
    -1, 1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16, //
    17, 17, 18, 20, 21, 23, 25, 26, 28, 29, 31, 33, 35, 37, 38, 40, 43, 45, 47, 49,
  ];

  static int _rawDataModules(int version) {
    var result = (16 * version + 128) * version + 64;
    if (version >= 2) {
      final align = version ~/ 7 + 2;
      result -= (25 * align - 10) * align - 55;
      if (version >= 7) result -= 36;
    }
    return result;
  }

  static int _dataCodewords(int version) =>
      _rawDataModules(version) ~/ 8 -
      _eccCodewordsPerBlock[version] * _numErrorCorrectionBlocks[version];

  static List<List<bool>> encode(String text) {
    final bytes = utf8.encode(text);
    int countBitsFor(int version) => version <= 9 ? 8 : 16;
    var version = 1;
    while (4 + countBitsFor(version) + bytes.length * 8 > _dataCodewords(version) * 8) {
      version++;
      if (version > 40) throw const FormatException('Data too long for a QR code');
    }
    final countBits = countBitsFor(version);
    final bits = <bool>[];
    void append(int value, int length) {
      for (var i = length - 1; i >= 0; i--) {
        bits.add(((value >> i) & 1) != 0);
      }
    }

    append(0x4, 4);
    append(bytes.length, countBits);
    for (final byte in bytes) {
      append(byte, 8);
    }
    final capacity = _dataCodewords(version) * 8;
    append(0, math.min(4, capacity - bits.length));
    append(0, (8 - bits.length % 8) % 8);
    for (var pad = 0xEC; bits.length < capacity; pad ^= 0xEC ^ 0x11) {
      append(pad, 8);
    }
    final data = <int>[
      for (var i = 0; i < bits.length; i += 8)
        [for (var j = 0; j < 8; j++) bits[i + j] ? 1 : 0].fold(0, (a, b) => a << 1 | b),
    ];
    final codewords = _addEccAndInterleave(data, version);

    final size = version * 4 + 17;
    final modules = List.generate(size, (_) => List.filled(size, false));
    final function = List.generate(size, (_) => List.filled(size, false));
    void setFunction(int x, int y, bool dark) {
      modules[y][x] = dark;
      function[y][x] = true;
    }

    // Timing patterns, then finders, alignment patterns, format and version areas.
    for (var i = 0; i < size; i++) {
      setFunction(6, i, i % 2 == 0);
      setFunction(i, 6, i % 2 == 0);
    }
    for (final (cx, cy) in [(3, 3), (size - 4, 3), (3, size - 4)]) {
      for (var dy = -4; dy <= 4; dy++) {
        for (var dx = -4; dx <= 4; dx++) {
          final distance = math.max(dx.abs(), dy.abs());
          final x = cx + dx, y = cy + dy;
          if (x >= 0 && x < size && y >= 0 && y < size) {
            setFunction(x, y, distance != 2 && distance != 4);
          }
        }
      }
    }
    final positions = _alignmentPositions(version, size);
    for (var i = 0; i < positions.length; i++) {
      for (var j = 0; j < positions.length; j++) {
        final corner = (i == 0 && j == 0) ||
            (i == 0 && j == positions.length - 1) ||
            (i == positions.length - 1 && j == 0);
        if (corner) continue;
        for (var dy = -2; dy <= 2; dy++) {
          for (var dx = -2; dx <= 2; dx++) {
            setFunction(
              positions[i] + dx,
              positions[j] + dy,
              math.max(dx.abs(), dy.abs()) != 1,
            );
          }
        }
      }
    }
    void drawFormat(int mask) {
      // Level M format bits are 00.
      final value = mask;
      var remainder = value;
      for (var i = 0; i < 10; i++) {
        remainder = (remainder << 1) ^ ((remainder >> 9) * 0x537);
      }
      final format = ((value << 10) | remainder) ^ 0x5412;
      bool bit(int i) => ((format >> i) & 1) != 0;
      for (var i = 0; i <= 5; i++) {
        setFunction(8, i, bit(i));
      }
      setFunction(8, 7, bit(6));
      setFunction(8, 8, bit(7));
      setFunction(7, 8, bit(8));
      for (var i = 9; i < 15; i++) {
        setFunction(14 - i, 8, bit(i));
      }
      for (var i = 0; i < 8; i++) {
        setFunction(size - 1 - i, 8, bit(i));
      }
      for (var i = 8; i < 15; i++) {
        setFunction(8, size - 15 + i, bit(i));
      }
      setFunction(8, size - 8, true);
    }

    drawFormat(0);
    if (version >= 7) {
      var remainder = version;
      for (var i = 0; i < 12; i++) {
        remainder = (remainder << 1) ^ ((remainder >> 11) * 0x1F25);
      }
      final value = (version << 12) | remainder;
      for (var i = 0; i < 18; i++) {
        final dark = ((value >> i) & 1) != 0;
        final a = size - 11 + i % 3, b = i ~/ 3;
        setFunction(a, b, dark);
        setFunction(b, a, dark);
      }
    }

    // Codewords in the zigzag order, skipping function modules.
    var index = 0;
    for (var right = size - 1; right >= 1; right -= 2) {
      if (right == 6) right = 5;
      for (var vertical = 0; vertical < size; vertical++) {
        for (var j = 0; j < 2; j++) {
          final x = right - j;
          final upward = ((right + 1) & 2) == 0;
          final y = upward ? size - 1 - vertical : vertical;
          if (!function[y][x] && index < codewords.length * 8) {
            modules[y][x] = ((codewords[index >> 3] >> (7 - (index & 7))) & 1) != 0;
            index++;
          }
        }
      }
    }

    void applyMask(int mask) {
      for (var y = 0; y < size; y++) {
        for (var x = 0; x < size; x++) {
          if (function[y][x]) continue;
          final invert = switch (mask) {
            0 => (x + y) % 2 == 0,
            1 => y % 2 == 0,
            2 => x % 3 == 0,
            3 => (x + y) % 3 == 0,
            4 => (x ~/ 3 + y ~/ 2) % 2 == 0,
            5 => x * y % 2 + x * y % 3 == 0,
            6 => (x * y % 2 + x * y % 3) % 2 == 0,
            _ => ((x + y) % 2 + x * y % 3) % 2 == 0,
          };
          if (invert) modules[y][x] = !modules[y][x];
        }
      }
    }

    var best = 0;
    var bestPenalty = -1;
    for (var mask = 0; mask < 8; mask++) {
      applyMask(mask);
      drawFormat(mask);
      final penalty = _penalty(modules);
      if (bestPenalty < 0 || penalty < bestPenalty) {
        best = mask;
        bestPenalty = penalty;
      }
      applyMask(mask);
    }
    applyMask(best);
    drawFormat(best);
    return modules;
  }

  static List<int> _alignmentPositions(int version, int size) {
    if (version == 1) return const [];
    final count = version ~/ 7 + 2;
    final step = (version * 8 + count * 3 + 5) ~/ (count * 4 - 4) * 2;
    return [
      6,
      for (var i = count - 2; i >= 0; i--) size - 7 - i * step,
    ];
  }

  static int _multiply(int x, int y) {
    var z = 0;
    for (var i = 7; i >= 0; i--) {
      z = (z << 1) ^ ((z >> 7) * 0x11D);
      z ^= ((y >> i) & 1) * x;
    }
    return z & 0xFF;
  }

  static List<int> _divisor(int degree) {
    final result = List.filled(degree, 0)..[degree - 1] = 1;
    var root = 1;
    for (var i = 0; i < degree; i++) {
      for (var j = 0; j < degree; j++) {
        result[j] = _multiply(result[j], root);
        if (j + 1 < degree) result[j] ^= result[j + 1];
      }
      root = _multiply(root, 0x02);
    }
    return result;
  }

  static List<int> _remainder(List<int> data, List<int> divisor) {
    final result = List<int>.filled(divisor.length, 0, growable: true);
    for (final byte in data) {
      final factor = byte ^ result.removeAt(0);
      result.add(0);
      for (var i = 0; i < result.length; i++) {
        result[i] ^= _multiply(divisor[i], factor);
      }
    }
    return result;
  }

  static List<int> _addEccAndInterleave(List<int> data, int version) {
    final blocks = _numErrorCorrectionBlocks[version];
    final eccLength = _eccCodewordsPerBlock[version];
    final raw = _rawDataModules(version) ~/ 8;
    final shortBlocks = blocks - raw % blocks;
    final shortLength = raw ~/ blocks;
    final divisor = _divisor(eccLength);
    final all = <List<int>>[];
    var offset = 0;
    for (var i = 0; i < blocks; i++) {
      final length = shortLength - eccLength + (i < shortBlocks ? 0 : 1);
      final block = data.sublist(offset, offset + length);
      offset += length;
      final ecc = _remainder(block, divisor);
      if (i < shortBlocks) block.add(0);
      all.add([...block, ...ecc]);
    }
    return [
      for (var i = 0; i < all.first.length; i++)
        for (var j = 0; j < all.length; j++)
          if (i != shortLength - eccLength || j >= shortBlocks) all[j][i],
    ];
  }

  /// ISO 18004 mask penalty rules 1 (runs), 2 (2×2 blocks), 3 (finder-like) and 4 (balance).
  static int _penalty(List<List<bool>> modules) {
    final size = modules.length;
    var result = 0;
    int runs(bool Function(int i, int j) at) {
      var penalty = 0;
      for (var i = 0; i < size; i++) {
        var run = 1;
        for (var j = 1; j < size; j++) {
          if (at(i, j) == at(i, j - 1)) {
            run++;
          } else {
            if (run >= 5) penalty += 3 + run - 5;
            run = 1;
          }
        }
        if (run >= 5) penalty += 3 + run - 5;
        // Finder-like 1:1:3:1:1 runs with four light modules on either side.
        for (var j = 0; j + 10 < size; j++) {
          const pattern = [true, false, true, true, true, false, true];
          var matches = true;
          for (var k = 0; k < 7 && matches; k++) {
            matches = at(i, j + k + 4) == pattern[k];
          }
          if (!matches) continue;
          final before = !at(i, j) && !at(i, j + 1) && !at(i, j + 2) && !at(i, j + 3);
          if (before) penalty += 40;
        }
        for (var j = 0; j + 10 < size; j++) {
          const pattern = [true, false, true, true, true, false, true];
          var matches = true;
          for (var k = 0; k < 7 && matches; k++) {
            matches = at(i, j + k) == pattern[k];
          }
          if (!matches) continue;
          final after = !at(i, j + 7) && !at(i, j + 8) && !at(i, j + 9) && !at(i, j + 10);
          if (after) penalty += 40;
        }
      }
      return penalty;
    }

    result += runs((i, j) => modules[i][j]);
    result += runs((i, j) => modules[j][i]);
    for (var y = 0; y + 1 < size; y++) {
      for (var x = 0; x + 1 < size; x++) {
        final color = modules[y][x];
        if (color == modules[y][x + 1] &&
            color == modules[y + 1][x] &&
            color == modules[y + 1][x + 1]) {
          result += 3;
        }
      }
    }
    var dark = 0;
    for (final row in modules) {
      for (final module in row) {
        if (module) dark++;
      }
    }
    final total = size * size;
    final k = ((dark * 20 - total * 10).abs() + total - 1) ~/ total - 1;
    result += math.max(0, k) * 10;
    return result;
  }
}
