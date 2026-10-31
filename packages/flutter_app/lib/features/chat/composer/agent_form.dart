import '../../../core/json.dart';
import '../chat_controller.dart' show maps;

/// create-agent-preferences: the remembered new-agent selection, stored in
/// the app preferences under this key (legacy imports land here too).
const formPreferencesKey = 'createAgentPreferences';

/// preferences.ts parseFormPreferences: the current shape, or the legacy one
/// whose per-provider `thinkingOptionId` belongs to that provider's model.
JsonMap parseFormPreferences(Object? value) {
  if (value is! Map) return {};
  final source = jsonMap(value);
  final providers = <String, dynamic>{};
  for (final entry in jsonMap(source['providerPreferences']).entries) {
    final prefs = jsonMap(entry.value);
    final model = prefs['model'] is String ? prefs['model'] as String : null;
    final thinking = <String, dynamic>{
      for (final row in jsonMap(prefs['thinkingByModel']).entries)
        if (row.value is String) row.key: row.value,
      if (model != null && prefs['thinkingOptionId'] is String) model: prefs['thinkingOptionId'],
    };
    providers[entry.key] = <String, dynamic>{
      'model': ?model,
      if (prefs['mode'] is String) 'mode': prefs['mode'],
      if (thinking.isNotEmpty) 'thinkingByModel': thinking,
      if (prefs['featureValues'] is Map) 'featureValues': jsonMap(prefs['featureValues']),
    };
  }
  return {
    if (source['provider'] is String) 'provider': source['provider'],
    if (providers.isNotEmpty) 'providerPreferences': providers,
    for (final key in ['favoriteModels', 'isolation', 'launchTarget'])
      if (source[key] != null) key: source[key],
  };
}

/// preferences.ts mergeProviderPreferences: [provider] becomes the remembered
/// provider and its entry takes the given fields (a null [mode] with
/// [clearMode] forgets the mode; record fields merge).
JsonMap mergeProviderPreferences(
  JsonMap preferences,
  String provider, {
  String? model,
  String? mode,
  bool clearMode = false,
  Map<String, String>? thinkingByModel,
  JsonMap? featureValues,
}) {
  final all = jsonMap(preferences['providerPreferences']);
  final next = <String, dynamic>{...jsonMap(all[provider])};
  if (model != null) next['model'] = model;
  if (clearMode) {
    next.remove('mode');
  } else if (mode != null) {
    next['mode'] = mode;
  }
  if (thinkingByModel != null) {
    next['thinkingByModel'] = {...jsonMap(next['thinkingByModel']), ...thinkingByModel};
  }
  if (featureValues != null) {
    next['featureValues'] = {...jsonMap(next['featureValues']), ...featureValues};
  }
  return {
    ...preferences,
    'provider': provider,
    'providerPreferences': {...all, provider: next},
  };
}

JsonMap? _entry(List<JsonMap> providers, String? provider) =>
    providers.where((entry) => entry['provider'] == provider).firstOrNull;

/// model-catalog.ts filterSelectableModels; null while the provider is not
/// ready (buildProviderModelsByProvider).
List<JsonMap>? readyModels(List<JsonMap> providers, String? provider) {
  final entry = _entry(providers, provider);
  if (entry == null || entry['status'] != 'ready') return null;
  return maps(entry['models']).where((model) => model['isSelectable'] != false).toList();
}

/// model-catalog.ts findModelByReference: by id, then by alias.
JsonMap? findModelByReference(List<JsonMap>? models, String modelId) {
  final id = modelId.trim();
  if (models == null || models.isEmpty || id.isEmpty) return null;
  return models.where((model) => model['id'] == id).firstOrNull ??
      models.where((model) => jsonList(model['aliases']).contains(id)).firstOrNull;
}

/// resolve-agent-form.ts resolveDefaultModelId.
String resolveDefaultModelId(List<JsonMap>? models) {
  if (models == null || models.isEmpty) return '';
  return stringValue((models.where((model) => model['isDefault'] == true).firstOrNull ??
      models.first)['id']);
}

