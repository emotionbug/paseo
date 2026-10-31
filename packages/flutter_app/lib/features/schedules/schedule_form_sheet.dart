part of 'schedules_screen.dart';

// --- Original `utils/device-timezone.ts` -------------------------------------

final _ianaZone = RegExp(r'^[A-Za-z]+(?:/[A-Za-z0-9_+\-]+)*$');

String? _cachedDeviceTimeZone;

/// Fallback when the platform exposes no IANA zone: the fixed-offset
/// `Etc/GMT∓N` zone for whole-hour offsets, otherwise UTC.
String _offsetTimeZone() {
  final offset = DateTime.now().timeZoneOffset;
  if (offset == Duration.zero || offset.inMinutes % 60 != 0) return 'UTC';
  final hours = offset.inHours;
  return hours > 0 ? 'Etc/GMT-$hours' : 'Etc/GMT+${-hours}';
}

Future<String?> _probeZone(Future<String?> Function() probe) async {
  try {
    final value = (await probe())?.trim().replaceFirst(RegExp('^:'), '');
    return value != null && _ianaZone.hasMatch(value) ? value : null;
  } catch (_) {
    return null;
  }
}

/// Original `getDeviceTimeZone` (`Intl.DateTimeFormat().resolvedOptions()
/// .timeZone`): the device's IANA zone from `TZ`, Android's
/// `persist.sys.timezone`, or the `/etc/localtime` zoneinfo link.
Future<String> _deviceTimeZone() async {
  final cached = _cachedDeviceTimeZone;
  if (cached != null) return cached;
  String? zone;
  if (!kIsWeb) {
    zone = await _probeZone(() async => io.Platform.environment['TZ']);
    if (zone == null && io.Platform.isAndroid) {
      zone = await _probeZone(() async {
        final result = await io.Process.run('getprop', ['persist.sys.timezone']);
        return '${result.stdout}';
      });
    }
    if (zone == null && (io.Platform.isLinux || io.Platform.isMacOS)) {
      zone = await _probeZone(() async {
        final target = await io.Link('/etc/localtime').target();
        final index = target.indexOf('zoneinfo/');
        return index < 0 ? null : target.substring(index + 'zoneinfo/'.length);
      });
    }
    if (zone == null && io.Platform.isLinux) {
      zone = await _probeZone(() => io.File('/etc/timezone').readAsString());
    }
  }
  return _cachedDeviceTimeZone = zone ?? _offsetTimeZone();
}

// --- Original `components/schedules/schedule-form-sheet.tsx` -----------------

/// The create/edit sheet. New-agent schedules show Name, Prompt, Host (edit or
/// several hosts), Project (once a host is chosen), then — once a project is
/// chosen — Model, Thinking, Mode, Isolation (git project on a host with
/// workspace multiplicity) and Archive on finish (multiplicity hosts), then
/// Cadence and Max runs; a heartbeat edit shows its read-only Target and the
/// Cadence. Controls use the md field geometry on phones and sm on wide
/// layouts. Footer: secondary Cancel + default submit, sharing the row.
class _ScheduleEditor extends StatefulWidget {
  const _ScheduleEditor({required this.store, this.schedule});
  final AppStore store;
  final JsonMap? schedule;
  @override
  State<_ScheduleEditor> createState() => _ScheduleEditorState();
}

class _ScheduleEditorState extends State<_ScheduleEditor> {
  late final TextEditingController name, prompt, cron, maxRuns;
  late final bool editing, heartbeat;
  String? serverId, error;

  /// Original `normalizeScheduleFormCadence` zone: the saved cron zone, else
  /// the device zone.
  late String timezone;
  String cwd = '';

  /// Original form-model selection (`selectedProvider` / `selectedModel` /
  /// `selectedMode` / `selectedThinkingOptionId`).
  String? provider;
  String model = '', mode = '', thinking = '';

  /// Original `thinkingDrafts`: the last thinking choice per `provider:model`.
  final thinkingDrafts = <String, String>{};

  /// Provider snapshot for (host, project) — original
  /// `useScheduleFormProviderSnapshot`.
  List<JsonMap> entries = const [];
  bool catalogLoading = false;
  Object? catalogError;
  int catalogGeneration = 0;

  String isolation = 'local';
  bool archive = true, cadenceChanged = false, saving = false;

