import '../../ui/localization.dart';

import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart' show defaultTargetPlatform;
import 'package:flutter/material.dart';
import 'package:file_selector/file_selector.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:uuid/uuid.dart';

import '../../core/client.dart';
import '../../core/json.dart';
import '../../core/scope.dart';
import '../../core/target.dart';
import '../../ui/common.dart';
import '../../ui/theme.dart';
import '../chat/attachment_pill.dart';
import '../chat/composer/agent_form.dart';
import '../chat/composer/attachment_files.dart';
import '../chat/composer/forge_auto_attach.dart';
import '../chat/composer/forge_icons.dart';
import '../chat/composer/forge_picker.dart';
import '../chat/composer/send_button.dart' show ComposerHoverGlyph;
import '../chat/forge_attachments.dart';
import '../chat/image_clipboard.dart';
import '../files/remote_file_service.dart';
import '../files/remote_directory_browser.dart';
import '../navigation/host_dialog.dart' show showHostDialog;
import '../navigation/session_history_dialog.dart';
import '../settings/settings_forms.dart';
import '../../ui/controls.dart';
import 'project_settings_panel.dart';
import 'workspace_rpc.dart';

class NewTabPanel extends StatefulWidget {
  const NewTabPanel({super.key, required this.target});
  final PanelTarget target;
  @override
  State<NewTabPanel> createState() => _NewTabPanelState();
}

class _NewTabPanelState extends State<NewTabPanel> {
  final _path = TextEditingController();
  final _title = TextEditingController();
  final _prompt = TextEditingController();
  final _branch = TextEditingController();
  final _ref = TextEditingController();
  final _pr = TextEditingController();
  final _forge = TextEditingController();
  final _projectPath = TextEditingController();
  final _repo = TextEditingController();
  final _directoryName = TextEditingController();
  PaseoClient? _client;
  OwnedObservation? _recoveryObservation;
  JsonMap _config = {};
  JsonMap? _intent;
  JsonMap? _snapshot;
  List<JsonMap> _profiles = [];
  Object? _error;
  String _source = 'directory';
  String _cloneProtocol = 'https';
  String? _profileId;
  String _catalogCwd = '';
  bool _withAgent = true;
  bool _existing = false;
  bool _busy = false;
  bool _preparationUnknown = false;
  bool _finishing = false;
  bool _opened = false;
  bool _projectAdded = false;
  bool _initialized = false;
  bool _optionsOpen = false;
  ConnectionStatus? _lastStatus;
  Timer? _directoryDebounce;

  /// Display label of the picked ref (original `pickerItemLabel`), e.g.
  /// `main (local)`; null when [_ref] was typed or is empty.
  String? _refLabel;

  /// Original `defaultBasePickerItem` for [_defaultRefCwd].
  _BranchItem? _defaultRef;
  String? _defaultRefCwd;

  /// Original composer attachments (`composer/index.tsx`): image pills and the
  /// uploaded file / forge attachments sent with the initial prompt.
  List<JsonMap> _images = [];
  List<JsonMap> _attachments = [];

  /// The file being uploaded (original `pendingFiles`), null when idle.
  String? _uploadName;

  /// The checkout's forge (`getForgePresentation`), loaded per directory when
  /// the attachment menu opens.
  String _forgeKind = 'github';
  String? _forgeCwd;
  Rect? _attachAnchor;

  @override
  void initState() {
    super.initState();
    _path.text = widget.target.cwd.isEmpty ? '~' : widget.target.cwd;
    _catalogCwd = _path.text;
    _existing = widget.target.workspaceId.isNotEmpty;
    _source = widget.target.data.text('source', 'directory');
    if (widget.target.data.flag('addProject')) _withAgent = false;
    // Original add-project flow: the directory/clone/new-directory options are
    // its whole form. A new session has none of them (original NewWorkspaceForm:
    // project, host, base ref, What to launch, then the composer).
    _optionsOpen = widget.target.data.flag('addProject');
    final draft = widget.target.data.object('creationDraft');
    _title.text = draft.text('title');
    _prompt.text = draft.text('prompt');
    _config = draft.object('config');
    _initialConfig = _config;
    _prompt.addListener(_syncForgeAutoAttach);
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    if (_initialized) return;
    _initialized = true;
    final store = PaseoScope.of(context);
    _client = store.clientFor(widget.target.serverId);
    _lastStatus = _client!.status;
    _client!.addListener(_connectionChanged);
    final saved = jsonMap(store.preferences['workspaceCreationIntents']);
    final pending = jsonMap(saved[widget.target.key]);
    if (pending.isNotEmpty) _intent = pending;
    _loadProfiles();
    _loadDefaultRef();
    _loadProviderEntries();
    if (_intent != null) _recover();
  }

  void _connectionChanged() {
    if (!mounted) return;
    final status = _client!.status;
    final reconnected =
        status == ConnectionStatus.connected && _lastStatus != status;
    _lastStatus = status;
    if (reconnected && _intent != null && !_finishing) _recover();
    if (reconnected && _defaultRef == null) {
      _defaultRefCwd = null;
      _loadDefaultRef();
    }
    if (reconnected && _providerEntries.isEmpty) _loadProviderEntries();
    _syncForgeAutoAttach();
  }

  Future<void> _loadProfiles() async {
    try {
      final response = await _client!.request(
        'get_daemon_config_request',
        {},
        responseType: 'get_daemon_config_response',
      );
      requireWorkspaceSuccess(response);
      if (mounted) {
        setState(
          () => _profiles = response
              .object('config')
              .array('agentProfiles')
              .map(jsonMap)
              .toList(),
        );
      }
    } catch (error) {
      if (mounted) setState(() => _error = error);
    }
  }

  Future<void> _persistIntent(JsonMap? intent) async {
    final store = PaseoScope.of(context);
    final values = Map<String, dynamic>.from(
      jsonMap(store.preferences['workspaceCreationIntents']),
    );
    if (intent == null) {
      values.remove(widget.target.key);
    } else {
      values[widget.target.key] = intent;
    }
    store.preferences['workspaceCreationIntents'] = values;
    await store.savePreferences();
  }

