import '../../ui/localization.dart';

import 'dart:convert';
import 'dart:io' show HttpClient, HttpClientResponse, HttpHeaders;
import 'dart:typed_data';

import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:url_launcher/url_launcher.dart';

import '../../core/client.dart';
import '../../core/json.dart';
import '../../core/scope.dart';
import '../../core/target.dart';
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/theme.dart';
import '../settings/settings_forms.dart';
import 'workspace_rpc.dart';

/// Original `createProjectViewKey`: a project's shared `projectKey` when the
/// host reports one, otherwise its JSON `[serverId, projectId]` placement.
String projectViewKey(String serverId, JsonMap project) {
  final key = project.text('projectKey');
  return key.isNotEmpty
      ? key
      : jsonEncode([serverId, project.text('projectId')]);
}

/// IDENTITY_COLORS fill tones (styles/identity-colors.ts). Order is
/// load-bearing: the view-key hash indexes into it.
const _identityFills = [
  Color(0xff7a6aa8), // violet
  Color(0xff3d7ea6), // sky
  Color(0xff388068), // emerald
  Color(0xffa4673a), // orange
  Color(0xffb05c80), // pink
  Color(0xff6a70b8), // indigo
  Color(0xff368080), // teal
  Color(0xffb06260), // red
  Color(0xff8f7838), // amber
  Color(0xff5179b0), // blue
];

/// Original `deriveIdentityColorName`: `hash * 31 + charCode` per code point,
/// kept unsigned 32-bit.
Color _identityFill(String key) {
  var hash = 0;
  for (final rune in key.runes) {
    hash = (hash * 31 + String.fromCharCode(rune).codeUnitAt(0)) & 0xffffffff;
  }
  return _identityFills[hash % _identityFills.length];
}

/// Original `ProjectIconView` fallback: the identity-coloured square (radius
/// a quarter of the box) carrying the project's white initial. 16px icons use
/// the 10px semibold initial; larger ones the base medium one.
class ProjectIconSquare extends StatelessWidget {
  const ProjectIconSquare({
    super.key,
    required this.name,
    required this.viewKey,
    required this.size,
  });
  final String name;
  final String viewKey;
  final double size;

  @override
  Widget build(BuildContext context) {
    final type = PaseoTypography.of(context);
    final trimmed = name.trim();
    final label =
        trimmed.split('/').where((s) => s.isNotEmpty).lastOrNull ?? trimmed;
    final initial = label.isEmpty
        ? '?'
        : String.fromCharCode(label.runes.first).toUpperCase();
    final small = size <= 16;
    return Container(
      width: size,
      height: size,
      alignment: Alignment.center,
      decoration: BoxDecoration(
        color: _identityFill(viewKey),
        borderRadius: BorderRadius.circular((size * .25).roundToDouble()),
      ),
      child: Text(
        initial,
        maxLines: 1,
        textHeightBehavior: const TextHeightBehavior(
          applyHeightToFirstAscent: false,
          applyHeightToLastDescent: false,
        ),
        style: TextStyle(
          color: Colors.white,
          fontSize: small ? 10 : type.uiSize(14),
          fontWeight: small ? FontWeight.w600 : FontWeight.w500,
          height: 1,
        ),
      ),
    );
  }
}

const _worktreeDocsUrl = 'https://paseo.sh/docs/worktrees';
const _metadataKeys = ['branchName', 'commitMessage', 'pullRequest'];

class ProjectSettingsPanel extends StatefulWidget {
  const ProjectSettingsPanel({super.key, required this.target});
  final PanelTarget target;
  @override
  State<ProjectSettingsPanel> createState() => _ProjectSettingsPanelState();
}

class _ProjectSettingsPanelState extends State<ProjectSettingsPanel> {
  PaseoClient? _client;
  JsonMap? _config;
  Object? _revision;

  /// Transport failure while reading (original `read-transport-callout`).
  Object? _readError;

  /// Host-reported read failure code (`invalid_project_config`, ...).
  String? _readErrorCode;