  @override
  void initState() {
    super.initState();
    final schedule = widget.schedule;
    editing = schedule != null;
    final target = jsonMap(schedule?['target']);
    heartbeat = editing && target['type'] == 'agent';
    final saved = jsonMap(target['config']);
    final savedProvider = stringValue(saved['provider']);
    provider = savedProvider.isEmpty ? null : savedProvider;
    model = stringValue(saved['model']);
    mode = stringValue(saved['modeId']);
    thinking = stringValue(saved['thinkingOptionId']);
    if (provider != null && model.isNotEmpty && thinking.isNotEmpty) {
      thinkingDrafts['$provider:$model'] = thinking;
    }
    final hosts = widget.store.hosts;
    serverId = editing
        ? stringValue(schedule!['_serverId'])
        : hosts.length == 1
        ? hosts.first.serverId
        : null;
    cwd = stringValue(saved['cwd']);
    archive = saved['archiveOnFinish'] != false;
    isolation = saved['isolation'] == 'worktree' ? 'worktree' : 'local';
    final cadence = jsonMap(schedule?['cadence']);
    final savedZone = cadence['type'] == 'cron' ? stringValue(cadence['timezone']) : '';
    timezone = savedZone.isNotEmpty ? savedZone : _cachedDeviceTimeZone ?? _offsetTimeZone();
    if (savedZone.isEmpty) {
      unawaited(
        _deviceTimeZone().then((zone) {
          if (mounted && zone != timezone) setState(() => timezone = zone);
        }),
      );
    }
    name = TextEditingController(text: stringValue(schedule?['name']));
    prompt = TextEditingController(text: stringValue(schedule?['prompt']))
      ..addListener(_changed);
    cron = TextEditingController(
      text: _cronExpressionFor(
        schedule == null ? const {'type': 'every', 'everyMs': 3600000} : cadence,
      ),
    );
    maxRuns = TextEditingController(text: schedule?['maxRuns']?.toString() ?? '');
    if (!heartbeat && host != null && cwd.trim().isNotEmpty) {
      catalogLoading = true;
      WidgetsBinding.instance.addPostFrameCallback((_) => _loadCatalog());
    }
  }

  void _changed() => setState(() {});

  @override
  void dispose() {
    ++catalogGeneration;
    for (final controller in [name, prompt, cron, maxRuns]) {
      controller.dispose();
    }
    super.dispose();
  }

  HostState? get host => serverId == null ? null : widget.store.hostFor(serverId!);

  bool get multiplicity =>
      jsonMap(host?.client.serverInfo['features'])['workspaceMultiplicity'] == true;

  JsonMap? get project => host?.projects
      .where((p) => cwd.isNotEmpty && stringValue(p['projectRootPath']).trim() == cwd.trim())
      .firstOrNull;

  bool get hasProject => cwd.trim().isNotEmpty;

  bool get canUseWorktree => multiplicity && project?['projectKind'] == 'git';

  /// Original `resolveEffectiveIsolation`: a worktree choice holds while the
  /// provider snapshot is still resolving, then falls back to local when the
  /// project cannot host one.
  String get effectiveIsolation =>
      isolation == 'worktree' && (canUseWorktree || catalogLoading) ? 'worktree' : 'local';

  bool get canManage {
    final info = host?.client.serverInfo ?? const {};
    return !info.containsKey('permissions') ||
        jsonList(info['permissions']).contains('automation.manage');
  }

  String? get cadenceError => _validateCron(cron.text);

  // --- Provider snapshot projections (original form model) -----------------

  Iterable<JsonMap> get _enabledEntries => entries.where((e) => e['enabled'] != false);

  JsonMap? _entry(String? id) =>
      id == null ? null : entries.where((e) => e['provider'] == id).firstOrNull;

  /// Original `filterSelectableModels`.
  List<JsonMap> _models(String? id) => jsonList(
    _entry(id)?['models'],
  ).map(jsonMap).where((m) => m['isSelectable'] != false).toList();

  /// Original `findModelByReference`: id, then alias.
  static JsonMap? _modelByReference(List<JsonMap> models, String id) {
    final reference = id.trim();
    if (reference.isEmpty) return null;
    return models.where((m) => m['id'] == reference).firstOrNull ??
        models.where((m) => jsonList(m['aliases']).contains(reference)).firstOrNull;
  }

  /// Original form-model `resolveEffectiveModel`: the chosen model, else the
  /// default, else the first.
  JsonMap? _effectiveModel(String? id, String modelId) {
    final models = _models(id);
    if (models.isEmpty || modelId.trim().isEmpty) return null;
    return models.where((m) => m['id'] == modelId.trim()).firstOrNull ??
        models.where((m) => m['isDefault'] == true).firstOrNull ??
        models.first;
  }

  List<JsonMap> get _modeOptions => jsonList(_entry(provider)?['modes']).map(jsonMap).toList();

  List<JsonMap> get _thinkingOptions =>
      jsonList(_effectiveModel(provider, model)?['thinkingOptions']).map(jsonMap).toList();

