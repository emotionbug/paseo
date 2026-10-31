import 'dart:math';

import 'package:flutter/foundation.dart';

import '../../core/json.dart';
import '../chat/agent_controls.dart' show formatAgentModeLabel;

/// Original `agent-profiles/internal/profile-id.ts` `generateAgentProfileId`.
String generateAgentProfileId([Random? random]) {
  final source = random ?? Random();
  var suffix = '';
  while (suffix.length < 10) {
    suffix += source.nextInt(1 << 30).toRadixString(36);
  }
  return 'agent_profile_${DateTime.now().millisecondsSinceEpoch.toRadixString(36)}_$suffix';
}

/// A selected value's trigger label (and description) before or after the
/// catalog resolves it.
typedef AgentProfileFormDisplay = ({String label, String? description});

/// One select option: every real option is selected by its own id.
typedef AgentProfileFormOption = ({String id, String label, String? description});

enum AgentProfileResolution { idle, pending, complete }

/// Original `agent-profiles/internal/profile-form-model.ts`
/// (`openAgentProfileForm`): the create/edit form's selections, seeded to the
/// catalog's defaults (no synthetic "provider default" rows), provider-scoped
/// resets, feature pruning and the submit value.
///
/// [formatThinkingLabel] is `formatThinkingOptionLabel` bound to a locale.
class AgentProfileFormModel extends ChangeNotifier {
  AgentProfileFormModel({
    required bool editing,
    JsonMap? profile,
    JsonMap? seed,
    String Function(JsonMap option)? formatThinkingLabel,
  }) : mode = editing ? 'edit' : 'create',
       _formatThinking = formatThinkingLabel ?? _rawLabel {
    final stored = profile ?? const <String, dynamic>{};
    final createSeed = editing ? null : seed;
    name = stringValue(createSeed?['name'], stringValue(stored['name']));
    provider = stringValue(createSeed?['provider'], stringValue(stored['provider']));
    modelId = stringValue(
      createSeed?['modelId'],
      stringValue(createSeed?['model'], stringValue(stored['model'])),
    );
    icon = stringValue(stored['icon']);
    color = stringValue(stored['color']);
    notes = stringValue(stored['notes']);
    modeId = stringValue(stored['modeId']);
    thinkingOptionId = stringValue(stored['thinkingOptionId']);
    featureValues = {...jsonMap(stored['featureValues'])};
    providerDisplay = _seedDisplay(provider);
    modelDisplay = _seedDisplay(modelId);
    modeDisplay = _seedDisplay(modeId);
    thinkingDisplay = _seedDisplay(thinkingOptionId);
    _derive();
  }

  final String mode;
  final String Function(JsonMap option) _formatThinking;

  String name = '', icon = '', color = '', notes = '';
  String provider = '', modelId = '', modeId = '', thinkingOptionId = '';
  Map<String, dynamic> featureValues = {};

  AgentProfileFormDisplay? providerDisplay, modelDisplay, modeDisplay, thinkingDisplay;

  List<AgentProfileFormOption> providerOptions = const [];
  List<AgentProfileFormOption> modelOptions = const [];
  List<AgentProfileFormOption> modeOptions = const [];
  List<AgentProfileFormOption> thinkingOptions = const [];

  /// The resolved features with the form's values applied.
  List<JsonMap> features = const [];

  AgentProfileResolution catalogResolution = AgentProfileResolution.idle;
  AgentProfileResolution featureResolution = AgentProfileResolution.idle;

  /// The `list_provider_features_request` draft (without `cwd`), or null.
  JsonMap? featureRequest;
  String? featureRequestKey;

  bool showModelField = false, showModeField = false;
  bool showThinkingField = false, showFeaturesField = false;
  bool isSubmitting = false;
  String? submitError;
  bool canSubmit = false;

  /// The persisted profile minus `id` (and `cliDefault`), or null.
  JsonMap? submitValue;

  List<JsonMap> _entries = const [];
  String? _resolvedFeatureKey;
  List<JsonMap> _resolvedFeatures = const [];
  bool _closed = false;

  static String _rawLabel(JsonMap option) =>
      stringValue(option['label'], stringValue(option['id']));

  static AgentProfileFormDisplay? _seedDisplay(String value) =>
      value.isEmpty ? null : (label: value, description: null);

  JsonMap? _findEntry(String id) =>
      _entries.where((entry) => entry['provider'] == id).firstOrNull;

  List<JsonMap> _models(String id) => jsonList(_findEntry(id)?['models'])
      .map(jsonMap)
      .where((model) => model['isSelectable'] != false)
      .toList();

