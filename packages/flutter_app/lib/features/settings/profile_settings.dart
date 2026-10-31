import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/client.dart';
import '../../core/json.dart';
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import '../chat/agent_controls.dart' show formatAgentModeLabel, formatThinkingOptionLabel;
import '../chat/provider_icon.dart';
import 'agent_profile_form.dart';
import 'settings_forms.dart';
import 'settings_host_widgets.dart';

/// Original `agent-profiles/internal/agent-profile-glyph.tsx` icon registry.
const agentProfileIcons = <String, IconData>{
  'code': LucideIcons.code300,
  'terminal': LucideIcons.terminal300,
  'bug': LucideIcons.bug300,
  'wrench': LucideIcons.wrench300,
  'hammer': LucideIcons.hammer300,
  'flask': LucideIcons.flaskConical300,
  'testTube': LucideIcons.testTube300,
  'microscope': LucideIcons.microscope300,
  'search': LucideIcons.search300,
  'eye': LucideIcons.eye300,
  'palette': LucideIcons.palette300,
  'feather': LucideIcons.feather300,
  'pencil': LucideIcons.pencil300,
  'fileText': LucideIcons.fileText300,
  'book': LucideIcons.bookOpen300,
  'rocket': LucideIcons.rocket300,
  'package': LucideIcons.package300,
  'boxes': LucideIcons.boxes300,
  'server': LucideIcons.server300,
  'database': LucideIcons.database300,
  'cpu': LucideIcons.cpu300,
  'cloud': LucideIcons.cloud300,
  'globe': LucideIcons.globe300,
  'gitBranch': LucideIcons.gitBranch300,
  'layers': LucideIcons.layers300,
  'compass': LucideIcons.compass300,
  'brain': LucideIcons.brain300,
  'sparkles': LucideIcons.sparkles300,
  'shield': LucideIcons.shield300,
};

/// Original `styles/identity-colors.ts` foregrounds (light, dark).
const _identityForegrounds = <String, (int, int)>{
  'violet': (0xff6d49b5, 0xffa392d5),
  'sky': (0xff3d6985, 0xff6aa6ce),
  'emerald': (0xff3e6e5d, 0xff6cae96),
  'orange': (0xff845838, 0xffcc8f64),
  'pink': (0xff974168, 0xffd87da3),
  'indigo': (0xff5251c2, 0xff9299d5),
  'teal': (0xff3e6d6c, 0xff6cacab),
  'red': (0xff9c4243, 0xffd88381),
  'amber': (0xff716239, 0xffb29d64),
  'blue': (0xff39649e, 0xff7ba1d5),
};

Color _identityForeground(BuildContext context, String? color) {
  final pair = _identityForegrounds[color];
  if (pair == null) return PaseoColors.of(context).muted;
  return Color(Theme.of(context).brightness == Brightness.dark ? pair.$2 : pair.$1);
}

/// Original `AgentProfileGlyph`: the registry icon (Star when unset) in the
/// profile's identity colour, muted without one.
class AgentProfileGlyph extends StatelessWidget {
  const AgentProfileGlyph({super.key, this.icon, this.color, this.size = 16});
  final String? icon, color;
  final double size;
  @override
  Widget build(BuildContext context) => Icon(
    agentProfileIcons[icon] ?? LucideIcons.star300,
    size: size,
    color: _identityForeground(context, color),
  );
}

class ProfileSettings extends StatelessWidget {
  final PaseoClient client;
  final JsonMap config;
  final Future<void> Function(JsonMap) patch;

  /// Host provider snapshot entries, for the row summaries' display labels.
  final List<JsonMap> providers;
  const ProfileSettings({
    super.key,
    required this.client,
    required this.config,
    required this.patch,
    this.providers = const [],
  });
  /// Original `useAgentProfiles().isSupported`: `agentProfiles` and
  /// `agentConfigApply` both advertised.
  static bool supportsAgentProfiles(PaseoClient client) {
    final features = jsonMap(client.serverInfo['features']);
    return features['agentProfiles'] == true && features['agentConfigApply'] == true;
  }

  /// Original `AgentProfilesSection.handleSave`: an edited profile is replaced,
  /// not merged (`{id, cliDefault, ...value}`), so cleared fields go away; a
  /// new one is appended. The list is re-read first so a concurrent host edit
  /// is not overwritten.
  static List<JsonMap> applyProfileSave(List<JsonMap> current, JsonMap record, {required bool editing}) {
    final id = record['id'];
    final value = {...record}..remove('id')..remove('cliDefault');
    if (!editing) return [...current, {'id': id, ...value}];
    return [
      for (final entry in current)
        entry['id'] == id ? {'id': id, 'cliDefault': ?entry['cliDefault'], ...value} : entry,
    ];
  }