  /// Original `resolveThinkingOptionId`.
  String _resolveThinking(String id, String modelId, String requested) {
    final effective = _modelByReference(_models(id), modelId);
    final options = jsonList(effective?['thinkingOptions']).map(jsonMap).toList();
    if (options.isEmpty) return '';
    final wanted = requested.trim();
    if (wanted.isNotEmpty && options.any((o) => o['id'] == wanted)) return wanted;
    return stringValue(effective?['defaultThinkingOptionId'], stringValue(options.first['id']));
  }

  /// Original `resolvePreferredModeId` without preferences: the provider's
  /// default mode, else its first.
  String _defaultMode(String id) {
    final entry = _entry(id);
    final modes = jsonList(entry?['modes']).map(jsonMap).toList();
    final preferred = stringValue(entry?['defaultModeId']);
    if (preferred.isNotEmpty && (modes.isEmpty || modes.any((m) => m['id'] == preferred))) {
      return preferred;
    }
    return stringValue(modes.firstOrNull?['id']);
  }

  /// Original `buildEntryModelSelection` kind: `models`, `loading` or `error`.
  String _selectionKind(JsonMap entry) {
    if (jsonList(entry['models']).isNotEmpty || entry['status'] == 'ready') return 'models';
    if (entry['status'] == 'loading') {
      return host?.client.status == ConnectionStatus.connected ? 'loading' : 'error';
    }
    return 'error';
  }

  String _selectionError(BuildContext context, JsonMap entry) {
    if (entry['status'] == 'loading') {
      return tr(context, 'providerSelection.readiness.hostDisconnected');
    }
    final message = stringValue(entry['error']);
    if (message.isNotEmpty) return message;
    return tr(
      context,
      entry['status'] == 'unavailable'
          ? 'providerSelection.unavailable'
          : 'providerSelection.unknownError',
    );
  }

  /// Original `isSelectedModelValidForProviders`.
  bool get _modelValid {
    final id = provider;
    if (id == null) return false;
    final entry = _enabledEntries.where((e) => e['provider'] == id).firstOrNull;
    if (entry == null || _selectionKind(entry) != 'models') return false;
    final selected = model.trim();
    return selected.isEmpty || _models(id).any((row) => row['id'] == selected);
  }

  /// Original `resolveSelectedModelLabel`.
  String _selectedModelLabel(BuildContext context) {
    final loadingLabel = tr(context, 'providerSelection.loading');
    final selectLabel = tr(context, 'providerSelection.selectModel');
    final id = provider;
    final entry = id == null
        ? null
        : _enabledEntries.where((e) => e['provider'] == id).firstOrNull;
    if (entry == null) return catalogLoading ? loadingLabel : selectLabel;
    switch (_selectionKind(entry)) {
      case 'loading':
        return loadingLabel;
      case 'error':
        return tr(context, 'providerSelection.error');
    }
    final models = _models(id);
    final selected = model.trim();
    final found = models.where((m) => m['id'] == selected).firstOrNull;
    if (found == null && selected.isNotEmpty) return selected;
    final row = found ??
        models.where((m) => m['isDefault'] == true).firstOrNull ??
        models.firstOrNull;
    return row == null ? selectLabel : stringValue(row['label'], stringValue(row['id']));
  }

  /// Original `resolveSnapshotSelection` once the snapshot lands: saved
  /// values stay, aliases become canonical ids, an empty mode takes the
  /// provider default and thinking resolves against the model.
  void _resolveSelection() {
    final id = provider;
    if (id == null) return;
    final models = _models(id);
    if (model.isNotEmpty && models.isNotEmpty) {
      final canonical = stringValue(_modelByReference(models, model)?['id']);
      if (canonical.isNotEmpty) model = canonical;
    }
    if (mode.isEmpty) mode = _defaultMode(id);
    if (models.isNotEmpty) {
      thinking = _resolveThinking(
        id,
        model,
        thinking.isNotEmpty ? thinking : thinkingDrafts['$id:$model'] ?? '',
      );
    }
  }

  Future<void> _loadCatalog() async {
    if (!mounted) return;
    final own = ++catalogGeneration;
    final client = host?.client;
    final dir = cwd.trim();
    if (client == null || dir.isEmpty) {
      setState(() {
        entries = const [];
        catalogLoading = false;
        catalogError = null;
      });
      return;
    }
    setState(() {
      catalogLoading = true;
      catalogError = null;
    });
    try {
      final result = await settingsRequest(
        client,
        'get_providers_snapshot_request',
        {'cwd': dir},
        responseType: 'get_providers_snapshot_response',
      );
      if (!mounted || own != catalogGeneration) return;
      setState(() {
        entries = providerEntries(result);
        catalogLoading = false;
        _resolveSelection();
      });
    } catch (e) {
      if (!mounted || own != catalogGeneration) return;
      setState(() {
        catalogLoading = false;
        catalogError = e;
      });
    }
  }