  /// [toast] mirrors the original submit handlers' `toast.error(message)`
  /// beside the inline error text.
  Future<void> _action(
    Future<void> Function() action, {
    bool toast = false,
  }) async {
    if (_busy) return;
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await action();
    } catch (error) {
      if (mounted) {
        setState(() => _error = error);
        if (toast) {
          showToast(context, errorMessage(error), variant: ToastVariant.error);
        }
      }
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<bool> _browse() async {
    final selected = await showAdaptiveSheet<String>(
      context: context,
      maxWidth: 720,
      builder: (context) => SheetContents(
        title: 'Choose a remote directory',
        scroll: false,
        child: SizedBox(
          height: 520,
          child: RemoteDirectoryBrowser(
            client: _client!,
            cwd: _path.text.trim().isEmpty ? '~' : _path.text.trim(),
            initialPath: '.',
            directoriesOnly: true,
            onDirectorySelected: (path) => Navigator.pop(context, path),
          ),
        ),
      ),
    );
    if (selected == null || !mounted) return false;
    _setDirectory(selected);
    return true;
  }

  void _setDirectory(String directory) {
    setState(() {
      _path.text = directory;
      _catalogCwd = directory;
      _ref.clear();
      _refLabel = null;
    });
    _loadDefaultRef();
    _loadProviderEntries();
  }

  /// The directory's provider catalog (use-providers-snapshot.ts), which the
  /// agent selection resolves against.
  List<JsonMap> _providerEntries = [];
  int _providerGeneration = 0;

  /// The config the panel opened with (resolve-agent-form.ts initialValues).
  JsonMap _initialConfig = {};

  /// resolve-agent-form.ts userModified: once the user picks a provider,
  /// model, mode or profile, catalog loads no longer re-resolve the selection.
  bool _configTouched = false;

  Future<void> _loadProviderEntries() async {
    final cwd = _catalogCwd.trim();
    final own = ++_providerGeneration;
    if (_client!.status != ConnectionStatus.connected) return;
    try {
      final result = await settingsRequest(_client!, 'get_providers_snapshot_request', {
        if (cwd.isNotEmpty) 'cwd': cwd,
      }, responseType: 'get_providers_snapshot_response');
      if (!mounted || own != _providerGeneration) return;
      final entries = providerEntries(result);
      setState(() {
        _providerEntries = entries;
        // use-agent-form-state.ts: the selection resolves from the initial
        // values and the remembered create-agent preferences.
        if (!_configTouched) {
          _config = resolveInitialAgentConfig(
            initial: _initialConfig,
            preferences: _formPreferences,
            providers: entries,
          );
        }
      });
    } catch (_) {
      // AgentConfigFields reports catalog failures beside the controls.
    }
  }

  JsonMap get _formPreferences =>
      parseFormPreferences(PaseoScope.of(context).preferences[formPreferencesKey]);

  void _rememberPreferences(JsonMap next) {
    final store = PaseoScope.of(context);
    store.preferences[formPreferencesKey] = next;
    unawaited(store.savePreferences());
  }

  /// use-agent-form-state.ts setProviderAndModelFromUser / setModelFromUser /
  /// setModeFromUser / setThinkingOptionFromUser.
  void _changeConfig(JsonMap value) {
    final preferences = _formPreferences;
    var next = Map<String, dynamic>.of(value);
    final provider = stringValue(value['provider']);
    if (provider.isNotEmpty &&
        (provider != _config['provider'] || value['model'] != _config['model'])) {
      next = applyUserModelChoice(
        previous: _config,
        provider: provider,
        modelId: stringValue(value['model']),
        preferences: preferences,
        providers: _providerEntries,
      );
    }
    _rememberPreferences(rememberUserSelection(preferences, _config, next));
    setState(() {
      _config = next;
      _configTouched = true;
      _profileId = null;
    });
  }

  /// index.tsx useCheckoutStatusQuery remoteUrl: scopes the forge URLs the
  /// prompt auto-attaches.
  String? _remoteUrl;
  late final _forgeAutoAttach = ComposerForgeAutoAttach(
    search: (cwd, query) => searchForgeItems(_client!, cwd, query),
    attach: (item) {
      if (!mounted ||
          _attachments.any((a) => forgeAttachmentKey(a) == forgeItemKey(item))) {
        return;
      }
      setState(() => _attachments = [
        ..._attachments,
        forgeAttachment(item, legacy: usesLegacyForge(_client!)),
      ]);
      _syncForgeAutoAttach();
    },
    onResolvingChanged: () {
      if (mounted) setState(() {});
    },
  );

  void _syncForgeAutoAttach() {
    if (_client == null) return;
    _forgeAutoAttach.update((
      text: _prompt.text,
      remoteUrl: _remoteUrl,
      attachments: _attachments,
      connected: _client!.status == ConnectionStatus.connected,
      serverId: widget.target.serverId,
      cwd: _path.text.trim(),
    ));
  }

  /// Original `defaultBasePickerItem`: the checkout's upstream (or local)
  /// current branch, read from `checkout_status_request` per directory.
  Future<void> _loadDefaultRef() async {
    final cwd = _path.text.trim();
    if (cwd.isEmpty || cwd == _defaultRefCwd) return;
    _defaultRefCwd = cwd;
    _defaultRef = null;
    try {
      final status = await _client!.request('checkout_status_request', {
        'cwd': cwd,
      }, responseType: 'checkout_status_response');
      if (!mounted || _defaultRefCwd != cwd) return;
      final remote = status.text('remoteUrl');
      _remoteUrl = remote.isEmpty ? null : remote;
      _syncForgeAutoAttach();
      final current = status.text('currentBranch');
      if (current.isEmpty) {
        setState(() => _defaultRef = null);
        return;
      }
      final upstream = status.text('upstreamRef');
      final refName = upstream.isNotEmpty ? upstream : 'refs/heads/$current';
      setState(
        () => _defaultRef = _BranchItem(
          name: _BranchItem.branchNameFromRef(refName),
          refName: refName,
          branchName: current,
          semanticLabel:
              '${_BranchItem.branchNameFromRef(refName)}, ${upstream.isNotEmpty ? 'upstream' : 'local'} branch',
        ),
      );
    } catch (_) {
      // Without status the trigger keeps the original "main" fallback.
      if (mounted && _defaultRefCwd == cwd) setState(() => _defaultRef = null);
    }
  }

  Future<void> _addProject() => _action(() async {
    if (_path.text.trim().isEmpty) {
      throw StateError('Choose a remote directory.');
    }
    final response = await _client!.request('project.add.request', {
      'cwd': _path.text.trim(),
    }, responseType: 'project.add.response');
    requireWorkspaceSuccess(response);
    if (response['project'] == null) {
      throw StateError('The host did not return a registered project.');
    }
    if (!mounted) return;
    await PaseoScope.of(context).refreshHost(widget.target.serverId);
    if (mounted) setState(() => _projectAdded = true);
  });

  Future<void> _prepareDirectory() => _action(() async {
    if (_path.text.trim().isEmpty) {
      throw StateError('Choose a remote parent directory.');
    }
    if (_source == 'new-directory' && _directoryName.text.trim().isEmpty) {
      throw StateError('Enter a directory name.');
    }
    if (_source == 'clone' && _repo.text.trim().isEmpty) {
      throw StateError('Choose a repository or enter owner/repository.');
    }
    setState(() => _preparationUnknown = true);
    final response = await _client!.request(
      _source == 'clone'
          ? 'project.github.clone.request'
          : 'project.create_directory.request',
      _source == 'clone'
          ? {
              'repo': _repo.text.trim(),
              'cloneProtocol': _cloneProtocol,
              'targetDirectory': _path.text.trim(),
            }
          : {
              'parentPath': _path.text.trim(),
              'name': _directoryName.text.trim(),
            },
      responseType: _source == 'clone'
          ? 'project.github.clone.response'
          : 'project.create_directory.response',
      timeout: const Duration(minutes: 10),
    );
    if (!mounted) return;
    setState(() => _preparationUnknown = false);
    requireWorkspaceSuccess(response);
    final directory = response.text(
      _source == 'clone' ? 'checkoutPath' : 'directoryPath',
    );
    if (directory.isEmpty) {
      throw StateError(
        'The host did not return the prepared directory. Browse to inspect its state.',
      );
    }
    if (!mounted) return;
    _setDirectory(directory);
    setState(() {
      _source = 'directory';
      _existing = false;
    });
    await PaseoScope.of(context).refreshHost(widget.target.serverId);
  });

  /// Searches GitHub, then offers the matches in the original combobox.
  Future<void> _searchRepositories(BuildContext anchor) async {
    List<JsonMap>? repositories;
    await _action(() async {
      final response = await _client!.request(
        'workspace.github.search_repositories.request',
        {'query': _repo.text.trim(), 'limit': 30},
        responseType: 'workspace.github.search_repositories.response',
      );
      requireWorkspaceSuccess(response);
      repositories = response.array('repositories').map(jsonMap).toList();
    });
    final found = repositories;
    if (found == null || !mounted || !anchor.mounted) return;
    if (found.isEmpty) {
      showToast(context, 'No repositories found.');
      return;
    }
    final picked = await showPaseoSelect<String>(
      context: anchor,
      title: 'GitHub repositories',
      value: _repo.text.trim(),
      searchable: true,
      options: [
        for (final repo in found)
          PaseoSelectOption(
            repo.text('nameWithOwner'),
            repo.text('nameWithOwner'),
            description: repo.text('description').isEmpty
                ? null
                : repo.text('description'),
          ),
      ],
    );
    if (picked != null && mounted) setState(() => _repo.text = picked);
  }

  /// Original ref picker (`RefOption` + `buildBranchPickerItems`): a
  /// searchable combobox of origin and local refs, each with the muted 14px
  /// branch glyph in a 16px box and a local branch's sm muted `+a −b`
  /// divergence on the trailing edge.
  Future<void> _pickRef(BuildContext anchor) async {
    List<_BranchItem>? items;
    await _action(() async {
      final response = await _client!.request('branch_suggestions_request', {
        'cwd': _path.text.trim(),
        'query': '',
        'limit': 100,
      }, responseType: 'branch_suggestions_response');
      requireWorkspaceSuccess(response);
      final details = response['branchDetails'] is List
          ? response.array('branchDetails').map(jsonMap).toList()
          : [
              for (final branch in response.array('branches'))
                <String, dynamic>{
                  'name': branch is Map ? jsonMap(branch).text('name') : '$branch',
                },
            ];
      items = _BranchItem.build(details);
    });
    final found = items;
    if (found == null || !mounted || !anchor.mounted) return;
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final checkout = _source == 'checkout';
    final selected = _ref.text.trim().isNotEmpty
        ? _ref.text.trim()
        : checkout
        ? null
        : _defaultRef?.refName;
    final picked = await showPaseoSelect<String>(
      context: anchor,
      title: tr(context, 'newWorkspace.refPicker.title'),
      value: selected,
      searchable: true,
      searchPlaceholder: tr(context, 'newWorkspace.refPicker.searchPlaceholder'),
      emptyText: tr(context, 'newWorkspace.refPicker.noMatchingRefs'),
      options: [
        for (final item in found)
          PaseoSelectOption(
            checkout ? item.branchName : item.refName,
            item.name,
            semanticLabel: item.semanticLabel,
            leading: _glyph(LucideIcons.gitBranch300, colors.muted),
            trailing: item.divergence == null
                ? null
                : Text(
                    item.divergence!,
                    style: TextStyle(
                      fontSize: type.uiSize(12),
                      color: colors.muted,
                      fontFeatures: const [FontFeature.tabularFigures()],
                    ),
                  ),
          ),
      ],
    );
    if (picked == null || !mounted) return;
    final item = found.firstWhere(
      (item) => (checkout ? item.branchName : item.refName) == picked,
    );
    setState(() {
      _ref.text = picked;
      _refLabel = item.name;
    });
  }

  static String _projectRoot(JsonMap project) => project.text(
    'projectRootPath',
    project.text('repoRoot', project.text('cwd')),
  );

  static String _projectName(JsonMap project) {
    final root = _projectRoot(project);
    return project.text(
      'projectDisplayName',
      project.text(
        'name',
        root.split(RegExp(r'[/\\]')).where((s) => s.isNotEmpty).lastOrNull ??
            root,
      ),
    );
  }

  /// Original project combobox: searchable, 360px desktop minimum, each row
  /// the project's icon, name and source directory, with Add project last.
  Future<void> _pickProject(BuildContext anchor, List<JsonMap> projects) async {
    const browse = '\u0000browse';
    final muted = PaseoColors.of(context).muted;
    final picked = await showPaseoSelect<String>(
      context: anchor,
      title: tr(context, 'newWorkspace.fields.project'),
      value: _path.text.trim(),
      searchable: true,
      searchPlaceholder: 'Search projects',
      emptyText: 'No projects available.',
      desktopMinWidth: 360,
      options: [
        for (final project in projects)
          PaseoSelectOption(
            _projectRoot(project),
            _projectName(project),
            description: _projectRoot(project),
            leading: ProjectIconSquare(
              name: _projectName(project),
              viewKey: projectViewKey(widget.target.serverId, project),
              size: 16,
            ),
          ),
        PaseoSelectOption(
          browse,
          tr(context, 'sidebar.actions.addProject'),
          leading: _glyph(LucideIcons.folderPlus300, muted),
          separatorBefore: projects.isNotEmpty,
        ),
      ],
    );
    if (picked == null || !mounted) return;
    if (picked == browse) {
      await _browse();
    } else {
      _setDirectory(picked);
    }
  }

  /// Original `HostPicker` (`includeAddHost`): unsearchable, 200px desktop
  /// minimum, each host led by its status dot, then the Add host row with
  /// the muted 14px plus.
  Future<void> _pickHost(BuildContext anchor) async {
    const addHost = '\u0000add-host';
    final store = PaseoScope.of(context);
    final serverId = await showPaseoSelect<String>(
      context: anchor,
      title: tr(context, 'newWorkspace.fields.host'),
      value: widget.target.serverId,
      desktopMinWidth: 200,
      options: [
        for (final candidate in store.hosts)
          PaseoSelectOption(
            candidate.serverId,
            candidate.label,
            leading: _HostStatusDot(status: candidate.client.status),
          ),
        PaseoSelectOption(
          addHost,
          'Add host',
          leading: _glyph(LucideIcons.plus300, PaseoColors.of(context).muted),
        ),
      ],
    );
    if (serverId == addHost) {
      if (mounted) await showHostDialog(context);
      return;
    }
    if (serverId == null || serverId == widget.target.serverId || !mounted) {
      return;
    }
    store.open(
      PanelTarget(
        kind: widget.target.kind,
        serverId: serverId,
        workspaceId: '',
        cwd: '',
        data: {
          ...widget.target.data,
          'creationDraft': {
            'title': _title.text,
            'prompt': _prompt.text,
            'config': _config,
          },
        },
      ),
    );
  }

  /// Original form icons: a muted 14px glyph centred in a 16px box.
  static Widget _glyph(IconData icon, Color color) => SizedBox.square(
    dimension: 16,
    child: Center(child: Icon(icon, size: 14, color: color)),
  );

  static bool get _native =>
      defaultTargetPlatform == TargetPlatform.android ||
      defaultTargetPlatform == TargetPlatform.iOS;

  /// Original `LaunchControl` (`new-workspace/form/launch-control.tsx`): one field
  /// trigger opening the "What to launch" menu (sheet on compact, 260px end-aligned
  /// popover otherwise). Its Terminal group and Manage profiles row are terminal
  /// surfaces this app does not ship, leaving Chat.
  Future<void> _pickLaunch(BuildContext anchor) async {
    final muted = PaseoColors.of(context).muted;
    await showPaseoMenu<String>(
      context,
      title: tr(context, 'newWorkspace.launch.title'),
      anchor: paseoAnchorRect(anchor),
      alignEnd: true,
      width: 260,
      entries: [
        PaseoMenuEntry.item(
          value: 'chat',
          label: tr(context, 'newWorkspace.launch.chat'),
          leading: _glyph(LucideIcons.messageCircle300, muted),
          selected: true,
        ),
      ],
    );
  }

  /// Original input.tsx `AttachmentDropdown`: a 28px round muted Plus opening
  /// composer/index.tsx `attachmentMenuItems` — Add image, Paste image (native),
  /// Add issue or PR (forge mark), Upload file.
  Widget _attachButton() {
    final colors = Theme.of(context).colorScheme;
    final label = tr(context, 'composer.input.addAttachment');
    return Builder(
      builder: (anchor) => ComposerHoverGlyph(
        disabled: _client!.status != ConnectionStatus.connected,
        hoverColor: PaseoColors.of(context).surface2,
        semanticsLabel: label,
        onPressed: () async {
          _attachAnchor = paseoAnchorRect(anchor);
          _ensureForge();
          final action = await showPaseoMenu<String>(
            context,
            title: label,
            anchor: _attachAnchor,
            entries: [
              PaseoMenuEntry.item(
                value: 'image',
                label: tr(context, 'composer.attachments.addImage'),
                icon: LucideIcons.image300,
              ),
              if (_native)
                PaseoMenuEntry.item(
                  value: 'paste',
                  label: tr(context, 'composer.attachments.pasteImage'),
                  icon: LucideIcons.clipboardPaste300,
                ),
              PaseoMenuEntry.item(
                value: 'forge',
                label: tr(
                  context,
                  _forgeKind == 'gitlab'
                      ? 'composer.attachments.addIssueOrPr_mr'
                      : 'composer.attachments.addIssueOrPr',
                ),
                leading: ForgeBrandIcon(
                  forge: _forgeKind,
                  size: 16,
                  color: colors.onSurfaceVariant,
                ),
              ),
              PaseoMenuEntry.item(
                value: 'file',
                label: tr(context, 'composer.attachments.addFile'),
                icon: LucideIcons.paperclip300,
              ),
            ],
          );
          if (!mounted) return;
          switch (action) {
            case 'image':
              await _pickFiles(images: true);
            case 'paste':
              await _pasteImage();
            case 'forge':
              await _pickForge();
            case 'file':
              await _pickFiles();
          }
        },
        builder: (hovered) => Icon(
          LucideIcons.plus300,
          size: _native ? 20 : 16,
          color: hovered ? colors.onSurface : colors.onSurfaceVariant,
        ),
      ),
    );
  }

  /// Original index.tsx `handlePasteImage`: clipboard images become image pills;
  /// none toasts composer.errors.noClipboardImage.
  Future<void> _pasteImage() async {
    try {
      final contents = await readChatClipboard(imagesOnly: true);
      if (!mounted) return;
      if (contents.images.isEmpty) {
        showToast(
          context,
          tr(context, 'composer.errors.noClipboardImage'),
          variant: ToastVariant.error,
        );
        return;
      }
      setState(() => _images = [..._images, ...contents.images]);
    } catch (_) {
      if (mounted) {
        showToast(
          context,
          tr(context, 'composer.errors.pasteImageFailed'),
          variant: ToastVariant.error,
        );
      }
    }
  }

  /// index.tsx handlePickImage / handlePickFile: picked images join the image
  /// tray; picked files upload (uploadSelectedFiles).
  Future<void> _pickFiles({bool images = false}) async {
    try {
      final files = await openFiles(
        acceptedTypeGroups: [
          if (images)
            XTypeGroup(
              label: 'images',
              extensions: rasterImageExtensions,
              mimeTypes: const ['image/*'],
              uniformTypeIdentifiers: const ['public.image'],
            ),
        ],
      );
      if (!mounted || files.isEmpty) return;
      if (images) {
        final added = [
          for (final file in files)
            {
              'data': base64Encode(await file.readAsBytes()),
              'mimeType': rasterImageMimeType(file.name) ?? file.mimeType ?? 'image/png',
            },
        ];
        if (mounted) setState(() => _images = [..._images, ...added]);
        return;
      }
      await _uploadFiles(files);
    } catch (e) {
      if (!mounted) return;
      final message = e is FileTooLargeError
          ? tr(context, 'composer.errors.fileTooLarge', args: {'size': '50MB', 'fileName': e.fileName})
          : errorMessage(e);
      showToast(
        context,
        message.trim().isEmpty ? tr(context, 'composer.errors.uploadFailed') : message,
        variant: ToastVariant.error,
      );
    }
  }

  /// actions.ts uploadFileAttachments: every file is size-checked first, then
  /// each uploads; the uploaded files attach together once all succeed.
  Future<void> _uploadFiles(List<XFile> files) async {
    checkUploadSizes([for (final file in files) (name: file.name, size: await file.length())]);
    try {
      final uploaded = <JsonMap>[];
      for (final file in files) {
        if (mounted) setState(() => _uploadName = file.name);
        uploaded.add(await RemoteFileService(_client!).upload(
          bytes: await file.readAsBytes(),
          fileName: file.name,
          mimeType: file.mimeType ?? mimeTypeForPath(file.name),
        ));
      }
      if (mounted) setState(() => _attachments = [..._attachments, ...uploaded]);
    } finally {
      if (mounted) setState(() => _uploadName = null);
    }
  }

  void _ensureForge() {
    final cwd = _path.text.trim();
    if (cwd.isEmpty ||
        cwd == _forgeCwd ||
        _client!.status != ConnectionStatus.connected) {
      return;
    }
    _forgeCwd = cwd;
    loadCheckoutForge(_client!, cwd)
        .then((forge) {
          if (mounted && _forgeCwd == cwd && forge != _forgeKind) {
            setState(() => _forgeKind = forge);
          }
        })
        .catchError((Object _) {
          if (_forgeCwd == cwd) _forgeCwd = null;
        });
  }

  /// Original index.tsx `handleToggleGithubItem`: an attached item is removed,
  /// any other is attached.
  Future<void> _pickForge() async {
    final keys = {
      for (final attachment in _attachments)
        if (AttachmentPill.forgeLabels(attachment) != null)
          forgeItemKey(attachment),
    };
    final item = await showForgePicker(
      context,
      client: _client!,
      cwd: _path.text.trim(),
      attachedKeys: keys,
      anchor: _attachAnchor,
      mergeRequests: _forgeKind == 'gitlab',
    );
    if (item == null || !mounted) return;
    final key = forgeItemKey(item);
    final current = List.of(_attachments);
    final index = current.indexWhere(
      (a) => AttachmentPill.forgeLabels(a) != null && forgeItemKey(a) == key,
    );
    if (index >= 0) {
      // toggleForgeAttachmentFromPicker: a picker removal never re-attaches.
      _forgeAutoAttach.markRemoved(current.removeAt(index));
    } else {
      current.add(forgeAttachment(item, legacy: usesLegacyForge(_client!)));
    }
    setState(() => _attachments = current);
  }

  /// Original index.tsx `attachmentTray`: a wrapping row of image thumbnails,
  /// attachment labels and the uploading file's pending pill.
  Widget _attachmentTray() {
    const overhang = AttachmentPill.removableOverhang;
    final busy = _busy || _intent != null;
    return Wrap(
      children: [
        for (var i = 0; i < _images.length; i++)
          AttachmentPill(
            key: ValueKey('image:$i'),
            disabled: busy,
            removeLabel: tr(context, 'composer.attachments.removeImage'),
            onRemove: () =>
                setState(() => _images = List.of(_images)..removeAt(i)),
            child: AttachmentPill.thumbnail(context, _images[i]),
          ),
        for (var i = 0; i < _attachments.length; i++)
          Builder(
            builder: (context) {
              final forge = AttachmentPill.forgeLabels(_attachments[i]);
              return AttachmentPill(
                key: ValueKey('attachment:$i'),
                disabled: busy,
                removeLabel: forge != null
                    ? tr(
                        context,
                        'composer.attachments.removeGithub',
                        args: {'kind': forge.kind, 'number': forge.number},
                      )
                    : tr(context, 'composer.attachments.removeFile'),
                onRemove: () {
                  _forgeAutoAttach.markRemoved(_attachments[i]);
                  setState(() => _attachments = List.of(_attachments)..removeAt(i));
                },
                child: AttachmentPill.label(context, _attachments[i]),
              );
            },
          ),
        if (_uploadName != null)
          Padding(
            padding: const EdgeInsets.only(top: overhang, left: overhang),
            child: AttachmentPill(
              child: AttachmentPill.pendingLabel(context, _uploadName!),
            ),
          ),
      ],
    );
  }

  Future<void> _create() => _action(() async {
    final cwd = _path.text.trim();
    if (cwd.isEmpty) throw StateError('Choose a remote directory.');
    // Original `isEmptyWorkspaceSubmission`: no prompt and no attachments creates
    // the workspace alone (`runCreateEmptyWorkspace`) and opens it.
    final hasInput = _prompt.text.trim().isNotEmpty ||
        _images.isNotEmpty ||
        _attachments.isNotEmpty;
    final withAgent = _withAgent && (_existing || hasInput);
    if (withAgent && _catalogCwd != cwd) {
      _directoryDebounce?.cancel();
      setState(() => _catalogCwd = cwd);
      throw StateError(
        'The directory changed. Review the refreshed directory-scoped provider configuration before creating.',
      );
    }
    if (withAgent && _config.text('provider').isEmpty) {
      throw StateError(tr(context, 'newWorkspace.errors.selectModel'));
    }
    if (!_existing && (_source == 'clone' || _source == 'new-directory')) {
      throw StateError('Prepare the directory before creating a session.');
    }
    if (!_existing &&
        (_source == 'branch-off' || _source == 'checkout') &&
        _ref.text.trim().isNotEmpty) {
      final validation = await _client!.request('validate_branch_request', {
        'cwd': cwd,
        'branchName': _ref.text.trim(),
      }, responseType: 'validate_branch_response');
      requireWorkspaceSuccess(validation);
      if (validation['exists'] != true) {
        throw StateError('The selected branch or base ref does not exist.');
      }
    }
    if (!_existing && _source == 'checkout' && _ref.text.trim().isEmpty) {
      throw StateError('Choose the branch to check out.');
    }
    if (!_existing &&
        _source == 'pr' &&
        (int.tryParse(_pr.text.trim()) ?? 0) < 1) {
      throw StateError('Enter a positive pull/merge request number.');
    }
    // new-workspace-screen.tsx createWorkspaceChatAgent: the effective model,
    // mode and thinking option; the selection is remembered on submit
    // (persistFormPreferences).
    final effective = effectiveSubmitConfig(_config, _providerEntries);
    final localFeatures = jsonMap(effective.remove('featureValues'));
    if (withAgent) {
      _rememberPreferences(rememberSubmittedSelection(_formPreferences, effective, _providerEntries));
      // use-draft-agent-features.ts: the features offered for the effective
      // selection, the composer's own values over the remembered ones (none
      // when the feature list cannot be loaded).
      var offered = const <JsonMap>[];
      try {
        final result = await settingsRequest(
          _client!,
          'list_provider_features_request',
          {'draftConfig': {...effective, 'cwd': cwd}},
          responseType: 'list_provider_features_response',
        );
        offered = jsonList(result['features']).map(jsonMap).toList();
      } catch (_) {}
      final featureValues = resolveFeatureValues(
        offered,
        rememberedFeatureValues(_formPreferences, effective.text('provider')),
        localFeatures,
      );
      if (featureValues.isNotEmpty) effective['featureValues'] = featureValues;
    }
    final config = <String, dynamic>{
      ...effective,
      'cwd': cwd,
      if (_title.text.trim().isNotEmpty) 'title': _title.text.trim(),
    };
    final prompt = _prompt.text.trim();
    final agent = <String, dynamic>{
      'config': config,
      if (prompt.isNotEmpty) 'initialPrompt': prompt,
      if (hasInput) 'clientMessageId': const Uuid().v4(),
      if (_images.isNotEmpty)
        'images': [
          for (final image in _images)
            {'data': image['data'], 'mimeType': image['mimeType']},
        ],
      if (_attachments.isNotEmpty) 'attachments': _attachments,
    };
    final source = _source == 'directory'
        ? <String, dynamic>{'kind': 'directory', 'path': cwd}
        : <String, dynamic>{
            'kind': 'worktree',
            'cwd': cwd,
            'action': _source == 'branch-off' ? 'branch-off' : 'checkout',
            if ((_source == 'branch-off' || _source == 'checkout') &&
                _ref.text.trim().isNotEmpty)
              'refName': _ref.text.trim()
            // Original: with no pick the default base item's ref is sent.
            else if (_source == 'branch-off' && _defaultRef != null)
              'refName': _defaultRef!.refName,
            if (_source == 'branch-off' && _branch.text.trim().isNotEmpty)
              'branchName': _branch.text.trim(),
            if (_source == 'pr')
              'checkoutSource': {
                'kind': 'change_request',
                'number': int.parse(_pr.text.trim()),
                if (_forge.text.trim().isNotEmpty) 'forge': _forge.text.trim(),
                if (_projectPath.text.trim().isNotEmpty)
                  'projectPath': _projectPath.text.trim(),
              },
          };
    final kind = _existing ? 'agent' : 'workspace';
    final request = <String, dynamic>{
      'idempotencyKey': const Uuid().v4(),
      'subscribe': false,
      if (_existing) ...{
        ...agent,
        'workspaceId': widget.target.workspaceId,
      } else ...{
        'source': source,
        if (withAgent) 'agent': agent,
        if (_title.text.trim().isNotEmpty) 'title': _title.text.trim(),
        // buildFirstAgentContext: the trimmed prompt and attachments name the
        // workspace (and its branch).
        if (hasInput)
          'firstAgentContext': {
            if (prompt.isNotEmpty) 'prompt': prompt,
            'attachments': _attachments,
          },
      },
    };
    final intent = <String, dynamic>{'kind': kind, 'request': request};
    // Persist the exact immutable intent before submission. Recovery never creates
    // another key or silently replays a changed form after an uncertain outcome.
    await _persistIntent(intent);
    if (!mounted) return;
    setState(() {
      _intent = intent;
      _snapshot = null;
      _opened = false;
    });
    await _submit();
  }, toast: true);

  Future<void> _submit() async {
    final intent = _intent;
    if (intent == null) return;
    final kind = intent.text('kind');
    final result = await _client!.request(
      '$kind.create.request',
      intent.object('request'),
      responseType: '$kind.create.response',
      timeout: const Duration(minutes: 10),
    );
    if (!mounted) return;
    if (result['creation'] != null) _receive(result.object('creation'));
    requireWorkspaceSuccess(result);
    await _finish(result);
    if (_intent != null &&
        !_finishing &&
        !_opened &&
        _recoveryObservation == null &&
        _snapshot?.text('phase') != 'failed') {
      await _observeReceipt(replayIfMissing: false);
    }
  }

  void _receive(JsonMap message) {
    if (!mounted || _intent == null) return;
    final payload = workspacePayload(message);
    if (message.text('type') == 'subscription.error') {
      setState(
        () => _error = payload.text(
          'error',
          'Creation observation failed. Recover the receipt to continue.',
        ),
      );
      return;
    }
    final snapshot = payload.containsKey('snapshot')
        ? payload.object('snapshot')
        : payload;
    if (snapshot.isEmpty ||
        snapshot.text('idempotencyKey') !=
            _intent!.object('request').text('idempotencyKey')) {
      return;
    }
    if (_snapshot != null &&
        snapshot.integer('revision') < _snapshot!.integer('revision')) {
      return;
    }
    setState(() {
      _snapshot = snapshot;
    });
    if (snapshot.text('phase') == 'completed') {
      _finish({
        'workspace': snapshot['workspace'],
        'agent': snapshot['agent'],
        'creation': snapshot,
      });
    } else if (snapshot.text('phase') == 'failed') {
      setState(
        () => _error = snapshot.text(
          'error',
          'Creation failed. Inspect any created workspace before starting again.',
        ),
      );
    }
  }

  Future<void> _recover() async {
    if (_intent == null || _busy || _finishing) return;
    await _action(() => _observeReceipt(replayIfMissing: true));
  }

  Future<void> _observeReceipt({required bool replayIfMissing}) async {
    final intent = _intent;
    if (intent == null) return;
    await _recoveryObservation?.close();
    _recoveryObservation = null;
    final observation = await _client!.observe(
      type: 'creation.subscribe.request',
      fields: {
        'kind': intent.text('kind'),
        'idempotencyKey': intent.object('request').text('idempotencyKey'),
        'subscribe': true,
      },
      responseType: 'creation.subscribe.response',
      onEvent: _receive,
    );
    if (!mounted || _intent == null) {
      await observation.close();
      return;
    }
    _recoveryObservation = observation;
    final payload = workspacePayload(observation.snapshot);
    requireWorkspaceSuccess(payload);
    if (payload['snapshot'] == null) {
      if (replayIfMissing) await _submit();
    } else {
      _receive(payload);
    }
  }

  Future<void> _finish(JsonMap result) async {
    if (_finishing || _opened || !mounted) return;
    final receipt = result.object('creation');
    if (receipt.isNotEmpty && receipt.text('phase') != 'completed') return;
    final workspace = result.object('workspace').isNotEmpty
        ? result.object('workspace')
        : receipt.object('workspace');
    final agent = result.object('agent').isNotEmpty
        ? result.object('agent')
        : receipt.object('agent');
    final workspaceId = workspace.text(
      'id',
      receipt.text(
        'workspaceId',
        agent.text('workspaceId', widget.target.workspaceId),
      ),
    );
    final agentId = agent.text('id', receipt.text('agentId'));
    final expectedAgent =
        _intent?.text('kind') == 'agent' ||
        _intent?.object('request').containsKey('agent') == true;
    if (workspaceId.isEmpty || (agentId.isEmpty && expectedAgent)) return;
    _finishing = true;
    try {
      final store = PaseoScope.of(context);
      await store.refreshHost(widget.target.serverId);
      if (!mounted) return;
      await _persistIntent(null);
      await _recoveryObservation?.close();
      _recoveryObservation = null;
      if (!mounted) return;
      final cwd = agent.text(
        'cwd',
        workspace.text(
          'workspaceDirectory',
          widget.target.cwd.isEmpty ? _path.text.trim() : widget.target.cwd,
        ),
      );
      setState(() {
        _opened = true;
        _intent = null;
      });
      store.open(
        PanelTarget(
          kind: agentId.isEmpty ? 'setup' : 'agent',
          serverId: widget.target.serverId,
          workspaceId: workspaceId,
          cwd: cwd,
          id: agentId.isEmpty ? null : agentId,
          title: agent.text('title', workspace.text('name', 'Session')),
          data: {'repoRoot': workspace.text('projectRootPath', cwd)},
        ),
      );
    } catch (error) {
      if (mounted) setState(() => _error = error);
    } finally {
      _finishing = false;
    }
  }

  Future<void> _discardFailedIntent() async {
    if (_snapshot?.text('phase') != 'failed') return;
    if (!await confirmAction(
          context,
          title: 'Start a new creation attempt?',
          message: 'The host reported failure. Any partial workspace or agent remains available below; a new attempt uses a new identity.',
          confirmLabel: 'New attempt',
        ) ||
        !mounted) {
      return;
    }
    await _action(() async {
      await _persistIntent(null);
      await _recoveryObservation?.close();
      _recoveryObservation = null;
      if (mounted) {
        setState(() {
          _intent = null;
          _snapshot = null;
        });
      }
    });
  }

  @override
  void dispose() {
    _directoryDebounce?.cancel();
    _forgeAutoAttach.dispose();
    _prompt.removeListener(_syncForgeAutoAttach);
    _client?.removeListener(_connectionChanged);
    _recoveryObservation?.close();
    for (final controller in [
      _path,
      _title,
      _prompt,
      _branch,
      _ref,
      _pr,
      _forge,
      _projectPath,
      _repo,
      _directoryName,
    ]) {
      controller.dispose();
    }
    super.dispose();
  }

  /// Original `new-workspace-screen.tsx` `NewWorkspaceLayout` + `ComposerDock`:
  /// desktop centres a 20px title (24px left inset, 32px below), a 28px badge
  /// row (16px inset, 8px gap, 16px below) and the composer, with the Import
  /// session pill under it. Compact has no title: free space pushes a stack of
  /// 56px fields (16px inset, 8px gap, 16px below) down onto the composer, and
  /// Import session moves to the top-right ghost button.
  @override
  Widget build(BuildContext context) {
    if (_client == null) return const LoadingState();
    final pending = _intent != null;
    final store = PaseoScope.of(context);
    final host = store.hostFor(widget.target.serverId);
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final compact = isCompactLayout(context);
    final addProject = widget.target.data.flag('addProject');
    final projects = [
      for (final project in host?.projects ?? const <JsonMap>[])
        if (_projectRoot(project).isNotEmpty) project,
    ];
    final selectedProject = projects
        .where((project) => _projectRoot(project) == _path.text.trim())
        .firstOrNull;
    final projectName = selectedProject != null
        ? _projectName(selectedProject)
        : _path.text.isEmpty
        ? 'Choose project'
        : _path.text.split(RegExp(r'[/\\]')).where((s) => s.isNotEmpty).lastOrNull ??
              _path.text;
    final showBranch =
        !_existing && (_source == 'branch-off' || _source == 'checkout');
    final canCreate =
        !_busy &&
        !pending &&
        !(!_existing && (_source == 'clone' || _source == 'new-directory')) &&
        // waitForForgeAutoAttachOnSubmit: pasted forge links resolve first.
        !_forgeAutoAttach.isResolving;
    final projectField = _FieldTrigger(
      compact: compact,
      label: tr(context, 'newWorkspace.fields.project'),
      semanticLabel: 'Workspace project',
      tooltip: tr(context, 'newWorkspace.tooltips.project'),
      icon: selectedProject == null
          ? Icon(LucideIcons.folder300, size: 14, color: colors.muted)
          : ProjectIconSquare(
              name: projectName,
              viewKey: projectViewKey(widget.target.serverId, selectedProject),
              size: 16,
            ),
      value: projectName,
      disabled: _existing || pending,
      onTap: (anchor) => _pickProject(anchor, projects),
    );
    final hostField = _FieldTrigger(
      compact: compact,
      label: tr(context, 'newWorkspace.fields.host'),
      tooltip: tr(context, 'newWorkspace.tooltips.host'),
      icon: _HostStatusDot(status: host?.client.status),
      value: host?.label ?? 'Host',
      disabled: _existing || pending,
      onTap: _pickHost,
    );
    final branchField = _FieldTrigger(
      compact: compact,
      label: tr(context, 'newWorkspace.fields.branch'),
      semanticLabel: tr(context, 'newWorkspace.refPicker.startingRef'),
      tooltip: tr(context, 'newWorkspace.tooltips.startingRef'),
      icon: Icon(LucideIcons.gitBranch300, size: 14, color: colors.muted),
      // Original `triggerLabel`: the picked item's label, else the default
      // base (current branch) item, else "main".
      value: _ref.text.trim().isNotEmpty
          ? (_refLabel ?? _ref.text.trim())
          : _source == 'checkout'
          ? tr(context, 'newWorkspace.refPicker.startingRef')
          : (_defaultRef?.name ?? 'main'),
      disabled: pending || _path.text.trim().isEmpty,
      onTap: _pickRef,
    );
    final launchField = _FieldTrigger(
      compact: compact,
      label: tr(context, 'newWorkspace.launch.title'),
      semanticLabel: tr(context, 'newWorkspace.tooltips.launch'),
      tooltip: tr(context, 'newWorkspace.tooltips.launch'),
      icon: _glyph(LucideIcons.messageCircle300, colors.muted),
      value: tr(context, 'newWorkspace.launch.chat'),
      disabled: pending,
      onTap: _pickLaunch,
    );
    final showLaunch = !_existing && !addProject;
    // Original `useImportSession({ serverId })`: the host's sessions, unscoped.
    final importSession = _ImportSessionButton(
      compact: compact,
      onPressed: () => showImportSessions(
        context,
        store: store,
        serverId: widget.target.serverId,
      ),
    );
    // The add-project flow keeps its directory options behind this toggle; a new
    // session has no options row (original NewWorkspaceLayout).
    final optionsToggle = _pillOrGhost(
      context,
      pill: !compact,
      child: PaseoButton(
        label: 'Workspace options',
        icon: LucideIcons.slidersHorizontal300,
        trailing: Icon(
          _optionsOpen ? LucideIcons.chevronUp300 : LucideIcons.chevronDown300,
          size: 14,
        ),
        variant: PaseoButtonVariant.ghost,
        size: PaseoControlSize.sm,
        onPressed: () => setState(() => _optionsOpen = !_optionsOpen),
      ),
    );
    final form = Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        if (pending) _pendingCard(context),
        if (_projectAdded)
          _notice('Project added. It is available in the sidebar.'),
        if (_opened) _notice('Session created. It is open in the tab bar.'),
        if (!compact)
          Padding(
            padding: const EdgeInsets.fromLTRB(24, 0, 16, 32),
            child: Text(
              tr(
                context,
                addProject
                    ? 'sidebar.actions.addProject'
                    : 'sessionNavigation.newSession',
              ),
              style: theme.textTheme.bodyMedium?.copyWith(
                fontSize: type.uiSize(20),
                fontWeight: FontWeight.normal,
                color: theme.colorScheme.onSurface,
              ),
            ),
          ),
        AbsorbPointer(
          absorbing: _busy || pending,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 0, 16, 16),
                child: compact
                    ? Column(
                        crossAxisAlignment: CrossAxisAlignment.stretch,
                        spacing: 8,
                        children: [
                          projectField,
                          hostField,
                          if (showBranch) branchField,
                          if (showLaunch) launchField,
                        ],
                      )
                    : ConstrainedBox(
                        constraints: const BoxConstraints(minHeight: 28),
                        child: Row(
                          spacing: 8,
                          children: [
                            Flexible(child: projectField),
                            Flexible(child: hostField),
                            if (showBranch) Flexible(child: branchField),
                            const Spacer(),
                            if (showLaunch) launchField,
                          ],
                        ),
                      ),
              ),
              // Original `inputAreaContainer`: 16px sides and bottom.
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 0, 16, 16),
                child: _composer(context, canCreate: canCreate),
              ),
              if (_error != null)
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 0, 16, 16),
                  child: SelectableText(
                    errorMessage(_error!),
                    style: theme.textTheme.bodyMedium?.copyWith(
                      color: theme.colorScheme.error,
                      height: 20 / 14,
                    ),
                  ),
                ),
              if (!compact || addProject)
                Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 16),
                  child: Wrap(
                    spacing: 8,
                    runSpacing: 8,
                    children: [
                      // Original ImportSessionButton: under the composer on wide
                      // layouts; compact layouts carry it in the header.
                      if (!compact) importSession,
                      if (addProject) optionsToggle,
                    ],
                  ),
                ),
              if (addProject && _optionsOpen)
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 16, 16, 0),
                  child: _options(context),
                ),
            ],
          ),
        ),
      ],
    );
    return ColoredBox(
      color: colors.surface0,
      child: Column(
        children: [
          Expanded(
            child: LayoutBuilder(
              builder: (context, constraints) {
                // Original ComposerDock: the centred (wide) viewport keeps a
                // HEADER_INNER_HEIGHT + 24 = 60px bottom inset so the form sits
                // a little above centre; compact docks at the bottom edge.
                final safeBottom = MediaQuery.paddingOf(context).bottom;
                final top = compact ? 16.0 : 0.0;
                final bottom = compact ? safeBottom : 60 + safeBottom;
                return SingleChildScrollView(
                padding: EdgeInsets.only(top: top, bottom: bottom),
                child: ConstrainedBox(
                  constraints: BoxConstraints(
                    minHeight: (constraints.maxHeight - top - bottom).clamp(
                      0.0,
                      double.infinity,
                    ),
                  ),
                  child: Align(
                    // Original ComposerDock: centred on wide, bottom on compact.
                    alignment: compact
                        ? Alignment.bottomCenter
                        : Alignment.center,
                    child: ConstrainedBox(
                      constraints: BoxConstraints(maxWidth: type.contentMaxWidth),
                      child: form,
                    ),
                  ),
                ),
              );
              },
            ),
          ),
        ],
      ),
    );
  }

  /// Original Import session pill: the ghost sm button with a full-radius
  /// borderAccent outline (pill only off compact).
  static Widget _pillOrGhost(
    BuildContext context, {
    required bool pill,
    required Widget child,
  }) => pill
      ? Container(
          foregroundDecoration: BoxDecoration(
            border: Border.all(color: PaseoColors.of(context).borderAccent),
            borderRadius: BorderRadius.circular(999),
          ),
          child: child,
        )
      : child;

  Widget _notice(String text) => Padding(
    padding: const EdgeInsets.fromLTRB(16, 0, 16, 12),
    child: Text(
      text,
      style: Theme.of(context).textTheme.bodyMedium?.copyWith(
        color: PaseoColors.of(context).muted,
      ),
    ),
  );

  /// Original `composer/input/input.tsx` `inputWrapper`: surface1, 1px
  /// borderAccent, 16px radius, 8/12 (xs) or 16/16/12 (md ≥720) insets, 12px
  /// gap, sm shadow on phones; a button row bleeding 6px past the inset and a
  /// 28px accent submit (return glyph; labelled pill when it reads Create).
  Widget _composer(BuildContext context, {required bool canCreate}) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final compact = isCompactLayout(context);
    final wide = MediaQuery.sizeOf(context).width >= 720;
    final inset = wide ? 16.0 : 12.0;
    final top = wide ? 16.0 : 8.0;
    final hasTray = _images.isNotEmpty || _attachments.isNotEmpty || _uploadName != null;
    final dark = theme.brightness == Brightness.dark;
    final submit = _SubmitButton(
      label: _withAgent ? null : tr(context, 'newWorkspace.create'),
      semanticLabel: tr(context, 'newWorkspace.create'),
      glyph: compact ? 20 : 16,
      loading: _busy || (_intent != null && !_opened),
      onPressed: canCreate ? _create : null,
    );
    return Container(
      decoration: BoxDecoration(
        color: colors.surface1,
        border: Border.all(color: colors.borderAccent),
        borderRadius: BorderRadius.circular(16),
        boxShadow: compact
            ? [
                BoxShadow(
                  color: dark ? const Color(0x40000000) : const Color(0x14000000),
                  offset: const Offset(0, 2),
                  blurRadius: dark ? 4 : 8,
                ),
              ]
            : null,
      ),
      padding: EdgeInsets.only(bottom: wide ? 12 : 8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 12,
        children: [
          if (hasTray)
            Padding(
              padding: EdgeInsets.fromLTRB(
                inset - AttachmentPill.removableOverhang,
                top - AttachmentPill.removableOverhang,
                inset,
                0,
              ),
              child: _attachmentTray(),
            ),
          Padding(
            padding: EdgeInsets.fromLTRB(inset, hasTray ? 0 : top, inset, 0),
            child: ConstrainedBox(
              constraints: BoxConstraints(minHeight: compact ? 30 : 46),
              child: _withAgent
                  // Original desktop `handleDesktopKeyPress`: Enter submits,
                  // Shift+Enter and IME composition insert; phones only use the
                  // send button. Autofocus is desktop-only (fine pointer).
                  ? Focus(
                      canRequestFocus: false,
                      skipTraversal: true,
                      onKeyEvent: (node, event) {
                        if (compact ||
                            event is! KeyDownEvent ||
                            (event.logicalKey != LogicalKeyboardKey.enter &&
                                event.logicalKey !=
                                    LogicalKeyboardKey.numpadEnter) ||
                            HardwareKeyboard.instance.isShiftPressed ||
                            _prompt.value.composing.isValid ||
                            !canCreate) {
                          return KeyEventResult.ignored;
                        }
                        _create();
                        return KeyEventResult.handled;
                      },
                      child: TextField(
                      controller: _prompt,
                      autofocus: !compact,
                      minLines: 1,
                      maxLines: 12,
                      style: theme.textTheme.bodyLarge?.copyWith(
                        fontSize: type.contentFontSize,
                        height: 1.4,
                        color: theme.colorScheme.onSurface,
                      ),
                      decoration: InputDecoration(
                        hintText: tr(
                          context,
                          compact
                              ? 'composer.placeholders.mobile'
                              : 'composer.placeholders.desktop',
                        ),
                        hintStyle: TextStyle(color: colors.surface4),
                        isDense: true,
                        filled: false,
                        border: InputBorder.none,
                        enabledBorder: InputBorder.none,
                        focusedBorder: InputBorder.none,
                        contentPadding: EdgeInsets.zero,
                      ),
                      ),
                    )
                  : Text(
                      _title.text.isEmpty
                          ? tr(context, 'newWorkspace.title')
                          : _title.text,
                      style: theme.textTheme.bodyLarge?.copyWith(
                        fontSize: type.contentFontSize,
                        height: 1.4,
                        color: colors.muted,
                      ),
                    ),
            ),
          ),
          Padding(
            padding: EdgeInsets.symmetric(horizontal: inset - 6),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.end,
              children: [
                Expanded(
                  child: _withAgent
                      // Original input.tsx `leftContent`: the attach Plus, then
                      // the model controls, left-aligned on the button row.
                      ? Row(
                          crossAxisAlignment: CrossAxisAlignment.end,
                          children: [
                            _attachButton(),
                            Flexible(
                              child: AgentConfigFields(
                                compact: true,
                                client: _client!,
                                cwd: _catalogCwd,
                                value: _config,
                                onChanged: _changeConfig,
                              ),
                            ),
                          ],
                        )
                      : const SizedBox.shrink(),
                ),
                submit,
              ],
            ),
          ),
        ],
      ),
    );
  }

  Widget _pendingCard(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(16, 0, 16, 16),
    child: SettingsCard(
      children: [
        SettingsRow(
          label: 'Creation: ${_snapshot?.text('phase') ?? 'awaiting confirmation'}',
          hint: [
            'Receipt: ${_intent!.object('request').text('idempotencyKey')}',
            'The original request is retained. Recover its receipt instead of creating a duplicate.',
            if (_snapshot?.flag('outcomeUnknown') == true)
              'The host reports an unknown outcome. Inspect the created resources before proceeding.',
          ].join('\n'),
        ),
        Wrap(
          spacing: 8,
          runSpacing: 8,
          children: [
            PaseoButton(
              label: 'Recover / refresh creation',
              variant: PaseoButtonVariant.primary,
              size: PaseoControlSize.sm,
              loading: _busy,
              onPressed: _busy ? null : _recover,
            ),
            if (_snapshot?.text('workspaceId').isNotEmpty == true)
              settingsButton(context, 'Open created workspace', () {
                final workspace = _snapshot!.object('workspace');
                PaseoScope.of(context).open(
                  PanelTarget(
                    kind: 'setup',
                    serverId: widget.target.serverId,
                    workspaceId: _snapshot!.text('workspaceId'),
                    cwd: workspace.text('workspaceDirectory', _path.text),
                    data: {
                      'repoRoot': workspace.text('projectRootPath', _path.text),
                    },
                  ),
                );
              }),
            if (_snapshot?.text('agentId').isNotEmpty == true)
              settingsButton(context, 'Open created session', () {
                PaseoScope.of(context).open(
                  PanelTarget(
                    kind: 'agent',
                    serverId: widget.target.serverId,
                    workspaceId: _snapshot!.text(
                      'workspaceId',
                      widget.target.workspaceId,
                    ),
                    cwd: _snapshot!.object('agent').text('cwd', _path.text),
                    id: _snapshot!.text('agentId'),
                  ),
                );
              }),
            if (_snapshot?.text('phase') == 'failed')
              settingsButton(
                context,
                'Start new attempt',
                _busy ? null : _discardFailedIntent,
                ghost: true,
              ),
          ],
        ),
      ],
    ),
  );

  /// Original `FormTextInput` (surface2, md 44px) for the option fields.
  Widget _input(
    TextEditingController controller, {
    String? hint,
    Widget? suffix,
    bool readOnly = false,
    TextInputType? keyboardType,
    ValueChanged<String>? onChanged,
    ValueChanged<String>? onSubmitted,
    VoidCallback? onEditingComplete,
  }) => TextField(
    controller: controller,
    readOnly: readOnly,
    keyboardType: keyboardType,
    style: Theme.of(context).textTheme.bodyMedium?.copyWith(height: 1.4),
    decoration: paseoInputDecoration(
      context,
      hintText: hint,
      suffixIcon: suffix,
    ),
    onChanged: onChanged,
    onSubmitted: onSubmitted,
    onEditingComplete: onEditingComplete,
  );

  /// Inline search glyph inside a field (original 28px ghost icon button).
  Widget _fieldAction(
    IconData icon,
    String tooltip,
    void Function(BuildContext anchor) onPressed,
  ) => Builder(
    builder: (anchor) => PaseoButton(
      icon: icon,
      tooltip: tooltip,
      variant: PaseoButtonVariant.ghost,
      size: PaseoControlSize.xs,
      onPressed: () => onPressed(anchor),
    ),
  );

  /// Original switch rows: base label growing beside the 34x20 switch.
  Widget _switchRow(String label, bool value, ValueChanged<bool> onChanged) =>
      Row(
        spacing: 12,
        children: [
          Expanded(
            child: Text(label, style: Theme.of(context).textTheme.bodyMedium),
          ),
          PaseoSwitch(value: value, onChanged: onChanged, semanticLabel: label),
        ],
      );

  /// Source, directory, ref, title and profile options that the original
  /// spread over its pickers; kept together so every creation path stays.
  /// Each is an original `FormField` (muted label, 8px gap) with 16px between.
  Widget _options(BuildContext context) {
    final colors = PaseoColors.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      spacing: 16,
      children: [
        if (widget.target.workspaceId.isNotEmpty)
          _switchRow(
            'Use this existing workspace',
            _existing,
            (value) => setState(() {
              _existing = value;
              if (value) _withAgent = true;
            }),
          ),
        if (!_existing)
          PaseoFormField(
            label: tr(context, 'newWorkspace.isolation.label'),
            child: PaseoSelectField<String>(
              value: _source,
              title: tr(context, 'newWorkspace.isolation.label'),
              options: [
                PaseoSelectOption(
                  'directory',
                  'Existing checkout or directory',
                  leading: _glyph(LucideIcons.folder300, colors.muted),
                ),
                PaseoSelectOption(
                  'branch-off',
                  'New worktree: branch off',
                  leading: _glyph(LucideIcons.gitBranch300, colors.muted),
                ),
                PaseoSelectOption(
                  'checkout',
                  'New worktree: existing branch',
                  leading: _glyph(LucideIcons.gitBranch300, colors.muted),
                ),
                PaseoSelectOption(
                  'pr',
                  'New worktree: pull / merge request',
                  leading: _glyph(LucideIcons.gitPullRequest300, colors.muted),
                ),
                PaseoSelectOption(
                  'clone',
                  'Clone GitHub repository',
                  leading: _glyph(LucideIcons.download300, colors.muted),
                ),
                PaseoSelectOption(
                  'new-directory',
                  'Create remote directory',
                  leading: _glyph(LucideIcons.folderPlus300, colors.muted),
                ),
              ],
              onChanged: (value) => setState(() {
                // Original `clearPickerSelectionForTargetChange`.
                if (value != _source) {
                  _ref.clear();
                  _refLabel = null;
                }
                _source = value;
              }),
            ),
          ),
        PaseoFormField(
          label: _source == 'new-directory'
              ? 'Parent directory'
              : _source == 'clone'
              ? 'Clone target directory'
              : 'Remote directory',
          child: Row(
            spacing: 8,
            children: [
              Expanded(
                child: _input(
                  _path,
                  readOnly: _existing,
                  onChanged: (value) {
                    _directoryDebounce?.cancel();
                    _directoryDebounce = Timer(
                      const Duration(milliseconds: 350),
                      () {
                        if (!mounted) return;
                        setState(() => _catalogCwd = value.trim());
                        _loadDefaultRef();
                        _loadProviderEntries();
                        _syncForgeAutoAttach();
                      },
                    );
                  },
                  onSubmitted: (value) => _setDirectory(value.trim()),
                  onEditingComplete: () => _setDirectory(_path.text.trim()),
                ),
              ),
              PaseoButton(
                icon: LucideIcons.folderOpen300,
                tooltip: 'Browse remote directories',
                variant: PaseoButtonVariant.outline,
                onPressed: _existing ? null : _browse,
              ),
            ],
          ),
        ),
        if (!_existing && (_source == 'branch-off' || _source == 'checkout'))
          PaseoFormField(
            label: _source == 'checkout'
                ? 'Branch to check out'
                : tr(context, 'newWorkspace.refPicker.startingRef'),
            child: _input(
              _ref,
              onChanged: (_) => setState(() => _refLabel = null),
              suffix: _fieldAction(
                LucideIcons.search300,
                tr(context, 'newWorkspace.refPicker.searchPlaceholder'),
                _pickRef,
              ),
            ),
          ),
        if (!_existing && _source == 'branch-off')
          PaseoFormField(
            label: 'New branch name',
            hint: 'Automatic if empty',
            child: _input(_branch),
          ),
        if (!_existing && _source == 'pr') ...[
          PaseoFormField(
            label: 'Pull / merge request number',
            child: _input(_pr, keyboardType: TextInputType.number),
          ),
          PaseoFormField(
            label: 'Forge',
            hint: 'Optional; the host detects the repository',
            child: _input(_forge),
          ),
          PaseoFormField(
            label: 'Forge project path',
            hint: 'Optional',
            child: _input(_projectPath),
          ),
        ],
        if (!_existing && _source == 'clone') ...[
          PaseoFormField(
            label: 'Repository or search query',
            child: _input(
              _repo,
              suffix: _fieldAction(
                LucideIcons.search300,
                'Search GitHub repositories',
                _searchRepositories,
              ),
            ),
          ),
          PaseoFormField(
            label: 'Clone protocol',
            child: PaseoSegmentedControl<String>(
              segments: const [
                PaseoSegment('https', 'HTTPS'),
                PaseoSegment('ssh', 'SSH'),
              ],
              value: _cloneProtocol,
              onChanged: (value) => setState(() => _cloneProtocol = value),
            ),
          ),
        ],
        if (!_existing && _source == 'new-directory')
          PaseoFormField(
            label: 'New directory name',
            child: _input(_directoryName),
          ),
        if (!_existing && (_source == 'clone' || _source == 'new-directory')) ...[
          if (_preparationUnknown)
            Text(
              'The directory operation has an unknown outcome. Browse the target and select the existing directory if it completed; do not blindly repeat the operation.',
              style: Theme.of(context).textTheme.bodyMedium?.copyWith(
                color: colors.muted,
              ),
            ),
          Wrap(
            spacing: 8,
            runSpacing: 8,
            children: [
              PaseoButton(
                label: _source == 'clone' ? 'Clone repository' : 'Create directory',
                icon: LucideIcons.folderPlus300,
                variant: PaseoButtonVariant.primary,
                size: PaseoControlSize.sm,
                loading: _busy,
                onPressed: _preparationUnknown ? null : _prepareDirectory,
              ),
              if (_preparationUnknown)
                settingsButton(
                  context,
                  'Inspect and select prepared directory',
                  () async {
                    if (await _browse() && mounted) {
                      setState(() {
                        _source = 'directory';
                        _preparationUnknown = false;
                      });
                    }
                  },
                  ghost: true,
                ),
            ],
          ),
        ],
        PaseoFormField(
          label: tr(context, 'newWorkspace.titlePlaceholder'),
          child: _input(_title, onChanged: (_) => setState(() {})),
        ),
        if (!_existing)
          _switchRow(
            tr(context, 'newWorkspace.launch.chat'),
            _withAgent,
            (value) => setState(() => _withAgent = value),
          ),
        if (_withAgent)
          PaseoFormField(
            label: 'Agent profile',
            child: PaseoSelectField<String>(
              value: _profileId ?? '',
              title: 'Agent profile',
              options: [
                const PaseoSelectOption('', 'Custom configuration'),
                for (final profile in _profiles)
                  PaseoSelectOption(profile.text('id'), profile.text('name')),
              ],
              onChanged: (id) {
                if (id.isEmpty) {
                  setState(() => _profileId = id);
                  return;
                }
                // resolve-agent-form.ts applyProfile and preferences.ts
                // applyAgentProfilePreferences.
                final profile = _profiles.firstWhere((p) => p.text('id') == id);
                final preferences = _formPreferences;
                final next = applyProfileChoice(
                  profile: {
                    for (final key in ['provider', 'model', 'modeId', 'thinkingOptionId'])
                      key: profile.text(key).trim(),
                    'featureValues': profile.object('featureValues'),
                  },
                  preferences: preferences,
                  providers: _providerEntries,
                );
                _rememberPreferences(rememberProfileSelection(
                  preferences,
                  previousProvider: _config.text('provider'),
                  providers: _providerEntries,
                  config: next,
                  profileFeatureValues: profile.object('featureValues'),
                ));
                setState(() {
                  _profileId = id;
                  _config = next;
                  _configTouched = true;
                });
              },
            ),
          ),
        if (widget.target.data.flag('addProject') &&
            !_existing &&
            _source == 'directory')
          Align(
            alignment: Alignment.centerLeft,
            child: settingsButton(
              context,
              'Add project without a workspace',
              _busy || _intent != null ? null : _addProject,
              icon: LucideIcons.folderOpen300,
            ),
          ),
      ],
    );
  }
}

