import 'package:flutter/foundation.dart' show ValueListenable, defaultTargetPlatform;
import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/json.dart';
import '../../ui/common.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import 'chat_controller.dart' show maps;
import 'composer/composer_tooltip.dart';
import 'provider_icon.dart';
import '../settings/profile_settings.dart' show AgentProfileGlyph;
import '../settings/provider_catalog.dart' show ProviderSettingsSnapshot;

/// Original composer/agent-controls: the toolbar model/thinking/mode controls
/// and the "Select model" sheet with its model browser and settings rows.
///
/// [config] carries `provider`, `model`, `modeId`, `thinkingOptionId` and
/// `featureValues`; every change is reported as a complete next config.
class AgentControls extends StatelessWidget {
  const AgentControls({
    super.key,
    required this.providers,
    required this.config,
    required this.modes,
    required this.features,
    required this.profiles,
    required this.canSwitchProvider,
    required this.onChanged,
    this.onApplyProfile,
    this.loadFeatures,
    this.live,
    this.onOpen,
    this.onEditProfiles,
    this.onCreateProfile,
    this.onEditProfile,
    this.onProviderSettings,
    this.disabled = false,
  });

  final List<JsonMap> providers, modes, features, profiles;
  final JsonMap config;
  final bool canSwitchProvider, disabled;
  final ValueChanged<JsonMap> onChanged;
  final ValueChanged<JsonMap>? onApplyProfile;

  /// Drafts resolve provider features for the selected configuration.
  final Future<List<JsonMap>> Function(JsonMap config)? loadFeatures;

  /// The live provider snapshot: an open sheet follows its updates (the
  /// catalog loads asynchronously after the sheet may already be open).
  final ValueListenable<ProviderSettingsSnapshot>? live;

  /// model-sheet.tsx onOpen (handleModelSelectorOpen → refetchIfStale).
  final VoidCallback? onOpen;

  /// model-browser.tsx CreateAgentProfileRow / AgentProfilesEditAction:
  /// opens the host's agent profile settings; null when unsupported.
  final VoidCallback? onEditProfiles;

  /// model-browser.tsx onCreateProfile: a model row's "+" opens the profile
  /// editor seeded with `{provider, model, name}`; null when unsupported.
  final ValueChanged<JsonMap>? onCreateProfile;

  /// model-browser.tsx onEditProfile: a model row naming exactly one
  /// profile edits it; null when unsupported.
  final ValueChanged<JsonMap>? onEditProfile;

  /// model-browser.tsx ProviderSettingsAction (the header gear).
  final void Function(BuildContext context, String provider)? onProviderSettings;

  AgentControlsModel get _model => AgentControlsModel(
    providers: providers,
    config: config,
    modes: modes,
    features: features,
  );

