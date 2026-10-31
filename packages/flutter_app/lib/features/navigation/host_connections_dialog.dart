import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:uuid/uuid.dart';

import '../../core/app_store.dart';
import '../../core/client.dart';
import '../../core/host_config.dart';
import '../../core/scope.dart';
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import 'host_dialog.dart';
import 'navigation_actions.dart';

Future<void> showHostConnectionsDialog(
  BuildContext context, {
  required AppStore store,
  required String serverId,
}) => showAdaptiveSheet<void>(
  context: context,
  builder: (_) => HostConnectionsDialog(store: store, serverId: serverId),
);

/// Original `formatHostConnectionLabel`: "TCP (endpoint)", "Relay (…)", "Local (path)",
/// "Remote SSH (host)".
String hostConnectionLabel(BuildContext context, HostConfig config) {
  try {
    final target = config.resolve();
    final uri = target.uri;
    final endpoint = '${uri.host}${uri.hasPort ? ':${uri.port}' : ''}';
    if (target.isRelay) return '${tr(context, 'settings.host.badges.relay')} ($endpoint)';
    if (uri.scheme == 'unix' || uri.scheme == 'pipe') {
      return '${tr(context, 'settings.host.badges.local')} (${uri.path})';
    }
    if (uri.scheme == 'ssh') {
      return '${tr(context, 'settings.host.badges.remoteSsh')} (${uri.userInfo.isEmpty ? '' : '${uri.userInfo}@'}${uri.host})';
    }
    return 'TCP ($endpoint)';
  } catch (_) {
    return 'Invalid endpoint — edit this connection';
  }
}

/// Original `formatActiveConnectionLabel`: sockets/pipes read as "Local"; standard web
/// ports are dropped from the address.
String _activeConnectionLabel(HostConfig config) {
  try {
    final uri = config.resolve().uri;
    if (uri.scheme == 'unix' || uri.scheme == 'pipe') return 'Local';
    return '${uri.host}${uri.hasPort && uri.port != 443 && uri.port != 80 ? ':${uri.port}' : ''}';
  } catch (_) {
    return '';
  }
}