  Future<void> edit(BuildContext context, [JsonMap? original]) async {
    await showAdaptiveSheet<JsonMap>(
      context: context,
      maxWidth: 520,
      builder: (context) => ProfileEditor(
        client: client,
        profile: original,
        onSave: (record) async {
          final fresh = await settingsRequest(
            client,
            'get_daemon_config_request',
            {},
            responseType: 'get_daemon_config_response',
          );
          final profiles = jsonList(jsonMap(fresh['config'])['agentProfiles']).map(jsonMap).toList();
          if (original != null && !profiles.any((p) => p['id'] == record['id'])) {
            throw StateError(
              'This profile was removed on the host while the editor was open. Close the editor and create a new profile instead.',
            );
          }
          await patch({
            'agentProfiles': applyProfileSave(profiles, record, editing: original != null),
          });
        },
      ),
    );
  }

  Future<void> _save(BuildContext context, List<JsonMap> next) async {
    try {
      await patch({'agentProfiles': next});
    } catch (e) {
      if (context.mounted) showError(context, e);
    }
  }

  /// Original `buildAgentProfileTags`: provider · model · mode · thinking ·
  /// feature count, resolved against the provider snapshot.
  String _summary(BuildContext context, JsonMap profile) {
    final providerId = stringValue(profile['provider']);
    final entry = providers.where((e) => e['provider'] == providerId).firstOrNull ?? const {};
    final tags = <String>[stringValue(entry['label'], providerId)];
    final model = stringValue(profile['model']).trim();
    if (model.isNotEmpty) {
      final found = jsonList(entry['models']).map(jsonMap).where((m) => m['id'] == model).firstOrNull;
      tags.add(stringValue(found?['label'], model));
    }
    final mode = stringValue(profile['modeId']).trim();
    if (mode.isNotEmpty) {
      final found = jsonList(entry['modes']).map(jsonMap).where((m) => m['id'] == mode).firstOrNull;
      tags.add(found == null ? mode : formatAgentModeLabel(found));
    }
    final thinking = stringValue(profile['thinkingOptionId']).trim();
    if (thinking.isNotEmpty) {
      tags.add(formatThinkingOptionLabel(context, {'id': thinking}));
    }
    final features = jsonMap(profile['featureValues']).length;
    if (features > 0) {
      tags.add(tr(
        context,
        features == 1
            ? 'settings.host.agentProfiles.featureCountOne'
            : 'settings.host.agentProfiles.featureCount',
        args: {'count': features},
      ));
    }
    return tags.where((tag) => tag.isNotEmpty).join(' · ');
  }

