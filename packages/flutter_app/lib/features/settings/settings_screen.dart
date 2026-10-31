import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/app_store.dart';
import '../../core/client.dart';
import '../../core/host_config.dart';
import '../../core/json.dart';
import '../../core/scope.dart';
import '../../core/target.dart';
import '../../platform/desktop/desktop_host.dart';
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import '../audio/audio_service.dart';
import '../audio/thinking_tone.dart';
import '../navigation/host_connections_dialog.dart';
import '../navigation/host_dialog.dart';
import '../workspace/project_settings_panel.dart';
import 'app_preferences.dart';
import 'daemon_settings.dart';
import 'settings_forms.dart';

final bool _desktop = Platform.isLinux || Platform.isMacOS || Platform.isWindows;
const _settingsSidebarWidth = 320.0;
const _appVersion = String.fromEnvironment('PASEO_VERSION');

typedef _NavItem = (String id, String labelKey, IconData icon);

/// Original `SIDEBAR_SECTION_ITEMS`, minus the excluded terminal, browser,
/// integrations and permissions pages. Desktop-only items (`isSectionAvailable`:
/// shortcuts, notifications) stay off phones, whose push registration has no page.
List<_NavItem> _appSections() => [
  ('general', 'settings.sections.general', LucideIcons.settings300),
  ('appearance', 'settings.sections.appearance', LucideIcons.palette300),
  ('sidebar', 'settings.sections.sidebar', LucideIcons.panelLeft300),
  ('chat', 'settings.sections.chat', LucideIcons.messageSquare300),
  if (_desktop) ('editor', 'settings.sections.editor', LucideIcons.code2300),
  ('files', 'settings.sections.files', LucideIcons.files300),
  if (_desktop) ('shortcuts', 'settings.sections.shortcuts', LucideIcons.keyboard300),
  if (_desktop)
    ('notifications', 'settings.sections.notifications', LucideIcons.bell300),
  ('diagnostics', 'settings.sections.diagnostics', LucideIcons.stethoscope300),
  ('about', 'settings.sections.about', LucideIcons.info300),
];

/// Original `HOST_SECTION_ITEMS`, minus plugins.
const List<_NavItem> _hostSections = [
  ('host', 'settings.hostSections.host', LucideIcons.server300),
  ('projects', 'settings.hostSections.projects', LucideIcons.folderGit2300),
  ('connections', 'settings.hostSections.connections', LucideIcons.network300),
  ('pair-device', 'openProject.tiles.pairDevice.title', LucideIcons.smartphone300),
  ('agents', 'settings.hostSections.agents', LucideIcons.bot300),
  ('metadata', 'settings.hostSections.metadata', LucideIcons.sparkles300),
  ('workspaces', 'settings.hostSections.workspaces', LucideIcons.folderGit2300),
  ('providers', 'settings.hostSections.providers', LucideIcons.boxes300),
  ('usage', 'settings.hostSections.usage', LucideIcons.gauge300),
  ('terminals', 'settings.hostSections.terminals', LucideIcons.squareTerminal300),
];

bool _isLocal(HostState host) => host.config.localCredentialPath != null;

List<HostState> _sortedHosts(AppStore store) => [
  ...store.hosts.where(_isLocal),
  ...store.hosts.where((h) => !_isLocal(h)),
];

/// Original `HostStatusDot`: online success, connecting warning, everything
/// else (offline/error) danger.
Color _statusColor(BuildContext context, ConnectionStatus status) {
  final colors = PaseoColors.of(context);
  return switch (status) {
    ConnectionStatus.connected => colors.success,
    ConnectionStatus.connecting => colors.warning,
    ConnectionStatus.error || ConnectionStatus.disconnected => colors.danger,
  };
}

/// Original `formatConnectionStatus` labels.
String _statusLabel(ConnectionStatus status) => switch (status) {
  ConnectionStatus.connected => 'Online',
  ConnectionStatus.connecting => 'Connecting',
  ConnectionStatus.error => 'Error',
  ConnectionStatus.disconnected => 'Offline',
};

/// IDENTITY_COLORS fill tones (styles/identity-colors.ts) for host swatches.
const _identityColors = <String, Color>{
  'violet': Color(0xff7a6aa8),
  'sky': Color(0xff3d7ea6),
  'emerald': Color(0xff388068),
  'orange': Color(0xffa4673a),
  'pink': Color(0xffb05c80),
  'indigo': Color(0xff6a70b8),
  'teal': Color(0xff368080),
  'red': Color(0xffb06260),
  'amber': Color(0xff8f7838),
  'blue': Color(0xff5179b0),
};

class _StatusDot extends StatelessWidget {
  const _StatusDot(this.host);
  final HostState host;
  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: host.client,
    builder: (context, _) => Container(
      width: 8,
      height: 8,
      decoration: BoxDecoration(
        color: _statusColor(context, host.client.status),
        shape: BoxShape.circle,
      ),
    ),
  );
}

/// Original `components/ui/alert.tsx` at size sm: 12/16 padding, 16px radius,
/// half-alpha accent border, 14px icon, medium accent title over a muted
/// description indented past the icon.
class _SettingsAlert extends StatelessWidget {
  const _SettingsAlert({
    required this.title,
    this.description,
    this.success = false,
  });
  final String title;
  final String? description;
  final bool success;
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final accent = success ? colors.success : theme.colorScheme.error;
    final text = theme.textTheme.bodyMedium!.copyWith(fontSize: type.uiSize(14));
    return Semantics(
      liveRegion: true,
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
        decoration: BoxDecoration(
          border: Border.all(
            color: success ? theme.dividerColor : accent.withValues(alpha: 0.5),
          ),
          borderRadius: BorderRadius.circular(16),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          spacing: 2,
          children: [
            Row(
              spacing: 12,
              children: [
                Icon(
                  success ? LucideIcons.circleCheck300 : LucideIcons.circleX300,
                  size: 14,
                  color: accent,
                ),
                Expanded(
                  child: SelectableText(
                    title,
                    style: text.copyWith(color: accent, fontWeight: FontWeight.w500),
                  ),
                ),
              ],
            ),
            if (description != null && description!.isNotEmpty)
              Padding(
                padding: const EdgeInsets.only(left: 26),
                child: Text(description!, style: text.copyWith(color: colors.muted)),
              ),
          ],
        ),
      ),
    );
  }
}

/// Original `StatusBadge` (sm): full-radius pill, 8/3 padding, 6px gap; the
/// success/warning/error variants fill with the status tint (12% light, 16%
/// dark) and drop the border.
class _StatusBadge extends StatelessWidget {
  const _StatusBadge(this.host);
  final HostState host;
  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: host.client,
    builder: (context, _) {
      final theme = Theme.of(context);
      final colors = PaseoColors.of(context);
      final type = PaseoTypography.of(context);
      final dark = theme.brightness == Brightness.dark;
      final status = host.client.status;
      final (Color? tone, Color dot) = switch (status) {
        ConnectionStatus.connected => (
          colors.success,
          dark ? const Color(0xff35c264) : const Color(0xff299f51),
        ),
        ConnectionStatus.connecting => (
          colors.warning,
          dark ? const Color(0xffdb932e) : const Color(0xffb37824),
        ),
        ConnectionStatus.error => (
          colors.danger,
          dark ? const Color(0xfff7796d) : const Color(0xfff12e2f),
        ),
        ConnectionStatus.disconnected => (null, colors.muted),
      };
      return Container(
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 3),
        decoration: BoxDecoration(
          color: tone?.withValues(alpha: dark ? 0.16 : 0.12) ?? colors.surface3,
          border: Border.all(
            color: tone == null ? theme.dividerColor : Colors.transparent,
          ),
          borderRadius: BorderRadius.circular(999),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          spacing: 6,
          children: [
            Container(
              width: 6,
              height: 6,
              decoration: BoxDecoration(color: dot, shape: BoxShape.circle),
            ),
            Text(
              _statusLabel(status),
              style: theme.textTheme.bodySmall?.copyWith(
                fontSize: type.uiSize(12),
                color: tone ?? colors.muted,
              ),
            ),
          ],
        ),
      );
    },
  );
}