  /// Host-reported write failure code (`stale_project_config`, `write_failed`).
  String? _writeErrorCode;
  Object? _error;
  JsonMap? _recovery;
  bool _loading = false;
  bool _saving = false;
  bool _busy = false;
  bool _dirty = false;
  bool _uncommitted = false;
  VoidCallback? _removeGuard;
  final Map<String, String> _loadedKinds = {};
  String get _root => widget.target.data.text('repoRoot', widget.target.cwd);

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    if (_client != null) return;
    final store = PaseoScope.of(context);
    _client = store.clientFor(widget.target.serverId);
    _client!.addListener(_connectionChanged);
    _removeGuard = store.registerCloseGuard(widget.target.key, _mayDiscard);
    _read();
  }

  /// Original `isHostGone`: an offline or errored host replaces the form
  /// with the no-editable-target notice.
  void _connectionChanged() {
    if (!mounted) return;
    setState(() {});
    // A config that never loaded is read again once the host is back.
    if (_client!.status == ConnectionStatus.connected &&
        _config == null &&
        !_loading) {
      _read();
    }
  }

  bool get _hostGone {
    final status = _client?.status;
    return status == ConnectionStatus.disconnected ||
        status == ConnectionStatus.error;
  }

  Future<bool> _mayDiscard() async =>
      !_dirty ||
      await confirmAction(
        context,
        title: 'Discard unsaved project settings?',
        message: 'Your edits have not been saved to the host.',
        confirmLabel: 'Discard',
        destructive: true,
      );

  Future<void> _reload() async {
    if (!await _mayDiscard() || !mounted) return;
    await _read();
  }

  Future<void> _read() async {
    setState(() {
      _loading = true;
      _readError = null;
      _readErrorCode = null;
      _writeErrorCode = null;
    });
    try {
      final response = await _client!.request('read_project_config_request', {
        'repoRoot': _root,
      }, responseType: 'read_project_config_response');
      if (!mounted) return;
      final code = response.object('error').text('code');
      if (code.isNotEmpty || response['ok'] == false) {
        setState(() => _readErrorCode = code.isEmpty ? 'read_failed' : code);
        return;
      }
      setState(() {
        _config = response.object('config');
        _revision = response['revision'];
        _dirty = false;
        _loadedKinds.clear();
        _uncommitted = response.flag('hasUncommittedWorktreeSetupChanges');
      });
    } catch (error) {
      if (mounted) setState(() => _readError = error);
    } finally {
      if (mounted) setState(() => _loading = false);
    }
  }

  Future<void> _action(Future<void> Function() action) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await action();
    } catch (error) {
      if (mounted) setState(() => _error = error);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  void _change(VoidCallback change) => setState(() {
    change();
    _dirty = true;
  });

  JsonMap get _worktree =>
      Map<String, dynamic>.from(_config!.object('worktree'));
  JsonMap get _scripts => Map<String, dynamic>.from(_config!.object('scripts'));

  /// Original `project-config-form.ts`: lifecycle hooks edit as one text area;
  /// a string stays a string, an array stays an array, missing picks by count.
  String _lifecycleText(String field) {
    final value = _worktree[field];
    return value is String
        ? value
        : jsonList(value).whereType<String>().join('\n');
  }

  void _setLifecycleText(String field, String text) => _change(() {
    final lines = text
        .split('\n')
        .where((line) => line.trim().isNotEmpty)
        .toList();
    final original = _config!.object('worktree')[field];
    final kind = _loadedKinds[field] ??= original is String
        ? 'string'
        : original is List
        ? 'array'
        : 'missing';
    final worktree = _worktree;
    if (lines.isEmpty) {
      worktree.remove(field);
    } else {
      worktree[field] = switch (kind) {
        'string' => lines.join('\n'),
        'array' => lines,
        _ => lines.length == 1 ? lines.first : lines,
      };
    }
    _config!['worktree'] = worktree;
  });

  Future<void> _save() async {
    if (_writeErrorCode == 'stale_project_config' || _saving) return;
    setState(() {
      _saving = true;
      _error = null;
    });
    try {
      final response = await _client!.request('write_project_config_request', {
        'repoRoot': _root,
        'config': _config,
        'expectedRevision': _revision,
      }, responseType: 'write_project_config_response');
      if (!mounted) return;
      final code = response.object('error').text('code');
      if (code == 'stale_project_config' || code == 'write_failed') {
        setState(() => _writeErrorCode = code);
        return;
      }
      requireWorkspaceSuccess(response);
      setState(() {
        _writeErrorCode = null;
        _config = response.object('config');
        _revision = response['revision'];
        _dirty = false;
        _loadedKinds.clear();
        if (response.containsKey('hasUncommittedWorktreeSetupChanges')) {
          _uncommitted = response.flag('hasUncommittedWorktreeSetupChanges');
        }
      });
      showToast(
        context,
        tr(context, 'settings.project.actions.saved'),
        variant: ToastVariant.success,
      );
      await PaseoScope.of(context).refreshHost(widget.target.serverId);
    } catch (error) {
      if (mounted) setState(() => _error = error);
    } finally {
      if (mounted) setState(() => _saving = false);
    }
  }

  /// Original `ScriptEditModal`: name and command inputs (surface2, 1px
  /// border, md radius, 8/12 insets) under muted sm labels, errors once a
  /// field is touched, the Run-as-service switch row, ghost Cancel + Save.
  Future<void> _editScript(String? oldName) async {
    String t(String key, [Map<String, Object?> args = const {}]) =>
        tr(context, key, args: args);
    final scripts = _scripts;
    final existing = oldName == null
        ? <String, dynamic>{}
        : jsonMap(scripts[oldName]);
    final name = TextEditingController(text: oldName ?? '');
    final command = TextEditingController(text: existing.text('command'));
    final port = TextEditingController(
      text: existing['port']?.toString() ?? '',
    );
    var service = existing.text('type') == 'service';
    var nameTouched = false, commandTouched = false;
    final result = await showAdaptiveSheet<JsonMap>(
      context: context,
      maxWidth: 560,
      builder: (context) => StatefulBuilder(
        builder: (context, update) {
          final theme = Theme.of(context);
          final colors = PaseoColors.of(context);
          final type = PaseoTypography.of(context);
          final base = theme.textTheme.bodyMedium!.copyWith(
            fontSize: type.uiSize(14),
          );
          final small = base.copyWith(
            fontSize: type.uiSize(12),
            color: colors.muted,
          );
          final key = name.text.trim();
          final nameError = key.isEmpty
              ? t('settings.project.scripts.nameRequired')
              : key != oldName && scripts.containsKey(key)
              ? 'A script named "$key" already exists'
              : null;
          final commandError = command.text.trim().isEmpty
              ? t('settings.project.scripts.commandRequired')
              : null;
          InputDecoration decoration(String hint) {
            OutlineInputBorder border(Color color) => OutlineInputBorder(
              borderRadius: BorderRadius.circular(6),
              borderSide: BorderSide(color: color),
            );
            return InputDecoration(
              isDense: true,
              filled: true,
              fillColor: colors.surface2,
              hoverColor: Colors.transparent,
              hintText: hint,
              hintStyle: TextStyle(color: colors.muted),
              contentPadding: const EdgeInsets.symmetric(
                horizontal: 12,
                vertical: 8,
              ),
              border: border(theme.dividerColor),
              enabledBorder: border(theme.dividerColor),
              focusedBorder: border(colors.borderAccent),
            );
          }

          Widget section(String label, Widget child, String? error) => Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            spacing: 8,
            children: [
              Text(label, style: small),
              child,
              if (error != null)
                Text(error, style: small.copyWith(color: paseoFieldErrorColor)),
            ],
          );
          return SheetContents(
            // Original: the live name decides — empty reads New script.
            title: key.isEmpty
                ? t('settings.project.scripts.newScript')
                : t('settings.project.scripts.editScript', {'name': key}),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              spacing: 16,
              children: [
                section(
                  t('settings.project.scripts.name'),
                  Focus(
                    onFocusChange: (focused) {
                      if (!focused) update(() => nameTouched = true);
                    },
                    child: TextField(
                      controller: name,
                      autofocus: oldName == null,
                      style: base.copyWith(color: theme.colorScheme.onSurface),
                      decoration: decoration('dev'),
                      onChanged: (_) => update(() {}),
                    ),
                  ),
                  nameTouched ? nameError : null,
                ),
                section(
                  t('settings.project.scripts.command'),
                  Focus(
                    onFocusChange: (focused) {
                      if (!focused) update(() => commandTouched = true);
                    },
                    child: ConstrainedBox(
                      constraints: const BoxConstraints(minHeight: 100),
                      child: TextField(
                        controller: command,
                        minLines: 4,
                        maxLines: 10,
                        style: base.copyWith(
                          color: theme.colorScheme.onSurface,
                        ),
                        decoration: decoration('npm run dev'),
                        onChanged: (_) => update(() {}),
                      ),
                    ),
                  ),
                  commandTouched ? commandError : null,
                ),
                section(
                  'Port',
                  TextField(
                    controller: port,
                    style: base.copyWith(color: theme.colorScheme.onSurface),
                    decoration: decoration('3000 or \$PORT'),
                  ),
                  null,
                ),
                Row(
                  spacing: 12,
                  children: [
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        spacing: 4,
                        children: [
                          Text(
                            t('settings.project.scripts.runAsService'),
                            style: base.copyWith(fontWeight: FontWeight.w500),
                          ),
                          Text(
                            t('settings.project.scripts.serviceHint'),
                            style: small,
                          ),
                        ],
                      ),
                    ),
                    PaseoSwitch(
                      value: service,
                      semanticLabel: t('settings.project.scripts.runAsService'),
                      onChanged: (value) => update(() => service = value),
                    ),
                  ],
                ),
                Padding(
                  padding: const EdgeInsets.only(top: 8),
                  child: Row(
                    mainAxisAlignment: MainAxisAlignment.end,
                    spacing: 8,
                    children: [
                      PaseoButton(
                        label: t('settings.project.actions.cancel'),
                        variant: PaseoButtonVariant.ghost,
                        onPressed: () => Navigator.pop(context),
                      ),
                      PaseoButton(
                        label: t('settings.project.actions.save'),
                        variant: PaseoButtonVariant.primary,
                        onPressed: () {
                          if (nameError != null || commandError != null) {
                            update(() {
                              nameTouched = true;
                              commandTouched = true;
                            });
                            return;
                          }
                          final entry = <String, dynamic>{
                            ...existing,
                            'command': command.text,
                          };
                          if (service) {
                            entry['type'] = 'service';
                          } else if (entry['type'] == 'service') {
                            entry.remove('type');
                          }
                          if (port.text.trim().isEmpty) {
                            entry.remove('port');
                          } else {
                            entry['port'] =
                                int.tryParse(port.text.trim()) ??
                                port.text.trim();
                          }
                          Navigator.pop(context, {'name': key, 'entry': entry});
                        },
                      ),
                    ],
                  ),
                ),
              ],
            ),
          );
        },
      ),
    );
    name.dispose();
    command.dispose();
    port.dispose();
    if (result == null || !mounted) return;
    _change(() {
      final next = <String, dynamic>{};
      // Keep the edited script in its original position.
      for (final entry in scripts.entries) {
        if (entry.key == oldName) {
          next[result.text('name')] = result.object('entry');
        } else if (entry.key != result.text('name')) {
          next[entry.key] = entry.value;
        }
      }
      if (oldName == null) next[result.text('name')] = result.object('entry');
      _config!['scripts'] = next;
    });
  }

  Future<void> _removeScript(String scriptName) async {
    String t(String key, [Map<String, Object?> args = const {}]) =>
        tr(context, key, args: args);
    if (!await confirmAction(
          context,
          title: t('settings.project.scripts.removeTitle'),
          message: t('settings.project.scripts.removeMessage', {
            'name': scriptName.isEmpty
                ? t('settings.project.scripts.removeFallbackName')
                : scriptName,
          }),
          confirmLabel: t('settings.project.scripts.actions.remove'),
          destructive: true,
        ) ||
        !mounted) {
      return;
    }
    _change(() {
      final next = _scripts;
      next.remove(scriptName);
      _config!['scripts'] = next;
    });
  }

  Future<void> _portAllocation() async {
    final ports = _worktree.object('servicePorts');
    final range = await promptText(
      context,
      title: 'Service port range (for example 3000-3999)',
      initialValue: ports.text('range'),
    );
    if (range == null || !mounted) return;
    final script = await promptText(
      context,
      title: 'Port allocation command (optional)',
      initialValue: ports.text('portScript'),
      multiline: true,
    );
    if (script == null || !mounted) return;
    if (range.trim().isNotEmpty) {
      final parts = range.trim().split('-');
      final start = parts.length == 2 ? int.tryParse(parts[0]) : null;
      final end = parts.length == 2 ? int.tryParse(parts[1]) : null;
      if (start == null ||
          end == null ||
          start < 1 ||
          end > 65535 ||
          start > end) {
        showError(
          context,
          'Enter an inclusive port range between 1 and 65535.',
        );
        return;
      }
    }
    _change(() {
      final worktree = _worktree;
      if (range.trim().isEmpty && script.trim().isEmpty) {
        worktree.remove('servicePorts');
      } else {
        worktree['servicePorts'] = {
          if (range.trim().isNotEmpty) 'range': range.trim(),
          if (script.trim().isNotEmpty) 'portScript': script,
        };
      }
      _config!['worktree'] = worktree;
    });
  }

  JsonMap? get _project {
    final host = PaseoScope.of(context).hostFor(widget.target.serverId);
    return host?.projects
        .where(
          (project) =>
              project.text('projectRootPath', project.text('repoRoot')) ==
              _root,
        )
        .firstOrNull;
  }

  /// Original `ProjectEditSheet` (440px desktop): the custom name field and
  /// the icon field (40px preview, outline Choose image, ghost Use automatic),
  /// Cancel / Save changes footer buttons at least 112px wide.
  Future<void> _editProject(JsonMap project, String displayName) async {
    final saved = await showAdaptiveSheet<bool>(
      context: context,
      maxWidth: 440,
      builder: (_) => _ProjectEditSheet(
        client: _client!,
        project: project,
        displayName: displayName,
        viewKey: projectViewKey(widget.target.serverId, project),
      ),
    );
    if (saved != true || !mounted) return;
    showToast(
      context,
      tr(context, 'settings.project.edit.savedToast'),
      variant: ToastVariant.success,
    );
    await PaseoScope.of(context).refreshHost(widget.target.serverId);
  }

  @override
  void dispose() {
    _client?.removeListener(_connectionChanged);
    _removeGuard?.call();
    super.dispose();
  }

  /// Original `SettingsGroup`: base normal-weight title with its info tip, an
  /// optional trailing control, 16px under the header and 32px under the group.
  Widget _group(
    String title,
    String info,
    List<Widget> children, {
    Widget? trailing,
  }) {
    final theme = Theme.of(context);
    final type = PaseoTypography.of(context);
    final muted = PaseoColors.of(context).muted;
    return Padding(
      padding: const EdgeInsets.only(bottom: 32),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Padding(
            padding: const EdgeInsets.only(bottom: 16),
            child: Row(
              spacing: 8,
              children: [
                Flexible(
                  child: Text(
                    title,
                    style: theme.textTheme.bodyMedium?.copyWith(
                      fontSize: type.uiSize(14),
                      fontWeight: FontWeight.normal,
                      color: theme.colorScheme.onSurface,
                    ),
                  ),
                ),
                Tooltip(
                  message: info,
                  triggerMode: TooltipTriggerMode.tap,
                  waitDuration: Duration.zero,
                  child: Icon(LucideIcons.info300, size: 14, color: muted),
                ),
                const Spacer(),
                ?trailing,
              ],
            ),
          ),
          ...children,
        ],
      ),
    );
  }

  /// Original `SettingsTextAreaCard`: the settings card holding a borderless
  /// base-size input, 12/16 insets, at least 96px tall, muted placeholder.
  Widget _textArea({
    required String keyId,
    required String value,
    required String placeholder,
    required String semantics,
    required ValueChanged<String> onChanged,
  }) {
    final type = PaseoTypography.of(context);
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    return DecoratedBox(
      decoration: BoxDecoration(
        color: colors.surface1,
        border: Border.all(color: theme.dividerColor),
        borderRadius: BorderRadius.circular(8),
      ),
      child: ConstrainedBox(
        constraints: const BoxConstraints(minHeight: 96),
        child: Semantics(
          label: semantics,
          child: TextFormField(
            key: ValueKey('$keyId:$_revision'),
            initialValue: value,
            enabled: !_saving,
            minLines: 3,
            maxLines: null,
            style: theme.textTheme.bodyMedium?.copyWith(
              fontSize: type.uiSize(14),
              height: 1.4,
              color: theme.colorScheme.onSurface,
            ),
            decoration: InputDecoration(
              hintText: placeholder,
              hintStyle: TextStyle(color: colors.muted),
              isDense: true,
              filled: false,
              border: InputBorder.none,
              enabledBorder: InputBorder.none,
              focusedBorder: InputBorder.none,
              disabledBorder: InputBorder.none,
              contentPadding: const EdgeInsets.symmetric(
                horizontal: 16,
                vertical: 12,
              ),
            ),
            onChanged: onChanged,
          ),
        ),
      ),
    );
  }

  /// Original `ExternalLink`: muted sm label + 12px arrow, 4px gap; the docs
  /// tooltip shows on desktop only.
  Widget _docsLink() {
    final t = tr(context, 'settings.project.worktree.docs');
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final link = Semantics(
      link: true,
      label: t,
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: () => launchUrl(
            Uri.parse(_worktreeDocsUrl),
            mode: LaunchMode.externalApplication,
          ),
          child: Padding(
            padding: const EdgeInsets.all(4),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              spacing: 4,
              children: [
                Text(
                  t,
                  style: TextStyle(
                    color: colors.muted,
                    fontSize: type.uiSize(12),
                  ),
                ),
                Icon(LucideIcons.arrowUpRight300, size: 12, color: colors.muted),
              ],
            ),
          ),
        ),
      ),
    );
    if (isCompactLayout(context)) return link;
    return Tooltip(
      message: tr(context, 'settings.project.worktree.docsTooltip'),
      waitDuration: Duration.zero,
      child: link,
    );
  }

  /// Original pressable glyph (`hitSlop` 8, 4px padding, 14px muted icon).
  Widget _glyphButton({
    required IconData icon,
    required String label,
    required void Function(BuildContext anchor)? onPressed,
  }) => Builder(
    builder: (anchor) => Semantics(
      button: true,
      label: label,
      child: Tooltip(
        message: label,
        child: MouseRegion(
          cursor: onPressed == null
              ? SystemMouseCursors.basic
              : SystemMouseCursors.click,
          child: GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTap: onPressed == null ? null : () => onPressed(anchor),
            child: Padding(
              padding: const EdgeInsets.all(4),
              child: Icon(
                icon,
                size: 14,
                color: PaseoColors.of(context).muted,
              ),
            ),
          ),
        ),
      ),
    ),
  );

  Widget _section(
    String title,
    List<Widget> children, {
    Widget? trailing,
    bool flush = false,
  }) => SettingsSection(
    title: title,
    card: false,
    flush: flush,
    trailing: trailing,
    children: children,
  );

  /// Original `ScriptRow`: 16px insets, 1px top rule after the first; the
  /// pressable name (base) and one-line sm hint 4px apart, a 4px-padded kebab
  /// opening a 160px Edit / Remove menu.
  Widget _scriptRow(String key, JsonMap script) {
    String t(String k, [Map<String, Object?> args = const {}]) =>
        tr(context, k, args: args);
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final port = script['port']?.toString() ?? '';
    final hint = [
      if (script.text('type').isNotEmpty) script.text('type'),
      if (port.isNotEmpty) t('settings.project.scripts.port', {'port': port}),
      if (script.text('command').isNotEmpty)
        script.text('command').split('\n').first,
    ].join(' · ');
    return Padding(
      padding: const EdgeInsets.all(16),
      child: Row(
        spacing: 8,
        children: [
          Expanded(
            child: MouseRegion(
              cursor: SystemMouseCursors.click,
              child: GestureDetector(
                behavior: HitTestBehavior.opaque,
                onTap: _saving ? null : () => _editScript(key),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  spacing: 4,
                  children: [
                    Text(
                      key.isEmpty ? t('settings.project.scripts.untitled') : key,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: theme.textTheme.bodyMedium?.copyWith(
                        fontSize: type.uiSize(14),
                        color: theme.colorScheme.onSurface,
                      ),
                    ),
                    if (hint.isNotEmpty)
                      Text(
                        hint,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: theme.textTheme.bodySmall?.copyWith(
                          fontSize: type.uiSize(12),
                          color: colors.muted,
                        ),
                      ),
                  ],
                ),
              ),
            ),
          ),
          _glyphButton(
            icon: LucideIcons.ellipsisVertical300,
            label: t('settings.project.scripts.menuAccessibility'),
            onPressed: _saving
                ? null
                : (anchor) async {
                    final action = await showPaseoMenu<String>(
                      context,
                      anchor: paseoAnchorRect(anchor),
                      width: 160,
                      alignEnd: true,
                      entries: [
                        PaseoMenuEntry.item(
                          value: 'edit',
                          label: t('settings.project.scripts.actions.edit'),
                        ),
                        PaseoMenuEntry.item(
                          value: 'remove',
                          label: t('settings.project.scripts.actions.remove'),
                          destructive: true,
                        ),
                      ],
                    );
                    if (!mounted) return;
                    if (action == 'edit') await _editScript(key);
                    if (action == 'remove') await _removeScript(key);
                  },
          ),
        ],
      ),
    );
  }

  Widget _readFailure() {
    String t(String key) => tr(context, key);
    final (title, description) = switch (_readErrorCode) {
      'invalid_project_config' => (
        t('settings.project.readFailures.invalidTitle'),
        t('settings.project.readFailures.invalidDescription'),
      ),
      'project_not_found' => (
        t('settings.project.readFailures.missingTitle'),
        t('settings.project.readFailures.missingSingleHost'),
      ),
      null => (
        t('settings.project.readFailures.transportTitle'),
        _readError == null
            ? t('settings.project.readFailures.transportFallback')
            : errorMessage(_readError!),
      ),
      _ => (
        t('settings.project.readFailures.failedTitle'),
        t('settings.project.readFailures.failedDescription'),
      ),
    };
    return Padding(
      padding: const EdgeInsets.only(top: 8),
      child: WorkspaceAlert(
        title: title,
        description: description,
        large: true,
        actions: [
          PaseoButton(
            label: t('settings.project.actions.reload'),
            variant: PaseoButtonVariant.outline,
            size: PaseoControlSize.sm,
            onPressed: _loading ? null : _reload,
          ),
        ],
      ),
    );
  }

  Widget _form() {
    String t(String key, [Map<String, Object?> args = const {}]) =>
        tr(context, key, args: args);
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final scripts = _scripts;
    final stale = _writeErrorCode == 'stale_project_config';
    final writeFailed = _writeErrorCode == 'write_failed';
    final invalidScripts = scripts.entries.any(
      (entry) =>
          entry.key.trim().isEmpty ||
          jsonMap(entry.value).text('command').trim().isEmpty,
    );
    final reloadButton = PaseoButton(
      label: t('settings.project.actions.reload'),
      variant: PaseoButtonVariant.outline,
      size: PaseoControlSize.sm,
      onPressed: _loading ? null : _reload,
    );
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _group(
          t('settings.project.worktree.title'),
          t('settings.project.worktree.info'),
          [
            _section(
              t('settings.project.worktree.setup'),
              trailing: _docsLink(),
              [
                if (_uncommitted)
                  WorkspaceAlert(
                    warning: true,
                    title: t('settings.project.worktree.uncommittedTitle'),
                    description: t(
                      'settings.project.worktree.uncommittedDescription',
                    ),
                  ),
                _textArea(
                  keyId: 'setup',
                  value: _lifecycleText('setup'),
                  placeholder: 'npm install',
                  semantics: t('settings.project.worktree.setupAccessibility'),
                  onChanged: (v) => _setLifecycleText('setup', v),
                ),
              ],
            ),
            _section(
              t('settings.project.worktree.teardown'),
              trailing: _docsLink(),
              [
                _textArea(
                  keyId: 'teardown',
                  value: _lifecycleText('teardown'),
                  placeholder: 'docker compose down',
                  semantics: t(
                    'settings.project.worktree.teardownAccessibility',
                  ),
                  onChanged: (v) => _setLifecycleText('teardown', v),
                ),
              ],
            ),
            SettingsSection(
              title: 'Service ports',
              flush: true,
              children: [
                SettingsActionRow(
                  label: 'Service port allocation',
                  hint: _worktree
                      .object('servicePorts')
                      .text('range', 'Host default'),
                  actionLabel: t('settings.project.scripts.actions.edit'),
                  onPressed: _saving ? null : _portAllocation,
                ),
              ],
            ),
          ],
        ),
        _group(
          t('settings.project.scripts.title'),
          t('settings.project.scripts.info'),
          trailing: _glyphButton(
            icon: LucideIcons.plus300,
            label: t('settings.project.scripts.actions.add'),
            onPressed: _saving ? null : (_) => _editScript(null),
          ),
          [
            SettingsCard(
              children: [
                if (scripts.isEmpty)
                  Padding(
                    padding: const EdgeInsets.all(16),
                    child: Text(
                      t('settings.project.scripts.empty'),
                      style: theme.textTheme.bodyMedium?.copyWith(
                        fontSize: type.uiSize(14),
                        color: colors.muted,
                      ),
                    ),
                  ),
                for (final entry in scripts.entries)
                  _scriptRow(entry.key, jsonMap(entry.value)),
              ],
            ),
          ],
        ),
        _group(
          t('settings.project.metadata.title'),
          t('settings.project.metadata.info'),
          [
            for (final (index, key) in _metadataKeys.indexed)
              _section(
                t('settings.project.metadata.$key'),
                flush: index == _metadataKeys.length - 1,
                [
                  _textArea(
                    keyId: 'metadata:$key',
                    value: _config!
                        .object('metadataGeneration')
                        .object(key)
                        .text('instructions'),
                    placeholder: t(
                      'settings.project.metadata.${key}Placeholder',
                    ),
                    semantics: t('settings.project.metadata.$key'),
                    onChanged: (text) => _change(() {
                      final metadata = Map<String, dynamic>.from(
                        _config!.object('metadataGeneration'),
                      );
                      final entry = Map<String, dynamic>.from(
                        jsonMap(metadata[key]),
                      );
                      if (text.trim().isEmpty) {
                        entry.remove('instructions');
                      } else {
                        entry['instructions'] = text;
                      }
                      if (entry.isEmpty) {
                        metadata.remove(key);
                      } else {
                        metadata[key] = entry;
                      }
                      _config!['metadataGeneration'] = metadata;
                    }),
                  ),
                ],
              ),
          ],
        ),
        if (stale)
          Padding(
            padding: const EdgeInsets.only(top: 12),
            child: WorkspaceAlert(
              title: t('settings.project.writeFailures.staleTitle'),
              description: t('settings.project.writeFailures.staleDescription'),
              actions: [reloadButton],
            ),
          ),
        if (writeFailed)
          Padding(
            padding: const EdgeInsets.only(top: 12),
            child: WorkspaceAlert(
              title: t('settings.project.writeFailures.failedTitle'),
              description: t(
                'settings.project.writeFailures.failedDescription',
              ),
              actions: [
                PaseoButton(
                  label: t('settings.project.actions.tryAgain'),
                  variant: PaseoButtonVariant.outline,
                  size: PaseoControlSize.sm,
                  onPressed: _saving ? null : _save,
                ),
                reloadButton,
              ],
            ),
          ),
        if (_error != null)
          Padding(
            padding: const EdgeInsets.only(top: 12),
            child: WorkspaceAlert(
              title: t('settings.project.writeFailures.failedTitle'),
              description: errorMessage(_error!),
            ),
          ),
        Padding(
          padding: const EdgeInsets.only(top: 16),
          child: Row(
            mainAxisAlignment: MainAxisAlignment.end,
            children: [
              PaseoButton(
                label: _saving
                    ? t('settings.project.actions.saving')
                    : t('settings.project.actions.save'),
                semanticLabel: t('settings.project.actions.save'),
                variant: PaseoButtonVariant.primary,
                loading: _saving,
                onPressed: _saving || stale || invalidScripts ? null : _save,
              ),
            ],
          ),
        ),
      ],
    );
  }

  Widget _recoverySection() => Padding(
    padding: const EdgeInsets.only(top: 32),
    child: SettingsSection(
      title: 'Workspace recovery',
      flush: true,
      children: [
        SettingsActionRow(
          label: 'Inspect missing workspace recovery',
          hint: _recovery?.text('message', _recovery!.text('action')),
          actionLabel: tr(context, 'settings.diagnostics.app.run'),
          onPressed: _busy
              ? null
              : () => _action(() async {
                  final response = await _client!.request(
                    'workspace.recovery.inspect.request',
                    {'workspaceId': widget.target.workspaceId},
                    responseType: 'workspace.recovery.inspect.response',
                  );
                  requireWorkspaceSuccess(response);
                  if (mounted) {
                    setState(() => _recovery = response.object('state'));
                  }
                }),
        ),
        if (_recovery?.text('kind') == 'recoverable')
          SettingsActionRow(
            label: 'Restore workspace',
            hint: '${_recovery!.text('action')}\n${_recovery!.text('branch')}',
            actionLabel: 'Restore',
            onPressed: _busy
                ? null
                : () async {
                    if (!await confirmAction(
                          context,
                          title: 'Restore workspace?',
                          message:
                              '${_recovery!.text('action')}\n${_recovery!.text('branch')}',
                          confirmLabel: 'Restore',
                        ) ||
                        !mounted) {
                      return;
                    }
                    await _action(() async {
                      final response = await _client!.request(
                        'workspace.recovery.restore.request',
                        {'workspaceId': widget.target.workspaceId},
                        responseType: 'workspace.recovery.restore.response',
                      );
                      requireWorkspaceSuccess(response);
                      if (!mounted) return;
                      await PaseoScope.of(
                        context,
                      ).refreshHost(widget.target.serverId);
                      if (mounted) setState(() => _recovery = null);
                    });
                  },
          ),
      ],
    ),
  );

  /// Original `project-settings-screen.tsx` `ProjectSettingsBody`: 16px body
  /// with 8px gaps; the header block (8px over, 16px under) is a 28px project
  /// icon, base medium name and 4px-padded pencil, 12px apart.
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final project = _project;
    final fallbackName =
        _root.split(RegExp(r'[/\\]')).where((s) => s.isNotEmpty).lastOrNull ??
        _root;
    final name = project?.text('projectDisplayName', fallbackName) ??
        fallbackName;
    final header = Padding(
      padding: const EdgeInsets.only(top: 8, bottom: 16),
      child: Row(
        spacing: 12,
        children: [
          ProjectIconSquare(
            name: name,
            viewKey: project == null
                ? jsonEncode([widget.target.serverId, _root])
                : projectViewKey(widget.target.serverId, project),
            size: 28,
          ),
          Flexible(
            child: Text(
              '$name${_dirty ? ' •' : ''}',
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: theme.textTheme.bodyMedium?.copyWith(
                fontSize: type.uiSize(14),
                fontWeight: FontWeight.w500,
                color: theme.colorScheme.onSurface,
              ),
            ),
          ),
          if (project != null)
            _glyphButton(
              icon: LucideIcons.pencil300,
              label: tr(context, 'settings.project.edit.title'),
              onPressed: (_) => _editProject(project, name),
            ),
        ],
      ),
    );
    final Widget body;
    if (_hostGone) {
      // Original `NoEditableTarget` (offline or errored host): 16px inset
      // muted base notice.
      body = Padding(
        padding: const EdgeInsets.all(16),
        child: Text(
          tr(context, 'settings.project.noEditableTarget'),
          style: theme.textTheme.bodyMedium?.copyWith(
            fontSize: type.uiSize(14),
            color: colors.muted,
          ),
        ),
      );
    } else if (_config == null &&
        (_loading || (_readError == null && _readErrorCode == null))) {
      body = Padding(
        padding: const EdgeInsets.all(24),
        child: Center(
          child: SizedBox.square(
            dimension: 20,
            child: CircularProgressIndicator(
              strokeWidth: 2,
              color: colors.muted,
            ),
          ),
        ),
      );
    } else if (_readError != null || _readErrorCode != null) {
      body = _readFailure();
    } else {
      body = _form();
    }
    return PopScope(
      canPop: !_dirty,
      onPopInvokedWithResult: (didPop, result) async {
        if (!didPop && await _mayDiscard() && mounted) {
          setState(() => _dirty = false);
          WidgetsBinding.instance.addPostFrameCallback((_) {
            if (mounted) Navigator.of(context).pop();
          });
        }
      },
      child: ColoredBox(
        color: colors.surface0,
        child: ListView(
          padding: EdgeInsets.only(
            bottom: MediaQuery.paddingOf(context).bottom,
          ),
          children: [
            Center(
              child: ConstrainedBox(
                constraints: const BoxConstraints(maxWidth: 720),
                child: Padding(
                  padding: const EdgeInsets.all(16),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    children: [
                      header,
                      body,
                      if (widget.target.workspaceId.isNotEmpty)
                        _recoverySection(),
                    ],
                  ),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// Original `components/ui/alert.tsx` (sm, or md when [large]): transparent,
/// 1px border at half the accent alpha, 16px radius, 12/16 (md 16/24) insets;
/// a 14px (md 16px) status glyph 12px before the medium accent title, the
/// muted description and outline actions (8px gap, 8px above) indented past it.
/// Without a title the muted description leads beside the glyph.
class WorkspaceAlert extends StatelessWidget {
  const WorkspaceAlert({
    super.key,
    this.title,
    this.description,
    this.warning = false,
    this.large = false,
    this.actions = const [],
  });
  final String? title;
  final String? description;
  final bool warning, large;
  final List<Widget> actions;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final accent = warning ? colors.warning : colors.danger;
    final iconSize = large ? 16.0 : 14.0;
    final text = theme.textTheme.bodyMedium!.copyWith(
      fontSize: type.uiSize(14),
      height: 1.4,
    );
    final hasTitle = title != null && title!.isNotEmpty;
    final hasDescription = description != null && description!.isNotEmpty;
    return Semantics(
      liveRegion: true,
      child: Container(
        padding: EdgeInsets.symmetric(
          horizontal: large ? 24 : 16,
          vertical: large ? 16 : 12,
        ),
        decoration: BoxDecoration(
          border: Border.all(color: accent.withValues(alpha: .5)),
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
                  warning
                      ? LucideIcons.triangleAlert300
                      : LucideIcons.circleX300,
                  size: iconSize,
                  color: accent,
                ),
                Expanded(
                  child: hasTitle
                      ? Text(
                          title!,
                          style: text.copyWith(
                            color: accent,
                            fontWeight: FontWeight.w500,
                          ),
                        )
                      : SelectableText(
                          description ?? '',
                          style: text.copyWith(color: colors.muted),
                        ),
                ),
              ],
            ),
            if ((hasTitle && hasDescription) || actions.isNotEmpty)
              Padding(
                padding: EdgeInsets.only(left: iconSize + 12),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    if (hasTitle && hasDescription)
                      SelectableText(
                        description!,
                        style: text.copyWith(color: colors.muted),
                      ),
                    if (actions.isNotEmpty)
                      Padding(
                        padding: const EdgeInsets.only(top: 8),
                        child: Wrap(spacing: 8, runSpacing: 8, children: actions),
                      ),
                  ],
                ),
              ),
          ],
        ),
      ),
    );
  }
}