  Future<void> _openSheet(BuildContext context) {
    onOpen?.call();
    return showAdaptiveSheet<void>(
      context: context,
      builder: (_) => _AgentControlsSheet(
        providers: providers,
        config: config,
        modes: modes,
        features: features,
        profiles: profiles,
        canSwitchProvider: canSwitchProvider,
        onChanged: onChanged,
        onApplyProfile: onApplyProfile,
        loadFeatures: loadFeatures,
        live: live,
        onEditProfiles: onEditProfiles,
        onCreateProfile: onCreateProfile,
        onEditProfile: onEditProfile,
        onProviderSettings: onProviderSettings,
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final model = _model;
    final compact = isCompactLayout(context);
    final colors = Theme.of(context).colorScheme;
    final palette = PaseoColors.of(context);
    // layout.ts resolveComposerToolbarGlyphSize: 20 native, 16 web.
    final native = defaultTargetPlatform == TargetPlatform.android ||
        defaultTargetPlatform == TargetPlatform.iOS;
    final glyph = native ? 20.0 : 16.0;
    final base = Theme.of(context).textTheme.bodyMedium!;
    final provider = stringValue(config['provider']);
    final thinking = model.thinkingOptions;
    Widget modelTrigger({required bool caret}) => _ToolbarTrigger(
      disabled: disabled,
      tooltip: tr(context, 'agentControls.hints.model'),
      semanticsLabel: tr(context, 'modelSelector.selectedModel', args: {
        'model': model.selectedModelLabel(context),
      }),
      caret: caret,
      onPressed: () => _openSheet(context),
      children: [
        if (provider.trim().isNotEmpty)
          SizedBox.square(
            dimension: glyph,
            child: ProviderIcon(
              provider: provider,
              snapshotSvg: stringValue(model.provider['iconSvg']),
              size: glyph,
              color: colors.onSurfaceVariant,
            ),
          ),
        Flexible(
          child: Text(
            shortModelLabel(model.selectedModelLabel(context)),
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: base.copyWith(color: colors.onSurfaceVariant),
          ),
        ),
        // model-sheet.tsx triggerThinking: the compact trigger appends the
        // thinking level in the extra-muted tone.
        if (compact && thinking.isNotEmpty)
          Text(
            model.thinkingLabel(context),
            maxLines: 1,
            style: base.copyWith(color: palette.extraMuted),
          ),
      ],
    );
    if (compact) return modelTrigger(caret: false);
    return _DensityLayout(
      hasThinking: thinking.isNotEmpty,
      hasMode: modes.isNotEmpty,
      features: features,
      builder: (context, presentation) {
        final thinkingValue = model.thinkingLabel(context);
        final modeValue = formatAgentModeLabel(model.selectedMode ?? const {});
        return Row(
          mainAxisSize: MainAxisSize.min,
          spacing: 4,
          children: [
            Flexible(child: modelTrigger(caret: presentation.showCarets)),
            if (thinking.isNotEmpty)
              _MenuTrigger(
                disabled: disabled,
                tooltip: tr(context, 'agentControls.hints.thinking'),
                semanticsLabel: tr(context, 'agentControls.thinking.selectWithValue',
                    args: {'value': thinkingValue}),
                icon: Icon(LucideIcons.brain300, size: glyph, color: colors.onSurfaceVariant),
                label: presentation.showThinkingLabel ? thinkingValue : null,
                caret: presentation.showCarets,
                menuWidth: 200,
                selected: stringValue(config['thinkingOptionId'],
                    stringValue(thinking.firstOrNull?['id'])),
                options: [
                  for (final option in thinking)
                    (
                      id: stringValue(option['id']),
                      label: formatThinkingOptionLabel(context, option),
                      icon: LucideIcons.brain300,
                    ),
                ],
                onSelected: (id) => onChanged(model.withThinking(id)),
              ),
            if (modes.isNotEmpty)
              _MenuTrigger(
                disabled: disabled,
                tooltip: tr(context, 'agentControls.hints.mode'),
                // mode-control.tsx: the cycle shortcut follows the hint.
                tooltipHelpId: 'cycle-agent-mode',
                semanticsLabel: tr(context, 'agentControls.mode.selectWithValue',
                    args: {'value': modeValue}),
                icon: Icon(modeIcon(model.selectedMode), size: glyph, color: colors.onSurfaceVariant),
                label: presentation.showModeLabel ? modeValue : null,
                caret: presentation.showCarets,
                menuWidth: 260,
                selected: stringValue(model.selectedMode?['id']),
                options: [
                  for (final mode in modes)
                    (id: stringValue(mode['id']), label: formatAgentModeLabel(mode), icon: modeOptionIcon(mode)),
                ],
                onSelected: (id) => onChanged(model.withMode(id)),
              ),
            if (features.isNotEmpty && presentation.aggregateFeatures)
              _ToolbarTrigger(
                disabled: disabled,
                iconOnly: true,
                tooltip: tr(context, 'agentControls.features.open'),
                semanticsLabel: tr(context, 'agentControls.features.open'),
                onPressed: () => _openSheet(context),
                children: [
                  Icon(LucideIcons.settings2300, size: glyph, color: colors.onSurfaceVariant),
                ],
              )
            else
              for (final feature in features) _featureControl(context, model, feature, glyph),
          ],
        );
      },
    );
  }

  /// index.tsx DesktopFeatureItem: a toggle is a 28px icon-only control that
  /// flips on press; a select shows its icon and selected option (icon only
  /// for desktopTrigger "icon") and opens its options above. Active features
  /// tint their glyph (getFeatureIconColor).
  Widget _featureControl(BuildContext context, AgentControlsModel model, JsonMap feature, double glyph) {
    final label = stringValue(feature['label'], stringValue(feature['id']));
    final value = model.featureValue(feature);
    final color = featureIconColor(context, feature, value);
    final tooltip = stringValue(feature['tooltip'], label);
    if (feature['type'] == 'toggle') {
      return _ToolbarTrigger(
        disabled: disabled,
        iconOnly: true,
        tooltip: tooltip,
        semanticsLabel: tooltip,
        onPressed: () => onChanged(model.withFeature(stringValue(feature['id']), value != true)),
        children: [Icon(featureIcon(feature), size: glyph, color: color)],
      );
    }
    if (feature['type'] != 'select') return const SizedBox.shrink();
    final options = maps(feature['options']);
    final selected = options.where((option) => option['id'] == value).firstOrNull;
    final selectedLabel = stringValue(selected?['label'], label);
    final iconOnly = feature['desktopTrigger'] == 'icon';
    final hint = iconOnly ? '$label: $selectedLabel' : tooltip;
    return _MenuTrigger(
      disabled: disabled,
      tooltip: hint,
      semanticsLabel: hint,
      icon: Icon(featureIcon(feature), size: glyph, color: color),
      label: iconOnly ? null : selectedLabel,
      caret: false,
      menuWidth: 220,
      selected: '$value',
      options: [
        for (final option in options)
          (id: stringValue(option['id']), label: stringValue(option['label'], stringValue(option['id'])), icon: null),
      ],
      onSelected: (id) => onChanged(model.withFeature(stringValue(feature['id']), id)),
    );
  }
}

/// Pure projections over the provider snapshot shared by toolbar and sheet.
class AgentControlsModel {
  const AgentControlsModel({
    required this.providers,
    required this.config,
    required this.modes,
    required this.features,
  });

  final List<JsonMap> providers, modes, features;
  final JsonMap config;

  JsonMap get provider =>
      providers.where((entry) => entry['provider'] == config['provider']).firstOrNull ??
      const {};

  static List<JsonMap> modelsOf(JsonMap provider) => maps(provider['models'])
      .where((model) => model['isSelectable'] != false)
      .toList();

  /// agent-controls/utils.ts pickSelectedModel: a configured model resolves
  /// by id or alias only (an unlisted id selects nothing, so no thinking
  /// options or label show); without one, the default or first model.
  JsonMap? get selectedModel {
    final models = modelsOf(provider);
    final id = stringValue(config['model']).trim();
    if (id.isEmpty) {
      return models.where((model) => model['isDefault'] == true).firstOrNull ??
          models.firstOrNull;
    }
    return models.where((model) => model['id'] == id).firstOrNull ??
        models.where((model) => jsonList(model['aliases']).contains(id)).firstOrNull;
  }

  /// provider-selection.ts resolveSelectedModelLabel.
  String selectedModelLabel(BuildContext context) {
    final selectModel = tr(context, 'modelSelector.selectModel');
    if (stringValue(config['provider']).trim().isEmpty || provider.isEmpty) {
      return selectModel;
    }
    final models = modelsOf(provider);
    if (models.isEmpty && provider['status'] == 'loading') {
      return tr(context, 'modelSelector.loading');
    }
    if (models.isEmpty && provider['error'] != null) {
      return tr(context, 'modelSelector.error');
    }
    final selectedId = stringValue(config['model']).trim();
    final exact = models.where((model) => model['id'] == config['model']).firstOrNull;
    if (exact == null && selectedId.isNotEmpty) return selectedId;
    final model = exact ??
        models.where((model) => model['isDefault'] == true).firstOrNull ??
        models.firstOrNull;
    return model == null ? selectModel : modelLabel(model);
  }

  static String modelLabel(JsonMap model) =>
      stringValue(model['label'], stringValue(model['id']));

  List<JsonMap> get thinkingOptions => maps(selectedModel?['thinkingOptions']);

  String thinkingLabel(BuildContext context) {
    final options = thinkingOptions;
    final selected = options
            .where((option) => option['id'] == config['thinkingOptionId'])
            .firstOrNull ??
        options.firstOrNull;
    return selected == null
        ? tr(context, 'agentControls.thinking.unknown')
        : formatThinkingOptionLabel(context, selected);
  }

  JsonMap? get selectedMode =>
      modes.where((mode) => mode['id'] == config['modeId']).firstOrNull ?? modes.firstOrNull;

  Object? featureValue(JsonMap feature) =>
      jsonMap(config['featureValues'])[feature['id']] ?? feature['value'];

  JsonMap withModel(String providerId, String modelId) {
    final next = <String, dynamic>{...config, 'provider': providerId, 'model': modelId};
    if (providerId != config['provider']) {
      next
        ..remove('modeId')
        ..remove('thinkingOptionId')
        ..remove('featureValues');
    } else if (modelId != config['model']) {
      next.remove('thinkingOptionId');
    }
    return next;
  }

  JsonMap withThinking(String id) => {...config, 'thinkingOptionId': id};
  JsonMap withMode(String id) => {...config, 'modeId': id};
  JsonMap withFeature(String id, Object? value) => {
    ...config,
    'featureValues': {...jsonMap(config['featureValues']), id: value},
  };
}

/// model-sheet.tsx shortModelLabel.
String shortModelLabel(String label) {
  final separator = label.lastIndexOf('/');
  return separator == -1 ? label : label.substring(separator + 1);
}

String _sentenceCase(String value) =>
    value.isEmpty ? value : value[0].toUpperCase() + value.substring(1).toLowerCase();

String _splitCompactLabel(String value, bool splitHyphen) => value
    .replaceAll(RegExp(splitHyphen ? r'[_-]+' : r'_+'), ' ')
    .replaceAllMapped(RegExp(r'([a-z])([A-Z])'), (m) => '${m[1]} ${m[2]}')
    .replaceAllMapped(RegExp(r'([A-Z]+)([A-Z][a-z])'), (m) => '${m[1]} ${m[2]}')
    .replaceAll(RegExp(r'\s+'), ' ')
    .trim();

/// agent-controls/labels.ts formatAgentModeLabel.
String formatAgentModeLabel(JsonMap mode) {
  final raw = stringValue(mode['label'], stringValue(mode['id'])).trim();
  return _sentenceCase(_splitCompactLabel(raw, mode['label'] == null));
}

/// agent-controls/labels.ts formatThinkingOptionLabel.
String formatThinkingOptionLabel(BuildContext context, JsonMap option) {
  final raw = stringValue(option['label'], stringValue(option['id'])).trim();
  final compact = raw.replaceAll(RegExp(r'[\s_-]+'), '').toLowerCase();
  if (compact == 'xhigh' || compact == 'extrahigh') {
    return tr(context, 'agentControls.thinking.extraHigh');
  }
  return _sentenceCase(_splitCompactLabel(raw, true));
}

/// agent-controls/icons.ts MODE_ICONS (lucide Shield family and Bot).
IconData? modeOptionIcon(JsonMap? mode) => switch (mode?['icon']) {
  'Bot' => LucideIcons.bot300,
  'Shield' => LucideIcons.shield300,
  'ShieldAlert' => LucideIcons.shieldAlert300,
  'ShieldCheck' => LucideIcons.shieldCheck300,
  'ShieldEllipsis' => LucideIcons.shieldEllipsis300,
  'ShieldOff' => LucideIcons.shieldOff300,
  'ShieldPlus' => LucideIcons.shieldPlus300,
  'ShieldQuestionMark' => LucideIcons.shieldQuestionMark300,
  _ => null,
};

IconData modeIcon(JsonMap? mode) => modeOptionIcon(mode) ?? LucideIcons.bot300;

/// agent-controls/icons.ts FEATURE_ICONS.
IconData featureIcon(JsonMap feature) => switch (feature['icon']) {
  'list-todo' => LucideIcons.listTodo300,
  'shield-check' => LucideIcons.shieldCheck300,
  'zap' => LucideIcons.zap300,
  _ => LucideIcons.settings2300,
};

/// agent-controls/utils.ts isFeatureActive + index.tsx getFeatureIconColor:
/// an active fast/speed feature is yellow-400, auto-accept green-400, plan
/// mode blue-400; anything else (or inactive) stays muted.
Color featureIconColor(BuildContext context, JsonMap feature, Object? value) {
  final muted = Theme.of(context).colorScheme.onSurfaceVariant;
  final active = feature['type'] == 'toggle'
      ? value == true
      : maps(feature['options'])
            .where((option) => option['id'] == value)
            .any((option) => option['isDefault'] != true);
  if (!active) return muted;
  return switch (feature['id']) {
    'fast_mode' || 'service_tier' => const Color(0xfffacc15),
    'auto_accept' => const Color(0xff4ade80),
    'plan_mode' => const Color(0xff60a5fa),
    _ => muted,
  };
}

enum ComposerControlDensity { full, condensed, tight }

typedef ComposerControlPresentation = ({
  bool showCarets,
  bool showThinkingLabel,
  bool showModeLabel,
  bool aggregateFeatures,
});

/// layout.ts resolveComposerControlDensity: estimated floors (7px per label
/// character at the font scale) with 12px hysteresis around the current
/// density, so the toolbar does not flicker at a boundary.
ComposerControlDensity resolveComposerControlDensity({
  required double availableWidth,
  required ComposerControlDensity currentDensity,
  required bool hasModel,
  required bool hasThinking,
  required bool hasMode,
  required List<JsonMap> features,
  required double fontScale,
}) {
  const hysteresis = 12.0;
  final scale = fontScale.isFinite ? fontScale.clamp(1.0, double.infinity).toDouble() : 1.0;
  double sum(List<double> widths) =>
      widths.isEmpty ? 0 : widths.reduce((a, b) => a + b) + (widths.length - 1) * 4;
  final full = sum([
    if (hasModel) 50 + 70 * scale,
    if (hasThinking) 54 + 48 * scale,
    if (hasMode) 54 + 96 * scale,
    for (final feature in features)
      feature['type'] == 'toggle'
          ? 28
          : 28 + 4 + 16 + stringValue(feature['label']).runes.length * 7 * scale,
  ]);
  final condensed = sum([
    if (hasModel) 36 + 60 * scale,
    if (hasThinking) 28,
    if (hasMode) 36 + 96 * scale,
    if (features.isNotEmpty) 28,
  ]);
  final width = availableWidth;
  switch (currentDensity) {
    case ComposerControlDensity.full:
      if (width >= full - hysteresis) return ComposerControlDensity.full;
      return width >= condensed ? ComposerControlDensity.condensed : ComposerControlDensity.tight;
    case ComposerControlDensity.condensed:
      if (width >= full + hysteresis) return ComposerControlDensity.full;
      if (width < condensed - hysteresis) return ComposerControlDensity.tight;
      return ComposerControlDensity.condensed;
    case ComposerControlDensity.tight:
      if (width >= full + hysteresis) return ComposerControlDensity.full;
      if (width >= condensed + hysteresis) return ComposerControlDensity.condensed;
      return ComposerControlDensity.tight;
  }
}

/// index.tsx ControlledAgentControls: measures its width and keeps the
/// resolved density between layouts (the hysteresis memory), starting full.
class _DensityLayout extends StatefulWidget {
  const _DensityLayout({
    required this.hasThinking,
    required this.hasMode,
    required this.features,
    required this.builder,
  });
  final bool hasThinking, hasMode;
  final List<JsonMap> features;
  final Widget Function(BuildContext context, ComposerControlPresentation presentation) builder;

  @override
  State<_DensityLayout> createState() => _DensityLayoutState();
}

class _DensityLayoutState extends State<_DensityLayout> {
  ComposerControlDensity density = ComposerControlDensity.full;

  @override
  Widget build(BuildContext context) => LayoutBuilder(
    builder: (context, constraints) {
      density = resolveComposerControlDensity(
        availableWidth: constraints.maxWidth,
        currentDensity: density,
        hasModel: true,
        hasThinking: widget.hasThinking,
        hasMode: widget.hasMode,
        features: widget.features,
        fontScale: MediaQuery.textScalerOf(context).scale(14) / 14,
      );
      return widget.builder(context, resolveComposerControlPresentation(density));
    },
  );
}

/// layout.ts resolveComposerControlPresentation.
ComposerControlPresentation resolveComposerControlPresentation(ComposerControlDensity density) =>
    switch (density) {
      ComposerControlDensity.full =>
        (showCarets: true, showThinkingLabel: true, showModeLabel: true, aggregateFeatures: false),
      ComposerControlDensity.condensed =>
        (showCarets: false, showThinkingLabel: false, showModeLabel: true, aggregateFeatures: true),
      ComposerControlDensity.tight =>
        (showCarets: false, showThinkingLabel: false, showModeLabel: false, aggregateFeatures: true),
    };

/// control.tsx toolbarControl: 28px high, 8px inset, 4px gaps, 16px radius,
/// transparent until hovered (surface2) or pressed/open (surface0), 0.5
/// opacity disabled; icon-only controls are 28px squares. With carets on,
/// combobox-trigger.tsx appends a 14px muted ChevronDown.
class _ToolbarTrigger extends StatelessWidget {
  const _ToolbarTrigger({
    required this.children,
    required this.onPressed,
    required this.tooltip,
    required this.semanticsLabel,
    this.disabled = false,
    this.iconOnly = false,
    this.caret = false,
    this.tooltipHelpId,
  });

  final List<Widget> children;
  final VoidCallback onPressed;
  final String tooltip, semanticsLabel;
  final String? tooltipHelpId;
  final bool disabled, iconOnly, caret;

  @override
  Widget build(BuildContext context) {
    final palette = PaseoColors.of(context);
    final trigger = Semantics(
      button: true,
      label: semanticsLabel,
      excludeSemantics: true,
      child: Opacity(
        opacity: disabled ? .5 : 1,
        child: Material(
          type: MaterialType.transparency,
          child: InkWell(
            onTap: disabled ? null : onPressed,
            borderRadius: BorderRadius.circular(16),
            hoverColor: palette.surface2,
            highlightColor: palette.surface0,
            child: Container(
              height: 28,
              width: iconOnly ? 28 : null,
              padding: iconOnly ? EdgeInsets.zero : const EdgeInsets.symmetric(horizontal: 8),
              // No alignment: an aligned Container fills a bounded width,
              // which centred the chip in the composer row.
              child: Row(
                mainAxisSize: MainAxisSize.min,
                mainAxisAlignment: MainAxisAlignment.center,
                spacing: 4,
                children: [
                  ...children,
                  if (caret && !iconOnly)
                    Icon(LucideIcons.chevronDown300, size: 14,
                        color: Theme.of(context).colorScheme.onSurfaceVariant),
                ],
              ),
            ),
          ),
        ),
      ),
    );
    return ComposerTooltip(label: tooltip, helpId: tooltipHelpId, child: trigger);
  }
}

typedef _Option = ({String id, String label, IconData? icon});

/// A toolbar control opening its options in the original desktop Combobox
/// popover above it (top-start, [menuWidth] minimum), each option with its
/// 16px leading glyph and a trailing check on the selected one.
class _MenuTrigger extends StatelessWidget {
  const _MenuTrigger({
    required this.icon,
    required this.label,
    required this.tooltip,
    required this.semanticsLabel,
    required this.options,
    required this.selected,
    required this.onSelected,
    this.caret = false,
    this.menuWidth = 220,
    this.disabled = false,
    this.tooltipHelpId,
  });

  final Widget icon;
  final String? label;
  final String tooltip, semanticsLabel, selected;
  final String? tooltipHelpId;
  final List<_Option> options;
  final ValueChanged<String> onSelected;
  final bool disabled, caret;
  final double menuWidth;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final base = Theme.of(context).textTheme.bodyMedium!;
    return Builder(
      builder: (anchor) => _ToolbarTrigger(
        disabled: disabled,
        iconOnly: label == null,
        caret: caret,
        tooltip: tooltip,
        tooltipHelpId: tooltipHelpId,
        semanticsLabel: semanticsLabel,
        onPressed: () async {
          final id = await showPaseoMenu<String>(
            context,
            anchor: paseoAnchorRect(anchor),
            width: menuWidth,
            entries: [
              for (final option in options)
                PaseoMenuEntry.item(
                  value: option.id,
                  label: option.label,
                  icon: option.icon,
                  selected: option.id == selected,
                ),
            ],
          );
          if (id != null) onSelected(id);
        },
        children: [
          icon,
          if (label != null)
            Flexible(
              child: Text(
                label!,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: base.copyWith(color: colors.onSurfaceVariant),
              ),
            ),
        ],
      ),
    );
  }
}

enum _Page { root, providers, models, thinking, mode, feature }

class _AgentControlsSheet extends StatefulWidget {
  const _AgentControlsSheet({
    required this.providers,
    required this.config,
    required this.modes,
    required this.features,
    required this.profiles,
    required this.canSwitchProvider,
    required this.onChanged,
    this.onApplyProfile,
    this.loadFeatures,
    this.live,
    this.onEditProfiles,
    this.onCreateProfile,
    this.onEditProfile,
    this.onProviderSettings,
  });