/// The original sidebar `HostPicker`: hosts with their status dot and active connection,
/// a settings gear per host, and "Add host". Returns a serverId, `settings:<serverId>`,
/// or `add`. Phones get the bottom sheet; wider layouts a popover above [anchor].
Future<String?> showHostPicker(BuildContext context, AppStore store) async {
  final colors = PaseoColors.of(context);
  final theme = Theme.of(context);
  Widget dot(HostState host) {
    final status = host.client.status;
    return SizedBox.square(
      dimension: densityIcon(context, 'sm'),
      child: Center(
        child: Container(
          width: 8,
          height: 8,
          decoration: BoxDecoration(
            shape: BoxShape.circle,
            color: status == ConnectionStatus.connected && host.error == null
                ? colors.success
                : status == ConnectionStatus.disconnected ||
                      status == ConnectionStatus.error
                ? colors.danger
                : colors.warning,
          ),
        ),
      ),
    );
  }

  /// Original `ComboboxItem`: 36px rows (12px/8px insets, 8px gaps), a 16px leading slot,
  /// the base-size label with the sm muted description inline after it on one line, and
  /// surface1 while pressed.
  Widget option(
    BuildContext context, {
    required Widget leading,
    required String label,
    String? description,
    Widget? trailing,
    required VoidCallback onTap,
  }) => InkWell(
    onTap: onTap,
    hoverColor: colors.surface1,
    highlightColor: colors.surface1,
    splashFactory: NoSplash.splashFactory,
    child: ConstrainedBox(
      constraints: const BoxConstraints(minHeight: 36),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
        child: Row(
          spacing: 8,
          children: [
            leading,
            Expanded(
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.baseline,
                textBaseline: TextBaseline.alphabetic,
                spacing: 8,
                children: [
                  Flexible(
                    flex: 0,
                    child: Text(label, maxLines: 1, overflow: TextOverflow.ellipsis,
                        style: theme.textTheme.bodyMedium),
                  ),
                  if (description != null && description.isNotEmpty)
                    Flexible(
                      child: Text(description, maxLines: 1, overflow: TextOverflow.ellipsis,
                          style: theme.textTheme.bodySmall!.copyWith(color: colors.muted)),
                    ),
                ],
              ),
            ),
            ?trailing,
          ],
        ),
      ),
    ),
  );

  Widget list(
    BuildContext context, {
    EdgeInsets padding = EdgeInsets.zero,
    ScrollController? controller,
  }) => _HostPickerList(
        store: store,
        padding: padding,
        controller: controller,
        build: (hosts) => [
          for (final host in hosts)
            option(
              context,
              leading: dot(host),
              label: hostDisplayLabel(host),
              description: _activeConnectionLabel(host.config),
              trailing: IconButton(
                tooltip: 'Open ${hostDisplayLabel(host)} settings',
                style: IconButton.styleFrom(
                  minimumSize: Size.zero,
                  padding: const EdgeInsets.all(4),
                  tapTargetSize: MaterialTapTargetSize.shrinkWrap,
                ),
                onPressed: () => Navigator.pop(context, 'settings:${host.serverId}'),
                icon: Icon(LucideIcons.settings300, size: densityIcon(context, 'sm'),
                    color: colors.muted),
              ),
              onTap: () => Navigator.pop(context, host.serverId),
            ),
          option(
            context,
            leading: SizedBox.square(
              dimension: densityIcon(context, 'sm'),
              child: Icon(LucideIcons.plus300, size: densityIcon(context, 'sm'), color: colors.muted),
            ),
            // Original SYSTEM_HOST_PICKER_OPTION_LABELS.add is an untranslated literal.
            label: 'Add host',
            onTap: () => Navigator.pop(context, 'add'),
          ),
        ],
      );

  // Original mobile Combobox: a bottom sheet snapped at 60% of the window (90% when
  // dragged up) with the base medium title 24px in and the options below it.
  if (isCompactLayout(context)) {
    return showModalBottomSheet<String>(
      context: context,
      isScrollControlled: true,
      useSafeArea: true,
      showDragHandle: true,
      backgroundColor: colors.surface0,
      barrierColor: const Color(0x73000000),
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.vertical(top: Radius.circular(16)),
      ),
      builder: (_) => DraggableScrollableSheet(
        expand: false,
        initialChildSize: .6,
        minChildSize: .3,
        maxChildSize: .9,
        snap: true,
        snapSizes: const [.6],
        builder: (sheet, controller) => Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(24, 0, 24, 8),
              child: Text(
                'Host',
                style: theme.textTheme.bodyMedium!.copyWith(fontWeight: FontWeight.w500),
              ),
            ),
            Expanded(
              child: list(
                sheet,
                padding: const EdgeInsets.fromLTRB(8, 4, 8, 32),
                controller: controller,
              ),
            ),
          ],
        ),
      ),
    );
  }
  final box = context.findRenderObject() as RenderBox?;
  final overlay = Overlay.of(context).context.findRenderObject() as RenderBox;
  final origin = box?.localToGlobal(Offset.zero, ancestor: overlay) ?? Offset.zero;
  return showDialog<String>(
    context: context,
    barrierColor: Colors.transparent,
    builder: (dialogContext) => Stack(
      children: [
        Positioned(
          left: origin.dx,
          bottom: overlay.size.height - origin.dy + 8,
          child: Material(
            color: colors.surface1,
            elevation: 8,
            shape: RoundedRectangleBorder(
              borderRadius: BorderRadius.circular(8),
              side: BorderSide(color: colors.surface2),
            ),
            clipBehavior: Clip.antiAlias,
            child: ConstrainedBox(
              constraints: BoxConstraints(
                maxHeight: overlay.size.height * .6,
              ),
              child: SizedBox(
                width: 280,
                child: Padding(
                  padding: const EdgeInsets.symmetric(vertical: 4),
                  child: list(dialogContext),
                ),
              ),
            ),
          ),
        ),
      ],
    ),
  );
}

/// Host rows, searchable above ten hosts as in the original picker.
class _HostPickerList extends StatefulWidget {
  const _HostPickerList({
    required this.store,
    required this.build,
    this.padding = EdgeInsets.zero,
    this.controller,
  });
  final AppStore store;
  final List<Widget> Function(List<HostState> hosts) build;
  final EdgeInsets padding;

  /// The sheet's drag controller: the list fills the sheet and always scrolls, so a drag
  /// anywhere on it moves the sheet between its 60% and 90% snaps.
  final ScrollController? controller;
  @override
  State<_HostPickerList> createState() => _HostPickerListState();
}