/// Original `new-workspace-picker-item.ts` branch `PickerItem`.
class _BranchItem {
  const _BranchItem({
    required this.name,
    required this.refName,
    required this.branchName,
    required this.semanticLabel,
    this.divergence,
  });

  /// `pickerItemLabel`: the branch name, `name (local)` when both refs show.
  final String name;

  /// Sent as the worktree `refName` (`refs/remotes/origin/x`, `refs/heads/x`).
  final String refName;

  /// The plain branch name (used by the Flutter-only checkout source).
  final String branchName;
  final String semanticLabel;

  /// `divergenceLabel`: `+ahead −behind` of the local branch vs origin.
  final String? divergence;

  /// Original `branchNameFromRef`.
  static String branchNameFromRef(String refName) {
    if (refName.startsWith('refs/heads/')) {
      return refName.substring('refs/heads/'.length);
    }
    if (refName.startsWith('refs/remotes/')) {
      final remainder = refName.substring('refs/remotes/'.length);
      final separator = remainder.indexOf('/');
      return separator == -1 ? remainder : remainder.substring(separator + 1);
    }
    return refName;
  }

  static String? _divergenceLabel(int ahead, int behind) {
    final parts = [
      if (ahead > 0) '+$ahead',
      if (behind > 0) '−$behind',
    ];
    return parts.isEmpty ? null : parts.join(' ');
  }