  final List<JsonMap> providers, modes, features, profiles;
  final JsonMap config;
  final bool canSwitchProvider;
  final ValueChanged<JsonMap> onChanged;
  final ValueChanged<JsonMap>? onApplyProfile;
  final Future<List<JsonMap>> Function(JsonMap config)? loadFeatures;
  final ValueListenable<ProviderSettingsSnapshot>? live;
  final VoidCallback? onEditProfiles;
  final ValueChanged<JsonMap>? onCreateProfile, onEditProfile;
  final void Function(BuildContext context, String provider)? onProviderSettings;

  @override
  State<_AgentControlsSheet> createState() => _AgentControlsSheetState();
}

class _AgentControlsSheetState extends State<_AgentControlsSheet> {
  late JsonMap config = {...widget.config};
  late List<JsonMap> features = widget.features;
  final search = TextEditingController();
  final List<_Page> pages = [_Page.root];
  String? drillProvider;
  JsonMap? openFeature;
  Object? featureError;
  int featureGeneration = 0;

  /// The latest provider snapshot (live while the sheet is open).
  List<JsonMap> get providers => widget.live?.value.providers ?? widget.providers;

  AgentControlsModel get model => AgentControlsModel(
    providers: providers,
    config: config,
    modes: widget.canSwitchProvider ? _draftModes : widget.modes,
    features: features,
  );