  Widget _row(BuildContext context, List<JsonMap> profiles, int i) {
    String t(String key) => tr(context, 'settings.host.agentProfiles.$key');
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final profile = profiles[i];
    final notes = stringValue(profile['notes']);
    final mutedSmall = theme.textTheme.bodySmall!.copyWith(
      color: colors.muted,
      fontSize: type.uiSize(12),
    );
    return ConstrainedBox(
      constraints: const BoxConstraints(minHeight: 56),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
        child: Row(
          spacing: 8,
          children: [
            Expanded(
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                spacing: 8,
                children: [
                  Padding(
                    padding: const EdgeInsets.only(top: 1),
                    child: AgentProfileGlyph(
                      icon: profile['icon'] as String?,
                      color: profile['color'] as String?,
                    ),
                  ),
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Row(
                          crossAxisAlignment: CrossAxisAlignment.baseline,
                          textBaseline: TextBaseline.alphabetic,
                          spacing: 8,
                          children: [
                            Flexible(
                              flex: 0,
                              child: Text(
                                stringValue(profile['name']),
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                                style: theme.textTheme.bodyMedium!.copyWith(
                                  fontSize: type.uiSize(14),
                                ),
                              ),
                            ),
                            Flexible(
                              child: Text(
                                _summary(context, profile),
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                                style: mutedSmall,
                              ),
                            ),
                          ],
                        ),
                        if (notes.isNotEmpty)
                          Padding(
                            padding: const EdgeInsets.only(top: 4),
                            child: Row(
                              crossAxisAlignment: CrossAxisAlignment.start,
                              spacing: 4,
                              children: [
                                Padding(
                                  padding: const EdgeInsets.only(top: 2),
                                  child: Icon(LucideIcons.fileText300, size: 12, color: colors.muted),
                                ),
                                Expanded(
                                  child: Text(
                                    notes,
                                    maxLines: 2,
                                    overflow: TextOverflow.ellipsis,
                                    style: mutedSmall,
                                  ),
                                ),
                              ],
                            ),
                          ),
                      ],
                    ),
                  ),
                ],
              ),
            ),
            Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                PaseoButton(
                  label: t(profile['cliDefault'] == true ? 'cliDefault' : 'useAsCliDefault'),
                  size: PaseoControlSize.sm,
                  variant: profile['cliDefault'] == true
                      ? PaseoButtonVariant.secondary
                      : PaseoButtonVariant.ghost,
                  onPressed: () => _save(context, [
                    for (final p in profiles)
                      {
                        ...p,
                        'cliDefault': p['id'] == profile['id'] && profile['cliDefault'] != true,
                      },
                  ]),
                ),
                settingsIconButton(
                  context,
                  LucideIcons.arrowUp300,
                  i == 0
                      ? null
                      : () => _save(context, [...profiles]..insert(i - 1, profiles[i])..removeAt(i + 1)),
                  label: t('moveUp'),
                ),
                settingsIconButton(
                  context,
                  LucideIcons.arrowDown300,
                  i == profiles.length - 1
                      ? null
                      : () => _save(context, [...profiles]..insert(i + 2, profiles[i])..removeAt(i)),
                  label: t('moveDown'),
                ),
                settingsIconButton(
                  context,
                  LucideIcons.pencil300,
                  () => edit(context, profile),
                  label: t('editProfile'),
                ),
                settingsIconButton(
                  context,
                  LucideIcons.trash2300,
                  () async {
                    if (!await confirmAction(
                      context,
                      title: t('removeConfirmTitle'),
                      message: tr(
                        context,
                        'settings.host.agentProfiles.removeConfirmMessage',
                        args: {'name': stringValue(profile['name'])},
                      ),
                      confirmLabel: t('remove'),
                      destructive: true,
                    )) {
                      return;
                    }
                    if (context.mounted) {
                      await _save(
                        context,
                        profiles.where((p) => p['id'] != profile['id']).toList(),
                      );
                    }
                  },
                  label: t('remove'),
                  destructive: true,
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    String t(String key) => tr(context, 'settings.host.agentProfiles.$key');
    final connected = client.status == ConnectionStatus.connected;
    if (!connected || !supportsAgentProfiles(client)) {
      return SettingsSection(
        title: t('sectionTitle'),
        card: false,
        children: [SettingsEmptyCard(t(connected ? 'unsupported' : 'unavailable'))],
      );
    }
    final profiles = jsonList(config['agentProfiles']).map(jsonMap).toList();
    return SettingsSection(
      title: t('sectionTitle'),
      card: false,
      trailing: settingsIconButton(
        context,
        LucideIcons.plus300,
        () => edit(context),
        label: t('addProfileTitle'),
      ),
      children: [
        if (profiles.isEmpty)
          SettingsEmptyCard(
            t('emptyState'),
            roomy: true,
            action: PaseoButton(
              label: t('newProfile'),
              icon: LucideIcons.plus300,
              size: PaseoControlSize.sm,
              onPressed: () => edit(context),
            ),
          )
        else
          SettingsRowsCard(
            children: [for (var i = 0; i < profiles.length; i++) _row(context, profiles, i)],
          ),
      ],
    );
  }
}

/// Original `agent-profile-edit-modal.tsx` (520px sheet): icon trigger beside
/// the name field, provider / model / mode / thinking selects, features, a
/// "When to use" textarea with hint, and stretched Cancel / Save buttons.
class ProfileEditor extends StatefulWidget {
  final PaseoClient client;
  final JsonMap? profile;

  /// profile-form-model.ts AgentProfileSeed: create-mode pre-fill
  /// (`provider`, `model`, `name`), ignored when editing [profile].
  final JsonMap? seed;
  final Future<void> Function(JsonMap) onSave;
  const ProfileEditor({
    super.key,
    required this.client,
    this.profile,
    this.seed,
    required this.onSave,
  });
  @override
  State<ProfileEditor> createState() => _ProfileEditorState();
}

class _ProfileEditorState extends State<ProfileEditor> {
  /// Original `use-profile-form-inputs.ts` HOME_SCOPE_CWD: profiles are
  /// host-wide, so features are probed in the daemon's home directory.
  static const _homeScopeCwd = '~';

  /// Original features query `staleTimeMs`.
  static const _featureStaleTime = Duration(minutes: 5);

  late final AgentProfileFormModel model;
  late final TextEditingController name, notes;
  final _featureCache = <String, (DateTime, List<JsonMap>?)>{};
  String? _requestedFeatureKey;