class _HostPickerListState extends State<_HostPickerList> {
  String _query = '';
  @override
  Widget build(BuildContext context) {
    final searchable = widget.store.hosts.length > 10;
    final hosts = widget.store.hosts
        .where((host) => hostDisplayLabel(host).toLowerCase().contains(_query))
        .toList();
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        if (searchable)
          Padding(
            padding: const EdgeInsets.fromLTRB(8, 4, 8, 4),
            child: TextField(
              autofocus: !isCompactLayout(context),
              decoration: const InputDecoration(
                hintText: 'Search hosts',
                isDense: true,
              ),
              onChanged: (value) =>
                  setState(() => _query = value.trim().toLowerCase()),
            ),
          ),
        Flexible(
          fit: widget.controller == null ? FlexFit.loose : FlexFit.tight,
          child: SingleChildScrollView(
            controller: widget.controller,
            physics: widget.controller == null
                ? null
                : const AlwaysScrollableScrollPhysics(),
            padding: widget.padding,
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: widget.build(hosts),
            ),
          ),
        ),
      ],
    );
  }
}

class HostConnectionsDialog extends StatefulWidget {
  const HostConnectionsDialog({
    super.key,
    required this.store,
    required this.serverId,
  });
  final AppStore store;
  final String serverId;
  @override
  State<HostConnectionsDialog> createState() => _HostConnectionsDialogState();
}