  // A draft's modes follow the selected provider; a live agent reports its own.
  List<JsonMap> get _draftModes {
    final entry = providers
        .where((provider) => provider['provider'] == config['provider'])
        .firstOrNull;
    return entry == null ? widget.modes : maps(entry['modes']);
  }

  List<JsonMap> get _availableProviders => widget.canSwitchProvider
      ? providers
      : [
          providers.where((p) => p['provider'] == config['provider']).firstOrNull ??
              providers.firstOrNull,
        ].whereType<JsonMap>().toList();

  void _providersChanged() {
    if (mounted) setState(() {});
  }

  @override
  void initState() {
    super.initState();
    search.addListener(() => setState(() {}));
    widget.live?.addListener(_providersChanged);
    if (widget.loadFeatures != null) _reloadFeatures();
  }

  @override
  void dispose() {
    widget.live?.removeListener(_providersChanged);
    search.dispose();
    super.dispose();
  }

  Future<void> _reloadFeatures() async {
    final load = widget.loadFeatures;
    if (load == null) return;
    final own = ++featureGeneration;
    try {
      final next = await load(config);
      if (mounted && own == featureGeneration) {
        setState(() {
          features = next;
          featureError = null;
        });
      }
    } catch (error) {
      if (mounted && own == featureGeneration) setState(() => featureError = error);
    }
  }

  void _apply(JsonMap next) {
    final reload = next['provider'] != config['provider'] ||
        next['model'] != config['model'] ||
        next['modeId'] != config['modeId'] ||
        next['thinkingOptionId'] != config['thinkingOptionId'];
    setState(() => config = next);
    widget.onChanged(next);
    if (reload) _reloadFeatures();
  }

  void _push(_Page page) => setState(() {
    pages.add(page);
    search.clear();
  });

  void _pop() => setState(() {
    pages.removeLast();
    search.clear();
    if (pages.last == _Page.providers || pages.last == _Page.root) drillProvider = null;
  });

  /// model-sheet.tsx openModelBrowser: on compact layouts the model browser
  /// is a second sheet pushed over this one (presentation "push"); a single
  /// provider opens drilled into it, without a back arrow, its glyph leading
  /// the title and its settings gear in the header.
  void _openModels() {
    final providers = _availableProviders;
    final single = !widget.canSwitchProvider || providers.length == 1;
    if (single && isCompactLayout(context)) {
      final providerId = stringValue(providers.firstOrNull?['provider']);
      showModalBottomSheet<void>(
        context: context,
        isScrollControlled: true,
        useSafeArea: true,
        showDragHandle: true,
        backgroundColor: PaseoColors.of(context).surface0,
        shape: const RoundedRectangleBorder(
          borderRadius: BorderRadius.vertical(top: Radius.circular(16)),
        ),
        builder: (_) => _ModelListSheet(owner: this, providerId: providerId),
      );
    } else if (single) {
      drillProvider = stringValue(providers.firstOrNull?['provider']);
      _push(_Page.models);
    } else {
      _push(_Page.providers);
    }
  }

