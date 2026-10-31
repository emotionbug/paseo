import 'dart:async';

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:url_launcher/url_launcher.dart';

import '../../core/json.dart';
import '../../core/scope.dart';
import '../../platform/desktop/desktop_host.dart';
import '../../platform/desktop/desktop_integrations.dart';
import '../../ui/common.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import '../chat/composer/forge_auto_attach.dart' show forgeCloudHosts, normalizeGitHost, parseGitRemoteLocation;
import '../chat/composer/forge_icons.dart';
import 'git_actions.dart' show CheckoutGitState, forgePresentation, normalizeForge;

/// git/forges urlGrammar: path infixes and the line anchor of each forge with
/// web URLs; a forge without one has no "Open on" target.
typedef _UrlGrammar = ({String tree, String blob, String Function(int, int?) line});

String _githubLine(int start, int? end) => end != null && end > start ? '#L$start-L$end' : '#L$start';
String _gitlabLine(int start, int? end) => end != null && end > start ? '#L$start-$end' : '#L$start';
const _UrlGrammar _giteaFamily = (tree: '/src/branch/', blob: '/src/branch/', line: _githubLine);
const Map<String, _UrlGrammar> _urlGrammars = {
  'github': (tree: '/tree/', blob: '/blob/', line: _githubLine),
  'gitlab': (tree: '/-/tree/', blob: '/-/blob/', line: _gitlabLine),
  'gitea': _giteaFamily,
  'forgejo': _giteaFamily,
  'codeberg': _giteaFamily,
};

/// git/forge.ts forgeFromRemoteUrl: a remote on a forge's cloud host.
String? forgeFromRemoteUrl(String? remoteUrl) {
  final host = remoteUrl == null ? null : parseGitRemoteLocation(remoteUrl)?.host;
  if (host == null) return null;
  for (final entry in forgeCloudHosts.entries) {
    if (entry.value.map(normalizeGitHost).contains(host)) return entry.key;
  }
  return null;
}

String _encodePath(String path) => path.split('/').map(Uri.encodeComponent).join('/');