String _canonicalModelId(List<JsonMap>? models, String modelId) {
  final id = modelId.trim();
  if (id.isEmpty || models == null) return id;
  return stringValue(findModelByReference(models, id)?['id']);
}

/// resolve-agent-form.ts resolveThinkingOptionId: the requested option when
/// the model offers it, else the model's default or first option.
String resolveThinkingOptionId(List<JsonMap>? models, String modelId, String requested) {
  final model = findModelByReference(models, modelId);
  final options = maps(model?['thinkingOptions']);
  if (options.isEmpty) return '';
  final id = requested.trim();
  if (id.isNotEmpty && options.any((option) => option['id'] == id)) return id;
  return stringValue(model?['defaultThinkingOptionId'], stringValue(options.first['id']));
}

String _preferredThinking(List<JsonMap>? models, JsonMap providerPrefs, String modelId) {
  final model = findModelByReference(models, modelId);
  final references = model == null
      ? [modelId]
      : [stringValue(model['id']), ...jsonList(model['aliases']).map((alias) => '$alias')];
  final byModel = jsonMap(providerPrefs['thinkingByModel']);
  for (final reference in references) {
    final id = stringValue(byModel[reference]).trim();
    if (id.isNotEmpty) return id;
  }
  return '';
}

/// resolve-agent-form.ts resolvePreferredModeId: an explicit or remembered
/// mode is kept as-is; otherwise the provider's default (when it offers it)
/// or first mode. Only ready/loading enabled providers define modes.
String _preferredModeId(List<JsonMap> providers, String? provider, String initial, String preferred) {
  if (initial.trim().isNotEmpty) return initial.trim();
  if (preferred.trim().isNotEmpty) return preferred.trim();
  final entry = _entry(providers, provider);
  if (entry == null ||
      entry['enabled'] == false ||
      !const ['ready', 'loading'].contains(entry['status'])) {
    return '';
  }
  final modes = maps(entry['modes']);
  final defaultMode = stringValue(entry['defaultModeId']);
  if (defaultMode.isNotEmpty && (modes.isEmpty || modes.any((mode) => mode['id'] == defaultMode))) {
    return defaultMode;
  }
  return stringValue(modes.firstOrNull?['id']);
}

JsonMap _config({
  required String? provider,
  required String model,
  required String modeId,
  required String thinkingOptionId,
  Object? featureValues,
}) => {
  if (provider != null && provider.isNotEmpty) 'provider': provider,
  if (model.isNotEmpty) 'model': model,
  if (modeId.isNotEmpty) 'modeId': modeId,
  if (thinkingOptionId.isNotEmpty) 'thinkingOptionId': thinkingOptionId,
  if (featureValues is Map && featureValues.isNotEmpty) 'featureValues': jsonMap(featureValues),
};