  void _selectModel(String provider, String modelId) {
    _apply(model.withModel(provider, modelId));
    final compact = isCompactLayout(context);
    if (compact && pages.length > 1) {
      setState(() {
        pages
          ..clear()
          ..add(_Page.root);
        drillProvider = null;
        search.clear();
      });
    } else if (!compact) {
      Navigator.pop(context);
    } else {
      search.clear();
    }
  }

  @override
  Widget build(BuildContext context) {
    final page = pages.last;
    final compact = isCompactLayout(context);
    final query = search.text.trim().toLowerCase();
    final drilled = providers
        .where((provider) => provider['provider'] == drillProvider)
        .firstOrNull;
    final title = switch (page) {
      _Page.root => tr(context, 'modelSelector.selectModel'),
      _Page.providers => tr(context, 'modelSelector.title'),
      _Page.models => stringValue(drilled?['label'], stringValue(drillProvider)),
      _Page.thinking => tr(context, 'agentControls.thinking.title'),
      _Page.mode => tr(context, 'agentControls.mode.title'),
      _Page.feature => stringValue(openFeature?['label']),
    };
    final searchable = page == _Page.root || page == _Page.providers || page == _Page.models;
    final placeholder = page == _Page.models
        ? tr(context, 'modelSelector.searchPlaceholder')
        : widget.canSwitchProvider
        ? tr(context, 'modelSelector.searchAllPlaceholder')
        : tr(context, 'modelSelector.searchPlaceholder');
    Widget body;
    if (searchable && query.isNotEmpty) {
      body = _searchResults(context, query, page == _Page.models ? drilled : null);
    } else {
      body = switch (page) {
        _Page.root => compact ? _mobileRoot(context) : _desktopRoot(context),
        _Page.providers => _providerRows(context),
        _Page.models => _modelRows(context, drilled ?? const {}),
        _Page.thinking => _optionRows(
          context,
          [
            for (final option in model.thinkingOptions)
              (id: stringValue(option['id']), label: formatThinkingOptionLabel(context, option), icon: null),
          ],
          stringValue(config['thinkingOptionId'],
              stringValue(model.thinkingOptions.firstOrNull?['id'])),
          (id) {
            _apply(model.withThinking(id));
            _pop();
          },
        ),
        _Page.mode => _optionRows(
          context,
          [
            for (final mode in model.modes)
              (id: stringValue(mode['id']), label: formatAgentModeLabel(mode), icon: modeOptionIcon(mode)),
          ],
          stringValue(model.selectedMode?['id']),
          (id) {
            _apply(model.withMode(id));
            _pop();
          },
        ),
        // index.tsx SheetFeatureItem: a pushed option list; a toggle offers
        // On / Off.
        _Page.feature => _optionRows(
          context,
          openFeature?['type'] == 'toggle'
              ? [
                  (id: 'true', label: tr(context, 'agentControls.features.on'), icon: null),
                  (id: 'false', label: tr(context, 'agentControls.features.off'), icon: null),
                ]
              : [
                  for (final option in maps(openFeature?['options']))
                    (id: stringValue(option['id']), label: stringValue(option['label'], stringValue(option['id'])), icon: null),
                ],
          '${model.featureValue(openFeature ?? const {})}',
          (id) {
            _apply(model.withFeature(
              stringValue(openFeature?['id']),
              openFeature?['type'] == 'toggle' ? id == 'true' : id,
            ));
            _pop();
          },
        ),
      };
    }
    final sheet = SheetContents(
      title: title,
      onBack: pages.length > 1 ? _pop : null,
      leading: page == _Page.models ? _providerGlyph(context, drillProvider ?? '') : null,
      headerActions: [
        if (page == _Page.models && widget.onProviderSettings != null)
          _providerSettingsAction(context, drillProvider ?? ''),
      ],
      scroll: false,
      header: searchable
          ? _SheetSearchRow(controller: search, placeholder: placeholder, autofocus: !compact)
          : null,
      child: body,
    );
    // model-sheet.tsx SNAP_POINTS ["80%", "90%"]: the compact sheet opens at 80%.
    return compact
        ? SizedBox(height: MediaQuery.sizeOf(context).height * .8, child: sheet)
        : sheet;
  }

  ListView _list(List<Widget> children) => ListView(
    shrinkWrap: true,
    padding: const EdgeInsets.only(bottom: 32),
    children: children,
  );

  /// model-sheet.tsx mobileRootContent: profiles, then the settings list.
  Widget _mobileRoot(BuildContext context) => _list([
    ..._profileRows(context),
    DecoratedBox(
      decoration: BoxDecoration(
        color: PaseoColors.of(context).sidebar,
        border: Border(top: BorderSide(color: Theme.of(context).colorScheme.outline)),
      ),
      child: Padding(
        padding: const EdgeInsets.fromLTRB(8, 12, 8, 12),
        child: Column(spacing: 4, children: _settingsRows(context, includeModel: true)),
      ),
    ),
  ]);

  /// Desktop sheet: the model browser, a rule, then the remaining controls.
  Widget _desktopRoot(BuildContext context) => _list([
    ..._profileRows(context),
    if (widget.profiles.isNotEmpty) _heading(context, tr(context, 'modelSelector.providers')),
    if (_availableProviders.length == 1)
      ..._modelRowWidgets(context, _availableProviders.first)
    else
      ..._providerRowWidgets(context),
    if (_settingsRows(context, includeModel: false).isNotEmpty) ...[
      Divider(height: 1, color: Theme.of(context).colorScheme.outline),
      Padding(
        padding: const EdgeInsets.fromLTRB(8, 12, 8, 12),
        child: Column(spacing: 4, children: _settingsRows(context, includeModel: false)),
      ),
    ],
  ]);

  /// model-browser.tsx AgentProfilesPickerContent: applicable profiles under
  /// a "Profiles" heading, or — when there are none and the host supports
  /// profiles — the muted "+ Create profile" row opening profile settings.
  List<Widget> _profileRows(BuildContext context) {
    final apply = widget.onApplyProfile;
    final profiles = widget.profiles
        .where((profile) =>
            widget.canSwitchProvider || profile['provider'] == config['provider'])
        .toList();
    final edit = widget.onEditProfiles;
    if (profiles.isEmpty && edit != null) {
      return [
        _BrowserRow(
          leading: Icon(
            LucideIcons.plus300,
            size: 16,
            color: Theme.of(context).colorScheme.onSurfaceVariant,
          ),
          label: tr(context, 'modelSelector.createProfile'),
          labelMuted: true,
          onTap: () {
            Navigator.pop(context);
            edit();
          },
        ),
      ];
    }
    if (apply == null || profiles.isEmpty) return const [];
    return [
      _heading(
        context,
        tr(context, 'modelSelector.profiles'),
        // AgentProfilesEditAction: the xs extra-muted pencil.
        action: edit == null
            ? null
            : _RowIconButton(
                label: tr(context, 'modelSelector.editProfilesLabel'),
                onPressed: () {
                  _closeSheets(context);
                  edit();
                },
                child: Icon(
                  LucideIcons.pencil300,
                  size: densityIcon(context, 'xs'),
                  color: PaseoColors.of(context).extraMuted,
                ),
              ),
      ),
      for (final profile in profiles)
        _BrowserRow(
          leading: ProviderIcon(
            provider: stringValue(profile['provider']),
            size: 16,
            color: Theme.of(context).colorScheme.onSurfaceVariant,
          ),
          label: stringValue(profile['name'], stringValue(profile['id'])),
          description: stringValue(profile['model']),
          onTap: () {
            apply(profile);
            Navigator.pop(context);
          },
        ),
    ];
  }