  /// Original `setHost`: a new host clears the project and model selection.
  void _setHost(String value) {
    if (value == serverId) return;
    ++catalogGeneration;
    setState(() {
      serverId = value;
      cwd = '';
      provider = null;
      model = mode = thinking = '';
      entries = const [];
      catalogLoading = false;
      catalogError = null;
    });
  }

  /// Original `setProject`: a new (host, cwd) scope refetches the provider
  /// snapshot. Without the original's persisted form preferences, the current
  /// provider/model carry over as the preferred pick; mode and thinking
  /// re-resolve against the new snapshot.
  void _setProject(String path) {
    if (path == cwd.trim()) return;
    setState(() {
      cwd = path;
      mode = thinking = '';
      entries = const [];
    });
    _loadCatalog();
  }

  /// Original `setModel`.
  void _setModel(String id, String modelId) {
    final models = _models(id);
    final picked = modelId.trim().isNotEmpty
        ? modelId.trim()
        : stringValue(
            (models.where((m) => m['isDefault'] == true).firstOrNull ?? models.firstOrNull)?['id'],
          );
    final nextThinking = _resolveThinking(id, picked, thinkingDrafts['$id:$picked'] ?? '');
    if (picked.isNotEmpty && nextThinking.isNotEmpty) {
      thinkingDrafts['$id:$picked'] = nextThinking;
    }
    setState(() {
      mode = provider == id && mode.trim().isNotEmpty ? mode : _defaultMode(id);
      provider = id;
      model = picked;
      thinking = nextThinking;
    });
  }

  void _setThinking(String id) {
    if (provider != null && model.isNotEmpty) thinkingDrafts['$provider:$model'] = id;
    setState(() => thinking = id);
  }

  /// Original `resolveCanSubmit` plus the submit-time cron validation.
  bool get canSubmit {
    if (saving || !canManage || cadenceError != null) return false;
    if (heartbeat) return cadenceChanged;
    if (prompt.text.trim().isEmpty) return false;
    if (!editing && project == null) return false;
    if (!hasProject) return false;
    return _modelValid;
  }

  JsonMap get cadence => {'type': 'cron', 'expression': cron.text.trim(), 'timezone': timezone};

  /// Original `parseMaxRuns` (`Number.parseInt`, positive only).
  int? get _maxRuns {
    final digits = RegExp(r'^\s*[+-]?\d+').stringMatch(maxRuns.text)?.trim();
    final parsed = digits == null ? null : int.tryParse(digits);
    return parsed != null && parsed > 0 ? parsed : null;
  }