/// git/forge-url.ts normalizeBlobPath: a workspace-relative path without `.`
/// segments; null when it escapes the workspace.
String? _blobPath(String? path) {
  final trimmed = path?.trim().replaceAll(r'\', '/').replaceFirst(RegExp(r'^/+'), '');
  if (trimmed == null || trimmed.isEmpty) return null;
  final segments = <String>[];
  for (final segment in trimmed.split('/')) {
    if (segment.isEmpty || segment == '.') continue;
    if (segment == '..') {
      if (segments.isEmpty) return null;
      segments.removeLast();
      continue;
    }
    segments.add(segment);
  }
  return segments.isEmpty ? null : segments.join('/');
}

/// git/forge-url.ts buildForgeBranchTreeUrl / buildForgeBlobUrl: the web
/// page of [branch] (or of [path] on it) on the remote's own host, so
/// self-hosted instances link correctly; cloud SSH aliases use the web host.
String? forgeWebUrl(
  String forge, {
  required String? remoteUrl,
  required String? branch,
  String? path,
  int? lineStart,
  int? lineEnd,
}) {
  final grammar = _urlGrammars[forge];
  final location = remoteUrl == null ? null : parseGitRemoteLocation(remoteUrl);
  final name = branch?.trim() ?? '';
  if (grammar == null || location == null || name.isEmpty || name == 'HEAD') return null;
  if (location.path.split('/').contains('..')) return null;
  final cloudHosts = (forgeCloudHosts[forge] ?? const <String>[]).map(normalizeGitHost).toList();
  final cloud = cloudHosts.contains(location.host);
  var authority = cloud ? cloudHosts.first : location.host;
  // A self-hosted http(s) origin keeps its port: the web UI shares it.
  final uri = Uri.tryParse(remoteUrl!.trim());
  if (!cloud && uri != null && (uri.scheme == 'http' || uri.scheme == 'https') && uri.hasPort) {
    authority = '$authority:${uri.port}';
  }
  final base = 'https://$authority/${location.path}';
  if (path == null) return '$base${grammar.tree}${_encodePath(name)}';
  final file = _blobPath(path);
  if (file == null) return null;
  var url = '$base${grammar.blob}${_encodePath(name)}/${_encodePath(file)}';
  if (lineStart != null && lineStart > 0) url += grammar.line(lineStart, lineEnd);
  return url;
}

/// One entry of the Open-in menu (open-in-editor/planner.ts).
class _OpenTarget {
  const _OpenTarget({required this.id, required this.label, required this.icon, required this.open});
  final String id, label;
  final Widget icon;
  final Future<void> Function() open;
}

/// workspace/open-in-editor/button.tsx WorkspaceOpenInEditorButton (desktop
/// header, `hideLabels`): a bordered split button whose primary half opens the
/// workspace — or the active file — in the preferred target, and whose caret
/// picks that preference. Local desktop editors are offered only for the
/// desktop's own daemon; the forge page needs a git checkout on a forge.
class WorkspaceOpenInButton extends StatefulWidget {
  const WorkspaceOpenInButton({
    super.key,
    required this.serverId,
    required this.cwd,
    this.activeFilePath,
  });
  final String serverId, cwd;

  /// The focused file tab's workspace-relative or absolute path.
  final String? activeFilePath;

  @override
  State<WorkspaceOpenInButton> createState() => _WorkspaceOpenInButtonState();
}

class _WorkspaceOpenInButtonState extends State<WorkspaceOpenInButton> {
  // use-is-local-daemon.ts: the desktop daemon's server id, asked once.
  static Future<String?>? _localServerId;
  static Future<List<DesktopEditorTarget>>? _editors;

  CheckoutGitState? _state;
  String? _localId;
  List<DesktopEditorTarget> _editorTargets = const [];
  bool _hovered = false, _caretHovered = false, _open = false, _opening = false;

  static Future<String?> _loadLocalServerId() async {
    final desktop = DesktopHost.instance;
    if (desktop == null) return null;
    final status = jsonMap(await desktop.invoke('desktop_daemon_status'));
    final id = status.text('serverId').trim();
    return id.isEmpty ? null : id;
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    _bind();
  }

  @override
  void didUpdateWidget(WorkspaceOpenInButton old) {
    super.didUpdateWidget(old);
    if (old.serverId != widget.serverId || old.cwd != widget.cwd) {
      _unbind();
      _bind();
    }
  }

  void _bind() {
    if (_state != null) return;
    final store = PaseoScope.read(context);
    _state = CheckoutGitState.acquire(store.clientFor(widget.serverId), widget.serverId, widget.cwd)
      ..addListener(_changed);
    final local = _localServerId ??= _loadLocalServerId().then((id) {
      // A daemon still starting has no id yet; ask again next time.
      if (id == null) _localServerId = null;
      return id;
    }, onError: (Object _) {
      _localServerId = null;
      return null;
    });
    unawaited(local.then((id) async {
      if (!mounted) return;
      _localId = id;
      if (id == widget.serverId) {
        final editors = await (_editors ??= DesktopIntegrations.instance.listEditorTargets());
        if (mounted) _editorTargets = editors;
      }
      _changed();
    }));
  }

  void _unbind() {
    _state?.removeListener(_changed);
    _state?.release();
    _state = null;
  }

  void _changed() {
    if (mounted) setState(() {});
  }

  @override
  void dispose() {
    _unbind();
    super.dispose();
  }

  String? get _relativeFile {
    final path = widget.activeFilePath;
    if (path == null || path.isEmpty) return null;
    final root = widget.cwd.replaceFirst(RegExp(r'/+$'), '');
    if (path.startsWith('$root/')) return path.substring(root.length + 1);
    return path.startsWith('/') ? null : path;
  }

  /// open-in-editor/planner.ts planWorkspaceOpenTargets.
  List<_OpenTarget> _targets(BuildContext context) {
    final colors = PaseoColors.of(context);
    final relative = _relativeFile;
    final targets = <_OpenTarget>[
      if (_localId == widget.serverId)
        for (final editor in _editorTargets)
          _OpenTarget(
            id: editor.id,
            label: editor.label,
            icon: Icon(
              editor.kind == 'file-manager' ? LucideIcons.folder300 : LucideIcons.codeXml300,
              size: 16,
              color: colors.muted,
            ),
            open: () => DesktopIntegrations.instance.openEditor(
              editorId: editor.id,
              workspacePath: widget.cwd,
              filePath: relative == null ? null : '${widget.cwd.replaceFirst(RegExp(r'/+$'), '')}/$relative',
            ),
          ),
    ];
    final status = _state?.status;
    if (status?.flag('isGit') == true) {
      final payloadForge = _state?.prPayload?['forge'];
      final forge = _state?.prPayload != null
          ? normalizeForge(payloadForge is String ? payloadForge : null)
          : forgeFromRemoteUrl(status!.text('remoteUrl'));
      if (forge != null) {
        final url = forgeWebUrl(
          forge,
          remoteUrl: status!.text('remoteUrl'),
          branch: status.text('currentBranch'),
          path: relative,
        );
        if (url != null) {
          targets.add(_OpenTarget(
            id: forge,
            label: forgePresentation(forge).brandLabel,
            icon: ForgeBrandIcon(forge: forge, size: 16, color: colors.muted),
            open: () async {
              if (!await launchUrl(Uri.parse(url), mode: LaunchMode.externalApplication)) {
                throw StateError('Could not open $url');
              }
            },
          ));
        }
      }
    }
    return targets;
  }

  Future<void> _run(_OpenTarget target) async {
    setState(() => _opening = true);
    try {
      await target.open();
    } catch (error) {
      if (mounted) showError(context, error.toString().isEmpty ? tr(context, 'workspace.git.openInEditor.failedOpen') : error);
    } finally {
      if (mounted) setState(() => _opening = false);
    }
  }

  Future<void> _choose(BuildContext anchor, List<_OpenTarget> targets, String preferred) async {
    final store = PaseoScope.read(context);
    setState(() => _open = true);
    try {
      final id = await showPaseoMenu<String>(
        context,
        anchor: paseoAnchorRect(anchor),
        alignEnd: true,
        title: tr(context, 'workspace.git.openInEditor.chooseEditor'),
        entries: [
          for (final target in targets)
            PaseoMenuEntry.item(
              value: target.id,
              label: target.label,
              leading: target.icon,
              selected: target.id == preferred,
            ),
        ],
      );
      // The menu only chooses the preferred target; the primary half opens it.
      if (id != null && id != preferred) {
        store.preferences['preferredEditor'] = id;
        await store.savePreferences();
      }
    } catch (error) {
      if (mounted) showError(context, error);
    } finally {
      if (mounted) setState(() => _open = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final targets = _targets(context);
    if (targets.isEmpty) return const SizedBox.shrink();
    final stored = PaseoScope.of(context).preferences['preferredEditor'];
    // use-preferred-editor.ts resolvePreferredEditorId.
    final primary = targets.where((target) => target.id == stored).firstOrNull ?? targets.first;
    final colors = PaseoColors.of(context);
    final fileName = _relativeFile?.split('/').last;
    const size = 28.0;
    return Container(
      height: size,
      clipBehavior: Clip.antiAlias,
      decoration: BoxDecoration(
        border: Border.all(color: colors.borderAccent),
        borderRadius: BorderRadius.circular(6),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Tooltip(
            message: fileName == null
                ? tr(context, 'workspace.git.openInEditor.openIn', args: {'target': primary.label})
                : tr(context, 'workspace.git.openInEditor.openFileIn',
                    args: {'fileName': fileName, 'target': primary.label}),
            child: Semantics(
              button: true,
              enabled: !_opening,
              excludeSemantics: true,
              label: fileName == null
                  ? tr(context, 'workspace.git.openInEditor.openIn', args: {'target': primary.label})
                  : tr(context, 'workspace.git.openInEditor.openFileIn',
                      args: {'fileName': fileName, 'target': primary.label}),
              child: MouseRegion(
                cursor: _opening ? MouseCursor.defer : SystemMouseCursors.click,
                onEnter: (_) => setState(() => _hovered = true),
                onExit: (_) => setState(() => _hovered = false),
                child: GestureDetector(
                  behavior: HitTestBehavior.opaque,
                  onTap: _opening ? null : () => unawaited(_run(primary)),
                  child: Opacity(
                    opacity: _opening ? .6 : 1,
                    child: Container(
                      width: size,
                      color: _hovered && !_opening ? colors.surface2 : null,
                      alignment: Alignment.center,
                      child: _opening
                          ? SizedBox.square(
                              dimension: 14,
                              child: CircularProgressIndicator(
                                strokeWidth: 1.5,
                                color: Theme.of(context).colorScheme.onSurface,
                              ),
                            )
                          : primary.icon,
                    ),
                  ),
                ),
              ),
            ),
          ),
          if (targets.length > 1)
            Builder(
              builder: (anchor) => Semantics(
                button: true,
                excludeSemantics: true,
                label: tr(context, 'workspace.git.openInEditor.chooseEditor'),
                child: MouseRegion(
                  cursor: SystemMouseCursors.click,
                  onEnter: (_) => setState(() => _caretHovered = true),
                  onExit: (_) => setState(() => _caretHovered = false),
                  child: GestureDetector(
                    behavior: HitTestBehavior.opaque,
                    onTap: () => unawaited(_choose(anchor, targets, primary.id)),
                    child: Container(
                      width: size,
                      decoration: BoxDecoration(
                        color: _caretHovered || _open ? colors.surface2 : null,
                        border: Border(left: BorderSide(color: colors.borderAccent)),
                      ),
                      alignment: Alignment.center,
                      child: Icon(LucideIcons.chevronDown300, size: 16, color: colors.extraMuted),
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