  /// model-browser.tsx sectionHeading: sm muted text 8 over 4, with an
  /// optional [action] pinned 4px from the top at the row inset.
  Widget _heading(BuildContext context, String text, {Widget? action}) {
    final heading = Padding(
      padding: EdgeInsets.fromLTRB(_rowInset(context), 8, _rowInset(context), 4),
      child: Text(
        text,
        style: Theme.of(context).textTheme.bodySmall!.copyWith(
          color: Theme.of(context).colorScheme.onSurfaceVariant,
        ),
      ),
    );
    if (action == null) return heading;
    return Stack(
      clipBehavior: Clip.none,
      children: [
        heading,
        Positioned(top: 4, right: _rowInset(context), child: action),
      ],
    );
  }

  /// model-browser.tsx SelectableModelRow profileAction: "+" creates a
  /// profile from a model no profile uses, the profile's glyph edits the one
  /// profile using it, and several open the profile settings.
  Widget? _modelProfileAction(BuildContext context, String providerId, JsonMap row) {
    final modelId = stringValue(row['id']);
    if (modelId.isEmpty) return null;
    final profiled = widget.profiles
        .where((profile) => profile['provider'] == providerId && profile['model'] == modelId)
        .toList();
    final size = densityIcon(context, 'xs');
    final muted = Theme.of(context).colorScheme.onSurfaceVariant;
    if (profiled.isEmpty) {
      final create = widget.onCreateProfile;
      if (create == null) return null;
      return _RowIconButton(
        label: tr(context, 'modelSelector.createProfileFromModel'),
        onPressed: () {
          _closeSheets(context);
          create({'provider': providerId, 'model': modelId, 'name': AgentControlsModel.modelLabel(row)});
        },
        child: Icon(LucideIcons.plus300, size: size, color: muted),
      );
    }
    final primary = profiled.last;
    final glyph = AgentProfileGlyph(
      icon: primary['icon'] as String?,
      color: primary['color'] as String?,
      size: size,
    );
    if (profiled.length == 1) {
      final edit = widget.onEditProfile;
      if (edit == null) return null;
      return _RowIconButton(
        label: tr(context, 'modelSelector.editProfileLabel', args: {'name': stringValue(primary['name'])}),
        onPressed: () {
          _closeSheets(context);
          edit(primary);
        },
        child: glyph,
      );
    }
    final editAll = widget.onEditProfiles;
    if (editAll == null) return null;
    return _RowIconButton(
      label: tr(context, 'modelSelector.editProfilesCount', args: {'count': profiled.length}),
      onPressed: () {
        _closeSheets(context);
        editAll();
      },
      child: glyph,
    );
  }

  /// model-sheet.tsx resolveModelSheetProfileActions: a profile action first
  /// closes the model sheet — the pushed model list (when [from] is in it)
  /// and the controls sheet under it.
  void _closeSheets(BuildContext from) {
    if (ModalRoute.of(from) != ModalRoute.of(context)) Navigator.pop(from);
    if (mounted) Navigator.pop(context);
  }

  List<Widget> _settingsRows(BuildContext context, {required bool includeModel}) {
    final colors = Theme.of(context).colorScheme;
    final provider = stringValue(config['provider']);
    return [
      if (includeModel)
        _SheetRow(
          icon: ProviderIcon(
            provider: provider,
            snapshotSvg: stringValue(model.provider['iconSvg']),
            size: 16,
            color: colors.onSurfaceVariant,
          ),
          label: tr(context, 'modelSelector.model'),
          value: model.selectedModelLabel(context),
          onTap: _openModels,
        ),
      if (model.thinkingOptions.isNotEmpty)
        _SheetRow(
          icon: Icon(LucideIcons.brain300, size: 16, color: colors.onSurfaceVariant),
          label: tr(context, 'agentControls.thinking.title'),
          value: model.thinkingLabel(context),
          onTap: () => _push(_Page.thinking),
        ),
      if (model.modes.isNotEmpty)
        _SheetRow(
          icon: Icon(modeIcon(model.selectedMode), size: 16, color: colors.onSurfaceVariant),
          label: tr(context, 'agentControls.mode.title'),
          value: formatAgentModeLabel(model.selectedMode ?? const {}),
          onTap: () => _push(_Page.mode),
        ),
      for (final feature in features)
        if (feature['type'] == 'toggle' || feature['type'] == 'select')
          _SheetRow(
            icon: Icon(featureIcon(feature), size: 16,
                color: featureIconColor(context, feature, model.featureValue(feature))),
            label: stringValue(feature['label'], stringValue(feature['id'])),
            value: feature['type'] == 'toggle'
                ? (model.featureValue(feature) == true
                    ? tr(context, 'agentControls.features.on')
                    : tr(context, 'agentControls.features.off'))
                : stringValue(
                    maps(feature['options'])
                        .where((option) => option['id'] == model.featureValue(feature))
                        .firstOrNull?['label'],
                    stringValue(feature['label']),
                  ),
            onTap: () {
              openFeature = feature;
              _push(_Page.feature);
            },
          ),
      if (featureError != null)
        Padding(
          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
          child: Text(
            '$featureError',
            style: Theme.of(context).textTheme.bodySmall!.copyWith(color: colors.error),
          ),
        ),
    ];
  }

  Widget _providerRows(BuildContext context) => _list(_providerRowWidgets(context));

  /// model-browser.tsx GroupedProviderRows / GroupProviderButton.
  List<Widget> _providerRowWidgets(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final small = Theme.of(context).textTheme.bodySmall!.copyWith(color: colors.onSurfaceVariant);
    final rows = <Widget>[];
    for (final provider in _availableProviders) {
      if (rows.isNotEmpty) rows.add(Divider(height: 1, color: colors.outline));
      final models = AgentControlsModel.modelsOf(provider);
      final loading = models.isEmpty && provider['status'] == 'loading';
      final failed = models.isEmpty && provider['error'] != null;
      rows.add(_BrowserRow(
        leading: ProviderIcon(
          provider: stringValue(provider['provider']),
          snapshotSvg: stringValue(provider['iconSvg']),
          size: 16,
          color: colors.onSurfaceVariant,
        ),
        label: stringValue(provider['label'], stringValue(provider['provider'])),
        trailing: Row(
          mainAxisSize: MainAxisSize.min,
          spacing: 4,
          children: [
            if (loading)
              SizedBox(
                width: 12,
                height: 12,
                child: CircularProgressIndicator(strokeWidth: 1.5, color: colors.onSurfaceVariant),
              ),
            if (failed) Icon(LucideIcons.alertTriangle300, size: 14, color: colors.onSurfaceVariant),
            Text(
              loading
                  ? tr(context, 'modelSelector.loadingShort')
                  : failed
                  ? tr(context, 'modelSelector.error')
                  : tr(context, models.length == 1 ? 'modelSelector.modelCount' : 'modelSelector.modelCountPlural',
                      args: {'count': models.length}),
              style: small,
            ),
            Icon(LucideIcons.chevronRight300, size: 14, color: colors.onSurfaceVariant),
          ],
        ),
        onTap: () {
          drillProvider = stringValue(provider['provider']);
          _push(_Page.models);
        },
      ));
    }
    return rows;
  }