/// Original `ProjectEditSheet` body and footer.
class _ProjectEditSheet extends StatefulWidget {
  const _ProjectEditSheet({
    required this.client,
    required this.project,
    required this.displayName,
    required this.viewKey,
  });
  final PaseoClient client;
  final JsonMap project;
  final String displayName;
  final String viewKey;
  @override
  State<_ProjectEditSheet> createState() => _ProjectEditSheetState();
}

/// Original `projects/edit-form.ts` icon choice.
enum _IconChoice { unchanged, automatic, upload, url }

class _ProjectEditSheetState extends State<_ProjectEditSheet> {
  late final _name = TextEditingController(
    text: widget.project.text('projectCustomName'),
  );
  final _url = TextEditingController();
  Uint8List? _picked;
  String? _pickedName;
  _IconChoice _choice = _IconChoice.unchanged;
  bool _saving = false;
  String? _nameError, _iconError;

  bool get _hasCustomIcon => widget.project['projectCustomIconRevision'] != null;

  /// Original `deriveRename`: the trimmed name, empty meaning the derived one;
  /// unchanged from the current override means no rename.
  bool get _renamed =>
      _name.text.trim() != widget.project.text('projectCustomName');

  /// Original `deriveIcon` as the request `source`; `url` is acquired on the
  /// client before it is sent.
  JsonMap? get _iconIntent {
    switch (_choice) {
      case _IconChoice.automatic:
        return _hasCustomIcon ? <String, dynamic>{'type': 'automatic'} : null;
      case _IconChoice.upload:
        return _picked == null
            ? null
            : <String, dynamic>{'type': 'upload', 'data': base64Encode(_picked!)};
      case _IconChoice.url:
        final url = _url.text.trim();
        return url.isEmpty ? null : <String, dynamic>{'type': 'url', 'url': url};
      case _IconChoice.unchanged:
        return null;
    }
  }