  static String _commits(int count) => '$count ${count == 1 ? 'commit' : 'commits'}';

  static String _divergenceAccessibility(int ahead, int behind, String counterpart) {
    if (ahead == 0 && behind == 0) return 'up to date with $counterpart';
    if (ahead > 0 && behind > 0) {
      return '${_commits(ahead)} ahead and $behind behind $counterpart';
    }
    if (ahead > 0) return '${_commits(ahead)} ahead of $counterpart';
    return '${_commits(behind)} behind $counterpart';
  }

  /// Original `buildBranchPickerItems`: an origin row for remote branches and
  /// a local row when there is no remote or the two refs differ.
  static List<_BranchItem> build(List<JsonMap> details) {
    final items = <_BranchItem>[];
    for (final detail in details) {
      final name = detail.text('name');
      if (name.isEmpty) continue;
      final knownProvenance =
          detail['hasLocal'] != null || detail['hasRemote'] != null;
      if (!knownProvenance) {
        items.add(
          _BranchItem(
            name: name,
            refName: name,
            branchName: name,
            semanticLabel: '$name, branch',
          ),
        );
        continue;
      }
      final hasLocal = detail['hasLocal'] == true;
      final hasRemote = detail['hasRemote'] == true;
      final ahead = detail['localAhead'] is num
          ? (detail['localAhead'] as num).toInt()
          : null;
      final behind = detail['localBehind'] is num
          ? (detail['localBehind'] as num).toInt()
          : null;
      final hasDivergence =
          hasLocal && hasRemote && ahead != null && behind != null;
      final refsDiffer = hasDivergence && (ahead > 0 || behind > 0);
      final showsBothRefs = hasLocal && hasRemote && (!hasDivergence || refsDiffer);
      if (hasRemote) {
        items.add(
          _BranchItem(
            name: name,
            refName: 'refs/remotes/origin/$name',
            branchName: name,
            semanticLabel: '$name, origin branch',
          ),
        );
      }
      if (hasLocal && (showsBothRefs || !hasRemote)) {
        items.add(
          _BranchItem(
            name: showsBothRefs ? '$name (local)' : name,
            refName: 'refs/heads/$name',
            branchName: name,
            semanticLabel: hasDivergence
                ? '$name, local branch, ${_divergenceAccessibility(ahead, behind, 'origin $name')}'
                : '$name, local branch',
            divergence: hasDivergence ? _divergenceLabel(ahead, behind) : null,
          ),
        );
      }
    }
    return items;
  }
}