  Widget _modelRows(
    BuildContext context,
    JsonMap provider, {
    void Function(String provider, String modelId)? onSelect,
  }) {
    final rows = _modelRowWidgets(context, provider, onSelect: onSelect);
    if (rows.isEmpty) {
      final error = provider['error'];
      if (error == null && provider['status'] == 'loading') {
        return const Padding(
          padding: EdgeInsets.symmetric(vertical: 24),
          child: Center(
            child: SizedBox.square(dimension: 16, child: CircularProgressIndicator(strokeWidth: 2)),
          ),
        );
      }
      return _emptyState(
        context,
        error == null ? LucideIcons.search300 : LucideIcons.alertTriangle300,
        error == null ? tr(context, 'modelSelector.noMatches') : '$error',
      );
    }
    return _list(rows);
  }

  /// useModelBrowser drilled header: the provider glyph (md, foreground).
  Widget _providerGlyph(BuildContext context, String providerId) => ProviderIcon(
    provider: providerId,
    snapshotSvg: stringValue(
      providers.where((p) => p['provider'] == providerId).firstOrNull?['iconSvg'],
    ),
    size: densityIcon(context, 'md'),
    color: Theme.of(context).colorScheme.onSurface,
  );

  /// model-browser.tsx ProviderSettingsAction: an sm Settings glyph.
  Widget _providerSettingsAction(BuildContext context, String providerId) {
    final open = widget.onProviderSettings!;
    return IconButton(
      tooltip: tr(context, 'modelSelector.openProviderSettings', args: {
        'provider': stringValue(
          providers.where((p) => p['provider'] == providerId).firstOrNull?['label'],
          providerId,
        ),
      }),
      style: IconButton.styleFrom(
        padding: const EdgeInsets.all(8),
        minimumSize: Size.zero,
        tapTargetSize: MaterialTapTargetSize.shrinkWrap,
        foregroundColor: PaseoColors.of(context).muted,
      ),
      onPressed: () => open(context, providerId),
      icon: Icon(LucideIcons.settings300, size: densityIcon(context, 'sm')),
    );
  }

  List<Widget> _modelRowWidgets(
    BuildContext context,
    JsonMap provider, {
    bool qualified = false,
    void Function(String provider, String modelId)? onSelect,
  }) {
    final colors = Theme.of(context).colorScheme;
    final providerId = stringValue(provider['provider']);
    final providerLabel = stringValue(provider['label'], providerId);
    return [
      for (final row in AgentControlsModel.modelsOf(provider))
        _BrowserRow(
          leading: ProviderIcon(
            provider: providerId,
            snapshotSvg: stringValue(provider['iconSvg']),
            size: 16,
            color: colors.onSurfaceVariant,
          ),
          label: AgentControlsModel.modelLabel(row),
          description: qualified
              ? [providerLabel, if (stringValue(row['description']).isNotEmpty) stringValue(row['description'])]
                  .join(' · ')
              : stringValue(row['description']),
          selected: providerId == config['provider'] &&
              row['id'] == (model.selectedModel?['id']),
          action: _modelProfileAction(context, providerId, row),
          modelRow: true,
          onTap: () => (onSelect ?? _selectModel)(providerId, stringValue(row['id'])),
        ),
    ];
  }

  Widget _searchResults(BuildContext context, String query, JsonMap? only) {
    final rows = <Widget>[];
    for (final provider in only == null ? _availableProviders : [only]) {
      final providerLabel = stringValue(provider['label'], stringValue(provider['provider']));
      final matches = AgentControlsModel.modelsOf(provider).where((row) => [
        AgentControlsModel.modelLabel(row),
        stringValue(row['id']),
        providerLabel,
        stringValue(row['description']),
      ].any((field) => field.toLowerCase().contains(query))).toList();
      rows.addAll(_modelRowWidgets(
        context,
        {...provider, 'models': matches},
        qualified: only == null && _availableProviders.length > 1,
      ));
    }
    if (rows.isEmpty) {
      return _emptyState(
        context,
        LucideIcons.search300,
        tr(context, 'modelSelector.noMatchesForQuery', args: {'query': search.text.trim()}),
      );
    }
    return _list(rows);
  }

  Widget _optionRows(
    BuildContext context,
    List<_Option> options,
    String selected,
    ValueChanged<String> onSelect,
  ) => _list([
    for (final option in options)
      _BrowserRow(
        leading: option.icon == null
            ? null
            : Icon(option.icon, size: 16, color: Theme.of(context).colorScheme.onSurfaceVariant),
        label: option.label,
        selected: option.id == selected,
        onTap: () => onSelect(option.id),
      ),
  ]);

  Widget _emptyState(BuildContext context, IconData icon, String text) => Padding(
    padding: const EdgeInsets.symmetric(vertical: 16, horizontal: 24),
    child: Column(
      mainAxisSize: MainAxisSize.min,
      spacing: 8,
      children: [
        Icon(icon, size: 16, color: Theme.of(context).colorScheme.onSurfaceVariant),
        Text(
          text,
          textAlign: TextAlign.center,
          style: Theme.of(context).textTheme.bodyMedium!.copyWith(
            color: Theme.of(context).colorScheme.onSurfaceVariant,
          ),
        ),
      ],
    ),
  );
}

/// model-sheet.tsx: the compact model browser sheet pushed over the
/// controls sheet (80% snap), drilled into one provider: its glyph and label
/// title, the provider settings gear, the search row and the model rows.
/// Selecting a model applies it and returns to the controls sheet.
class _ModelListSheet extends StatefulWidget {
  const _ModelListSheet({required this.owner, required this.providerId});
  final _AgentControlsSheetState owner;
  final String providerId;

  @override
  State<_ModelListSheet> createState() => _ModelListSheetState();
}

class _ModelListSheetState extends State<_ModelListSheet> {
  final search = TextEditingController();

  void _changed() {
    if (mounted) setState(() {});
  }

  @override
  void initState() {
    super.initState();
    search.addListener(_changed);
    widget.owner.widget.live?.addListener(_changed);
  }

  @override
  void dispose() {
    widget.owner.widget.live?.removeListener(_changed);
    search.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final owner = widget.owner;
    final provider = owner.providers
            .where((entry) => entry['provider'] == widget.providerId)
            .firstOrNull ??
        {'provider': widget.providerId};
    final query = search.text.trim().toLowerCase();
    void select(String providerId, String modelId) {
      if (owner.mounted) owner._apply(owner.model.withModel(providerId, modelId));
      Navigator.pop(context);
    }

    final Widget body;
    if (query.isEmpty) {
      body = owner._modelRows(context, provider, onSelect: select);
    } else {
      final label = stringValue(provider['label'], widget.providerId);
      final matches = AgentControlsModel.modelsOf(provider).where((row) => [
        AgentControlsModel.modelLabel(row),
        stringValue(row['id']),
        label,
        stringValue(row['description']),
      ].any((field) => field.toLowerCase().contains(query))).toList();
      body = matches.isEmpty
          ? owner._emptyState(
              context,
              LucideIcons.search300,
              tr(context, 'modelSelector.noMatchesForQuery', args: {'query': search.text.trim()}),
            )
          : owner._list(owner._modelRowWidgets(context, {...provider, 'models': matches}, onSelect: select));
    }
    return SizedBox(
      height: MediaQuery.sizeOf(context).height * .8,
      child: SheetContents(
        title: stringValue(provider['label'], widget.providerId),
        leading: owner._providerGlyph(context, widget.providerId),
        headerActions: [
          if (owner.widget.onProviderSettings != null)
            owner._providerSettingsAction(context, widget.providerId),
        ],
        scroll: false,
        header: _SheetSearchRow(
          controller: search,
          placeholder: tr(context, 'modelSelector.searchPlaceholder'),
        ),
        child: body,
      ),
    );
  }
}

/// adaptive-modal-sheet.tsx searchRow: a muted md Search glyph and a
/// borderless base input (8px vertical padding), 24px inset, 12px above the
/// header rule.
class _SheetSearchRow extends StatelessWidget {
  const _SheetSearchRow({
    required this.controller,
    required this.placeholder,
    this.autofocus = false,
  });
  final TextEditingController controller;
  final String placeholder;
  final bool autofocus;

