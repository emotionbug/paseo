import 'package:flutter_test/flutter_test.dart';

import 'package:paseo_sessions/core/json.dart';
import 'package:paseo_sessions/features/settings/agent_profile_form.dart';
import 'package:paseo_sessions/features/settings/profile_settings.dart';

// Fixtures and cases follow the original
// agent-profiles/internal/profile-form-model.test.ts.
const JsonMap _claude = {
  'provider': 'claude',
  'status': 'ready',
  'enabled': true,
  'label': 'Claude Code',
  'defaultModeId': 'plan',
  'modes': [
    {'id': 'plan', 'label': 'Plan'},
    {'id': 'accept-edits', 'label': 'Accept edits'},
  ],
  'models': [
    {
      'provider': 'claude',
      'id': 'claude-opus-5',
      'label': 'Opus 5',
      'isDefault': true,
      'thinkingOptions': [
        {'id': 'think', 'label': 'Think'},
        {'id': 'think-hard', 'label': 'Think hard'},
      ],
    },
    {
      'provider': 'claude',
      'id': 'claude-haiku-4-5',
      'label': 'Haiku 4.5',
      'thinkingOptions': [
        {'id': 'think', 'label': 'Think'},
      ],
    },
  ],
};

const JsonMap _codex = {
  'provider': 'codex',
  'status': 'ready',
  'enabled': true,
  'label': 'Codex',
  'modes': [
    {'id': 'read-only', 'label': 'Read only'},
  ],
  'models': [
    {'provider': 'codex', 'id': 'gpt-5.2-codex', 'label': 'GPT-5.2 Codex', 'isDefault': true},
  ],
};

const JsonMap _disabled = {'provider': 'pi', 'status': 'ready', 'enabled': false, 'label': 'Pi'};

const _entries = [_claude, _codex, _disabled];

const JsonMap _webSearch = {'type': 'toggle', 'id': 'webSearch', 'label': 'Web search', 'value': false};

AgentProfileFormModel _create({JsonMap? seed}) =>
    AgentProfileFormModel(editing: false, seed: seed)..applyProviderCatalog(_entries);

void _selectClaude(AgentProfileFormModel model) =>
    model.setProvider('claude', (label: 'Claude Code', description: null));

List<String> _ids(List<AgentProfileFormOption> options) => [for (final o in options) o.id];

