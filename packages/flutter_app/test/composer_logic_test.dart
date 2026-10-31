import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'package:paseo_sessions/core/json.dart';
import 'package:paseo_sessions/features/chat/chat_drafts.dart';
import 'package:paseo_sessions/features/chat/composer/agent_form.dart';
import 'package:paseo_sessions/features/chat/composer/attachment_files.dart';
import 'package:paseo_sessions/features/chat/composer/forge_auto_attach.dart';

JsonMap _model(String id, {bool isDefault = false, List<String> thinking = const [], String? defaultThinking}) => {
  'id': id,
  'label': id,
  'isDefault': isDefault,
  if (thinking.isNotEmpty) 'thinkingOptions': [for (final t in thinking) {'id': t, 'label': t}],
  'defaultThinkingOptionId': ?defaultThinking,
};

final _providers = <JsonMap>[
  {
    'provider': 'claude',
    'status': 'ready',
    'enabled': true,
    'defaultModeId': 'default',
    'modes': [
      {'id': 'default'},
      {'id': 'plan'},
    ],
    'models': [
      _model('sonnet', isDefault: true, thinking: ['low', 'high'], defaultThinking: 'low'),
      _model('opus', thinking: ['low', 'high'], defaultThinking: 'high'),
    ],
  },
  {
    'provider': 'codex',
    'status': 'ready',
    'enabled': true,
    'modes': [
      {'id': 'auto'},
      {'id': 'read-only'},
    ],
    'models': [_model('gpt', isDefault: true)],
  },
];

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  group('agent form (resolve-agent-form.ts)', () {
    test('a new draft resolves the remembered provider, model, mode and per-model thinking', () {
      final preferences = parseFormPreferences({
        'provider': 'claude',
        'providerPreferences': {
          'claude': {
            'model': 'opus',
            'mode': 'plan',
            'thinkingByModel': {'opus': 'low'},
          },
        },
      });
      expect(
        resolveInitialAgentConfig(initial: {}, preferences: preferences, providers: _providers),
        {'provider': 'claude', 'model': 'opus', 'modeId': 'plan', 'thinkingOptionId': 'low'},
      );
    });

    test('without preferences nothing is selected; initial values win over preferences', () {
      expect(resolveInitialAgentConfig(initial: {}, preferences: {}, providers: _providers), isEmpty);
      final resolved = resolveInitialAgentConfig(
        initial: {'provider': 'codex'},
        preferences: {'provider': 'claude'},
        providers: _providers,
      );
      expect(resolved['provider'], 'codex');
      // No remembered mode: the provider's first mode (it declares no default).
      expect(resolved['modeId'], 'auto');
    });

    test('legacy preferences keep their thinking option for the remembered model', () {
      final preferences = parseFormPreferences({
        'provider': 'claude',
        'serverId': 'old',
        'providerPreferences': {
          'claude': {'model': 'opus', 'thinkingOptionId': 'low'},
        },
      });
      expect(jsonMap(jsonMap(preferences['providerPreferences'])['claude'])['thinkingByModel'], {
        'opus': 'low',
      });
    });

    test('switching provider takes its remembered mode; switching model resets thinking', () {
      final preferences = parseFormPreferences({
        'providerPreferences': {
          'codex': {'mode': 'read-only'},
        },
      });
      final switched = applyUserModelChoice(
        previous: {'provider': 'claude', 'model': 'opus', 'modeId': 'plan', 'thinkingOptionId': 'low'},
        provider: 'codex',
        modelId: '',
        preferences: preferences,
        providers: _providers,
      );
      expect(switched, {'provider': 'codex', 'model': 'gpt', 'modeId': 'read-only'});
      final remodeled = applyUserModelChoice(
        previous: {'provider': 'claude', 'model': 'sonnet', 'modeId': 'plan', 'thinkingOptionId': 'high'},
        provider: 'claude',
        modelId: 'opus',
        preferences: const {},
        providers: _providers,
      );
      expect(remodeled['modeId'], 'plan');
      expect(remodeled['thinkingOptionId'], 'high');
    });

    test('user choices are remembered per provider', () {
      var preferences = rememberUserSelection(
        const {},
        {'provider': 'claude', 'model': 'sonnet'},
        {'provider': 'codex', 'model': 'gpt', 'modeId': 'auto'},
      );
      expect(preferences['provider'], 'codex');
      expect(jsonMap(jsonMap(preferences['providerPreferences'])['codex']), {'model': 'gpt'});
      preferences = rememberUserSelection(
        preferences,
        {'provider': 'codex', 'model': 'gpt', 'modeId': 'auto'},
        {'provider': 'codex', 'model': 'gpt', 'modeId': 'read-only'},
      );
      expect(jsonMap(jsonMap(preferences['providerPreferences'])['codex'])['mode'], 'read-only');
    });

    test('a draft submits the default model, an offered mode and the default thinking', () {
      expect(
        effectiveSubmitConfig({'provider': 'claude', 'modeId': 'gone', 'cwd': '/repo'}, _providers),
        {'provider': 'claude', 'cwd': '/repo', 'model': 'sonnet', 'modeId': 'default', 'thinkingOptionId': 'low'},
      );
    });

    test('draft feature values: own over remembered, offered features only, remembered on change', () {
      final features = <JsonMap>[
        {'id': 'fast', 'value': false},
        {'id': 'web', 'value': true},
      ];
      expect(
        resolveFeatureValues(features, {'fast': true, 'gone': 1}, {'web': false, 'stale': 2}),
        {'fast': true, 'web': false},
      );
      final preferences = rememberUserSelection(
        const {},
        {'provider': 'claude', 'model': 'sonnet', 'featureValues': {'fast': false}},
        {'provider': 'claude', 'model': 'sonnet', 'featureValues': {'fast': true}},
      );
      expect(rememberedFeatureValues(preferences, 'claude'), {'fast': true});
    });

    test('readiness blocks a draft without a connection, providers or a provider', () {
      expect(
        draftSubmissionBlocker(config: const {}, providers: _providers, connected: false, catalogLoading: false),
        'providerSelection.readiness.hostDisconnected',
      );
      expect(
        draftSubmissionBlocker(config: const {}, providers: const [], connected: true, catalogLoading: true),
        'providerSelection.readiness.noProviders',
      );
      expect(
        draftSubmissionBlocker(config: const {}, providers: _providers, connected: true, catalogLoading: false),
        'providerSelection.selectModel',
      );
      expect(
        draftSubmissionBlocker(
          config: const {'provider': 'claude'},
          providers: _providers,
          connected: true,
          catalogLoading: false,
        ),
        isNull,
      );
    });
  });

  group('attachments', () {
    test('only raster images are images; other files upload as octet-stream', () {
      expect(mimeTypeForPath('/a/shot.PNG'), 'image/png');
      expect(mimeTypeForPath('/a/photo.heic'), 'image/heic');
      expect(mimeTypeForPath('/a/notes.md'), 'application/octet-stream');
      expect(rasterImageMimeType('report.pdf'), isNull);
    });

    test('one oversized file rejects the whole selection before any upload', () {
      expect(
        () => checkUploadSizes([
          (name: 'small.txt', size: 10),
          (name: 'huge.bin', size: fileUploadMaxBytes + 1),
        ]),
        throwsA(isA<FileTooLargeError>().having((e) => e.fileName, 'fileName', 'huge.bin')),
      );
      checkUploadSizes([(name: 'edge.bin', size: fileUploadMaxBytes)]);
    });
  });

  group('forge refs (forge-refs.ts)', () {
    test('finds this repository\'s pull requests and issues, deduplicated in order', () {
      const text = 'See https://github.com/Owner/Repo/pull/12, and https://github.com/owner/repo/issues/3/ '
          'again https://github.com/Owner/Repo/pull/12 plus https://github.com/other/repo/pull/9.';
      expect(extractForgeRefs(text, 'git@github.com:Owner/Repo.git'), [
        (kind: 'change_request', number: 12),
        (kind: 'issue', number: 3),
      ]);
    });

    test('gitlab merge requests; unknown hosts try every route; no remote finds nothing', () {
      expect(
        extractForgeRefs('https://gitlab.com/group/proj/-/merge_requests/5', 'https://gitlab.com/group/proj.git'),
        [(kind: 'change_request', number: 5)],
      );
      expect(
        extractForgeRefs('https://git.example.com/team/app/pull/7', 'ssh://git@git.example.com:2222/team/app.git'),
        [(kind: 'change_request', number: 7)],
      );
      expect(extractForgeRefs('https://github.com/o/r/pull/1', null), isEmpty);
    });
  });

  group('forge auto-attach (forge-auto-attach.ts)', () {
    ForgeAutoAttachInput input(String text, List<JsonMap> attachments) => (
      text: text,
      remoteUrl: 'https://github.com/o/r.git',
      attachments: attachments,
      connected: true,
      serverId: 'server',
      cwd: '/repo',
    );

    test('resolved links attach in source order and a removed one never re-attaches', () async {
      final attached = <JsonMap>[];
      final lookups = <String, Completer<List<JsonMap>>>{};
      final auto = ComposerForgeAutoAttach(
        debounce: Duration.zero,
        search: (cwd, query) => (lookups[query] = Completer<List<JsonMap>>()).future,
        attach: attached.add,
      );
      const text = 'https://github.com/o/r/pull/1 https://github.com/o/r/issues/2';
      auto.update(input(text, const []));
      expect(auto.isResolving, isTrue);
      await pumpEventQueue();
      // The later reference resolves first but attaches after the earlier one.
      lookups['2']!.complete([
        {'kind': 'issue', 'number': 2, 'title': 'Bug'},
      ]);
      await pumpEventQueue();
      expect(attached, isEmpty);
      lookups['1']!.complete([
        {'kind': 'change_request', 'number': 1, 'title': 'Fix'},
      ]);
      await pumpEventQueue();
      expect(attached.map((item) => item['number']), [1, 2]);
      expect(auto.isResolving, isFalse);

      // The user removes the PR: editing the text never brings it back.
      auto.markRemoved({'type': 'forge_change_request', 'number': 1});
      auto.update(input('$text more', [
        {'type': 'forge_issue', 'number': 2},
      ]));
      await pumpEventQueue();
      // No new lookup starts: #1 was removed and #2 is attached.
      expect(lookups.keys, ['1', '2']);
      expect(attached, hasLength(2));
      auto.dispose();
    });

    test('a link deleted before its lookup returns does not attach', () async {
      final attached = <JsonMap>[];
      final lookup = Completer<List<JsonMap>>();
      final auto = ComposerForgeAutoAttach(
        debounce: Duration.zero,
        search: (cwd, query) => lookup.future,
        attach: attached.add,
      );
      auto.update(input('https://github.com/o/r/pull/4', const []));
      await pumpEventQueue();
      auto.update(input('', const []));
      lookup.complete([
        {'kind': 'change_request', 'number': 4},
      ]);
      await pumpEventQueue();
      expect(attached, isEmpty);
      expect(auto.isResolving, isFalse);
      auto.dispose();
    });
  });

  group('drafts', () {
    setUpAll(() async {
      SharedPreferences.setMockInitialValues({});
      await ChatDrafts.instance.initialize();
    });

    test('a submit sends the trimmed text and acknowledging clears that composer text', () {
      final drafts = ChatDrafts.instance;
      final key = ChatDrafts.key('server', 'workspace', 'trim-agent');
      drafts.update(key, {'text': '  hello\n\n'});
      final message = drafts.snapshot(key);
      expect(message['text'], 'hello');
      drafts.acknowledge(key, message);
      expect(drafts.draft(key)['text'], '');
    });
  });
}