  @override
  Widget build(BuildContext context) {
    final palette = PaseoColors.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(24, 0, 24, 12),
      child: Row(
        spacing: 8,
        children: [
          Icon(LucideIcons.search300, size: densityIcon(context, 'md'), color: palette.muted),
          Expanded(
            child: TextField(
              controller: controller,
              autofocus: autofocus,
              autocorrect: false,
              style: Theme.of(context).textTheme.bodyMedium,
              decoration: InputDecoration(
                isDense: true,
                filled: false,
                hoverColor: Colors.transparent,
                hintText: placeholder,
                hintStyle: TextStyle(color: palette.muted),
                border: InputBorder.none,
                enabledBorder: InputBorder.none,
                focusedBorder: InputBorder.none,
                contentPadding: const EdgeInsets.symmetric(vertical: 8),
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// model-browser.tsx row inset: 24px on native, 12px on web.
double _rowInset(BuildContext context) =>
    defaultTargetPlatform == TargetPlatform.android || defaultTargetPlatform == TargetPlatform.iOS
        ? 24
        : 12;

bool get _nativePlatform =>
    defaultTargetPlatform == TargetPlatform.android || defaultTargetPlatform == TargetPlatform.iOS;

/// model-browser.tsx ModelBrowserRow: 36px min, 8px vertical, 24/12px inset,
/// 16px leading slot, base label, small muted inline description, check.
/// A model row ([modelRow]) keeps 4px below it on native and reserves a 24px
/// slot after the check for its profile [action], shown on native/compact
/// layouts and while the row is hovered elsewhere (ModelRowProfileAction).
class _BrowserRow extends StatefulWidget {
  const _BrowserRow({
    required this.label,
    required this.onTap,
    this.leading,
    this.description,
    this.trailing,
    this.action,
    this.selected = false,
    this.labelMuted = false,
    this.modelRow = false,
  });

  final String label;
  final String? description;
  final Widget? leading, trailing, action;
  final bool selected, modelRow;

  /// labelMuted: rows offering an action rather than naming a choice.
  final bool labelMuted;
  final VoidCallback onTap;

  @override
  State<_BrowserRow> createState() => _BrowserRowState();
}

class _BrowserRowState extends State<_BrowserRow> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final theme = Theme.of(context).textTheme;
    final inset = _rowInset(context);
    final action = widget.action;
    final description = widget.description;
    final actionVisible = _hovered || _nativePlatform || isCompactLayout(context);
    Widget row = InkWell(
      onTap: widget.onTap,
      hoverColor: PaseoColors.of(context).surface1,
      highlightColor: PaseoColors.of(context).surface1,
      child: ConstrainedBox(
        constraints: const BoxConstraints(minHeight: 36),
        child: Padding(
          padding: EdgeInsets.symmetric(horizontal: inset, vertical: 8),
          child: Row(
            spacing: 8,
            children: [
              SizedBox(width: 16, child: Center(child: widget.leading)),
              Expanded(
                child: Row(
                  spacing: 8,
                  crossAxisAlignment: CrossAxisAlignment.baseline,
                  textBaseline: TextBaseline.alphabetic,
                  children: [
                    Flexible(
                      flex: 0,
                      child: Text(
                        widget.label,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: widget.labelMuted
                            ? theme.bodyMedium!.copyWith(color: colors.onSurfaceVariant)
                            : theme.bodyMedium,
                      ),
                    ),
                    if (description != null && description.isNotEmpty)
                      Flexible(
                        child: Text(
                          description,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: theme.bodySmall!.copyWith(color: colors.onSurfaceVariant),
                        ),
                      ),
                  ],
                ),
              ),
              ?widget.trailing,
              if (widget.trailing == null)
                Row(
                  mainAxisSize: MainAxisSize.min,
                  spacing: 4,
                  children: [
                    SizedBox(
                      width: 16,
                      child: widget.selected
                          ? Icon(LucideIcons.check300, size: 14, color: colors.onSurfaceVariant)
                          : null,
                    ),
                    // The profile action sits beside the row's own press
                    // target, over this reserved slot.
                    if (action != null)
                      IgnorePointer(
                        ignoring: !actionVisible,
                        child: Opacity(opacity: actionVisible ? 1 : 0, child: action),
                      ),
                  ],
                ),
            ],
          ),
        ),
      ),
    );
    if (action != null) {
      row = MouseRegion(
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: row,
      );
    }
    if (widget.modelRow && _nativePlatform) {
      row = Padding(padding: const EdgeInsets.only(bottom: 4), child: row);
    }
    return row;
  }
}

/// model-browser.tsx rowIconButton: a 24px round hit target (8px slop)
/// filled surface2 while hovered and surface1 while pressed, with a
/// desktop-only tooltip.
class _RowIconButton extends StatelessWidget {
  const _RowIconButton({required this.label, required this.onPressed, required this.child});
  final String label;
  final VoidCallback onPressed;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    final palette = PaseoColors.of(context);
    Widget button = Semantics(
      button: true,
      label: label,
      excludeSemantics: true,
      child: SizedBox.square(
        dimension: 24,
        child: Material(
          type: MaterialType.transparency,
          shape: const CircleBorder(),
          clipBehavior: Clip.antiAlias,
          child: InkWell(
            onTap: onPressed,
            hoverColor: palette.surface2,
            highlightColor: palette.surface1,
            child: Center(child: child),
          ),
        ),
      ),
    );
    if (!isCompactLayout(context) && !_nativePlatform) {
      button = Tooltip(message: label, waitDuration: const Duration(milliseconds: 250), child: button);
    }
    return button;
  }
}

/// control.tsx sheetRow: 44px min, 16px inset, 16px radius, surface1 fill
/// (surface2 pressed), glyph, label, muted value.
class _SheetRow extends StatelessWidget {
  const _SheetRow({
    required this.icon,
    required this.label,
    required this.value,
    required this.onTap,
  });

  final Widget icon;
  final String label, value;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final palette = PaseoColors.of(context);
    final base = Theme.of(context).textTheme.bodyMedium!;
    return Material(
      color: palette.surface1,
      borderRadius: BorderRadius.circular(16),
      clipBehavior: Clip.antiAlias,
      child: InkWell(
        onTap: onTap,
        hoverColor: palette.surface2,
        highlightColor: palette.surface2,
        child: ConstrainedBox(
          constraints: const BoxConstraints(minHeight: 44),
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16),
            child: Row(
              spacing: 8,
              children: [
                SizedBox(width: 20, height: 20, child: Center(child: icon)),
                Expanded(child: Text(label, maxLines: 1, overflow: TextOverflow.ellipsis, style: base)),
                ConstrainedBox(
                  constraints: BoxConstraints(
                    maxWidth: MediaQuery.sizeOf(context).width * .45,
                  ),
                  child: Text(
                    value,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: base.copyWith(color: palette.muted),
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