/// Original host-page `badgePill`: surface3 full-radius pill with a border,
/// 8/4 padding, 6px gap, 200px max, muted 12px text.
class _BadgePill extends StatelessWidget {
  const _BadgePill(this.text, {this.icon});
  final String text;
  final IconData? icon;
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    return Container(
      constraints: const BoxConstraints(maxWidth: 200),
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
      decoration: BoxDecoration(
        color: colors.surface3,
        border: Border.all(color: theme.dividerColor),
        borderRadius: BorderRadius.circular(999),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        spacing: 6,
        children: [
          if (icon != null) Icon(icon, size: 14, color: colors.muted),
          Flexible(
            child: Text(
              text,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: theme.textTheme.bodySmall?.copyWith(
                fontSize: type.uiSize(12),
                color: colors.muted,
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// Original `screens/settings-screen.tsx`: compact root is the sidebar as a
/// full-screen list of islands; compact detail is a back-header page; wide
/// layouts split a 320px sidebar from a centered 720px detail column.
class SettingsScreen extends StatefulWidget {
  const SettingsScreen({super.key, this.initialSection, this.initialServerId});

  /// `general`…`about`, or `host/<section>`; null opens the root list.
  final String? initialSection;
  final String? initialServerId;
  @override
  State<SettingsScreen> createState() => _SettingsScreenState();
}

class _SettingsScreenState extends State<SettingsScreen> {
  late String? view = widget.initialSection;
  late String? selectedServerId = widget.initialServerId;
  String? projectRoot;

  String t(String key, [Map<String, Object?> args = const {}]) =>
      tr(context, key, args: args);

  HostState? activeHost(AppStore store) {
    final hosts = _sortedHosts(store);
    final preferred = selectedServerId ?? store.selected?.serverId;
    return hosts.where((h) => h.serverId == preferred).firstOrNull ??
        hosts.firstOrNull;
  }

  void select(String next) => setState(() {
    view = next;
    projectRoot = null;
  });

  Future<void> addHost() async {
    await showHostDialog(context);
    if (mounted) setState(() {});
  }

  void selectHostSection(AppStore store, String section) {
    if (activeHost(store) == null) {
      addHost();
      return;
    }
    select('host/$section');
  }

  void selectHost(String serverId) => setState(() {
    selectedServerId = serverId;
    if (projectRoot != null) {
      projectRoot = null;
      view = 'host/projects';
    }
  });

  String? title(AppStore store) {
    final current = view;
    if (current == null) return null;
    if (projectRoot != null) return t('settings.projects');
    if (current == 'local-daemon') return t('desktop.daemon.title');
    if (current.startsWith('host/')) {
      final item = _hostSections
          .where((s) => s.$1 == current.substring(5))
          .firstOrNull;
      return item == null ? null : t(item.$2);
    }
    final item = _appSections().where((s) => s.$1 == current).firstOrNull;
    return item == null ? null : t(item.$2);
  }

  /// Returns the page body and whether it scrolls itself.
  (Widget, bool) content(AppStore store) {
    final current = view ?? 'general';
    final host = activeHost(store);
    if (projectRoot != null && host != null) {
      final root = projectRoot!;
      return (
        ProjectSettingsPanel(
          key: ValueKey('${host.serverId}:$root'),
          target: PanelTarget(
            kind: 'project-settings',
            serverId: host.serverId,
            workspaceId: '',
            cwd: root,
            id: root,
            data: {'repoRoot': root},
          ),
        ),
        true,
      );
    }
    if (current == 'local-daemon') return (const LocalDaemonSettings(), false);
    if (current.startsWith('host/')) {
      final section = current.substring(5);
      if (host == null) {
        return (
          SettingsSection(
            title: t('settings.groups.host'),
            children: [
              SettingsActionRow(
                label: t('settings.host.notFound'),
                actionLabel: t('settings.addHost'),
                onPressed: addHost,
              ),
            ],
          ),
          false,
        );
      }
      return (
        switch (section) {
          'projects' => _ProjectsList(
            host: host,
            onOpen: (root) => setState(() => projectRoot = root),
          ),
          'connections' => Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              _HostConnectionError(host: host),
              _HostConnections(host: host),
              DaemonSettings(
                key: ValueKey(
                  '${host.serverId}:${host.client.status}:connections',
                ),
                host: host,
                section: 'connections',
              ),
            ],
          ),
          'pair-device' => SettingsSection(
            title: t('settings.host.pairDevices.title'),
            children: [
              SettingsActionRow(
                label: t('settings.host.pairDevices.rowTitle'),
                hint: t('settings.host.pairDevices.rowHint'),
                actionLabel: t('settings.host.pairDevices.rowTitle'),
                onPressed: () async {
                  try {
                    await showHostPairing(context, store, host.serverId);
                  } catch (e) {
                    if (mounted) showError(context, e);
                  }
                },
              ),
            ],
          ),
          'host' => Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              _HostOverview(key: ValueKey(host.serverId), host: host),
              if (_desktop && _isLocal(host)) const LocalDaemonSettings(),
              DaemonSettings(
                key: ValueKey('${host.serverId}:${host.client.status}:host'),
                host: host,
                section: 'host',
              ),
              if (!_isLocal(host))
                UpdateDaemonCard(key: ValueKey(host.serverId), host: host),
              _RemoveHost(
                host: host,
                onRemoved: () => setState(() {
                  selectedServerId = null;
                  view = isCompactLayout(context) ? null : 'general';
                }),
              ),
            ],
          ),
          _ => DaemonSettings(
            key: ValueKey('${host.serverId}:${host.client.status}:$section'),
            host: host,
            section: section,
          ),
        },
        false,
      );
    }
    return (
      switch (current) {
        'notifications' => const _DesktopNotificationSettings(),
        'diagnostics' => const _DiagnosticsSettings(),
        'about' => const _AboutSettings(),
        _ => AppPreferenceSettings(
          key: ValueKey(current),
          store: store,
          section: current,
        ),
      },
      false,
    );
  }

  @override
  Widget build(BuildContext context) {
    final store = PaseoScope.of(context);
    return ListenableBuilder(
      listenable: store,
      builder: (context, _) {
        final compact = isCompactLayout(context);
        final colors = PaseoColors.of(context);
        if (compact && view == null) {
          // Original mobile root: BackHeader over the sidebar-as-list, 8px
          // container padding plus the bottom safe-area inset.
          return Scaffold(
            backgroundColor: colors.surface0,
            body: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                _CompactBackHeader(
                  title: t('settings.title'),
                  onBack: () => Navigator.of(context).maybePop(),
                ),
                Expanded(
                  child: ListView(
                    padding: EdgeInsets.fromLTRB(
                      8, 8, 8, 8 + MediaQuery.paddingOf(context).bottom,
                    ),
                    children: _sidebarBody(store, mobile: true),
                  ),
                ),
              ],
            ),
          );
        }
        final (body, scrolls) = content(store);
        final pageTitle = title(store);
        if (compact) {
          void back() => setState(() {
            if (projectRoot != null) {
              projectRoot = null;
            } else {
              view = null;
            }
          });
          return PopScope(
            canPop: false,
            onPopInvokedWithResult: (didPop, _) {
              if (!didPop) back();
            },
            child: Scaffold(
              backgroundColor: colors.surface0,
              body: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  _CompactBackHeader(title: pageTitle, onBack: back),
                  Expanded(child: scrolls ? body : _PageLayout(child: body)),
                ],
              ),
            ),
          );
        }
        final border = Theme.of(context).dividerColor;
        return Scaffold(
          backgroundColor: colors.surface0,
          body: SafeArea(
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Container(
                  width: _settingsSidebarWidth,
                  decoration: BoxDecoration(
                    color: colors.sidebar,
                    border: Border(right: BorderSide(color: border)),
                  ),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    children: [
                      // Original `SidebarHeaderRow` (header variant): a 36px
                      // strip with a bottom border, 8px sides, the back row
                      // drawn like a sidebar item with a md icon.
                      Container(
                        height: 36,
                        padding: const EdgeInsets.symmetric(horizontal: 8),
                        alignment: Alignment.center,
                        decoration: BoxDecoration(
                          border: Border(bottom: BorderSide(color: border)),
                        ),
                        child: _SidebarItem(
                          icon: LucideIcons.arrowLeft300,
                          label: t('settings.backToWorkspace'),
                          mobile: false,
                          onTap: () => Navigator.of(context).maybePop(),
                        ),
                      ),
                      Expanded(
                        child: ListView(
                          padding: EdgeInsets.zero,
                          children: _sidebarBody(store, mobile: false),
                        ),
                      ),
                    ],
                  ),
                ),
                Expanded(
                  // Original desktop PageLayout keeps a borderless 36px
                  // MenuHeader strip above the scrolling page column.
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    children: [
                      const SizedBox(height: 36),
                      Expanded(
                        child: scrolls
                            ? body
                            : _PageLayout(title: pageTitle, child: body),
                      ),
                    ],
                  ),
                ),
              ],
            ),
          ),
        );
      },
    );
  }

  List<Widget> _sidebarBody(AppStore store, {required bool mobile}) {
    final hosts = _sortedHosts(store);
    final current = view ?? (mobile ? null : 'general');
    final hostSection = projectRoot != null
        ? 'projects'
        : current != null && current.startsWith('host/')
        ? current.substring(5)
        : null;
    final noLocalHost = _desktop && !hosts.any(_isLocal);
    return [
      _SidebarGroup(
        label: t('settings.groups.app'),
        mobile: mobile,
        children: [
          for (final item in _appSections())
            _SidebarItem(
              icon: item.$3,
              label: t(item.$2),
              mobile: mobile,
              selected: !mobile && hostSection == null && current == item.$1,
              onTap: () => select(item.$1),
            ),
        ],
      ),
      if (hosts.isNotEmpty)
        _SidebarGroup(
          label: t('settings.groups.host'),
          mobile: mobile,
          children: [
            _HostPicker(
              hosts: hosts,
              active: activeHost(store),
              mobile: mobile,
              onSelect: selectHost,
              onAddHost: addHost,
              onEnableBuiltInDaemon: noLocalHost
                  ? () => select('local-daemon')
                  : null,
            ),
            for (final item in _hostSections)
              _SidebarItem(
                icon: item.$3,
                label: t(item.$2),
                mobile: mobile,
                selected: !mobile && hostSection == item.$1,
                onTap: () => selectHostSection(store, item.$1),
              ),
          ],
        )
      else
        _SidebarGroup(
          mobile: mobile,
          children: [
            _SidebarItem(
              icon: LucideIcons.plus300,
              label: t('settings.addHost'),
              mobile: mobile,
              onTap: addHost,
            ),
            if (noLocalHost)
              _SidebarItem(
                icon: LucideIcons.server300,
                label: t('settings.enableBuiltInDaemon'),
                mobile: mobile,
                selected: !mobile && current == 'local-daemon',
                onTap: () => select('local-daemon'),
              ),
          ],
        ),
    ];
  }
}