/// resolve-agent-form.ts resolveFormStateFromProviderModels with nothing
/// user-modified: the initial values first, then the remembered provider and
/// its remembered model, mode and per-model thinking option, then the
/// provider defaults. A remembered "default" model (pre-0.7.2) resolves to
/// the provider's default model.
JsonMap resolveInitialAgentConfig({
  required JsonMap initial,
  required JsonMap preferences,
  required List<JsonMap> providers,
}) {
  final provider = stringValue(initial['provider'], stringValue(preferences['provider']));
  if (provider.isEmpty) return _config(provider: null, model: '', modeId: '', thinkingOptionId: '');
  final prefs = jsonMap(jsonMap(preferences['providerPreferences'])[provider]);
  final models = readyModels(providers, provider);
  final modeId = _preferredModeId(
    providers,
    provider,
    stringValue(initial['modeId']),
    stringValue(prefs['mode']),
  );
  final initialModel = stringValue(initial['model']).trim();
  final preferredModel = stringValue(prefs['model']).trim();
  String model;
  if ((initialModel.isNotEmpty ? initialModel : preferredModel) == 'default' &&
      (models?.isNotEmpty ?? false)) {
    model = stringValue(findModelByReference(models, 'default')?['id']);
    if (model.isEmpty) model = resolveDefaultModelId(models);
  } else if (initialModel.isNotEmpty) {
    model = models == null ? initialModel : _canonicalModelId(models, initialModel);
    if (model.isEmpty) model = initialModel;
  } else if (preferredModel.isNotEmpty) {
    model = models == null ? preferredModel : _canonicalModelId(models, preferredModel);
    if (model.isEmpty) model = preferredModel;
  } else {
    model = '';
  }
  final initialThinking = stringValue(initial['thinkingOptionId']).trim();
  var thinking = initialThinking.isNotEmpty
      ? initialThinking
      : _preferredThinking(models, prefs, model);
  if (models != null) thinking = resolveThinkingOptionId(models, model, thinking);
  return _config(
    provider: provider,
    model: model,
    modeId: modeId,
    thinkingOptionId: thinking,
    featureValues: initial['featureValues'],
  );
}

/// resolve-agent-form.ts SET_PROVIDER_AND_MODEL_FROM_USER and
/// SET_MODEL_FROM_USER: the chosen model (canonical, or the provider default);
/// the mode stays within the same provider, else the remembered or default
/// mode; the thinking option stays for the same provider and model, else the
/// one remembered for that model, resolved against what it offers.
JsonMap applyUserModelChoice({
  required JsonMap previous,
  required String provider,
  required String modelId,
  required JsonMap preferences,
  required List<JsonMap> providers,
}) {
  final models = readyModels(providers, provider);
  final prefs = jsonMap(jsonMap(preferences['providerPreferences'])[provider]);
  var model = _canonicalModelId(models, modelId);
  if (model.isEmpty) model = resolveDefaultModelId(models);
  final sameProvider = previous['provider'] == provider;
  final requestedThinking =
      sameProvider && _canonicalModelId(models, stringValue(previous['model'])) == model
      ? stringValue(previous['thinkingOptionId'])
      : _preferredThinking(models, prefs, model);
  final currentMode = stringValue(previous['modeId']).trim();
  return _config(
    provider: provider,
    model: model,
    modeId: sameProvider && currentMode.isNotEmpty
        ? currentMode
        : _preferredModeId(providers, provider, '', stringValue(prefs['mode'])),
    thinkingOptionId: resolveThinkingOptionId(models, model, requestedThinking),
    featureValues: sameProvider ? previous['featureValues'] : null,
  );
}

/// resolve-agent-form.ts applyProfile: a profile's model (or the remembered
/// one, else the default); its mode, the remembered mode or the provider
/// default only when offered; its thinking option or the remembered/default.
JsonMap applyProfileChoice({
  required JsonMap profile,
  required JsonMap preferences,
  required List<JsonMap> providers,
}) {
  final provider = stringValue(profile['provider']);
  final models = readyModels(providers, provider);
  final prefs = jsonMap(jsonMap(preferences['providerPreferences'])[provider]);
  final preferredModel = stringValue(profile['model'], stringValue(prefs['model']));
  var model = _canonicalModelId(models, preferredModel);
  if (model.isEmpty) model = resolveDefaultModelId(models);
  final entry = _entry(providers, provider);
  final modeIds = {for (final mode in maps(entry?['modes'])) stringValue(mode['id'])};
  final preferredMode = stringValue(profile['modeId'], stringValue(prefs['mode']));
  final defaultMode = stringValue(entry?['defaultModeId']);
  final modeId = modeIds.contains(preferredMode)
      ? preferredMode
      : modeIds.contains(defaultMode)
      ? defaultMode
      : '';
  final profileThinking = stringValue(profile['thinkingOptionId']);
  return _config(
    provider: provider,
    model: model,
    modeId: modeId,
    thinkingOptionId: profileThinking.isNotEmpty
        ? profileThinking
        : resolveThinkingOptionId(models, model, _preferredThinking(models, prefs, model)),
    featureValues: profile['featureValues'],
  );
}