  @override
  void initState() {
    super.initState();
    model = AgentProfileFormModel(
      editing: widget.profile != null,
      profile: widget.profile,
      seed: widget.seed,
      formatThinkingLabel: (option) => formatThinkingOptionLabel(context, option),
    )..addListener(_changed);
    name = TextEditingController(text: model.name);
    notes = TextEditingController(text: model.notes);
    widget.client.addListener(_connectionChanged);
    _loadCatalog();
  }

  @override
  void dispose() {
    widget.client.removeListener(_connectionChanged);
    model
      ..removeListener(_changed)
      ..close();
    for (final c in [name, notes]) {
      c.dispose();
    }
    super.dispose();
  }

  void _changed() {
    if (!mounted) return;
    setState(() {});
    _loadFeatures();
  }

  /// The original queries are gated on the connection and run again once it
  /// returns.
  void _connectionChanged() {
    if (!mounted || widget.client.status != ConnectionStatus.connected) return;
    if (model.catalogResolution != AgentProfileResolution.complete) _loadCatalog();
    _requestedFeatureKey = null;
    _loadFeatures();
  }

  /// Original `useAgentProfileFormCatalog`: the host-wide (`cwd: null`)
  /// provider snapshot; the provider select stays loading until it lands.
  Future<void> _loadCatalog() async {
    if (widget.client.status != ConnectionStatus.connected) return;
    try {
      final result = await settingsRequest(
        widget.client,
        'get_providers_snapshot_request',
        {},
        responseType: 'get_providers_snapshot_response',
      );
      if (mounted) model.applyProviderCatalog(providerEntries(result));
    } catch (_) {
      // Without a snapshot the original catalog stays unresolved (loading).
    }
  }

  /// Original `useAgentProfileFormFeatures`: one request per feature key,
  /// cached for the stale time and never retried; an error resolves the key
  /// to no features.
  Future<void> _loadFeatures() async {
    final key = model.featureRequestKey;
    final request = model.featureRequest;
    if (key == null || request == null || key == _requestedFeatureKey) return;
    if (widget.client.status != ConnectionStatus.connected) return;
    _requestedFeatureKey = key;
    final cached = _featureCache[key];
    if (cached != null && DateTime.now().difference(cached.$1) < _featureStaleTime) {
      final features = cached.$2;
      features == null ? model.applyFeaturesUnavailable(key) : model.applyFeatures(key, features);
      return;
    }
    try {
      final result = await settingsRequest(
        widget.client,
        'list_provider_features_request',
        {
          'draftConfig': {...request, 'cwd': _homeScopeCwd},
        },
        responseType: 'list_provider_features_response',
      );
      final features = jsonList(result['features']).map(jsonMap).toList();
      _featureCache[key] = (DateTime.now(), features);
      if (mounted) model.applyFeatures(key, features);
    } catch (_) {
      _featureCache[key] = (DateTime.now(), null);
      if (mounted) model.applyFeaturesUnavailable(key);
    }
  }

  /// Original `handleSave`: the form's submit value; create mints a new id,
  /// edit keeps the profile's. The caller owns how the record is stored.
  Future<void> save() async {
    final value = model.submitValue;
    if (value == null) return;
    model
      ..setSubmitError(null)
      ..setSubmitting(true);
    final record = <String, dynamic>{
      'id': widget.profile?['id'] ?? generateAgentProfileId(),
      ...value,
    };
    try {
      await widget.onSave(record);
      if (mounted) Navigator.pop(context, record);
    } catch (e) {
      if (mounted) model.setSubmitError(errorMessage(e));
    } finally {
      if (mounted) model.setSubmitting(false);
    }
  }

  Widget _select({
    required String label,
    required String value,
    required AgentProfileFormDisplay? display,
    required List<AgentProfileFormOption> options,
    required String placeholder,
    required String emptyText,
    required void Function(String id, AgentProfileFormDisplay? display) onChanged,
    bool loading = false,
  }) => Padding(
    padding: const EdgeInsets.only(bottom: 16),
    child: PaseoFormField(
      label: label,
      child: PaseoSelectField<String>(
        title: label,
        value: value.isEmpty ? null : value,
        // A stored value this catalog cannot resolve still reads as itself.
        placeholder: display?.label ?? placeholder,
        emptyText: emptyText,
        loading: loading,
        searchable: options.length > 6,
        options: [
          for (final option in options)
            PaseoSelectOption(option.id, option.label, description: option.description),
        ],
        onChanged: model.isSubmitting
            ? null
            : (id) {
                final option = options.where((o) => o.id == id).firstOrNull;
                onChanged(
                  id,
                  option == null || id.isEmpty
                      ? null
                      : (label: option.label, description: option.description),
                );
              },
      ),
    ),
  );

