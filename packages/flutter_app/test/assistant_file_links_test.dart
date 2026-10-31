import 'package:flutter_test/flutter_test.dart';
import 'package:paseo_sessions/features/chat/assistant_file_links.dart';
import 'package:paseo_sessions/features/chat/markdown_image.dart';

/// Original assistant-file-links/parse.ts + markdown/source.ts FILE_PATTERN
/// and utils/assistant-image-source.ts, over the F02/F15 fixture cases.
void main() {
  const root = '/tmp/paseo-parity-fixtures/project';

  List<String> linkified(String text) =>
      assistantLinkifyPattern.allMatches(text).map((m) => m[0]!).toList();

  test('prose paths and URLs linkify like the original', () {
    expect(linkified('Relative file: src/foo.ts:12'), ['src/foo.ts:12']);
    expect(linkified('Relative file with column: src/foo.ts:12:5'), ['src/foo.ts:12:5']);
    expect(linkified('Line-range style: src/foo.ts#L11-L16'), ['src/foo.ts']);
    expect(linkified('Missing file: src/does-not-exist.ts:3'), ['src/does-not-exist.ts:3']);
    expect(
      linkified('Absolute path: /tmp/paseo-parity-fixtures/project/docs/guide.md'),
      ['/tmp/paseo-parity-fixtures/project/docs/guide.md'],
    );
    expect(linkified('see https://github.com/getpaseo/paseo now'), ['https://github.com/getpaseo/paseo']);
    expect(linkified('Email: mailto:ada@fixtures.invalid'), isEmpty);
    expect(linkified('plain words only'), isEmpty);
  });

  test('inline code links when it names a file or is a whole http(s) URL', () {
    expect(isInlineCodeFileLink('src/utils/math.ts', workspaceRoot: root), isTrue);
    expect(isInlineCodeFileLink('orderTotal', workspaceRoot: root), isFalse);
    // source.ts linkRecognition.inlineCode: `^(?:https?://[^\s<>]+|…)$`.
    expect(isInlineCodeFileLink('https://example.com', workspaceRoot: root), isTrue);
    expect(isInlineCodeFileLink('see https://example.com', workspaceRoot: root), isFalse);
    expect(isInlineCodeFileLink('mailto:ada@fixtures.invalid', workspaceRoot: root), isFalse);
  });

  group('parse.test.ts parity', () {
    const project = '/Users/test/project';
    (String, int?, int?)? parsed(String value, {String? workspaceRoot, bool decodeHref = false}) {
      final target = parseAssistantFileLink(value, workspaceRoot: workspaceRoot, decodeHref: decodeHref);
      return target == null ? null : (target.path, target.lineStart, target.lineEnd);
    }

    test('line markers: :line, :line:col, (line,col), lines a-b, #La-Lb', () {
      expect(parseInlinePathToken('src/app.ts'), isNull);
      expect(parseInlinePathToken(':12'), isNull);
      final colon = parseInlinePathToken('src/app.ts:12:4')!;
      expect((colon.path, colon.lineStart, colon.lineEnd), ('src/app.ts', 12, null));
      final paren = parseInlinePathToken('src/app.ts(12,4)')!;
      expect((paren.path, paren.lineStart), ('src/app.ts', 12));
      final words = parseInlinePathToken('src/app.ts lines 12-20')!;
      expect((words.lineStart, words.lineEnd), (12, 20));
      expect(parsed('file:///Users/test/project/src/app.tsx#L81C5-L83C2'), ('/Users/test/project/src/app.tsx', 81, 83));
      expect(parsed('file:///C:/Users/test/app.tsx#L12-L20'), ('C:/Users/test/app.tsx', 12, 20));
      expect(parsed('$project/src/app.tsx#L20-L12', workspaceRoot: project), isNull);
    });

    test('bare names are ambiguous workspace candidates; paths are direct', () {
      expect(classifyAssistantFileLink('dumm.md', workspaceRoot: project), isA<AmbiguousFileLink>());
      final direct = classifyAssistantFileLink('src/components/message.tsx#L33', workspaceRoot: project);
      expect((direct as DirectFileLink).target.path, '$project/src/components/message.tsx');
      expect(direct.target.lineStart, 33);
      expect(parsed('file.ts:12', workspaceRoot: project), ('$project/file.ts', 12, null));
    });

    test('domains, identifiers, commands and URL schemes are not files', () {
      for (final value in ['google.com', 'openai.com/path', 'main', 'origin/main', '1f7fc232b']) {
        expect(classifyAssistantFileLink(value, workspaceRoot: project), isNull, reason: value);
      }
      expect(parsed('google.com:80', workspaceRoot: project), isNull);
      expect(classifyAssistantFileLink('npm run lint -- src/a.ts', workspaceRoot: project), isNull);
      expect(classifyAssistantFileLink('http://dumm.md', workspaceRoot: project), isA<ExternalFileLink>());
      expect(classifyAssistantFileLink('mailto:test@example.com'), isA<ExternalFileLink>());
      expect(parsed('https://example.com/Users/test/project/src/app.tsx'), isNull);
    });

    test('absolute, outside-workspace, home-relative and escaping paths', () {
      expect(parsed('/tmp/outside.txt', workspaceRoot: project), ('/tmp/outside.txt', null, null));
      expect(parsed('file:///tmp/outside.txt', workspaceRoot: project), ('/tmp/outside.txt', null, null));
      expect(parsed('~/.paseo/plans/file-preview.md:12', workspaceRoot: project), ('~/.paseo/plans/file-preview.md', 12, null));
      expect(parsed(r'~\.paseo\plans\file-preview.md', workspaceRoot: project), ('~/.paseo/plans/file-preview.md', null, null));
      expect(parsed('../outside.ts', workspaceRoot: project), isNull);
      expect(parsed('src/app.ts'), isNull, reason: 'relative paths need a workspace');
    });

    test('only markdown hrefs are percent-decoded', () {
      expect(parsed('docs/my%20file.md#L5', workspaceRoot: project, decodeHref: true), ('$project/docs/my file.md', 5, null));
      expect(parsed('/tmp/%E5%BC%80%E6%88%B7.md:3', decodeHref: true), ('/tmp/开户.md', 3, null));
      expect(parsed('C:/repo/my%20dir/app.tsx#L8', workspaceRoot: 'C:/repo', decodeHref: true), ('C:/repo/my dir/app.tsx', 8, null));
      expect(parsed('docs/my%20file.md', workspaceRoot: project), ('$project/docs/my%20file.md', null, null));
      expect(parsed('/tmp/开户.md#L4'), ('/tmp/开户.md', 4, null));
      expect(parsed('docs/report.md%3A12', workspaceRoot: '/workspace', decodeHref: true), ('/workspace/docs/report.md:12', null, null));
      expect(() => parsed('/tmp/100% packet loss', workspaceRoot: project), returnsNormally);
    });

    test('normalizeInlinePathTarget: workspace-relative, outside, root and folders', () {
      expect(normalizeInlinePathTarget('/tmp/message.tsx', project), (directory: '/tmp', file: '/tmp/message.tsx'));
      expect(normalizeInlinePathTarget(project, project), (directory: '.', file: null));
      expect(normalizeInlinePathTarget('$project/packages/app/', project), (directory: 'packages/app', file: null));
      expect(
        normalizeInlinePathTarget('~/.paseo/plans/file-preview.md', project),
        (directory: '~/.paseo/plans', file: '~/.paseo/plans/file-preview.md'),
      );
    });
  });

  test('resolver.test.ts: which sources can resolve to a file', () {
    const project = '/Users/test/project';
    bool canResolve(String href, FileLinkSourceType type, [String? text]) => canResolveAssistantFileLinkToFile(
      AssistantFileLinkSource(href: href, text: text ?? href, type: type),
      project,
    );
    expect(canResolve('file.ts:12', FileLinkSourceType.inlineCode), isTrue);
    expect(canResolve('docs/my%20file.md', FileLinkSourceType.markdown), isTrue);
    expect(canResolve('http://dumm.md', FileLinkSourceType.markdown, 'dumm.md'), isFalse);
    expect(canResolve('http://google.com', FileLinkSourceType.linkify, 'google.com'), isFalse);
    expect(canResolve('', FileLinkSourceType.markdown), isFalse);
    expect(canResolve('https://example.com', FileLinkSourceType.inlineCode), isFalse);
  });

  test('line suffixes and workspace normalization', () {
    final colon = parseAssistantFileLink('src/foo.ts:12:5', workspaceRoot: root)!;
    expect(colon.path, '$root/src/foo.ts');
    expect(colon.lineStart, 12);
    final range = parseAssistantFileLink('src/foo.ts#L11-L16', workspaceRoot: root, explicitLink: true)!;
    expect((range.lineStart, range.lineEnd), (11, 16));
    expect(normalizeInlinePathTarget('$root/src/foo.ts', root)?.file, 'src/foo.ts');
    expect(normalizeInlinePathTarget('/etc/hosts', root)?.file, '/etc/hosts');
  });

  test('assistant image sources resolve to daemon reads or direct URIs', () {
    final absolute = resolveAssistantImageSource('$root/assets/screenshot.png', workspaceRoot: root);
    expect(absolute, isA<FileImageSource>());
    expect((absolute as FileImageSource).cwd, root);
    final relative = resolveAssistantImageSource('assets/new-image.png', workspaceRoot: root);
    expect((relative as FileImageSource).path, 'assets/new-image.png');
    final outside = resolveAssistantImageSource('/var/tmp/x.png', workspaceRoot: root);
    expect((outside as FileImageSource).cwd, '/');
    expect(resolveAssistantImageSource('data:image/png;base64,AAAA'), isA<DirectImageSource>());
    expect(resolveAssistantImageSource('assets/x.png'), isNull);
  });
}