/// Original `HostStatusDot`: 8px dot, statusSuccess online, statusWarning
/// connecting, statusDanger offline.
class _HostStatusDot extends StatelessWidget {
  const _HostStatusDot({required this.status});
  final ConnectionStatus? status;
  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    return SizedBox.square(
      dimension: 16,
      child: Center(
        child: Container(
          width: 8,
          height: 8,
          decoration: BoxDecoration(
            shape: BoxShape.circle,
            color: switch (status) {
              ConnectionStatus.connected => colors.success,
              ConnectionStatus.connecting => colors.warning,
              _ => colors.danger,
            },
          ),
        ),
      ),
    );
  }
}

/// Original `ImportSessionButton`: ghost sm with the Import glyph; on wide
/// layouts a full-radius borderAccent pill under the composer.
class _ImportSessionButton extends StatelessWidget {
  const _ImportSessionButton({required this.compact, required this.onPressed});
  final bool compact;
  final VoidCallback onPressed;
  @override
  Widget build(BuildContext context) => _NewTabPanelState._pillOrGhost(
    context,
    pill: !compact,
    child: PaseoButton(
      label: tr(context, 'importSession.title'),
      icon: LucideIcons.import300,
      variant: PaseoButtonVariant.ghost,
      size: PaseoControlSize.sm,
      onPressed: onPressed,
    ),
  );
}