  List<JsonMap> _modes(String id) => jsonList(_findEntry(id)?['modes']).map(jsonMap).toList();

  /// Thinking options hang off the model the daemon would pick when unset.
  static JsonMap? _effectiveModel(List<JsonMap> models, String id) {
    final trimmed = id.trim();
    if (trimmed.isNotEmpty) return models.where((m) => m['id'] == trimmed).firstOrNull;
    return models.where((m) => m['isDefault'] == true).firstOrNull ?? models.firstOrNull;
  }

  List<JsonMap> _thinking(String providerId, String model) =>
      jsonList(_effectiveModel(_models(providerId), model)?['thinkingOptions']).map(jsonMap).toList();

  static String _defaultModelId(List<JsonMap> models) => stringValue(
    (models.where((m) => m['isDefault'] == true).firstOrNull ?? models.firstOrNull)?['id'],
  );

  static String _defaultModeId(JsonMap? entry, List<JsonMap> modes) =>
      stringValue(entry?['defaultModeId'], stringValue(modes.firstOrNull?['id']));

  static String _defaultThinkingId(JsonMap? model) {
    final options = jsonList(model?['thinkingOptions']).map(jsonMap).toList();
    final declared = model?['defaultThinkingOptionId'];
    if (declared is String) return declared;
    return stringValue(
      (options.where((o) => o['isDefault'] == true).firstOrNull ?? options.firstOrNull)?['id'],
    );
  }

  static AgentProfileFormDisplay? _refine(
    AgentProfileFormDisplay? current,
    String selected,
    String? resolved,
  ) {
    if (selected.isEmpty) return null;
    if (resolved != null && resolved.isNotEmpty) return (label: resolved, description: null);
    return current ?? (label: selected, description: null);
  }

  void _derive() {
    final entry = _findEntry(provider);
    final models = _models(provider);
    final modes = _modes(provider);
    // seedSelections: every selection resolves to a concrete id once known.
    if (modelId.isEmpty) modelId = _defaultModelId(models);
    if (modeId.isEmpty) modeId = _defaultModeId(entry, modes);
    if (thinkingOptionId.isEmpty) {
      thinkingOptionId = _defaultThinkingId(_effectiveModel(models, modelId));
    }
    final thinking = _thinking(provider, modelId);

    featureRequest = provider.isEmpty
        ? null
        : {
            'provider': provider,
            if (modelId.isNotEmpty) 'model': modelId,
            if (modeId.isNotEmpty) 'modeId': modeId,
            if (thinkingOptionId.isNotEmpty) 'thinkingOptionId': thinkingOptionId,
          };
    featureRequestKey = featureRequest == null
        ? null
        : [provider, modelId, modeId, thinkingOptionId].join('|');
    final current = featureRequestKey != null && featureRequestKey == _resolvedFeatureKey;
    features = current
        ? [
            for (final feature in _resolvedFeatures)
              featureValues.containsKey(feature['id'])
                  ? {...feature, 'value': featureValues[feature['id']]}
                  : feature,
          ]
        : const [];
    featureResolution = featureRequestKey == null
        ? AgentProfileResolution.idle
        : current
        ? AgentProfileResolution.complete
        : AgentProfileResolution.pending;

    providerOptions = [
      for (final e in _entries)
        if (e['enabled'] == true)
          (
            id: stringValue(e['provider']),
            label: stringValue(e['label'], stringValue(e['provider'])),
            description: e['description'] as String?,
          ),
    ];
    modelOptions = [
      for (final m in models)
        (
          id: stringValue(m['id']),
          label: stringValue(m['label'], stringValue(m['id'])),
          // Models are labelled by family; the id tells two of them apart.
          description: stringValue(m['description'], stringValue(m['id'])),
        ),
    ];
    modeOptions = [
      for (final m in modes)
        (id: stringValue(m['id']), label: formatAgentModeLabel(m), description: m['description'] as String?),
    ];
    thinkingOptions = [
      for (final o in thinking)
        (id: stringValue(o['id']), label: _formatThinking(o), description: o['description'] as String?),
    ];

    providerDisplay = _refine(providerDisplay, provider, entry?['label'] as String?);
    modelDisplay = _refine(
      modelDisplay,
      modelId,
      models.where((m) => m['id'] == modelId).firstOrNull?['label'] as String?,
    );
    final mode = modes.where((m) => m['id'] == modeId).firstOrNull;
    modeDisplay = _refine(modeDisplay, modeId, mode == null ? null : formatAgentModeLabel(mode));
    final option = thinking.where((o) => o['id'] == thinkingOptionId).firstOrNull;
    thinkingDisplay = _refine(
      thinkingDisplay,
      thinkingOptionId,
      option == null ? null : _formatThinking(option),
    );

    // A field appears once the catalog can populate it, and stays while a
    // stored value needs a home.
    final hasProvider = provider.isNotEmpty;
    showModelField = hasProvider && (models.isNotEmpty || modelId.isNotEmpty);
    showModeField = hasProvider && (modes.isNotEmpty || modeId.isNotEmpty);
    showThinkingField = hasProvider && (thinking.isNotEmpty || thinkingOptionId.isNotEmpty);
    showFeaturesField = hasProvider && features.isNotEmpty;
    canSubmit = name.trim().isNotEmpty && provider.isNotEmpty && !isSubmitting;
    final trimmedNotes = notes.trim();
    submitValue = canSubmit
        ? {
            'name': name.trim(),
            if (icon.isNotEmpty) 'icon': icon,
            if (color.isNotEmpty) 'color': color,
            'provider': provider,
            if (modelId.isNotEmpty) 'model': modelId,
            if (modeId.isNotEmpty) 'modeId': modeId,
            if (thinkingOptionId.isNotEmpty) 'thinkingOptionId': thinkingOptionId,
            if (featureValues.isNotEmpty) 'featureValues': {...featureValues},
            if (trimmedNotes.isNotEmpty) 'notes': trimmedNotes,
          }
        : null;
  }