  /// Original `AgentProfileFeatureRow`: a select or a switch per feature.
  Widget _feature(JsonMap feature, {required bool first}) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final id = stringValue(feature['id']);
    final label = stringValue(feature['label']);
    final rawDescription = stringValue(feature['description']);
    final description = rawDescription.isEmpty ? null : rawDescription;
    final Widget child;
    if (feature['type'] == 'select') {
      final options = jsonList(feature['options']).map(jsonMap).toList();
      final value = feature['value'] is String ? feature['value'] as String : null;
      child = PaseoFormField(
        label: label,
        hint: description,
        child: PaseoSelectField<String>(
          title: label,
          value: value,
          placeholder: value == null || value.isEmpty
              ? label
              : stringValue(options.where((o) => o['id'] == value).firstOrNull?['label'], value),
          emptyText: label,
          options: [
            for (final o in options)
              PaseoSelectOption(
                stringValue(o['id']),
                stringValue(o['label']),
                description: o['description'] is String ? o['description'] as String : null,
              ),
          ],
          onChanged: model.isSubmitting ? null : (v) => model.setFeatureValue(id, v),
        ),
      );
    } else {
      child = Row(
        spacing: 12,
        children: [
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              spacing: 4,
              children: [
                Text(label, maxLines: 1, overflow: TextOverflow.ellipsis, style: theme.textTheme.bodyMedium),
                if (description != null)
                  Text(
                    description,
                    maxLines: 2,
                    overflow: TextOverflow.ellipsis,
                    style: theme.textTheme.bodySmall!.copyWith(color: colors.muted),
                  ),
              ],
            ),
          ),
          PaseoSwitch(
            value: feature['value'] == true,
            semanticLabel: label,
            onChanged: model.isSubmitting ? null : (v) => model.setFeatureValue(id, v),
          ),
        ],
      );
    }
    return Container(
      padding: const EdgeInsets.symmetric(vertical: 12),
      decoration: first
          ? null
          : BoxDecoration(border: Border(top: BorderSide(color: theme.dividerColor))),
      child: child,
    );
  }

  @override
  Widget build(BuildContext context) {
    String t(String key) => tr(context, 'settings.host.agentProfiles.$key');
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final base = theme.textTheme.bodyMedium!.copyWith(fontSize: type.uiSize(14), height: 1.4);
    final submitting = model.isSubmitting;
    return SheetContents(
      title: t(widget.profile == null ? 'addProfileTitle' : 'editProfileTitle'),
      stretchActions: true,
      actions: [
        PaseoButton(
          label: tr(context, 'common.actions.cancel'),
          expand: true,
          onPressed: submitting ? null : () => Navigator.pop(context),
        ),
        PaseoButton(
          label: t(submitting ? 'saving' : 'save'),
          variant: PaseoButtonVariant.primary,
          expand: true,
          onPressed: model.canSubmit ? save : null,
        ),
      ],
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            spacing: 8,
            children: [
              PaseoFormField(
                label: t('iconLabel'),
                child: _AppearanceTrigger(
                  icon: model.icon.isEmpty ? null : model.icon,
                  color: model.color.isEmpty ? null : model.color,
                  enabled: !submitting,
                  onChanged: (nextIcon, nextColor) => model.setAppearance(
                    icon: nextIcon ?? '',
                    color: nextColor == null || nextColor == 'none' ? '' : nextColor,
                  ),
                ),
              ),
              Expanded(
                child: PaseoFormField(
                  label: t('nameLabel'),
                  child: TextField(
                    controller: name,
                    enabled: !submitting,
                    autocorrect: false,
                    style: base,
                    onChanged: model.setName,
                    decoration: paseoInputDecoration(context, hintText: t('namePlaceholder')),
                  ),
                ),
              ),
            ],
          ),
          const SizedBox(height: 16),
          _select(
            label: t('providerLabel'),
            value: model.provider,
            display: model.providerDisplay,
            options: model.providerOptions,
            placeholder: t('providerPlaceholder'),
            emptyText: t('noProviders'),
            loading: model.catalogResolution != AgentProfileResolution.complete,
            onChanged: model.setProvider,
          ),
          if (model.showModelField)
            _select(
              label: t('modelLabel'),
              value: model.modelId,
              display: model.modelDisplay,
              options: model.modelOptions,
              placeholder: t('modelLabel'),
              emptyText: t('noModels'),
              onChanged: model.setModel,
            ),
          if (model.showModeField)
            _select(
              label: t('modeLabel'),
              value: model.modeId,
              display: model.modeDisplay,
              options: model.modeOptions,
              placeholder: t('modeLabel'),
              emptyText: t('noModes'),
              onChanged: model.setMode,
            ),
          if (model.showThinkingField)
            _select(
              label: t('thinkingLabel'),
              value: model.thinkingOptionId,
              display: model.thinkingDisplay,
              options: model.thinkingOptions,
              placeholder: t('thinkingLabel'),
              emptyText: t('noThinkingOptions'),
              onChanged: model.setThinking,
            ),
          if (model.showFeaturesField)
            Padding(
              padding: const EdgeInsets.only(bottom: 16),
              child: PaseoFormField(
                label: t('featuresLabel'),
                child: Container(
                  padding: const EdgeInsets.symmetric(horizontal: 12),
                  decoration: BoxDecoration(
                    color: colors.surface2,
                    border: Border.all(color: theme.dividerColor),
                    borderRadius: BorderRadius.circular(8),
                  ),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    children: [
                      for (var i = 0; i < model.features.length; i++)
                        _feature(model.features[i], first: i == 0),
                    ],
                  ),
                ),
              ),
            ),
          PaseoFormField(
            label: t('notesLabel'),
            hint: t('notesHint'),
            child: ConstrainedBox(
              constraints: const BoxConstraints(minHeight: 88),
              child: TextField(
                controller: notes,
                enabled: !submitting,
                style: base,
                minLines: 4,
                maxLines: 8,
                onChanged: model.setNotes,
                decoration: paseoInputDecoration(
                  context,
                  hintText: t('notesPlaceholder'),
                  multiline: true,
                ),
              ),
            ),
          ),
          if (model.submitError != null)
            Padding(
              padding: const EdgeInsets.only(top: 16),
              child: SelectableText(
                model.submitError!,
                style: base.copyWith(color: colors.danger),
              ),
            ),
        ],
      ),
    );
  }
}