void main() {
  group('agent profile form (profile-form-model.ts)', () {
    test('create mode seeds from a model seed, then upgrades labels from the catalog', () {
      final model = AgentProfileFormModel(
        editing: false,
        seed: {'provider': 'claude', 'model': 'claude-opus-5', 'name': 'Opus 5'},
      );
      expect(model.providerDisplay?.label, 'claude');
      expect(model.modelDisplay?.label, 'claude-opus-5');
      expect(model.catalogResolution, AgentProfileResolution.idle);

      model.applyProviderCatalog(_entries);
      expect(model.providerDisplay?.label, 'Claude Code');
      expect(model.modelDisplay?.label, 'Opus 5');
      expect(model.modelId, 'claude-opus-5');
      expect(model.canSubmit, isTrue);
      expect(model.submitValue, containsPair('model', 'claude-opus-5'));
    });

    test('edit mode ignores the seed and keeps a value the catalog does not know', () {
      final model = AgentProfileFormModel(
        editing: true,
        profile: {'id': 'p1', 'name': 'Legacy', 'provider': 'claude', 'model': 'claude-retired-3'},
        seed: {'provider': 'codex', 'model': 'gpt-5.2-codex', 'name': 'Nope'},
      )..applyProviderCatalog(_entries);
      expect(model.name, 'Legacy');
      expect(model.modelId, 'claude-retired-3');
      expect(model.modelDisplay?.label, 'claude-retired-3');
      expect(model.showModelField, isTrue);
    });

    test('only enabled providers are offered; a name and a provider are required', () {
      final model = _create();
      expect(_ids(model.providerOptions), ['claude', 'codex']);
      model.setName('UI work');
      expect(model.canSubmit, isFalse);
      _selectClaude(model);
      expect(model.canSubmit, isTrue);
      model.setSubmitting(true);
      expect(model.canSubmit, isFalse);
      expect(model.submitValue, isNull);
    });

    test('every selection is seeded to a concrete id; blank text fields are omitted', () {
      final model = _create()
        ..setName('  Cheap grunt  ')
        ..setProvider('codex', (label: 'Codex', description: null))
        ..setNotes('   ');
      expect(model.submitValue, {
        'name': 'Cheap grunt',
        'provider': 'codex',
        'model': 'gpt-5.2-codex',
        'modeId': 'read-only',
      });
      final claude = _create();
      _selectClaude(claude);
      expect(claude.modeId, 'plan');
      expect(claude.modelId, 'claude-opus-5');
      expect(claude.thinkingOptionId, 'think');
      expect(
        _ids([...claude.modelOptions, ...claude.modeOptions, ...claude.thinkingOptions]),
        isNot(contains('')),
      );
    });

    test('changing provider reseeds every provider-scoped selection', () {
      final model = _create()..setName('Test writer');
      _selectClaude(model);
      model
        ..setThinking('think-hard', (label: 'Think hard', description: null))
        ..applyFeatures(model.featureRequestKey!, [_webSearch])
        ..setFeatureValue('webSearch', true)
        ..setProvider('codex', (label: 'Codex', description: null));
      expect(model.modelId, 'gpt-5.2-codex');
      expect(model.modelDisplay?.label, 'GPT-5.2 Codex');
      expect(model.thinkingOptionId, '');
      expect(model.featureValues, isEmpty);
      expect(model.features, isEmpty);
      expect(model.showThinkingField, isFalse);
    });

    test('changing model reseeds a thinking level the new model does not offer', () {
      final model = _create();
      _selectClaude(model);
      model
        ..setThinking('think-hard', (label: 'Think hard', description: null))
        ..setModel('claude-haiku-4-5', (label: 'Haiku 4.5', description: null));
      expect(model.thinkingOptionId, 'think');
      expect(model.thinkingDisplay?.label, 'Think');
    });

    test('features resolve only for the current request; stored values are pruned', () {
      final model = _create();
      _selectClaude(model);
      expect(model.featureRequest, {
        'provider': 'claude',
        'model': 'claude-opus-5',
        'modeId': 'plan',
        'thinkingOptionId': 'think',
      });
      expect(model.featureResolution, AgentProfileResolution.pending);
      model.applyFeatures('stale-key', [_webSearch]);
      expect(model.features, isEmpty);
      model.applyFeatures(model.featureRequestKey!, [_webSearch]);
      expect(model.featureResolution, AgentProfileResolution.complete);
      expect(model.showFeaturesField, isTrue);

      final editing = AgentProfileFormModel(
        editing: true,
        profile: {
          'id': 'p1',
          'name': 'UI work',
          'provider': 'claude',
          'featureValues': {'webSearch': true, 'retiredFlag': true},
        },
      )..applyProviderCatalog(_entries);
      expect(editing.submitValue?['featureValues'], {'webSearch': true, 'retiredFlag': true});
      editing.applyFeatures(editing.featureRequestKey!, [_webSearch]);
      expect(editing.featureValues, {'webSearch': true});
      expect(editing.features.single['value'], isTrue);
    });

    test('an unavailable feature listing resolves without showing the field', () {
      final model = _create();
      _selectClaude(model);
      model.applyFeaturesUnavailable(model.featureRequestKey!);
      expect(model.featureResolution, AgentProfileResolution.complete);
      expect(model.showFeaturesField, isFalse);
    });

    test('a closed form stops publishing', () {
      final model = _create();
      var notifications = 0;
      model
        ..addListener(() => notifications++)
        ..setName('a');
      expect(notifications, 1);
      model
        ..close()
        ..setName('b');
      expect(notifications, 1);
      expect(model.name, 'a');
    });
  });

  group('agent profile save (agent-profiles-section.tsx handleSave)', () {
    final stored = <JsonMap>[
      {'id': 'p1', 'name': 'Old', 'provider': 'claude', 'model': 'm', 'notes': 'n', 'cliDefault': true},
      {'id': 'p2', 'name': 'Other', 'provider': 'codex'},
    ];

    test('an edit replaces the record (cleared fields go) but keeps cliDefault', () {
      final next = ProfileSettings.applyProfileSave(
        stored,
        {'id': 'p1', 'name': 'New', 'provider': 'claude'},
        editing: true,
      );
      expect(next, [
        {'id': 'p1', 'cliDefault': true, 'name': 'New', 'provider': 'claude'},
        stored[1],
      ]);
    });

    test('a new profile is appended', () {
      final next = ProfileSettings.applyProfileSave(
        stored,
        {'id': 'p3', 'name': 'Third', 'provider': 'codex'},
        editing: false,
      );
      expect(next.map((p) => p['id']), ['p1', 'p2', 'p3']);
    });
  });

  group('metadata generation (metadata-generation-page.tsx)', () {
    test('a picked model leads, later fallbacks stay, an empty model is omitted', () {
      final configured = <JsonMap>[
        {'provider': 'claude', 'model': 'old', 'thinkingOptionId': 'x'},
        {'provider': 'codex'},
      ];
      expect(metadataProvidersAfterSelect(configured, 'codex', 'gpt-5.2-codex'), [
        {'provider': 'codex', 'model': 'gpt-5.2-codex'},
        {'provider': 'codex'},
      ]);
      expect(metadataProvidersAfterSelect(const [], 'claude', ''), [
        {'provider': 'claude'},
      ]);
    });

    test('a provider still loading on a disconnected host reads as failed', () {
      expect(metadataSelectionKind({'status': 'loading'}, connected: true), 'loading');
      expect(metadataSelectionKind({'status': 'loading'}, connected: false), 'error');
      expect(metadataSelectionKind({'status': 'ready'}, connected: false), 'models');
    });
  });
}
