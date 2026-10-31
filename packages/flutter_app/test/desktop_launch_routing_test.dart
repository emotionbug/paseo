import 'dart:io';

import 'package:flutter_test/flutter_test.dart';

import 'package:paseo_sessions/platform/desktop/desktop_host.dart';

void main() {
  group('parseAgentDeepLink (protocol agent-deep-link.ts)', () {
    ({String serverId, String agentId})? parse(String input) =>
        parseAgentDeepLink(Uri.parse(input));

    test('accepts the agent route and decodes its segments', () {
      expect(parse('paseo://h/srv%201/agent/agent-1'), (
        serverId: 'srv 1',
        agentId: 'agent-1',
      ));
    });

    test('rejects other shapes, decorations and blank or malformed segments', () {
      for (final input in [
        'paseo://x/srv/agent/a',
        'paseo://h/srv/agents/a',
        'paseo://h/srv/agent/a/extra',
        'paseo://h/srv/agent/a?x=1',
        'paseo://h/srv/agent/a#frag',
        'paseo://user@h/srv/agent/a',
        'paseo://h:9/srv/agent/a',
        'paseo://h/%20/agent/a',
        'other://h/srv/agent/a',
      ]) {
        expect(parse(input), isNull, reason: input);
      }
      expect(parse('paseo://h/%E0%A4/agent/a'), isNull);
    });
  });

  group('openProjectPathFromArgv (desktop open-project-routing.ts)', () {
    late Directory first;
    late Directory second;
    setUp(() {
      first = Directory.systemTemp.createTempSync('paseo-open-a');
      second = Directory.systemTemp.createTempSync('paseo-open-b');
    });
    tearDown(() {
      first.deleteSync();
      second.deleteSync();
    });

    test('the first positional directory wins over the flag', () async {
      expect(
        await openProjectPathFromArgv(['--open-project', 'relative', first.path]),
        first.path,
      );
      expect(
        await openProjectPathFromArgv(['-psn_0_1', first.path, second.path]),
        first.path,
      );
    });

    test('the flag value is used only when it names an existing directory', () async {
      expect(await openProjectPathFromArgv(['--open-project', 'relative']), isNull);
      expect(
        await openProjectPathFromArgv(['--open-project', '${first.path}/missing']),
        isNull,
      );
      expect(await openProjectPathFromArgv(['--open-project']), isNull);
    });
  });
}