/// Original `AgentProfileAppearanceField` trigger: md field geometry (44px,
/// 8px radius) on surface2 with an 8px inset, the glyph and a muted chevron
/// 4px apart; the transparent border turns borderAccent on hover/press.
/// Picks the icon, then the identity colour.
class _AppearanceTrigger extends StatefulWidget {
  const _AppearanceTrigger({
    required this.icon,
    required this.color,
    required this.enabled,
    required this.onChanged,
  });
  final String? icon, color;
  final bool enabled;
  final void Function(String? icon, String? color) onChanged;

  @override
  State<_AppearanceTrigger> createState() => _AppearanceTriggerState();
}

class _AppearanceTriggerState extends State<_AppearanceTrigger> {
  bool hovered = false, pressed = false;
  String? get icon => widget.icon;
  String? get color => widget.color;
  bool get enabled => widget.enabled;
  void Function(String? icon, String? color) get onChanged => widget.onChanged;

  Future<void> pick(BuildContext context) async {
    final label = tr(context, 'settings.host.agentProfiles.iconLabel');
    final nextIcon = await showPaseoSelect<String>(
      context: context,
      title: label,
      value: icon ?? '',
      searchable: true,
      options: [
        PaseoSelectOption(
          '',
          tr(context, 'settings.host.agentProfiles.noIcon'),
          leading: AgentProfileGlyph(color: color),
        ),
        for (final key in agentProfileIcons.keys)
          PaseoSelectOption(key, key, leading: AgentProfileGlyph(icon: key, color: color)),
      ],
    );
    if (nextIcon == null || !context.mounted) return;
    final chosenIcon = nextIcon.isEmpty ? null : nextIcon;
    final nextColor = await showPaseoSelect<String>(
      context: context,
      title: label,
      value: color ?? 'none',
      options: [
        for (final name in ['none', ..._identityForegrounds.keys])
          PaseoSelectOption(
            name,
            name == 'none' ? tr(context, 'settings.host.agentProfiles.noIcon') : name,
            leading: AgentProfileGlyph(icon: chosenIcon, color: name),
          ),
      ],
    );
    onChanged(chosenIcon, nextColor ?? color);
  }

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    return Opacity(
      opacity: enabled ? 1 : .5,
      child: MouseRegion(
        cursor: enabled ? SystemMouseCursors.click : SystemMouseCursors.basic,
        onEnter: (_) => setState(() => hovered = true),
        onExit: (_) => setState(() => hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: enabled ? (_) => setState(() => pressed = true) : null,
          onTapCancel: () => setState(() => pressed = false),
          onTapUp: (_) => setState(() => pressed = false),
          onTap: enabled ? () => pick(context) : null,
          child: Container(
            height: 44,
            padding: const EdgeInsets.symmetric(horizontal: 8),
            decoration: BoxDecoration(
              color: colors.surface2,
              border: Border.all(
                color: enabled && (hovered || pressed) ? colors.borderAccent : Colors.transparent,
              ),
              borderRadius: BorderRadius.circular(8),
            ),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              spacing: 4,
              children: [
                AgentProfileGlyph(icon: icon, color: color),
                Icon(LucideIcons.chevronDown300, size: 16, color: colors.muted),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// Original `screens/settings/metadata-generation-page.tsx`: a "Model
/// selection" row with an Automatic / Manual segmented control (sm). Choosing
/// Automatic saves at once; Manual reveals the "Model" row, whose combined
/// provider/model picker saves the pick at once.
class MetadataSettings extends StatefulWidget {
  final PaseoClient client;
  final JsonMap config;
  final Future<void> Function(JsonMap) patch;
  const MetadataSettings({
    super.key,
    required this.client,
    required this.config,
    required this.patch,
  });
  @override
  State<MetadataSettings> createState() => _MetadataSettingsState();
}

/// Original `buildSelectableProviderSelectorProviders` model selection kind.
String metadataSelectionKind(JsonMap entry, {required bool connected}) {
  if (jsonList(entry['models']).isNotEmpty || entry['status'] == 'ready') return 'models';
  if (entry['status'] == 'loading') return connected ? 'loading' : 'error';
  return 'error';
}

/// Original `handleModelSelect`: the pick leads, later fallbacks stay; an
/// empty model is left out.
List<JsonMap> metadataProvidersAfterSelect(List<JsonMap> configured, String provider, String model) => [
  {'provider': provider, if (model.isNotEmpty) 'model': model},
  ...configured.skip(1),
];

class _MetadataSettingsState extends State<MetadataSettings> {
  List<JsonMap> entries = const [];
  bool catalogLoading = false, saving = false;
  Object? catalogError;

  /// Original `draftMode`: null follows the saved mode.
  bool? draftAutomatic;

  List<JsonMap> get configured =>
      jsonList(jsonMap(widget.config['metadataGeneration'])['providers']).map(jsonMap).toList();
  JsonMap? get configuredProvider => configured.firstOrNull;
  bool get automatic => draftAutomatic ?? configuredProvider == null;

  @override
  void initState() {
    super.initState();
    loadCatalog();
  }

  @override
  void didUpdateWidget(MetadataSettings old) {
    super.didUpdateWidget(old);
    final before = jsonList(jsonMap(old.config['metadataGeneration'])['providers']).map(jsonMap).firstOrNull;
    final after = configuredProvider;
    // Original: the draft resets whenever the saved provider or model changes.
    if (before?['provider'] != after?['provider'] || before?['model'] != after?['model']) {
      draftAutomatic = null;
    }
  }

  /// Original `useProvidersSnapshot(serverId)`.
  Future<void> loadCatalog() async {
    if (widget.client.status != ConnectionStatus.connected) return;
    setState(() {
      catalogLoading = true;
      catalogError = null;
    });
    try {
      final result = await settingsRequest(
        widget.client,
        'get_providers_snapshot_request',
        {},
        responseType: 'get_providers_snapshot_response',
      );
      if (mounted) setState(() => entries = providerEntries(result));
    } catch (e) {
      if (mounted) setState(() => catalogError = e);
    } finally {
      if (mounted) setState(() => catalogLoading = false);
    }
  }

  Future<void> saveProviders(List<JsonMap> providers) async {
    setState(() => saving = true);
    try {
      await widget.patch({
        'metadataGeneration': {'providers': providers},
      });
    } catch (e) {
      if (mounted) {
        setState(() => draftAutomatic = null);
        showError(
          context,
          '${tr(context, 'settings.metadataGeneration.saveError')}: ${errorMessage(e)}',
        );
      }
    } finally {
      if (mounted) setState(() => saving = false);
    }
  }

  void setMode(bool nextAutomatic) {
    setState(() => draftAutomatic = nextAutomatic);
    if (nextAutomatic) saveProviders(const []);
  }

  /// Original `CombinedModelSelector`: every enabled provider's selectable
  /// models in one list; providers still loading or failing are disabled rows.
  List<PaseoSelectOption<(String, String)>> modelOptions(BuildContext context) {
    final muted = PaseoColors.of(context).muted;
    final connected = widget.client.status == ConnectionStatus.connected;
    Widget glyph(String id) => SizedBox.square(
      dimension: 16,
      child: Center(child: ProviderIcon(provider: id, size: 16, color: muted)),
    );
    final options = <PaseoSelectOption<(String, String)>>[];
    for (final entry in entries.where((e) => e['enabled'] == true)) {
      final id = stringValue(entry['provider']);
      final label = stringValue(entry['label'], id);
      final kind = metadataSelectionKind(entry, connected: connected);
      if (kind != 'models') {
        final message = stringValue(entry['error']);
        options.add(PaseoSelectOption(
          (id, ''),
          label,
          description: kind == 'loading'
              ? tr(context, 'providerSelection.loading')
              : message.isNotEmpty
              ? message
              : tr(context, entry['status'] == 'unavailable' ? 'providerSelection.unavailable' : 'providerSelection.unknownError'),
          leading: glyph(id),
          enabled: false,
          separatorBefore: options.isNotEmpty,
        ));
        continue;
      }
      var first = true;
      for (final model in jsonList(entry['models']).map(jsonMap).where((m) => m['isSelectable'] != false)) {
        final modelId = stringValue(model['id']);
        options.add(PaseoSelectOption(
          (id, modelId),
          stringValue(model['label'], modelId),
          description: label,
          leading: glyph(id),
          separatorBefore: first && options.isNotEmpty,
        ));
        first = false;
      }
    }
    return options;
  }

  void selectModel((String, String) picked) {
    setState(() => draftAutomatic = false);
    saveProviders(metadataProvidersAfterSelect(configured, picked.$1, picked.$2));
  }

  /// Original `resolveSelectedModelLabel` for the trigger.
  String selectedLabel(BuildContext context) {
    final providerId = stringValue(configuredProvider?['provider']);
    final modelId = stringValue(configuredProvider?['model']);
    final entry = entries.where((e) => e['enabled'] == true && e['provider'] == providerId).firstOrNull;
    if (entry == null) {
      return tr(context, catalogLoading ? 'providerSelection.loading' : 'providerSelection.selectModel');
    }
    switch (metadataSelectionKind(entry, connected: widget.client.status == ConnectionStatus.connected)) {
      case 'loading':
        return tr(context, 'providerSelection.loading');
      case 'error':
        return tr(context, 'providerSelection.error');
    }
    final models = jsonList(entry['models']).map(jsonMap).where((m) => m['isSelectable'] != false).toList();
    final found = models.where((m) => m['id'] == modelId).firstOrNull;
    if (found == null && modelId.isNotEmpty) return modelId;
    final row = found ?? models.where((m) => m['isDefault'] == true).firstOrNull ?? models.firstOrNull;
    return row == null ? tr(context, 'providerSelection.selectModel') : stringValue(row['label'], stringValue(row['id']));
  }

  @override
  Widget build(BuildContext context) {
    String t(String key) => tr(context, 'settings.metadataGeneration.$key');
    final providerId = stringValue(configuredProvider?['provider']);
    final muted = PaseoColors.of(context).muted;
    return SettingsSection(
      title: t('title'),
      info: t('description'),
      card: false,
      trailing: SettingsExternalLink(
        label: t('docs'),
        url: 'https://paseo.sh/docs/metadata-generation',
      ),
      children: [
        SettingsRowsCard(
          children: [
            SettingsRow(
              label: t('selection'),
              hint: t(automatic ? 'automaticHint' : 'preferredHint'),
              control: PaseoSegmentedControl<bool>(
                size: PaseoControlSize.sm,
                value: automatic,
                segments: [
                  PaseoSegment(true, t('automatic')),
                  PaseoSegment(false, t('preferred')),
                ],
                onChanged: setMode,
              ),
            ),
            if (!automatic)
              SettingsRow(
                label: t('model'),
                hint: t('modelHint'),
                control: ConstrainedBox(
                  constraints: const BoxConstraints(maxWidth: 260),
                  child: PaseoSelectField<(String, String)>(
                    title: tr(context, 'providerSelection.selectModel'),
                    value: (providerId, stringValue(configuredProvider?['model'])),
                    placeholder: selectedLabel(context),
                    size: PaseoControlSize.sm,
                    loading: catalogLoading,
                    searchable: true,
                    searchPlaceholder: tr(context, 'modelSelector.searchAllPlaceholder'),
                    emptyText: catalogLoading
                        ? tr(context, 'modelSelector.loadingSelector')
                        : catalogError != null
                        ? errorMessage(catalogError!)
                        : tr(context, 'providerSelection.readiness.noProviders'),
                    leading: providerId.isEmpty
                        ? null
                        : SizedBox.square(
                            dimension: 16,
                            child: Center(child: ProviderIcon(provider: providerId, size: 16, color: muted)),
                          ),
                    options: modelOptions(context),
                    onChanged: saving ? null : selectModel,
                  ),
                ),
              ),
          ],
        ),
      ],
    );
  }
}