  Future<void> save() async {
    if (!canSubmit) return;
    final client = host?.client;
    setState(() => error = null);
    if (client == null) {
      setState(() => error = tr(context, 'common.errors.daemonClientUnavailable'));
      return;
    }
    final max = _maxRuns;
    final lifecycle = multiplicity;
    final trimmedName = name.text.trim();
    final JsonMap fields;
    if (heartbeat) {
      fields = {'scheduleId': widget.schedule!['id'], 'cadence': cadence};
    } else if (editing) {
      fields = {
        'scheduleId': widget.schedule!['id'],
        'name': trimmedName.isEmpty ? null : trimmedName,
        'prompt': prompt.text.trim(),
        if (cadenceChanged) 'cadence': cadence,
        'newAgentConfig': {
          'provider': provider,
          'model': model.isEmpty ? null : model,
          'modeId': mode.isEmpty ? null : mode,
          'thinkingOptionId': thinking.isEmpty ? null : thinking,
          'cwd': cwd.trim(),
          if (lifecycle) 'archiveOnFinish': archive,
          if (lifecycle) 'isolation': effectiveIsolation,
        },
        'maxRuns': max,
      };
    } else {
      fields = {
        'prompt': prompt.text.trim(),
        if (trimmedName.isNotEmpty) 'name': trimmedName,
        'cadence': cadence,
        'target': {
          'type': 'new-agent',
          'config': {
            'provider': provider,
            'cwd': cwd.trim(),
            if (model.isNotEmpty) 'model': model,
            if (mode.isNotEmpty) 'modeId': mode,
            if (thinking.isNotEmpty) 'thinkingOptionId': thinking,
            if (lifecycle) 'archiveOnFinish': archive,
            if (lifecycle) 'isolation': effectiveIsolation,
            if (trimmedName.isNotEmpty) 'title': trimmedName,
          },
        },
        'maxRuns': ?max,
      };
    }
    final action = editing ? 'update' : 'create';
    setState(() => saving = true);
    try {
      await settingsRequest(
        client,
        'schedule/$action',
        fields,
        responseType: 'schedule/$action/response',
      );
      if (mounted) Navigator.pop(context, true);
    } catch (e) {
      if (mounted) {
        setState(() {
          saving = false;
          error = errorMessage(e);
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final size = isCompactLayout(context) ? PaseoControlSize.md : PaseoControlSize.sm;
    final title = !editing
        ? 'New schedule'
        : heartbeat
        ? 'Edit heartbeat'
        : 'Edit schedule';
    final fields = <Widget>[
      if (host != null && !canManage)
        Text(
          'This connection does not have automation.manage permission.',
          style: _text(context, 12, _red300),
        ),
      if (heartbeat) ...[
        _readonlyTarget(context, size),
        _cadenceField(context, size),
      ] else ...[
        PaseoFormField(
          label: 'Name',
          child: _input(context, name, hint: 'Optional', size: size),
        ),
        PaseoFormField(
          label: 'Prompt',
          child: _input(
            context,
            prompt,
            hint: 'What should the agent do each run?',
            size: size,
            multiline: true,
          ),
        ),
        if (editing || widget.store.hosts.length > 1) _hostField(context, size),
        if (editing || serverId != null) _projectField(context, size),
        if (hasProject) _modelField(context, size),
        if (hasProject && provider != null && model.trim().isNotEmpty && _thinkingOptions.isNotEmpty)
          _thinkingField(context, size),
        if (hasProject && provider != null && _modeOptions.isNotEmpty) _modeField(context, size),
        if (hasProject && canUseWorktree) _isolationField(context, size),
        if (hasProject && multiplicity)
          PaseoFormField(
            label: 'Archive on finish',
            child: Align(
              alignment: Alignment.centerLeft,
              child: PaseoSwitch(
                value: archive,
                semanticLabel: 'Archive on finish',
                onChanged: (value) => setState(() => archive = value),
              ),
            ),
          ),
        _cadenceField(context, size),
        PaseoFormField(
          label: 'Max runs',
          child: _input(context, maxRuns, hint: 'Unlimited', size: size, number: true),
        ),
      ],
      if (error != null) SelectableText(error!, style: _text(context, 12, _red300)),
    ];
    return SheetContents(
      title: title,
      stretchActions: true,
      actions: [
        Row(
          spacing: 12,
          children: [
            Expanded(
              child: PaseoButton(
                label: 'Cancel',
                expand: true,
                onPressed: saving ? null : () => Navigator.pop(context),
              ),
            ),
            Expanded(
              child: PaseoButton(
                label: editing ? 'Save changes' : 'Create schedule',
                variant: PaseoButtonVariant.primary,
                expand: true,
                loading: saving,
                onPressed: canSubmit ? save : null,
              ),
            ),
          ],
        ),
      ],
      // Original sheet content: 16px gap between fields.
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 16,
        children: fields,
      ),
    );
  }

  /// Original `FormTextInput`: [paseoInputDecoration] chrome, base text.
  Widget _input(
    BuildContext context,
    TextEditingController controller, {
    required String hint,
    required PaseoControlSize size,
    bool multiline = false,
    bool number = false,
    bool mono = false,
    ValueChanged<String>? onChanged,
  }) {
    final theme = Theme.of(context);
    final type = PaseoTypography.of(context);
    var style = _text(context, 14, theme.colorScheme.onSurface).copyWith(height: 1.4);
    if (mono) {
      style = style.copyWith(
        fontFamily: type.monoFontFamily,
        fontFamilyFallback: type.monoFontFamilyFallback,
      );
    }
    return ConstrainedBox(
      constraints: BoxConstraints(minHeight: multiline ? 96 : 0),
      child: TextField(
        controller: controller,
        style: style,
        minLines: multiline ? 4 : 1,
        maxLines: multiline ? 12 : 1,
        textAlignVertical: multiline ? TextAlignVertical.top : null,
        keyboardType: number
            ? TextInputType.number
            : multiline
            ? TextInputType.multiline
            : TextInputType.text,
        autocorrect: !mono && multiline,
        enableSuggestions: !mono,
        textCapitalization: TextCapitalization.none,
        onChanged: onChanged,
        decoration: paseoInputDecoration(
          context,
          hintText: hint,
          size: size,
          multiline: multiline,
        ),
      ),
    );
  }

  /// Original `SelectField`: an optional `Field` (label, hint) around a
  /// `SelectFieldTrigger` showing [display] (or the muted [placeholder]),
  /// opening the combobox of [options].
  Widget _selectField<T>(
    BuildContext context, {
    required String label,
    required String title,
    required T? value,
    required String? display,
    required String placeholder,
    required List<PaseoSelectOption<T>> options,
    required ValueChanged<T>? onChanged,
    required PaseoControlSize size,
    required String emptyText,
    String? hint,
    bool searchable = false,
    String? searchPlaceholder,
    Widget? leading,
    bool field = true,
  }) {
    final trigger = _FieldTrigger(
      title: title,
      label: display ?? placeholder,
      isPlaceholder: display == null,
      size: size,
      leading: leading,
      onTap: onChanged == null
          ? null
          : (anchor) async {
              final picked = await showPaseoSelect<T>(
                context: anchor,
                options: options,
                title: title,
                value: value,
                searchable: searchable,
                searchPlaceholder: searchPlaceholder,
                emptyText: emptyText,
              );
              if (picked is T && picked != value) onChanged(picked);
            },
    );
    if (!field) return trigger;
    return PaseoFormField(label: label, hint: hint, child: trigger);
  }

  /// Original 18px `optionIconBox` holding a muted 16px glyph.
  Widget _optionGlyph(BuildContext context, IconData icon) => SizedBox.square(
    dimension: 18,
    child: Center(child: Icon(icon, size: 16, color: PaseoColors.of(context).muted)),
  );

  /// Original `ScheduleAgentTargetField`: surface2, 1px border, field
  /// geometry, one foreground base line.
  Widget _readonlyTarget(BuildContext context, PaseoControlSize size) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final small = size == PaseoControlSize.sm;
    final agentId = jsonMap(widget.schedule?['target'])['agentId'];
    final agent = host?.agents.where((a) => a['id'] == agentId).firstOrNull;
    final label = agent == null
        ? 'Agent unavailable'
        : stringValue(agent['title']).trim().isEmpty
        ? 'Untitled agent'
        : stringValue(agent['title']).trim();
    return PaseoFormField(
      label: 'Target',
      child: Container(
        constraints: BoxConstraints(minHeight: small ? 32 : 44),
        padding: EdgeInsets.symmetric(horizontal: small ? 12 : 16),
        alignment: Alignment.centerLeft,
        decoration: BoxDecoration(
          color: colors.surface2,
          border: Border.all(color: theme.dividerColor),
          borderRadius: BorderRadius.circular(small ? 6 : 8),
        ),
        child: Text(
          label,
          maxLines: 1,
          overflow: TextOverflow.ellipsis,
          style: _text(context, 14, theme.colorScheme.onSurface),
        ),
      ),
    );
  }

  /// Original Host `SelectField`: status-dot options, read-only when editing.
  Widget _hostField(BuildContext context, PaseoControlSize size) {
    final selected = widget.store.hosts.where((h) => h.serverId == serverId).firstOrNull;
    return _selectField<String>(
      context,
      label: 'Host',
      title: 'Host',
      value: serverId,
      display: selected?.label ?? serverId,
      placeholder: 'Select host',
      emptyText: 'No hosts found',
      size: size,
      options: [
        for (final host in widget.store.hosts)
          PaseoSelectOption(
            host.serverId,
            host.label,
            leading: _HostStatusDot(status: host.client.status),
          ),
      ],
      onChanged: editing ? null : _setHost,
    );
  }

  /// Original Project `SelectField`: searchable Folder-glyph options from the
  /// host's projects; a saved cwd outside them displays its shortened path.
  Widget _projectField(BuildContext context, PaseoControlSize size) {
    final options = <PaseoSelectOption<String>>[];
    final seen = <String>{};
    for (final entry in host?.projects ?? const <JsonMap>[]) {
      final path = stringValue(entry['projectRootPath']).trim();
      if (path.isEmpty || !seen.add(path)) continue;
      options.add(
        PaseoSelectOption(
          path,
          stringValue(entry['projectDisplayName'], _shortenPath(path)),
          leading: _optionGlyph(context, LucideIcons.folder300),
        ),
      );
    }
    final matched = project;
    final display = matched != null
        ? stringValue(matched['projectDisplayName'], _shortenPath(cwd.trim()))
        : hasProject
        ? _shortenPath(cwd.trim())
        : null;
    return _selectField<String>(
      context,
      label: 'Project',
      title: 'Select project',
      value: matched == null ? null : cwd.trim(),
      display: display,
      placeholder: 'Select project',
      emptyText: 'No projects found',
      hint: serverId == null ? 'Choose a host first.' : null,
      searchable: true,
      searchPlaceholder: 'Search projects...',
      size: size,
      options: options,
      onChanged: serverId == null ? null : _setProject,
    );
  }

  /// Original `CombinedModelSelector` in the Model field: a field trigger with
  /// the provider glyph and the model label (muted until a model is chosen),
  /// opening every enabled provider's models; providers still loading or
  /// failing show as disabled rows with their status.
  Widget _modelField(BuildContext context, PaseoControlSize size) {
    final muted = PaseoColors.of(context).muted;
    final id = provider;
    final display = model.trim().isEmpty
        ? null
        : stringValue(_effectiveModel(id, model)?['label'], model.trim());
    Widget glyph(String providerId) => SizedBox.square(
      dimension: 16,
      child: Center(child: ProviderIcon(provider: providerId, size: 16, color: muted)),
    );
    final trigger = _FieldTrigger(
      title: tr(context, 'providerSelection.selectModel'),
      label: display ?? _selectedModelLabel(context),
      isPlaceholder: model.trim().isEmpty,
      size: size,
      leading: id == null ? null : glyph(id),
      onTap: serverId == null
          ? null
          : (anchor) async {
              // Original `refetchIfStale` on open.
              if (!catalogLoading && (catalogError != null || entries.isEmpty)) {
                unawaited(_loadCatalog());
              }
              final options = <PaseoSelectOption<(String, String)>>[];
              for (final entry in _enabledEntries) {
                final entryId = stringValue(entry['provider']);
                final entryLabel = stringValue(entry['label'], entryId);
                final kind = _selectionKind(entry);
                if (kind != 'models') {
                  options.add(
                    PaseoSelectOption(
                      (entryId, ''),
                      entryLabel,
                      description: kind == 'loading'
                          ? tr(context, 'providerSelection.loading')
                          : _selectionError(context, entry),
                      leading: glyph(entryId),
                      enabled: false,
                      separatorBefore: options.isNotEmpty,
                    ),
                  );
                  continue;
                }
                var first = true;
                for (final row in _models(entryId)) {
                  final rowId = stringValue(row['id']);
                  options.add(
                    PaseoSelectOption(
                      (entryId, rowId),
                      stringValue(row['label'], rowId),
                      description: entryLabel,
                      leading: glyph(entryId),
                      separatorBefore: first && options.isNotEmpty,
                    ),
                  );
                  first = false;
                }
              }
              final failure = catalogError;
              final picked = await showPaseoSelect<(String, String)>(
                context: anchor,
                options: options,
                title: tr(context, 'providerSelection.selectModel'),
                value: (id ?? '', model.trim()),
                searchable: true,
                searchPlaceholder: tr(context, 'modelSelector.searchAllPlaceholder'),
                emptyText: catalogLoading
                    ? tr(context, 'modelSelector.loadingSelector')
                    : failure != null
                    ? errorMessage(failure)
                    : tr(context, 'providerSelection.readiness.noProviders'),
              );
              if (!mounted || picked == null) return;
              _setModel(picked.$1, picked.$2);
            },
    );
    return PaseoFormField(label: 'Model', child: trigger);
  }

  /// Original Thinking `SelectField`: Brain-glyph options.
  Widget _thinkingField(BuildContext context, PaseoControlSize size) {
    final options = _thinkingOptions;
    final selected = thinking.trim();
    final current = options.where((o) => o['id'] == selected).firstOrNull ?? {'id': selected};
    return _selectField<String>(
      context,
      label: 'Thinking',
      title: 'Select thinking',
      value: selected.isEmpty ? null : selected,
      display: selected.isEmpty ? null : formatThinkingOptionLabel(context, current),
      placeholder: 'Select thinking',
      emptyText: 'No thinking options found',
      searchable: options.length > 6,
      size: size,
      options: [
        for (final option in options)
          PaseoSelectOption(
            stringValue(option['id']),
            formatThinkingOptionLabel(context, option),
            leading: _optionGlyph(context, LucideIcons.brain300),
          ),
      ],
      onChanged: _setThinking,
    );
  }

  /// Original Mode `SelectField`: "Default mode" until a mode is chosen.
  Widget _modeField(BuildContext context, PaseoControlSize size) {
    final options = _modeOptions;
    final selected = mode.trim();
    final current = options.where((o) => o['id'] == selected).firstOrNull;
    return _selectField<String>(
      context,
      label: 'Mode',
      title: 'Select mode',
      value: selected.isEmpty ? null : selected,
      display: selected.isEmpty
          ? 'Default mode'
          : stringValue(current?['label'], selected),
      placeholder: 'Default mode',
      emptyText: 'No modes found',
      hint: options.isEmpty ? 'No modes are available for this model.' : null,
      searchable: options.length > 6,
      size: size,
      options: [
        for (final option in options)
          PaseoSelectOption(
            stringValue(option['id']),
            stringValue(option['label'], stringValue(option['id'])),
          ),
      ],
      onChanged: options.isEmpty ? null : (value) => setState(() => mode = value),
    );
  }

  /// Original `ScheduleIsolationField`: Local (Folder) / Worktree (GitBranch),
  /// with the effective choice's glyph leading the trigger.
  Widget _isolationField(BuildContext context, PaseoControlSize size) {
    final worktree = effectiveIsolation == 'worktree';
    return _selectField<String>(
      context,
      label: 'Isolation',
      title: 'Isolation',
      value: effectiveIsolation,
      display: worktree ? 'Worktree' : 'Local',
      placeholder: 'Select isolation',
      emptyText: 'No isolation options found',
      size: size,
      leading: _optionGlyph(
        context,
        worktree ? LucideIcons.gitBranch300 : LucideIcons.folder300,
      ),
      options: [
        PaseoSelectOption(
          'local',
          'Local',
          leading: _optionGlyph(context, LucideIcons.folder300),
        ),
        PaseoSelectOption(
          'worktree',
          'Worktree',
          leading: _optionGlyph(context, LucideIcons.gitBranch300),
        ),
      ],
      onChanged: (value) => setState(() => isolation = value),
    );
  }

  /// Original `CadenceEditor`: the preset select ("Custom cron" when the
  /// expression matches no preset) over the mono cron input (12px stack),
  /// then a red-300 error or the muted humanized preview in the cadence zone.
  Widget _cadenceField(BuildContext context, PaseoControlSize size) {
    final colors = PaseoColors.of(context);
    final expression = cron.text.trim();
    final preset = _cadencePresets.where((p) => p.$3 == expression).firstOrNull;
    final failure = cadenceError;
    final preview = failure != null || expression.isEmpty
        ? null
        : _describeCron(expression, timezone) ?? expression;
    return PaseoFormField(
      label: 'Cadence',
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 12,
        children: [
          _selectField<String>(
            context,
            label: 'Cadence',
            title: 'Cadence',
            field: false,
            value: preset?.$1,
            display: preset?.$2 ?? 'Custom cron',
            placeholder: 'Select cadence',
            emptyText: 'No cadences found',
            size: size,
            options: [
              for (final (id, label, _) in _cadencePresets) PaseoSelectOption(id, label),
            ],
            onChanged: (id) {
              final next = _cadencePresets.firstWhere((p) => p.$1 == id).$3;
              setState(() {
                cron.text = next;
                cadenceChanged = true;
              });
            },
          ),
          _input(
            context,
            cron,
            hint: '0 9 * * *',
            size: size,
            mono: true,
            onChanged: (_) => setState(() => cadenceChanged = true),
          ),
          if (failure != null)
            Text(failure, style: _text(context, 12, _red300))
          else if (preview != null)
            Text(preview, style: _text(context, 12, colors.muted)),
        ],
      ),
    );
  }
}

/// Original `SelectFieldTrigger`: surface2 field (sm 32px/6px radius, md
/// 44px/8px), transparent 1px border turning borderAccent while hovered or
/// open, optional leading glyph, base label (muted placeholder) and a muted
/// 16px chevron; 0.5 opacity when disabled.
class _FieldTrigger extends StatefulWidget {
  const _FieldTrigger({
    required this.title,
    required this.label,
    required this.isPlaceholder,
    required this.size,
    required this.onTap,
    this.leading,
  });
  final String title, label;
  final bool isPlaceholder;
  final PaseoControlSize size;
  final Widget? leading;
  final Future<void> Function(BuildContext anchor)? onTap;
  @override
  State<_FieldTrigger> createState() => _FieldTriggerState();
}

class _FieldTriggerState extends State<_FieldTrigger> {
  bool hovered = false, open = false;