/// Original `PageLayout`: centered 720px column, 26px medium title on wide.
class _PageLayout extends StatelessWidget {
  const _PageLayout({this.title, required this.child});
  final String? title;
  final Widget child;
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final type = PaseoTypography.of(context);
    return ListView(
      padding: EdgeInsets.only(bottom: MediaQuery.paddingOf(context).bottom),
      children: [
        Center(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 720),
            child: Padding(
              padding: const EdgeInsets.fromLTRB(16, 24, 16, 16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  if (title != null)
                    Padding(
                      padding: const EdgeInsets.only(left: 4, bottom: 24),
                      child: Text(
                        title!,
                        style: theme.textTheme.headlineLarge?.copyWith(
                          fontSize: type.uiSize(26),
                          fontWeight: FontWeight.w500,
                        ),
                      ),
                    ),
                  child,
                ],
              ),
            ),
          ),
        ),
      ],
    );
  }
}

/// Original `CompactHeader` with `navigation="back"`: no background or
/// border; the safe-area top plus 8px, 12px sides, a 56px row holding the
/// 32px round back island (16px muted arrow) and, 8px after it, the
/// left-aligned lg medium title.
class _CompactBackHeader extends StatelessWidget {
  const _CompactBackHeader({required this.title, required this.onBack});
  final String? title;
  final VoidCallback onBack;
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final type = PaseoTypography.of(context);
    final insets = MediaQuery.paddingOf(context);
    return Padding(
      padding: EdgeInsets.fromLTRB(
        12 + insets.left,
        insets.top + 8,
        12 + insets.right,
        0,
      ),
      child: SizedBox(
        height: 56,
        child: Row(
          spacing: 8,
          children: [
            _HeaderBackButton(onPressed: onBack),
            Expanded(
              child: title == null
                  ? const SizedBox.shrink()
                  : Text(
                      title!,
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

/// Original header-island back button: a 32px circle filled with
/// `interactionHighlight` while hovered or pressed, 8px vertical hit slop.
class _HeaderBackButton extends StatefulWidget {
  const _HeaderBackButton({required this.onPressed});
  final VoidCallback onPressed;
  @override
  State<_HeaderBackButton> createState() => _HeaderBackButtonState();
}

class _HeaderBackButtonState extends State<_HeaderBackButton> {
  bool hovered = false, pressed = false;
  @override
  Widget build(BuildContext context) => Semantics(
    button: true,
    label: tr(context, 'common.actions.back'),
    child: MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => hovered = true),
      onExit: (_) => setState(() => hovered = false),
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTapDown: (_) => setState(() => pressed = true),
        onTapCancel: () => setState(() => pressed = false),
        onTapUp: (_) => setState(() => pressed = false),
        onTap: widget.onPressed,
        child: Padding(
          padding: const EdgeInsets.symmetric(vertical: 8),
          child: Container(
            width: 32,
            height: 32,
            alignment: Alignment.center,
            decoration: BoxDecoration(
              shape: BoxShape.circle,
              color: hovered || pressed
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
  );
}

/// Original `SidebarGroup`: 8px padding, 4/2 gaps, a base extra-muted label
/// inset 16/8, and on compact layouts an island of rows 2px apart with a
/// 16px (`2xl`) radius.
class _SidebarGroup extends StatelessWidget {
  const _SidebarGroup({this.label, required this.mobile, required this.children});
  final String? label;
  final bool mobile;
  final List<Widget> children;
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    return Padding(
      padding: const EdgeInsets.all(8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: mobile ? 4 : 2,
        children: [
          if (label != null)
            Padding(
              padding: EdgeInsets.symmetric(
                horizontal: mobile ? 16 : 8,
                vertical: 4,
              ),
              child: Text(
                label!,
                style: theme.textTheme.bodyMedium?.copyWith(
                  color: colors.extraMuted,
                  fontSize: type.uiSize(14),
                  fontWeight: FontWeight.w400,
                ),
              ),
            ),
          ClipRRect(
            borderRadius: BorderRadius.circular(mobile ? 16 : 0),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              spacing: 2,
              children: children,
            ),
          ),
        ],
      ),
    );
  }
}

/// Original settings sidebar item: 52/28 rows, 16/8 sides, 12/8 icon gap,
/// md density icon, lg/base label; compact rows fill surface1 inside their
/// island, desktop rows round 8px. Hover and selection share
/// `surfaceSidebarHover`, pressed reads surface2 — no focus/ink layers, so
/// only the selected row stays highlighted.
class _SidebarItem extends StatefulWidget {
  const _SidebarItem({
    required this.icon,
    required this.label,
    required this.mobile,
    required this.onTap,
    this.selected = false,
    this.leading,
    this.trailing,
    this.gap,
    this.foreground = false,
  });
  final IconData icon;
  final String label;
  final bool mobile, selected, foreground;
  final VoidCallback onTap;
  final Widget? leading;

  /// Pinned to the trailing edge (the host picker's `ComboboxTrigger` chevron).
  final Widget? trailing;

  /// Icon-to-label gap; defaults to the section rows' 12 (compact) / 8.
  final double? gap;
  @override
  State<_SidebarItem> createState() => _SidebarItemState();
}

class _SidebarItemState extends State<_SidebarItem> {
  bool hovered = false, pressed = false;
  @override
  Widget build(BuildContext context) {
    final item = widget;
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final active = item.selected || item.foreground;
    final color = active ? theme.colorScheme.onSurface : colors.muted;
    final iconSize = densityIcon(context, 'md');
    final background = pressed
        ? colors.surface2
        : hovered || item.selected
        ? paseoSidebarHover(context)
        : item.mobile
        ? colors.surface1
        : Colors.transparent;
    return Semantics(
      button: true,
      selected: item.selected,
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        onEnter: (_) => setState(() => hovered = true),
        onExit: (_) => setState(() => hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: (_) => setState(() => pressed = true),
          onTapCancel: () => setState(() => pressed = false),
          onTapUp: (_) => setState(() => pressed = false),
          onTap: item.onTap,
          child: Container(
            constraints: BoxConstraints(minHeight: item.mobile ? 52 : 28),
            padding: EdgeInsets.symmetric(
              horizontal: item.mobile ? 16 : 8,
              vertical: 4,
            ),
            decoration: BoxDecoration(
              color: background,
              borderRadius: BorderRadius.circular(item.mobile ? 0 : 8),
            ),
            child: Row(
              spacing: item.gap ?? (item.mobile ? 12 : 8),
              children: [
                SizedBox.square(
                  dimension: iconSize,
                  child: Center(
                    child: item.leading ??
                        Icon(item.icon, size: iconSize, color: color),
                  ),
                ),
                Expanded(
                  child: Text(
                    item.label,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: theme.textTheme.bodyMedium?.copyWith(
                      color: color,
                      fontSize: type.uiSize(item.mobile ? 16 : 14),
                      fontWeight: FontWeight.w400,
                    ),
                  ),
                ),
                ?item.trailing,
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// Original settings `HostPicker`: a row-styled trigger (status dot in the
/// icon slot, foreground label) opening the shared host list — a 240px
/// popover on wide layouts, the "Switch host" sheet on compact — with each
/// host's active connection, Add host and Enable built-in daemon.
class _HostPicker extends StatelessWidget {
  const _HostPicker({
    required this.hosts,
    required this.active,
    required this.mobile,
    required this.onSelect,
    required this.onAddHost,
    this.onEnableBuiltInDaemon,
  });
  final List<HostState> hosts;
  final HostState? active;
  final bool mobile;
  final ValueChanged<String> onSelect;
  final VoidCallback onAddHost;
  final VoidCallback? onEnableBuiltInDaemon;

  static const _addHost = '\u0000add-host';
  static const _builtInDaemon = '\u0000built-in-daemon';

  /// Original `formatActiveConnectionLabel`: sockets/pipes read as Local;
  /// standard web ports are dropped.
  static String _connection(BuildContext context, HostState host) {
    if (_isLocal(host)) return tr(context, 'settings.hostPicker.local');
    try {
      final uri = host.config.resolve().uri;
      if (uri.scheme == 'unix' || uri.scheme == 'pipe') {
        return tr(context, 'settings.hostPicker.local');
      }
      final port = uri.hasPort && uri.port != 443 && uri.port != 80;
      return port ? '${uri.host}:${uri.port}' : uri.host;
    } catch (_) {
      return '';
    }
  }

  Future<void> _open(BuildContext anchor) async {
    final picked = await showPaseoMenu<String>(
      anchor,
      title: tr(anchor, 'settings.hostPicker.switchHost'),
      anchor: paseoAnchorRect(anchor),
      width: 240,
      entries: [
        for (final host in hosts)
          PaseoMenuEntry.item(
            value: host.serverId,
            label: host.label,
            leading: _StatusDot(host),
            hint: _connection(anchor, host),
          ),
        const PaseoMenuEntry.separator(),
        PaseoMenuEntry.item(
          value: _addHost,
          label: tr(anchor, 'settings.addHost'),
          icon: LucideIcons.plus300,
        ),
        if (onEnableBuiltInDaemon != null)
          PaseoMenuEntry.item(
            value: _builtInDaemon,
            label: tr(anchor, 'settings.enableBuiltInDaemon'),
            icon: LucideIcons.server300,
          ),
      ],
    );
    switch (picked) {
      case null:
        return;
      case _addHost:
        onAddHost();
      case _builtInDaemon:
        onEnableBuiltInDaemon?.call();
      default:
        onSelect(picked);
    }
  }

  @override
  Widget build(BuildContext context) => Semantics(
    label: tr(context, 'settings.hostPicker.switchHost'),
    child: Builder(
      builder: (anchor) => _SidebarItem(
        icon: LucideIcons.circle300,
        leading: active == null ? null : _StatusDot(active!),
        label: active?.label ?? tr(context, 'settings.groups.host'),
        mobile: mobile,
        foreground: true,
        // Original `ComboboxTrigger block`: dot, label and the trailing 14px
        // foregroundMuted ChevronDown (nudged 1px down) share one 8px-gap row.
        gap: 8,
        trailing: Transform.translate(
          offset: const Offset(0, 1),
          child: Icon(
            LucideIcons.chevronDown300,
            size: 14,
            color: PaseoColors.of(context).muted,
          ),
        ),
        onTap: () => _open(anchor),
      ),
    ),
  );
}

/// Original `ProjectsScreen`: one card of project rows for the host.
class _ProjectsList extends StatelessWidget {
  const _ProjectsList({required this.host, required this.onOpen});
  final HostState host;
  final ValueChanged<String> onOpen;
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    if (host.projects.isEmpty) {
      // Original centered state: 24px padding around a large muted spinner
      // or the muted base empty text.
      return Padding(
        padding: const EdgeInsets.all(24),
        child: Center(
          child: host.loading
              ? SizedBox.square(
                  dimension: 36,
                  child: CircularProgressIndicator(
                    strokeWidth: 2,
                    color: colors.muted,
                  ),
                )
              : Text(
                  tr(context, 'sidebar.project.empty.title'),
                  style: theme.textTheme.bodyMedium?.copyWith(
                    color: colors.muted,
                  ),
                ),
        ),
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        // Original `HostErrorsBanner`: bordered 8px-radius box, 12px padding,
        // 12px above the card, red-300 small text.
        if (host.error != null)
          Container(
            margin: const EdgeInsets.only(bottom: 12),
            padding: const EdgeInsets.all(12),
            decoration: BoxDecoration(
              border: Border.all(color: theme.dividerColor),
              borderRadius: BorderRadius.circular(8),
            ),
            child: Text(
              tr(context, 'settings.projectList.hostLoadFailed', args: {
                'hostName': host.label,
                'message': host.error.toString(),
              }),
              style: theme.textTheme.bodySmall?.copyWith(
                color: paseoFieldErrorColor,
              ),
            ),
          ),
        SettingsCard(
          children: [
            for (final project in host.projects)
              Builder(builder: (context) {
                final root = project.text(
                  'projectRootPath',
                  project.text('repoRoot', project.text('cwd')),
                );
                final name = project.text('name', root.split('/').last);
                return Semantics(
                  button: true,
                  label: tr(context, 'settings.projectList.editProject',
                      args: {'projectName': name}),
                  child: SettingsRow(
                    // Original `ProjectRowIcon` fallback: the identity square.
                    leading: ProjectIconSquare(
                      name: name,
                      viewKey: projectViewKey(host.serverId, project),
                      size: 16,
                    ),
                    label: name,
                    control: Icon(LucideIcons.chevronRight300, size: 14,
                        color: colors.muted),
                    onTap: root.isEmpty ? null : () => onOpen(root),
                  ),
                );
              }),
          ],
        ),
      ],
    );
  }
}

/// Original `HostConnectionError`: the last connection error as a small
/// error alert, 24px above the page content.
class _HostConnectionError extends StatelessWidget {
  const _HostConnectionError({required this.host});
  final HostState host;
  @override
  Widget build(BuildContext context) {
    final message = host.error?.toString().trim() ?? '';
    if (message.isEmpty) return const SizedBox.shrink();
    return Padding(
      padding: const EdgeInsets.only(bottom: 24),
      child: _SettingsAlert(title: message),
    );
  }
}

/// Original host overview: `HostStatusBadges` (status badge, active
/// connection and daemon version pills; 4px gap, 24px below), the
/// connection error and `HostAppearanceSection` (name + rename, color).
class _HostOverview extends StatefulWidget {
  const _HostOverview({super.key, required this.host});
  final HostState host;
  @override
  State<_HostOverview> createState() => _HostOverviewState();
}

class _HostOverviewState extends State<_HostOverview> {
  static const _colors = [
    'none', 'violet', 'sky', 'emerald', 'orange', 'pink', 'indigo', 'teal',
    'red', 'amber', 'blue',
  ];
  bool busy = false;

  Future<void> update({String? label, String? color}) async {
    final store = PaseoScope.of(context);
    final config = widget.host.config;
    final json = <String, dynamic>{...config.toJson(), 'label': ?label};
    if (color != null) {
      json.remove('color');
      if (color != 'none') json['color'] = color;
    }
    final next = HostConfig.fromJson(json);
    if (next.label == config.label && next.color == config.color) return;
    setState(() => busy = true);
    try {
      await store.updateHost(next);
    } catch (e) {
      if (mounted) showError(context, e);
    } finally {
      if (mounted) setState(() => busy = false);
    }
  }

  Future<void> rename() async {
    final value = await promptText(
      context,
      title: tr(context, 'settings.host.daemon.rename.title'),
      initialValue: widget.host.config.label,
      hint: tr(context, 'settings.host.daemon.rename.placeholder'),
    );
    final label = value?.trim() ?? '';
    if (label.isNotEmpty && mounted) await update(label: label);
  }

  /// Original `formatActiveConnectionBadge`.
  (IconData, String)? _connectionBadge() {
    String t(String key) => tr(context, key);
    try {
      final target = widget.host.config.resolve();
      final uri = target.uri;
      if (target.isRelay) return (LucideIcons.globe300, t('settings.host.badges.relay'));
      if (uri.scheme == 'unix' || uri.scheme == 'pipe') {
        return (LucideIcons.monitor300, t('settings.host.badges.local'));
      }
      if (uri.scheme == 'ssh') {
        return (LucideIcons.globe300, t('settings.host.badges.remoteSsh'));
      }
      final port = uri.hasPort && uri.port != 443 && uri.port != 80;
      return (LucideIcons.monitor300, port ? '${uri.host}:${uri.port}' : uri.host);
    } catch (_) {
      return null;
    }
  }

  @override
  Widget build(BuildContext context) {
    final host = widget.host;
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    String t(String key, [Map<String, Object?> args = const {}]) =>
        tr(context, key, args: args);
    final version = host.client.serverInfo['version']?.toString().trim() ?? '';
    final connection = _connectionBadge();
    final color = _colors.contains(host.config.color) ? host.config.color! : 'none';
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.only(bottom: 24),
          child: Wrap(
            spacing: 4,
            runSpacing: 4,
            crossAxisAlignment: WrapCrossAlignment.center,
            children: [
              _StatusBadge(host),
              if (connection != null) _BadgePill(connection.$2, icon: connection.$1),
              if (version.isNotEmpty)
                _BadgePill(version.startsWith('v') ? version : 'v$version'),
            ],
          ),
        ),
        _HostConnectionError(host: host),
        SettingsSection(
          title: t('settings.host.appearance.title'),
          children: [
            SettingsRow(
              label: t('settings.host.appearance.name.label'),
              inline: true,
              control: Row(
                mainAxisSize: MainAxisSize.min,
                spacing: 8,
                children: [
                  Flexible(
                    child: Text(
                      host.label,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: theme.textTheme.bodyMedium?.copyWith(
                        fontSize: type.uiSize(14),
                        color: colors.muted,
                      ),
                    ),
                  ),
                  _RenameButton(
                    label: t('settings.host.daemon.rename.editLabel'),
                    onPressed: busy ? null : rename,
                  ),
                ],
              ),
            ),
            SettingsRow(
              label: t('settings.host.appearance.color.label'),
              control: SettingsDropdown<String>(
                value: color,
                menuWidth: 200,
                semanticLabel: t('settings.host.appearance.color.label'),
                leading: (value) => _ColorSwatch(_identityColors[value]),
                options: [
                  for (final option in _colors)
                    (option, t('settings.host.appearance.color.options.$option')),
                ],
                onChanged: busy ? null : (v) => update(color: v),
              ),
            ),
          ],
        ),
      ],
    );
  }
}

/// Original host `ColorSwatch`: a 16px bordered circle, empty for "none".
class _ColorSwatch extends StatelessWidget {
  const _ColorSwatch(this.color);
  final Color? color;
  @override
  Widget build(BuildContext context) => Container(
    width: 16,
    height: 16,
    decoration: BoxDecoration(
      color: color,
      shape: BoxShape.circle,
      border: Border.all(color: Theme.of(context).dividerColor),
    ),
  );
}

/// Original `HostRenameButton`: 4px padding, 6px radius, 14px muted pencil.
class _RenameButton extends StatefulWidget {
  const _RenameButton({required this.label, required this.onPressed});
  final String label;
  final VoidCallback? onPressed;
  @override
  State<_RenameButton> createState() => _RenameButtonState();
}

class _RenameButtonState extends State<_RenameButton> {
  bool hovered = false, pressed = false;
  @override
  Widget build(BuildContext context) {
    final enabled = widget.onPressed != null;
    return Semantics(
      button: true,
      label: widget.label,
      child: MouseRegion(
        cursor: enabled ? SystemMouseCursors.click : MouseCursor.defer,
        onEnter: (_) => setState(() => hovered = true),
        onExit: (_) => setState(() => hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: enabled ? (_) => setState(() => pressed = true) : null,
          onTapCancel: () => setState(() => pressed = false),
          onTapUp: (_) => setState(() => pressed = false),
          onTap: widget.onPressed,
          child: Opacity(
            opacity: enabled ? 1 : .5,
            child: Container(
              padding: const EdgeInsets.all(4),
              decoration: BoxDecoration(
                color: enabled && (hovered || pressed)
                    ? paseoInteractionHighlight(context)
                    : null,
                borderRadius: BorderRadius.circular(6),
              ),
              child: Icon(
                LucideIcons.pencil300,
                size: 14,
                color: PaseoColors.of(context).muted,
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// Original `ConnectionsSection`: one row per saved connection with its
/// `formatHostConnectionLabel` title and a destructive ghost Remove action
/// behind a confirmation.
class _HostConnections extends StatelessWidget {
  const _HostConnections({required this.host});
  final HostState host;

  Future<void> _remove(BuildContext context, HostConfig connection) async {
    String t(String key, [Map<String, Object?> args = const {}]) =>
        tr(context, key, args: args);
    final store = PaseoScope.of(context);
    final confirmed = await confirmAction(
      context,
      title: t('settings.host.connections.removeTitle'),
      message: t('settings.host.connections.removeMessage', {
        'name': hostConnectionLabel(context, connection),
      }),
      destructive: true,
    );
    if (!confirmed) return;
    try {
      await store.removeConnection(host.serverId, connection.id);
    } catch (_) {
      if (context.mounted) {
        showToast(
          context,
          t('settings.host.connections.removeErrorMessage'),
          variant: ToastVariant.error,
        );
      }
    }
  }

  @override
  Widget build(BuildContext context) => SettingsSection(
    title: tr(context, 'settings.host.connections.title'),
    children: [
      for (final connection in host.connections)
        SettingsRow(
          label: hostConnectionLabel(context, connection),
          labelMaxLines: 1,
          inline: true,
          control: PaseoButton(
            label: tr(context, 'settings.host.connections.removeAction'),
            size: PaseoControlSize.sm,
            variant: PaseoButtonVariant.ghost,
            foreground: Theme.of(context).colorScheme.error,
            onPressed: () => _remove(context, connection),
          ),
        ),
    ],
  );
}

class _RemoveHost extends StatelessWidget {
  const _RemoveHost({required this.host, required this.onRemoved});
  final HostState host;
  final VoidCallback onRemoved;
  @override
  Widget build(BuildContext context) {
    final local = _isLocal(host);
    String t(String key, [Map<String, Object?> args = const {}]) =>
        tr(context, key, args: args);
    // Original `RemoveHostSection`: one "Danger zone" title over the restart
    // card, then the remove card.
    return SettingsSection(
      title: t('settings.host.daemon.dangerZone'),
      card: false,
      children: [
        RestartDaemonCard(host: host),
        SettingsCard(
          children: [
            SettingsActionRow(
              label: t(local
                  ? 'settings.host.daemon.remove.localTitle'
                  : 'settings.host.daemon.remove.title'),
              hint: t(local
                  ? 'settings.host.daemon.remove.localHint'
                  : 'settings.host.daemon.remove.hint'),
              actionLabel: t('settings.host.connections.removeAction'),
              onPressed: () async {
                final store = PaseoScope.of(context);
                final confirmed = await confirmAction(
                  context,
                  title: local
                      ? t('settings.host.daemon.remove.localConfirmTitle')
                      : t('settings.host.daemon.remove.title'),
                  message: local
                      ? t('settings.host.daemon.remove.localConfirmMessage')
                      : t('settings.host.daemon.remove.confirmMessage',
                          {'name': host.label}),
                  destructive: true,
                );
                if (!confirmed) return;
                try {
                  if (local && _desktop && DesktopHost.instance != null) {
                    await DesktopHost.instance!.updateSettings({
                      'daemon': {'manageBuiltInDaemon': false},
                    });
                  }
                  await store.removeHost(host.config.id);
                  onRemoved();
                } catch (e) {
                  if (context.mounted) showError(context, e);
                }
              },
            ),
          ],
        ),
      ],
    );
  }
}

mixin _DesktopInvoke<T extends StatefulWidget> on State<T> {
  bool busy = false;
  Object? error;
  Future<dynamic> invoke(String command, [JsonMap args = const {}]) {
    final service = DesktopHost.instance;
    if (service == null) {
      throw StateError('The native desktop host service has not initialized.');
    }
    return service.invoke(command, args);
  }

  Future<void> run(Future<void> Function() operation) async {
    if (busy) return;
    setState(() => busy = true);
    try {
      await operation();
      if (mounted) setState(() => error = null);
    } catch (e) {
      if (mounted) setState(() => error = e);
    } finally {
      if (mounted) setState(() => busy = false);
    }
  }

  Future<void> report(String command, String title) => run(() async {
    final result = await invoke(command);
    if (mounted) await showSettingsReport(context, title, result);
  });
}

/// Original desktop `LocalDaemonSection`: status, built-in management, keep
/// running, logs, full status and owned-daemon lifecycle.
class LocalDaemonSettings extends StatefulWidget {
  const LocalDaemonSettings({super.key});
  @override
  State<LocalDaemonSettings> createState() => _LocalDaemonSettingsState();
}

class _LocalDaemonSettingsState extends State<LocalDaemonSettings>
    with _DesktopInvoke {
  JsonMap settings = {}, daemon = {};
  bool loading = true;

  @override
  void initState() {
    super.initState();
    load();
  }

  Future<void> load() async {
    try {
      final result = await Future.wait([
        invoke('desktop_settings_get'),
        invoke('desktop_daemon_status'),
      ]);
      if (mounted) {
        setState(() {
          settings = jsonMap(result[0]);
          daemon = jsonMap(result[1]);
          error = null;
          loading = false;
        });
      }
    } catch (e) {
      if (mounted) {
        setState(() {
          error = e;
          loading = false;
        });
      }
    }
  }

  Future<void> patchDaemon(JsonMap patch) => run(() async {
    await invoke('desktop_settings_update', {
      'patch': {'daemon': patch},
    });
    await load();
  });

  @override
  Widget build(BuildContext context) {
    String t(String key, [Map<String, Object?> args = const {}]) =>
        tr(context, key, args: args);
    final colors = PaseoColors.of(context);
    final theme = Theme.of(context);
    final daemonSettings = jsonMap(settings['daemon']);
    final running = daemon['status'] == 'running';
    final owned = daemon['ownedByDesktop'] == true;
    final pid = daemon['pid'];
    return SettingsSection(
      title: t('desktop.daemon.title'),
      card: false,
      children: [
        // Original loading card: a small muted spinner, 24px above/below.
        // Busy actions only disable their controls (no progress bar).
        if (loading)
          SettingsCard(
            children: [
              Padding(
                padding: const EdgeInsets.symmetric(vertical: 24),
                child: Center(
                  child: SizedBox.square(
                    dimension: 20,
                    child: CircularProgressIndicator(
                      strokeWidth: 2,
                      color: colors.muted,
                    ),
                  ),
                ),
              ),
            ],
          )
        else ...[
          SettingsCard(
            children: [
              SettingsRow(
                label: t('desktop.daemon.status.title'),
                hint: t('desktop.daemon.status.builtInOnly'),
                error: error?.toString(),
                control: Column(
                  crossAxisAlignment: CrossAxisAlignment.end,
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Text(
                      t(running
                          ? 'desktop.daemon.status.running'
                          : 'desktop.daemon.status.notRunning'),
                      style: theme.textTheme.bodyMedium?.copyWith(
                        color: colors.muted,
                      ),
                    ),
                    Text(
                      t('desktop.daemon.status.pid', {'pid': pid ?? '—'}),
                      style: theme.textTheme.bodySmall?.copyWith(
                        color: colors.muted,
                      ),
                    ),
                  ],
                ),
              ),
              SettingsSwitchRow(
                label: t('desktop.daemon.management.title'),
                hint: t('desktop.daemon.management.hint'),
                value: daemonSettings['manageBuiltInDaemon'] != false,
                onChanged: busy
                    ? null
                    : (v) async {
                        if (!v &&
                            !await confirmAction(
                              context,
                              title: t('desktop.daemon.management.pauseTitle'),
                              message: t(
                                'desktop.daemon.management.pauseMessage',
                              ),
                              destructive: true,
                            )) {
                          return;
                        }
                        await patchDaemon({'manageBuiltInDaemon': v});
                      },
              ),
              SettingsSwitchRow(
                label: t('desktop.daemon.keepRunning.title'),
                hint: t('desktop.daemon.keepRunning.hint'),
                value: daemonSettings['keepRunningAfterQuit'] == true,
                onChanged: busy
                    ? null
                    : (v) => patchDaemon({'keepRunningAfterQuit': v}),
              ),
              SettingsActionRow(
                label: t('desktop.daemon.logs.title'),
                hint: daemon['logPath']?.toString() ??
                    t('desktop.daemon.logs.unavailable'),
                icon: LucideIcons.fileText300,
                actionLabel: t('desktop.daemon.logs.open'),
                onPressed: busy
                    ? null
                    : () => report(
                        'desktop_daemon_logs',
                        t('desktop.daemon.logs.modalTitle'),
                      ),
              ),
              SettingsActionRow(
                label: t('desktop.daemon.fullStatus.title'),
                hint: t('desktop.daemon.fullStatus.hint'),
                icon: LucideIcons.activity300,
                actionLabel: t('desktop.daemon.fullStatus.view'),
                onPressed: busy
                    ? null
                    : () => report(
                        'desktop_daemon_status',
                        t('desktop.daemon.fullStatus.modalTitle'),
                      ),
              ),
            ],
          ),
          SettingsCard(
            children: [
              SettingsRow(
                label: pid == null
                    ? t('desktop.daemon.status.notRunning')
                    : t(owned
                        ? 'desktop.daemon.lifecycle.owned'
                        : 'desktop.daemon.lifecycle.attached'),
                hint: daemon['version'] == null
                    ? null
                    : 'v${daemon['version']}',
                control: Wrap(
                  spacing: 8,
                  runSpacing: 8,
                  children: [
                    if (!running)
                      settingsButton(context, 'Start daemon', busy
                          ? null
                          : () => run(() async {
                              await invoke('start_desktop_daemon');
                              await load();
                            })),
                    if (running && owned) ...[
                      settingsButton(
                        context,
                        t('settings.host.daemon.restart.title'),
                        busy
                            ? null
                            : () => run(() async {
                                if (!await confirmAction(
                                  context,
                                  title: t('settings.host.daemon.restart.title'),
                                  message: t(
                                    'settings.host.daemon.restart.hint',
                                  ),
                                )) {
                                  return;
                                }
                                await invoke('restart_desktop_daemon', {
                                  'reason': 'settings',
                                });
                                await load();
                              }),
                      ),
                      settingsButton(
                        context,
                        t('desktop.daemon.lifecycle.stop'),
                        busy
                            ? null
                            : () => run(() async {
                                if (!await confirmAction(
                                  context,
                                  title: t('desktop.daemon.lifecycle.stopTitle'),
                                  message: t(
                                    'desktop.daemon.lifecycle.ownedMessage',
                                  ),
                                  destructive: true,
                                )) {
                                  return;
                                }
                                await invoke('stop_desktop_daemon', {
                                  'reason': 'settings',
                                });
                                await load();
                              }),
                      ),
                    ],
                  ],
                ),
              ),
            ],
          ),
        ],
      ],
    );
  }
}

/// Original `DesktopNotificationsSection`.
class _DesktopNotificationSettings extends StatefulWidget {
  const _DesktopNotificationSettings();
  @override
  State<_DesktopNotificationSettings> createState() =>
      _DesktopNotificationSettingsState();
}

class _DesktopNotificationSettingsState
    extends State<_DesktopNotificationSettings> with _DesktopInvoke {
  JsonMap settings = {};
  bool? granted;
  bool testSent = false;
  String? testError;

  @override
  void initState() {
    super.initState();
    load();
  }

  Future<void> load() async {
    try {
      final result = jsonMap(await invoke('desktop_settings_get'));
      if (mounted) setState(() => settings = result);
    } catch (e) {
      if (mounted) setState(() => error = e);
    }
  }

  @override
  Widget build(BuildContext context) {
    String t(String key) => tr(context, key);
    final service = DesktopHost.instance;
    return SettingsSection(
      title: t('settings.notifications.title'),
      footer: testSent
          ? _SettingsAlert(
              success: true,
              title: t('settings.notifications.sentTitle'),
              description: t('settings.notifications.sentDescription'),
            )
          : testError != null
          ? _SettingsAlert(
              title: t('settings.notifications.sendFailedTitle'),
              description: testError,
            )
          : null,
      children: [
        SettingsActionRow(
          label: t('settings.notifications.permission'),
          hint: granted == true ? t('settings.permissions.actions.granted') : null,
          icon: LucideIcons.rotateCw300,
          actionLabel: t('settings.permissions.actions.request'),
          onPressed: busy || service == null
              ? null
              : () => run(() async {
                  final result =
                      await service.integrations.requestNotificationPermission();
                  if (mounted) setState(() => granted = result);
                }),
        ),
        SettingsSwitchRow(
          label: t('settings.notifications.playSound'),
          hint: t('settings.notifications.playSoundHint'),
          value: jsonMap(settings['notifications'])['playSound'] != false,
          onChanged: busy || settings.isEmpty
              ? null
              : (v) => run(() async {
                  await invoke('desktop_settings_update', {
                    'patch': {
                      'notifications': {'playSound': v},
                    },
                  });
                  await load();
                }),
        ),
        SettingsRow(
          label: t('settings.notifications.test'),
          hint: granted == false
              ? t('settings.notifications.permissionRequired')
              : t('settings.notifications.testHint'),
          error: error?.toString(),
          control: settingsButton(
            context,
            busy ? t('settings.notifications.sending') : t('settings.notifications.send'),
            busy || service == null || granted == false
                ? null
                : () => run(() async {
                    setState(() {
                      testSent = false;
                      testError = null;
                    });
                    try {
                      await service.notify(
                        id: DateTime.now().millisecondsSinceEpoch & 0x7fffffff,
                        title: 'Paseo',
                        body: t('settings.notifications.testHint'),
                      );
                      if (mounted) setState(() => testSent = true);
                    } catch (e) {
                      if (mounted) setState(() => testError = e.toString());
                    }
                  }),
          ),
        ),
      ],
    );
  }
}

/// Original `DiagnosticsSection`: app diagnostic and audio output test.
class _DiagnosticsSettings extends StatefulWidget {
  const _DiagnosticsSettings();
  @override
  State<_DiagnosticsSettings> createState() => _DiagnosticsSettingsState();
}

class _DiagnosticsSettingsState extends State<_DiagnosticsSettings> {
  bool running = false, playing = false;
  String? playbackResult;

  Future<void> diagnostic() async {
    final store = PaseoScope.of(context);
    setState(() => running = true);
    try {
      final report = <String, dynamic>{
        'client': {
          'version': _appVersion.isEmpty ? null : _appVersion,
          'platform': Platform.operatingSystem,
          'platformVersion': Platform.operatingSystemVersion,
        },
        'hosts': [
          for (final host in _sortedHosts(store))
            {
              'label': host.label,
              'serverId': host.serverId,
              'status': host.client.status.name,
              'version': host.client.serverInfo['version'],
              if (host.error != null) 'error': host.error.toString(),
            },
        ],
      };
      final desktop = DesktopHost.instance;
      if (_desktop && desktop != null) {
        for (final (key, command) in const [
          ('desktop', 'desktop_get_runtime_info'),
          ('daemon', 'desktop_daemon_status'),
          ('updates', 'desktop_update_diagnostics'),
          ('logs', 'desktop_app_logs'),
        ]) {
          try {
            report[key] = await desktop.invoke(command);
          } catch (e) {
            report[key] = {'error': e.toString()};
          }
        }
      }
      for (final host in store.hosts) {
        if (host.client.status != ConnectionStatus.connected) continue;
        try {
          final result = await settingsRequest(
            host.client,
            'diagnostics.request',
            {},
            responseType: 'diagnostics.response',
          );
          (report['hostDiagnostics'] ??= <String, dynamic>{})[host.serverId] =
              result['diagnostic'];
        } catch (e) {
          (report['hostDiagnostics'] ??= <String, dynamic>{})[host.serverId] =
              {'error': e.toString()};
        }
      }
      if (mounted) {
        await showSettingsReport(
          context,
          tr(context, 'settings.diagnostics.app.title'),
          report,
        );
      }
    } finally {
      if (mounted) setState(() => running = false);
    }
  }

  /// Original `handlePlaybackTest`: stop current output, then play the
  /// thinking tone to completion. Output does not depend on a host.
  Future<void> playTest() async {
    setState(() {
      playing = true;
      playbackResult = null;
    });
    try {
      await AudioService.cancelPlayback();
      await AudioService.playAudio(
        base64Decode(thinkingTonePcmBase64),
        mimeType: 'audio/pcm;rate=16000;bits=16',
      );
    } catch (e) {
      if (mounted) {
        playbackResult = tr(context, 'settings.diagnostics.playbackFailed',
            args: {'message': e.toString()});
      }
    } finally {
      if (mounted) setState(() => playing = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    String t(String key) => tr(context, key);
    return SettingsSection(
      title: t('settings.diagnostics.title'),
      children: [
        SettingsActionRow(
          label: t('settings.diagnostics.app.rowTitle'),
          hint: t('settings.diagnostics.app.rowHint'),
          actionLabel: running
              ? t('settings.diagnostics.app.running')
              : t('settings.diagnostics.app.run'),
          onPressed: running ? null : diagnostic,
        ),
        SettingsRow(
          label: t('settings.diagnostics.testAudio'),
          hint: playbackResult,
          control: settingsButton(
            context,
            playing
                ? t('settings.diagnostics.playing')
                : t('settings.diagnostics.playTest'),
            playing ? null : playTest,
          ),
        ),
      ],
    );
  }
}

/// Original `AboutSection`: app version, desktop release channel + updates,
/// connected host versions.
class _AboutSettings extends StatefulWidget {
  const _AboutSettings();
  @override
  State<_AboutSettings> createState() => _AboutSettingsState();
}

class _AboutSettingsState extends State<_AboutSettings> with _DesktopInvoke {
  JsonMap settings = {}, update = {};
  bool checking = false, installing = false;

  @override
  void initState() {
    super.initState();
    if (_desktop && DesktopHost.instance != null) {
      load();
      check(manual: false);
    }
  }

  Future<void> load() async {
    try {
      final result = jsonMap(await invoke('desktop_settings_get'));
      if (mounted) setState(() => settings = result);
    } catch (e) {
      if (mounted) setState(() => error = e);
    }
  }

  Future<void> check({bool manual = true}) async {
    setState(() => checking = true);
    try {
      final result = jsonMap(await invoke('check_app_update', {
        'intent': manual ? 'manual' : 'automatic',
      }));
      if (mounted) setState(() => update = result);
    } catch (e) {
      if (mounted && manual) setState(() => error = e);
    } finally {
      if (mounted) setState(() => checking = false);
    }
  }

  Future<void> install() async {
    String t(String key) => tr(context, key);
    if (!await confirmAction(
      context,
      title: t('settings.about.updates.installTitle'),
      message: t('settings.about.updates.installMessage'),
    )) {
      return;
    }
    setState(() => installing = true);
    try {
      final result = await invoke('install_app_update');
      if (mounted) {
        await showSettingsReport(
          context,
          t('settings.about.updates.label'),
          result,
        );
      }
    } catch (e) {
      if (mounted) setState(() => error = e);
    } finally {
      if (mounted) setState(() => installing = false);
    }
  }

  String _version(Object? version) {
    final text = version?.toString().trim() ?? '';
    if (text.isEmpty) return '—';
    return text.startsWith(RegExp('[vV]')) ? 'v${text.substring(1)}' : 'v$text';
  }

  @override
  Widget build(BuildContext context) {
    String t(String key, [Map<String, Object?> args = const {}]) =>
        tr(context, key, args: args);
    final store = PaseoScope.of(context);
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final hosts = _sortedHosts(store);
    final desktop = _desktop && DesktopHost.instance != null;
    final latest = update['latestVersion'];
    final ready = update['readyToInstall'] == true;
    final value = theme.textTheme.bodyMedium?.copyWith(color: colors.muted);
    final client = _appVersion.isEmpty ? null : _appVersion.replaceFirst(RegExp('^[vV]'), '');
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        SettingsSection(
          title: t('settings.about.title'),
          children: [
            SettingsRow(
              label: t('settings.about.appVersion'),
              hint: t('settings.about.thisDevice'),
              control: Text(_version(client), style: value),
            ),
            if (desktop) ...[
              SettingsRow(
                label: t('settings.about.releaseChannel.label'),
                hint: t('settings.about.releaseChannel.description'),
                control: PaseoSegmentedControl<String>(
                  size: PaseoControlSize.sm,
                  segments: [
                    PaseoSegment(
                      'stable',
                      t('settings.about.releaseChannel.stable'),
                    ),
                    PaseoSegment('beta', t('settings.about.releaseChannel.beta')),
                  ],
                  value: stringValue(settings['releaseChannel'], 'stable') == 'beta'
                      ? 'beta'
                      : 'stable',
                  onChanged: busy || settings.isEmpty
                      ? null
                      : (v) => run(() async {
                          await invoke('desktop_settings_update', {
                            'patch': {'releaseChannel': v},
                          });
                          await load();
                        }),
                ),
              ),
              SettingsRow(
                label: t('settings.about.updates.label'),
                hint: [
                  if (update['hasUpdate'] == true || ready)
                    t('settings.about.updates.updateTo',
                        {'version': _version(latest)})
                  else if (update.isNotEmpty)
                    _version(update['currentVersion'] ?? client),
                  if (ready)
                    t('settings.about.updates.readyToInstall',
                        {'version': _version(latest)}),
                ].join('\n'),
                error: error?.toString() ?? update['errorMessage']?.toString(),
                control: Wrap(
                  spacing: 8,
                  runSpacing: 8,
                  children: [
                    settingsButton(
                      context,
                      checking
                          ? t('settings.about.updates.checking')
                          : t('settings.about.updates.check'),
                      checking || installing ? null : check,
                    ),
                    if (update['hasUpdate'] == true || ready)
                      PaseoButton(
                        size: PaseoControlSize.sm,
                        variant: PaseoButtonVariant.primary,
                        onPressed: installing ? null : install,
                        label: installing
                            ? t('settings.about.updates.installing')
                            : latest == null
                            ? t('settings.about.updates.update')
                            : t('settings.about.updates.updateTo',
                                {'version': _version(latest)}),
                      ),
                  ],
                ),
              ),
              SettingsActionRow(
                label: 'Update diagnostics',
                actionLabel: t('settings.diagnostics.app.run'),
                onPressed: busy
                    ? null
                    : () => report('desktop_update_diagnostics',
                        'Update diagnostics'),
              ),
            ],
          ],
        ),
        if (hosts.isNotEmpty)
          SettingsSection(
            title: t('settings.about.connectedHosts'),
            children: [
              for (final host in hosts)
                ListenableBuilder(
                  listenable: host.client,
                  builder: (context, _) {
                    final connected =
                        host.client.status == ConnectionStatus.connected;
                    final hostVersion = host.client.serverInfo['version']
                        ?.toString()
                        .replaceFirst(RegExp('^[vV]'), '');
                    final mismatch = hostVersion != null &&
                        client != null &&
                        hostVersion != client;
                    return SettingsRow(
                      label: host.label,
                      hint: mismatch ? t('settings.about.versionDiffers') : null,
                      control: Text(
                        !connected
                            ? t('settings.about.offline')
                            : _version(hostVersion),
                        style: value?.copyWith(
                          color: mismatch ? colors.warning : null,
                        ),
                      ),
                    );
                  },
                ),
            ],
          ),
      ],
    );
  }
}