/// Original composer `sendButton`: a 28px accent circle (4px left margin)
/// with the accentForeground glyph, or a 12px-padded pill with base medium
/// text when labelled; 0.5 opacity while disabled.
class _SubmitButton extends StatelessWidget {
  const _SubmitButton({
    required this.semanticLabel,
    required this.glyph,
    required this.loading,
    required this.onPressed,
    this.label,
  });
  final String? label;
  final String semanticLabel;
  final double glyph;
  final bool loading;
  final VoidCallback? onPressed;
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final foreground = theme.colorScheme.onTertiary;
    final enabled = onPressed != null && !loading;
    final Widget content = loading
        ? SizedBox.square(
            dimension: 14,
            child: CircularProgressIndicator(strokeWidth: 2, color: foreground),
          )
        : label != null
        ? Text(
            label!,
            style: theme.textTheme.bodyMedium?.copyWith(
              fontSize: type.uiSize(14),
              fontWeight: FontWeight.w500,
              color: foreground,
            ),
          )
        : Icon(LucideIcons.cornerDownLeft300, size: glyph, color: foreground);
    return Padding(
      padding: const EdgeInsets.only(left: 4),
      child: Tooltip(
        message: semanticLabel,
        child: Semantics(
          button: true,
          enabled: enabled,
          label: semanticLabel,
          child: Opacity(
            opacity: onPressed == null && !loading ? .5 : 1,
            child: Material(
              color: colors.accent,
              shape: const StadiumBorder(),
              clipBehavior: Clip.antiAlias,
              child: InkWell(
                onTap: enabled ? onPressed : null,
                child: Container(
                  constraints: const BoxConstraints(minWidth: 28, minHeight: 28),
                  height: 28,
                  padding: EdgeInsets.symmetric(horizontal: label == null ? 0 : 12),
                  alignment: Alignment.center,
                  child: content,
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// Original `FieldTrigger` (`new-workspace/form/field-trigger.tsx`). `badge`
/// (desktop): borderless 28px pill, ≤240px, 8px inset, 2xl radius, 4px gap,
/// muted base value, surface2 hover / surface0 press, tooltip. `field`
/// (compact): 56px surface1 box, 12px inset, xl radius, 8px gap, muted sm
/// label 2px over a foreground base value (6px icon gap), surface2 hover and
/// press. 16px icon box, extra-muted 14px chevron nudged 1px down, 0.6
/// opacity when disabled.
class _FieldTrigger extends StatefulWidget {
  const _FieldTrigger({
    required this.compact,
    required this.label,
    required this.icon,
    required this.value,
    required this.onTap,
    this.tooltip,
    this.semanticLabel,
    this.disabled = false,
  });
  final bool compact, disabled;
  final String label, value;
  final String? tooltip, semanticLabel;
  final Widget icon;
  final void Function(BuildContext anchor) onTap;
  @override
  State<_FieldTrigger> createState() => _FieldTriggerState();
}

class _FieldTriggerState extends State<_FieldTrigger> {
  bool _hovered = false, _pressed = false;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final field = widget.compact;
    final enabled = !widget.disabled;
    final rest = field ? colors.surface1 : Colors.transparent;
    final background = !enabled
        ? rest
        : _pressed
        ? (field ? colors.surface2 : colors.surface0)
        : _hovered
        ? colors.surface2
        : rest;
    final chevron = Transform.translate(
      offset: const Offset(0, 1),
      child: Icon(LucideIcons.chevronDown300, size: 14, color: colors.extraMuted),
    );
    final iconBox = SizedBox.square(
      dimension: 16,
      child: Center(child: widget.icon),
    );
    final value = Text(
      widget.value,
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
      style: theme.textTheme.bodyMedium?.copyWith(
        color: field ? theme.colorScheme.onSurface : colors.muted,
      ),
    );
    Widget content = field
        ? Container(
            height: 56,
            padding: const EdgeInsets.symmetric(horizontal: 12),
            decoration: BoxDecoration(
              color: background,
              borderRadius: BorderRadius.circular(12),
            ),
            child: Row(
              spacing: 8,
              children: [
                Expanded(
                  child: Column(
                    mainAxisAlignment: MainAxisAlignment.center,
                    crossAxisAlignment: CrossAxisAlignment.start,
                    spacing: 2,
                    children: [
                      Text(
                        widget.label,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: theme.textTheme.bodySmall?.copyWith(
                          color: colors.muted,
                        ),
                      ),
                      Row(
                        spacing: 6,
                        children: [iconBox, Flexible(child: value)],
                      ),
                    ],
                  ),
                ),
                chevron,
              ],
            ),
          )
        : Container(
            height: 28,
            constraints: const BoxConstraints(maxWidth: 240),
            padding: const EdgeInsets.symmetric(horizontal: 8),
            decoration: BoxDecoration(
              color: background,
              borderRadius: BorderRadius.circular(16),
            ),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              spacing: 4,
              children: [iconBox, Flexible(child: value), chevron],
            ),
          );
    content = Semantics(
      button: true,
      enabled: enabled,
      label: widget.semanticLabel ?? widget.label,
      child: MouseRegion(
        cursor: enabled ? SystemMouseCursors.click : SystemMouseCursors.basic,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: enabled ? (_) => setState(() => _pressed = true) : null,
          onTapUp: enabled ? (_) => setState(() => _pressed = false) : null,
          onTapCancel: () => setState(() => _pressed = false),
          onTap: enabled ? () => widget.onTap(context) : null,
          child: Opacity(opacity: enabled ? 1 : .6, child: content),
        ),
      ),
    );
    if (field || widget.tooltip == null) return content;
    return Tooltip(message: widget.tooltip!, child: content);
  }
}