/// The preferences after the user changed [previous] to [next]
/// (use-agent-form-state.ts setProviderAndModelFromUser / setModeFromUser /
/// setThinkingOptionFromUser): the provider and its model, a changed mode,
/// and a changed thinking option for its model.
JsonMap rememberUserSelection(JsonMap preferences, JsonMap previous, JsonMap next) {
  final provider = stringValue(next['provider']);
  if (provider.isEmpty) return preferences;
  final model = stringValue(next['model']);
  if (previous['provider'] != provider || previous['model'] != next['model']) {
    return mergeProviderPreferences(preferences, provider, model: model.isEmpty ? null : model);
  }
  if (previous['modeId'] != next['modeId']) {
    final mode = stringValue(next['modeId']);
    return mergeProviderPreferences(preferences, provider, mode: mode.isEmpty ? null : mode);
  }
  if (previous['thinkingOptionId'] != next['thinkingOptionId'] && model.isNotEmpty) {
    return mergeProviderPreferences(
      preferences,
      provider,
      thinkingByModel: {model: stringValue(next['thinkingOptionId'])},
    );
  }
  // use-draft-agent-features.ts setFeatureValue.
  final before = jsonMap(previous['featureValues']);
  final changed = {
    for (final entry in jsonMap(next['featureValues']).entries)
      if (!before.containsKey(entry.key) || before[entry.key] != entry.value) entry.key: entry.value,
  };
  if (changed.isNotEmpty) {
    return mergeProviderPreferences(preferences, provider, featureValues: changed);
  }
  return preferences;
}

/// The feature values remembered for [provider].
JsonMap rememberedFeatureValues(JsonMap preferences, String provider) =>
    jsonMap(jsonMap(jsonMap(preferences['providerPreferences'])[provider])['featureValues']);

/// feature-preferences.ts resolveFeatureValues: for each offered feature, the
/// draft's own value, else the remembered one; values for features the
/// provider does not offer are dropped.
JsonMap resolveFeatureValues(List<JsonMap> features, JsonMap remembered, JsonMap local) => {
  for (final feature in features)
    if (local.containsKey(feature['id']))
      stringValue(feature['id']): local[feature['id']]
    else if (remembered.containsKey(feature['id']))
      stringValue(feature['id']): remembered[feature['id']],
};

/// feature-preferences.ts applyFeatureValues: the features showing [values].
List<JsonMap> applyFeatureValues(List<JsonMap> features, JsonMap values) => [
  for (final feature in features)
    values.containsKey(feature['id']) ? {...feature, 'value': values[feature['id']]} : feature,
];

/// preferences.ts applyAgentProfilePreferences: a mode remembered for the
/// previous provider that it no longer offers is forgotten; the profile's
/// provider, model, mode, thinking option and feature values are remembered.
JsonMap rememberProfileSelection(
  JsonMap preferences, {
  required String? previousProvider,
  required List<JsonMap> providers,
  required JsonMap config,
  required JsonMap profileFeatureValues,
}) {
  var result = preferences;
  if (previousProvider != null && previousProvider.isNotEmpty) {
    final previousMode = stringValue(
      jsonMap(jsonMap(result['providerPreferences'])[previousProvider])['mode'],
    );
    final offered = maps(_entry(providers, previousProvider)?['modes'])
        .map((mode) => stringValue(mode['id']));
    if (previousMode.isNotEmpty && !offered.contains(previousMode)) {
      result = mergeProviderPreferences(result, previousProvider, clearMode: true);
    }
  }
  final model = stringValue(config['model']);
  final mode = stringValue(config['modeId']);
  final thinking = stringValue(config['thinkingOptionId']);
  return mergeProviderPreferences(
    result,
    stringValue(config['provider']),
    model: model.isEmpty ? null : model,
    mode: mode.isEmpty ? null : mode,
    clearMode: mode.isEmpty,
    thinkingByModel: model.isNotEmpty && thinking.isNotEmpty ? {model: thinking} : null,
    featureValues: profileFeatureValues,
  );
}