  /// Original `canUseAutomatic`: false once the resulting icon is automatic.
  bool get _canUseAutomatic {
    final intent = _iconIntent;
    return intent != null ? intent.text('type') != 'automatic' : _hasCustomIcon;
  }

  bool get _canSubmit => _renamed || _iconIntent != null;

  @override
  void dispose() {
    _name.dispose();
    _url.dispose();
    super.dispose();
  }

  Future<void> _chooseImage() async {
    final file = await openFile(
      acceptedTypeGroups: const [
        XTypeGroup(
          label: 'Images',
          extensions: ['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'ico'],
          mimeTypes: ['image/*'],
          uniformTypeIdentifiers: ['public.image'],
        ),
      ],
    );
    if (file == null) return;
    final bytes = await file.readAsBytes();
    if (!mounted) return;
    setState(() {
      _picked = bytes;
      _pickedName = file.name;
      _choice = _IconChoice.upload;
      _nameError = null;
      _iconError = null;
    });
  }

  void _setImageUrl(String url) => setState(() {
    // Emptying the box withdraws the URL without discarding an earlier pick.
    _choice = url.trim().isNotEmpty
        ? _IconChoice.url
        : _picked != null
        ? _IconChoice.upload
        : _IconChoice.unchanged;
    _nameError = null;
    _iconError = null;
  });

  void _useAutomaticIcon() => setState(() {
    _picked = null;
    _pickedName = null;
    _url.clear();
    _choice = _hasCustomIcon ? _IconChoice.automatic : _IconChoice.unchanged;
    _nameError = null;
    _iconError = null;
  });

  static const _maxIconBytes = 512 * 1024;

  static Future<Uint8List> _readBytes(HttpClientResponse response) async {
    if (response.contentLength > _maxIconBytes) {
      throw StateError('Icon must be 512 KB or smaller');
    }
    final builder = BytesBuilder(copy: false);
    await for (final chunk in response) {
      builder.add(chunk);
      if (builder.length > _maxIconBytes) {
        throw StateError('Icon must be 512 KB or smaller');
      }
    }
    return builder.takeBytes();
  }

  static Future<HttpClientResponse> _fetchOk(HttpClient http, Uri url) async {
    final response = await (await http.getUrl(url)).close();
    if (response.statusCode < 200 || response.statusCode >= 300) {
      await response.drain<void>();
      throw StateError('Download failed (${response.statusCode})');
    }
    return response;
  }

  static String? _attribute(String tag, String name) {
    final match = RegExp(
      '\\b$name\\s*=\\s*["\']([^"\']+)["\']',
      caseSensitive: false,
    ).firstMatch(tag);
    final value = match?.group(1)?.trim();
    return value == null || value.isEmpty ? null : value;
  }

  /// Original `importProjectIconFromUrl`: an image URL uploads its bytes; a
  /// website offers its declared non-SVG `<link rel=icon>`s, then
  /// `/favicon.ico`, at most eight candidates.
  static Future<String> _importIconFromUrl(String value) async {
    final url = Uri.tryParse(value.trim());
    if (url == null || !url.hasScheme || url.host.isEmpty) {
      throw StateError('Enter a valid website or image URL');
    }
    if (!const ['http', 'https'].contains(url.scheme) ||
        url.userInfo.isNotEmpty) {
      throw StateError('URL must use HTTP or HTTPS without credentials');
    }
    final http = HttpClient();
    try {
      final response = await _fetchOk(http, url);
      final contentType =
          response.headers
              .value(HttpHeaders.contentTypeHeader)
              ?.toLowerCase() ??
          '';
      if (!contentType.contains('text/html')) {
        return base64Encode(await _readBytes(response));
      }
      final html = utf8.decode(
        await _readBytes(response),
        allowMalformed: true,
      );
      final candidates = <Uri>[];
      for (final match in RegExp(
        r'<link\b[^>]*>',
        caseSensitive: false,
      ).allMatches(html)) {
        final tag = match.group(0)!;
        final rel = _attribute(tag, 'rel')?.toLowerCase() ?? '';
        final href = _attribute(tag, 'href');
        final type = _attribute(tag, 'type')?.toLowerCase() ?? '';
        if (href == null ||
            !rel.contains('icon') ||
            type.contains('svg') ||
            href.endsWith('.svg')) {
          continue;
        }
        try {
          candidates.add(url.resolve(href));
        } catch (_) {
          // Try the next declared icon and then the conventional fallback.
        }
      }
      candidates.add(url.resolve('/favicon.ico'));
      Object lastError = StateError('Website does not expose a supported icon');
      for (final candidate in candidates.take(8)) {
        try {
          return base64Encode(
            await _readBytes(await _fetchOk(http, candidate)),
          );
        } catch (error) {
          lastError = error;
        }
      }
      throw lastError;
    } finally {
      http.close(force: true);
    }
  }

  /// Original `submitProjectEdit`: icon bytes are acquired first, then the
  /// rename, then the icon; each failure lands under its own field.
  Future<void> _submit() async {
    final intent = _iconIntent;
    final rename = _renamed;
    setState(() {
      _saving = true;
      _nameError = null;
      _iconError = null;
    });
    final projectId = widget.project.text('projectId');
    try {
      JsonMap? source = intent;
      if (intent != null && intent.text('type') == 'url') {
        try {
          source = <String, dynamic>{
            'type': 'upload',
            'data': await _importIconFromUrl(intent.text('url')),
          };
        } catch (error) {
          if (mounted) setState(() => _iconError = errorMessage(error));
          return;
        }
      }
      if (rename) {
        try {
          final name = _name.text.trim();
          final response = await widget.client.request(
            'project.rename.request',
            {'projectId': projectId, 'customName': name.isEmpty ? null : name},
            responseType: 'project.rename.response',
          );
          requireWorkspaceSuccess(response);
        } catch (error) {
          if (mounted) setState(() => _nameError = errorMessage(error));
          return;
        }
      }
      if (source != null) {
        try {
          final response = await widget.client.request(
            'project.icon.set.request',
            {'projectId': projectId, 'source': source},
            responseType: 'project.icon.set.response',
          );
          requireWorkspaceSuccess(response);
        } catch (error) {
          if (mounted) setState(() => _iconError = errorMessage(error));
          return;
        }
      }
      if (mounted) Navigator.pop(context, true);
    } finally {
      if (mounted) setState(() => _saving = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final type = PaseoTypography.of(context);
    final compact = isCompactLayout(context);
    final size = compact ? PaseoControlSize.md : PaseoControlSize.sm;
    final initial = widget.displayName.trim();
    final inputStyle = theme.textTheme.bodyMedium?.copyWith(
      fontSize: type.uiSize(14),
      height: 1.4,
    );
    final showPicked = _choice == _IconChoice.upload && _picked != null;
    return PopScope(
      // Original `handleClose`: the sheet stays while a save is in flight.
      canPop: !_saving,
      child: SheetContents(
        title: tr(context, 'settings.project.edit.title'),
        actions: [
          ConstrainedBox(
            constraints: const BoxConstraints(minWidth: 112),
            child: PaseoButton(
              label: tr(context, 'common.actions.cancel'),
              onPressed: _saving ? null : () => Navigator.pop(context),
            ),
          ),
          ConstrainedBox(
            constraints: const BoxConstraints(minWidth: 112),
            child: PaseoButton(
              label: tr(context, 'settings.project.edit.save'),
              variant: PaseoButtonVariant.primary,
              loading: _saving,
              onPressed: _canSubmit && !_saving ? _submit : null,
            ),
          ),
        ],
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          spacing: 16,
          children: [
            PaseoFormField(
              label: tr(context, 'settings.project.edit.name'),
              error: _nameError,
              child: Semantics(
                label: tr(context, 'settings.project.edit.nameLabel'),
                child: TextField(
                  controller: _name,
                  enabled: !_saving,
                  autocorrect: false,
                  style: inputStyle,
                  decoration: paseoInputDecoration(
                    context,
                    size: size,
                    hintText: initial,
                  ),
                  onChanged: (_) => setState(() {
                    _nameError = null;
                    _iconError = null;
                  }),
                ),
              ),
            ),
            PaseoFormField(
              label: tr(context, 'settings.project.edit.icon'),
              hint: showPicked ? _pickedName : null,
              error: _iconError,
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                spacing: 8,
                children: [
                  Wrap(
                    spacing: 8,
                    runSpacing: 8,
                    crossAxisAlignment: WrapCrossAlignment.center,
                    children: [
                      showPicked
                          ? SizedBox.square(
                              dimension: 40,
                              child: Image.memory(
                                _picked!,
                                fit: BoxFit.contain,
                              ),
                            )
                          : ProjectIconSquare(
                              name: initial,
                              viewKey: widget.viewKey,
                              size: 40,
                            ),
                      PaseoButton(
                        label: tr(context, 'settings.project.edit.chooseImage'),
                        variant: PaseoButtonVariant.outline,
                        size: size,
                        onPressed: _saving ? null : _chooseImage,
                      ),
                      if (_canUseAutomatic)
                        PaseoButton(
                          label: tr(
                            context,
                            'settings.project.edit.useAutomatic',
                          ),
                          variant: PaseoButtonVariant.ghost,
                          size: size,
                          onPressed: _saving ? null : _useAutomaticIcon,
                        ),
                    ],
                  ),
                  Semantics(
                    label: tr(context, 'settings.project.edit.imageUrl'),
                    child: TextField(
                      controller: _url,
                      enabled: !_saving,
                      autocorrect: false,
                      keyboardType: TextInputType.url,
                      style: inputStyle,
                      decoration: paseoInputDecoration(
                        context,
                        size: size,
                        hintText: tr(context, 'settings.project.edit.imageUrl'),
                      ),
                      onChanged: _setImageUrl,
                    ),
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}