  Future<void> tap(BuildContext anchor) async {
    setState(() => open = true);
    try {
      await widget.onTap!(anchor);
    } finally {
      if (mounted) setState(() => open = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final disabled = widget.onTap == null;
    final small = widget.size == PaseoControlSize.sm || widget.size == PaseoControlSize.xs;
    final active = (open || hovered) && !disabled;
    return Semantics(
      button: true,
      enabled: !disabled,
      label: '${widget.title} (${widget.label})',
      child: MouseRegion(
        cursor: disabled ? SystemMouseCursors.basic : SystemMouseCursors.click,
        onEnter: (_) => setState(() => hovered = true),
        onExit: (_) => setState(() => hovered = false),
        child: Builder(
          builder: (anchor) => GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTap: disabled ? null : () => tap(anchor),
            child: Opacity(
              opacity: disabled ? .5 : 1,
              child: Container(
                constraints: BoxConstraints(minHeight: small ? 32 : 44),
                padding: EdgeInsets.symmetric(horizontal: small ? 12 : 16),
                decoration: BoxDecoration(
                  color: colors.surface2,
                  border: Border.all(color: active ? colors.borderAccent : Colors.transparent),
                  borderRadius: BorderRadius.circular(small ? 6 : 8),
                ),
                child: Row(
                  spacing: 8,
                  children: [
                    ?widget.leading,
                    Expanded(
                      child: Text(
                        widget.label,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: theme.textTheme.bodyMedium!.copyWith(
                          fontSize: type.uiSize(14),
                          height: 1.4,
                          color: widget.isPlaceholder ? colors.muted : null,
                        ),
                      ),
                    ),
                    Icon(LucideIcons.chevronDown300, size: 16, color: colors.muted),
                  ],
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}