class _HostConnectionsDialogState extends State<HostConnectionsDialog> {
  bool _busy = false;
  String? _error;
  Future<void> _run(Future<void> Function() action) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await action();
    } catch (error) {
      if (mounted) setState(() => _error = error.toString());
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  /// Original host-page remove-connection confirm: muted message, then small Cancel /
  /// destructive Remove buttons 16px below; a failed removal shows the original error copy.
  Future<void> _remove(HostConfig connection) async {
    final colors = PaseoColors.of(context);
    final confirmed = await showAdaptiveSheet<bool>(
      context: context,
      builder: (sheet) => SheetContents(
        title: tr(sheet, 'settings.host.connections.removeTitle'),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          spacing: 16,
          children: [
            Text(
              tr(sheet, 'settings.host.connections.removeMessage', args: {
                'name': hostConnectionLabel(sheet, connection),
              }),
              style: Theme.of(sheet).textTheme.bodyMedium!.copyWith(color: colors.muted),
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
                      label: tr(sheet, 'common.actions.cancel'),
                      onPressed: () => Navigator.pop(sheet, false),
                    ),
                  ),
                  Expanded(
                    child: PaseoButton(
                      expand: true,
                      size: PaseoControlSize.sm,
                      variant: PaseoButtonVariant.destructive,
                      label: tr(sheet, 'settings.host.connections.removeAction'),
                      onPressed: () => Navigator.pop(sheet, true),
                    ),
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
    if (confirmed != true) return;
    try {
      await widget.store.removeConnection(widget.serverId, connection.id);
    } catch (_) {
      if (!mounted) return;
      showToast(
        context,
        '${tr(context, 'settings.host.connections.removeErrorTitle')}: '
        '${tr(context, 'settings.host.connections.removeErrorMessage')}',
        variant: ToastVariant.error,
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    PaseoScope.of(context);
    final host = widget.store.hostFor(widget.serverId);
    final colors = PaseoColors.of(context);
    final theme = Theme.of(context);
    final connections = host == null ? const <HostConfig>[] : List<HostConfig>.of(host.connections);
    return SheetContents(
      title: tr(context, 'settings.host.connections.title'),
      subtitle: host == null ? null : Text(hostDisplayLabel(host)),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 16,
        children: [
          if (_error != null)
            SelectableText(
              _error!,
              style: theme.textTheme.bodyMedium!.copyWith(color: theme.colorScheme.error),
            ),
          // Original settings card: one bordered surface, rows divided by hairlines.
          DecoratedBox(
            decoration: BoxDecoration(
              color: colors.surface1,
              borderRadius: BorderRadius.circular(12),
              border: Border.all(color: theme.dividerColor),
            ),
            child: Column(
              children: [
                for (final (index, connection) in connections.indexed)
                  DecoratedBox(
                    decoration: BoxDecoration(
                      border: index == 0
                          ? null
                          : Border(top: BorderSide(color: theme.dividerColor)),
                    ),
                    child: InkWell(
                      onTap: _busy
                          ? null
                          : () => _run(
                              () => showHostDialog(context, config: connection),
                            ),
                      child: Padding(
                        padding: const EdgeInsets.fromLTRB(16, 10, 8, 10),
                        child: Row(
                          spacing: 8,
                          children: [
                            Expanded(
                              child: Column(
                                crossAxisAlignment: CrossAxisAlignment.start,
                                children: [
                                  Text(
                                    hostConnectionLabel(context, connection),
                                    maxLines: 1,
                                    overflow: TextOverflow.ellipsis,
                                    style: theme.textTheme.bodyMedium,
                                  ),
                                  if (connection.id == host?.config.id)
                                    Text(
                                      'Active',
                                      style: theme.textTheme.bodySmall!.copyWith(
                                        color: colors.muted,
                                      ),
                                    ),
                                ],
                              ),
                            ),
                            if (connection.id != host?.config.id)
                              TextButton(
                                onPressed: _busy
                                    ? null
                                    : () => _run(() => widget.store.updateHost(connection)),
                                child: const Text('Use'),
                              ),
                            TextButton(
                              style: TextButton.styleFrom(
                                foregroundColor: theme.colorScheme.error,
                              ),
                              onPressed: _busy ? null : () => _run(() => _remove(connection)),
                              child: Text(tr(context, 'settings.host.connections.removeAction')),
                            ),
                          ],
                        ),
                      ),
                    ),
                  ),
              ],
            ),
          ),
          if (_busy) const LinearProgressIndicator(minHeight: 2),
          if (host != null)
            Align(
              alignment: Alignment.centerLeft,
              child: OutlinedButton.icon(
                icon: const Icon(LucideIcons.plus300, size: 16),
                label: Text(tr(context, 'pairing.connectionMethods.title')),
                onPressed: _busy
                    ? null
                    : () => _run(
                        () => showAdaptiveSheet<void>(
                          context: context,
                          builder: (_) => HostConnectionDialog(
                            store: widget.store,
                            config: HostConfig(
                              id: const Uuid().v4(),
                              label: host.label,
                              endpoint: '',
                              expectedServerId: host.serverId,
                              color: host.config.color,
                            ),
                          ),
                        ),
                      ),
              ),
            ),
        ],
      ),
    );
  }
}

/// Original `hosts/host-chooser.tsx` `useHostChooser`: no hosts resolves null (the caller
/// opens add-host), one host is chosen without asking, and more open the chooser listing
/// hosts local-first. [filter] narrows the candidates.
Future<HostState?> showHostChooser(
  BuildContext context,
  AppStore store, {
  String title = 'Choose host',
  bool Function(HostState host)? filter,
}) async {
  final hosts = [
    ...store.hosts.where((host) => host.config.localCredentialPath != null),
    ...store.hosts.where((host) => host.config.localCredentialPath == null),
  ].where(filter ?? (_) => true).toList();
  if (hosts.isEmpty) return null;
  if (hosts.length == 1) return hosts.first;
  return showDialog<HostState>(
    context: context,
    barrierColor: const Color(0x80000000),
    builder: (_) => _HostChooser(title: title, hosts: hosts),
  );
}

/// Original `HostChooserModal`: a top-aligned 640px panel (92% / 80% max) under a 48px
/// inset with a title + borderless search header, then 56px rows of status dot, label over
/// server ID and a server glyph. Arrows move the active row, Enter chooses, Escape closes.
class _HostChooser extends StatefulWidget {
  const _HostChooser({required this.title, required this.hosts});
  final String title;
  final List<HostState> hosts;

  @override
  State<_HostChooser> createState() => _HostChooserState();
}

class _HostChooserState extends State<_HostChooser> {
  String _query = '';
  int _active = 0;
  late final FocusNode _focus = FocusNode(onKeyEvent: _key);

  @override
  void dispose() {
    _focus.dispose();
    super.dispose();
  }

  List<HostState> get _options {
    final query = _query.trim().toLowerCase();
    if (query.isEmpty) return widget.hosts;
    return widget.hosts
        .where((host) =>
            hostDisplayLabel(host).toLowerCase().contains(query) ||
            host.serverId.toLowerCase().contains(query))
        .toList();
  }

  KeyEventResult _key(FocusNode node, KeyEvent event) {
    if (event is! KeyDownEvent && event is! KeyRepeatEvent) return KeyEventResult.ignored;
    final options = _options;
    final active = options.isEmpty ? 0 : math.min(_active, options.length - 1);
    final key = event.logicalKey;
    if (key == LogicalKeyboardKey.escape) {
      Navigator.pop(context);
      return KeyEventResult.handled;
    }
    if (key == LogicalKeyboardKey.enter || key == LogicalKeyboardKey.numpadEnter) {
      if (options.isEmpty) return KeyEventResult.ignored;
      Navigator.pop(context, options[active]);
      return KeyEventResult.handled;
    }
    if (key != LogicalKeyboardKey.arrowDown && key != LogicalKeyboardKey.arrowUp) {
      return KeyEventResult.ignored;
    }
    if (options.isEmpty) return KeyEventResult.ignored;
    final next = active + (key == LogicalKeyboardKey.arrowDown ? 1 : -1);
    setState(() => _active = next < 0 ? options.length - 1 : next >= options.length ? 0 : next);
    return KeyEventResult.handled;
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final size = MediaQuery.sizeOf(context);
    final base = theme.textTheme.bodyMedium!;
    final options = _options;
    final active = options.isEmpty ? 0 : math.min(_active, options.length - 1);
    return Align(
      alignment: Alignment.topCenter,
      child: Padding(
        padding: const EdgeInsets.only(top: 48),
        child: ConstrainedBox(
          constraints: BoxConstraints(
            maxWidth: math.min(640, size.width * .92),
            maxHeight: size.height * .8,
          ),
          child: Material(
            color: colors.surface0,
            elevation: 12,
            shadowColor: Colors.black54,
            clipBehavior: Clip.antiAlias,
            shape: RoundedRectangleBorder(
              borderRadius: BorderRadius.circular(8),
              side: BorderSide(color: theme.dividerColor),
            ),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Container(
                  padding: const EdgeInsets.all(16),
                  decoration: BoxDecoration(
                    border: Border(bottom: BorderSide(color: theme.dividerColor)),
                  ),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    spacing: 12,
                    children: [
                      Text(
                        widget.title,
                        style: base.copyWith(
                          color: theme.colorScheme.onSurface,
                          fontWeight: FontWeight.w500,
                        ),
                      ),
                      TextField(
                        focusNode: _focus,
                        autofocus: true,
                        autocorrect: false,
                        enableSuggestions: false,
                        style: base.copyWith(color: theme.colorScheme.onSurface),
                        decoration: InputDecoration(
                          hintText: 'Search hosts...',
                          hintStyle: base.copyWith(color: colors.muted),
                          filled: false,
                          isDense: true,
                          isCollapsed: true,
                          contentPadding: EdgeInsets.zero,
                          border: InputBorder.none,
                          enabledBorder: InputBorder.none,
                          focusedBorder: InputBorder.none,
                        ),
                        onChanged: (value) => setState(() {
                          _query = value;
                          _active = 0;
                        }),
                      ),
                    ],
                  ),
                ),
                Flexible(
                  child: ConstrainedBox(
                    constraints: const BoxConstraints(maxHeight: 420),
                    child: ScrollConfiguration(
                      behavior: ScrollConfiguration.of(context).copyWith(scrollbars: false),
                      child: ListView(
                        shrinkWrap: true,
                        padding: const EdgeInsets.symmetric(vertical: 8),
                        children: [
                          if (options.isEmpty)
                            Padding(
                              padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
                              child: Text(
                                'No matching hosts',
                                style: base.copyWith(color: colors.muted),
                              ),
                            ),
                          for (final (index, host) in options.indexed)
                            _HostChooserRow(
                              host: host,
                              active: index == active,
                              onTap: () => Navigator.pop(context, host),
                            ),
                        ],
                      ),
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

class _HostChooserRow extends StatefulWidget {
  const _HostChooserRow({required this.host, required this.active, required this.onTap});
  final HostState host;
  final bool active;
  final VoidCallback onTap;

  @override
  State<_HostChooserRow> createState() => _HostChooserRowState();
}

class _HostChooserRowState extends State<_HostChooserRow> {
  bool _hovered = false, _pressed = false;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final status = widget.host.client.status;
    // Original `HostStatusDot`: online success, connecting warning, otherwise danger.
    final dot = status == ConnectionStatus.connected && widget.host.error == null
        ? colors.success
        : status == ConnectionStatus.disconnected || status == ConnectionStatus.error
        ? colors.danger
        : colors.warning;
    return Semantics(
      button: true,
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: (_) => setState(() => _pressed = true),
          onTapCancel: () => setState(() => _pressed = false),
          onTapUp: (_) => setState(() => _pressed = false),
          onTap: widget.onTap,
          child: Container(
            constraints: const BoxConstraints(minHeight: 56),
            padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
            color: widget.active || _hovered || _pressed ? colors.surface1 : null,
            child: Row(
              spacing: 12,
              children: [
                SizedBox(
                  width: 18,
                  child: Center(
                    child: Container(
                      width: 8,
                      height: 8,
                      decoration: BoxDecoration(shape: BoxShape.circle, color: dot),
                    ),
                  ),
                ),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    spacing: 4,
                    children: [
                      Text(
                        hostDisplayLabel(widget.host),
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: theme.textTheme.bodyMedium!.copyWith(
                          color: theme.colorScheme.onSurface,
                        ),
                      ),
                      Text(
                        widget.host.serverId,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: theme.textTheme.bodySmall!.copyWith(color: colors.muted),
                      ),
                    ],
                  ),
                ),
                Icon(LucideIcons.server300, size: 14, color: colors.muted),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