  void _publish(void Function() mutate) {
    if (_closed) return;
    mutate();
    _derive();
    notifyListeners();
  }

  /// Late input: the host-wide provider catalog. Never touches selections
  /// beyond seeding unset ones.
  void applyProviderCatalog(List<JsonMap> entries) => _publish(() {
    _entries = [...entries];
    catalogResolution = AgentProfileResolution.complete;
  });

  /// Late input: features for one request; stale keys are ignored. Values for
  /// features the request no longer lists are dropped.
  void applyFeatures(String requestKey, List<JsonMap> next) {
    if (requestKey != featureRequestKey) return;
    _publish(() {
      _resolvedFeatureKey = requestKey;
      _resolvedFeatures = [...next];
      final allowed = {for (final f in next) f['id']};
      if (featureValues.keys.any((id) => !allowed.contains(id))) {
        featureValues = {
          for (final e in featureValues.entries)
            if (allowed.contains(e.key)) e.key: e.value,
        };
      }
    });
  }

  /// A request that produced no usable features (provider error).
  void applyFeaturesUnavailable(String requestKey) {
    if (requestKey != featureRequestKey) return;
    _publish(() {
      _resolvedFeatureKey = requestKey;
      _resolvedFeatures = const [];
    });
  }

  void setName(String value) => _publish(() => name = value);

  void setAppearance({required String icon, required String color}) => _publish(() {
    this.icon = icon;
    this.color = color;
  });

  void setNotes(String value) => _publish(() => notes = value);

  /// Everything below the provider is provider-scoped; clearing it lets the
  /// new provider's defaults seed.
  void setProvider(String id, AgentProfileFormDisplay? display) {
    if (provider == id) return;
    _publish(() {
      provider = id;
      providerDisplay = display;
      modelId = '';
      modelDisplay = null;
      modeId = '';
      modeDisplay = null;
      thinkingOptionId = '';
      thinkingDisplay = null;
      featureValues = {};
    });
  }

  /// A thinking level the new model does not offer has to go.
  void setModel(String id, AgentProfileFormDisplay? display) {
    if (modelId == id) return;
    _publish(() {
      final keeps = thinkingOptionId.isNotEmpty &&
          _thinking(provider, id).any((o) => o['id'] == thinkingOptionId);
      modelId = id;
      modelDisplay = display;
      if (!keeps) {
        thinkingOptionId = '';
        thinkingDisplay = null;
      }
    });
  }

  void setMode(String id, AgentProfileFormDisplay? display) => _publish(() {
    modeId = id;
    modeDisplay = display;
  });

  void setThinking(String id, AgentProfileFormDisplay? display) => _publish(() {
    thinkingOptionId = id;
    thinkingDisplay = display;
  });

  void setFeatureValue(String id, Object? value) =>
      _publish(() => featureValues = {...featureValues, id: value});

  void setSubmitting(bool value) => _publish(() => isSubmitting = value);

  void setSubmitError(String? value) => _publish(() => submitError = value);

  void close() => _closed = true;
}