/// use-agent-form-state.ts persistProviderPreferences, on submit: the
/// effective model (resolved, else as typed), the mode and that model's
/// thinking option.
JsonMap rememberSubmittedSelection(JsonMap preferences, JsonMap config, List<JsonMap> providers) {
  final provider = stringValue(config['provider']);
  if (provider.isEmpty) return preferences;
  final models = readyModels(providers, provider);
  final typed = stringValue(config['model']);
  final model = stringValue(
    (typed.trim().isEmpty ? null : findModelByReference(models, typed))?['id'],
    typed,
  );
  final mode = stringValue(config['modeId']);
  final thinking = stringValue(config['thinkingOptionId']);
  return mergeProviderPreferences(
    preferences,
    provider,
    model: model.isEmpty ? null : model,
    mode: mode.isEmpty ? null : mode,
    thinkingByModel: model.isNotEmpty && thinking.isNotEmpty ? {model: thinking} : null,
  );
}

/// provider-selection.ts resolveEffectiveComposer{Model,Mode,ThinkingOption}Id
/// and workspace-draft-agent-config.ts: the config a draft submits — the
/// selected or default model; the selected mode only when the provider offers
/// it (else its first mode; kept while the catalog is loading); the selected
/// or the model's default thinking option. Empty fields are omitted.
JsonMap effectiveSubmitConfig(JsonMap config, List<JsonMap> providers) {
  final provider = stringValue(config['provider']);
  final entry = _entry(providers, provider);
  final models = maps(entry?['models']).where((model) => model['isSelectable'] != false).toList();
  var model = stringValue(config['model']).trim();
  if (model.isEmpty) {
    model = stringValue((models.where((m) => m['isDefault'] == true).firstOrNull ??
        models.firstOrNull)?['id']);
  }
  final loading = entry == null || entry['status'] == 'loading';
  final selectedMode = stringValue(config['modeId']).trim();
  final modeIds = maps(entry?['modes']).map((mode) => stringValue(mode['id'])).toList();
  final modeId = loading
      ? selectedMode
      : modeIds.contains(selectedMode)
      ? selectedMode
      : (modeIds.firstOrNull ?? '');
  var thinking = stringValue(config['thinkingOptionId']).trim();
  if (thinking.isEmpty) {
    thinking = stringValue(
      models.where((m) => m['id'] == model).firstOrNull?['defaultThinkingOptionId'],
    );
  }
  return {...config, 'provider': provider}
    ..remove('model')
    ..remove('modeId')
    ..remove('thinkingOptionId')
    ..addAll({
      if (model.isNotEmpty) 'model': model,
      if (modeId.isNotEmpty) 'modeId': modeId,
      if (thinking.isNotEmpty) 'thinkingOptionId': thinking,
    });
}

/// provider-selection.ts resolveSubmissionReadiness for a draft with content:
/// the localization key of the first reason it cannot be created yet, or
/// null. [catalogLoading] is the provider snapshot still loading. (A model is
/// always resolvable once the provider lists any, and a draft always has a
/// directory, so those reasons cannot arise here.)
String? draftSubmissionBlocker({
  required JsonMap config,
  required List<JsonMap> providers,
  required bool connected,
  required bool catalogLoading,
}) {
  if (!connected) return 'providerSelection.readiness.hostDisconnected';
  if (providers.isEmpty) return 'providerSelection.readiness.noProviders';
  final provider = stringValue(config['provider']);
  if (provider.isEmpty) return 'providerSelection.selectModel';
  if (catalogLoading || _entry(providers, provider)?['status'] == 'loading') {
    return 'providerSelection.readiness.modelDefaultsLoading';
  }
  return null;
}
