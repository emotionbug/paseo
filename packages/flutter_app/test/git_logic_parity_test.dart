import 'package:flutter_test/flutter_test.dart';
import 'package:paseo_sessions/features/git/git_actions.dart';
import 'package:paseo_sessions/features/git/git_controller.dart';
import 'package:paseo_sessions/features/git/review_panel.dart';
import 'package:paseo_sessions/features/git/worktree_archive_warning.dart';

/// Original git/policy.ts, git/forges/*.ts, review/store.ts,
/// git/worktree-archive-warning.ts, git/pr-hint.ts and use-diff-query.ts.
void main() {
  group('buildGitActions', () {
    test('dirty checkout promotes commit; clean behind promotes pull', () {
      expect(
        buildGitActions(const GitActionsInput(isGit: true, hasUncommittedChanges: true)).primary?.id,
        'commit',
      );
      expect(
        buildGitActions(const GitActionsInput(isGit: true, hasRemote: true, behindOfOrigin: 2)).primary?.id,
        'pull',
      );
    });

    test('no-upstream Paseo worktree with commits is first-pushable', () {
      final actions = buildGitActions(
        const GitActionsInput(isGit: true, hasRemote: true, isPaseoOwnedWorktree: true, aheadCount: 1),
      );
      expect(actions.primary?.id, 'push');
    });

    test('unavailable pull explains itself instead of being hidden', () {
      final actions = buildGitActions(const GitActionsInput(isGit: true, hasRemote: true, behindOfOrigin: 0));
      final pull = actions.secondary.firstWhere((a) => a.id == 'pull');
      expect(pull.unavailable?.key, 'workspace.git.actions.unavailable.pullUpToDate');
      expect(pull.disabled, isFalse);
    });

    test('merge PR uses the forge preferred method and allowed set', () {
      final capability = deriveMergeCapability({
        'forge': 'github',
        'mergeStateStatus': 'CLEAN',
        'repository': {
          'mergeCommitAllowed': true,
          'rebaseMergeAllowed': true,
          'viewerDefaultMergeMethod': 'REBASE',
        },
      });
      final input = GitActionsInput(
        isGit: true,
        hasPullRequest: true,
        pullRequestUrl: 'https://github.com/o/r/pull/1',
        pullRequestState: 'open',
        aheadCount: 1,
        mergeCapability: capability,
      );
      final actions = buildGitActions(input);
      expect(actions.primary?.id, 'merge-pr-rebase');
      expect(
        actions.secondary.map((a) => a.id).where((id) => id.startsWith('merge-pr-')),
        ['merge-pr-merge', 'merge-pr-rebase'],
      );
    });

    test('merged PR without local changes promotes archive', () {
      final actions = buildGitActions(
        const GitActionsInput(isGit: true, shouldPromoteArchive: true, isPaseoOwnedWorktree: true),
      );
      expect(actions.primary?.id, 'archive-workspace');
      expect(actions.secondary.any((a) => a.id == 'archive-workspace'), isFalse);
    });

    test('non-git checkout has no actions', () {
      final actions = buildGitActions(const GitActionsInput(isGit: false));
      expect(actions.primary, isNull);
      expect(actions.secondary, isEmpty);
    });
  });

  group('deriveMergeCapability', () {
    test('legacy github facts are used only without forgeSpecific', () {
      expect(deriveMergeCapability(null, {'mergeStateStatus': 'CLEAN'})?.directMergeReady, isTrue);
      expect(deriveMergeCapability(null), isNull);
    });

    test('gitlab canceling pipeline still allows enabling auto-merge', () {
      final capability = deriveMergeCapability({'forge': 'gitlab', 'pipelineStatus': 'canceling'});
      expect(capability?.canEnableAutoMerge, isTrue);
      expect(capability?.allowedMethods, ['merge', 'squash', 'rebase']);
    });

    test('schema mismatch and unknown families fall back to raw git state', () {
      expect(deriveMergeCapability({'forge': 'github', 'mergeStateStatus': 3}), isNull);
      expect(deriveMergeCapability({'forge': 'forgejo'}), isNull);
    });
  });

  test('PR hint parses the number from forge URLs and prefers merged', () {
    final hint = selectPrHint({
      'forge': 'gitlab',
      'status': {'url': 'https://gitlab.com/g/p/-/merge_requests/42', 'state': 'open', 'isMerged': true},
    })!;
    expect((hint.number, hint.state, hint.forge), (42, 'merged', 'gitlab'));
    expect(selectPrHint({'status': {'url': 'https://example.com/x'}}), isNull);
  });

  test('uncommitted comparison never sends a base ref', () {
    expect(
      normalizeCheckoutDiffCompare(mode: 'uncommitted', baseRef: 'main', ignoreWhitespace: false),
      {'mode': 'uncommitted', 'ignoreWhitespace': false},
    );
    expect(
      normalizeCheckoutDiffCompare(mode: 'base', baseRef: ' main ', ignoreWhitespace: true),
      {'mode': 'base', 'ignoreWhitespace': true, 'baseRef': 'main'},
    );
  });

  test('comparison selection is shared per workspace and defaults to all', () {
    final store = WorkingDiffComparisons.instance..reset();
    store.select('s', 'w1', '/repo', 'uncommitted');
    expect(store.resolve('s', 'w1', '/other'), 'uncommitted');
    expect(store.resolve('s', 'w2', '/repo'), 'all');
  });

  group('review attachment snapshot', () {
    final file = {
      'path': 'a.ts',
      'hunks': [
        {
          'oldStart': 1,
          'newStart': 1,
          'lines': [
            {'type': 'header', 'content': '@@ -1,2 +1,2 @@ fn'},
            {'type': 'context', 'content': 'a'},
            {'type': 'remove', 'content': 'b'},
            {'type': 'add', 'content': 'c'},
          ],
        },
      ],
    };
    JsonComment comment(String side, int line) => {
      'id': '$side$line',
      'filePath': 'a.ts',
      'side': side,
      'lineNumber': line,
      'body': 'x',
    };

    test('resolves context from the current diff and drops stale comments', () {
      final snapshot = buildReviewAttachmentSnapshot(
        cwd: '/repo',
        mode: 'uncommitted',
        baseRef: '',
        comments: [comment('new', 2), comment('new', 9)],
        diffFile: (path) => path == 'a.ts' ? file : null,
      )!;
      expect(snapshot.commentCount, 1);
      expect(snapshot.attachment['baseRef'], isNull);
      final context = (snapshot.attachment['comments'] as List).single['context'] as Map;
      expect(context['hunkHeader'], '@@ -1,2 +1,2 @@ fn');
      expect((context['targetLine'] as Map)['content'], 'c');
      expect((context['lines'] as List).length, 3);
    });

    test('null when no comment matches', () {
      expect(
        buildReviewAttachmentSnapshot(
          cwd: '/repo',
          mode: 'all',
          baseRef: 'main',
          comments: [comment('old', 7)],
          diffFile: (_) => file,
        ),
        isNull,
      );
    });

    test('feedback disabled reasons follow the original order', () {
      expect(reviewFeedbackDisabledReason(recipientCount: 0, connected: false, hasAttachment: false), 'no-agents');
      expect(reviewFeedbackDisabledReason(recipientCount: 1, connected: false, hasAttachment: false), 'disconnected');
      expect(reviewFeedbackDisabledReason(recipientCount: 2, connected: true, hasAttachment: false), 'no-context');
      expect(reviewFeedbackDisabledReason(recipientCount: 2, connected: true, hasAttachment: true), isNull);
    });
  });

  test('review draft key scopes by workspace, mode, base and whitespace', () {
    expect(
      buildReviewDraftKey(
        serverId: 's',
        workspaceId: '',
        cwd: '/repo/',
        mode: 'base',
        baseRef: 'main',
        ignoreWhitespace: true,
      ),
      'review:server=s:cwd=%2Frepo:mode=base:base=main:ignoreWhitespace=true',
    );
  });

  test('worktree archive reasons use the diff stat only when dirtiness is unknown', () {
    List<String> reasons({bool? isDirty, int? ahead, Map<String, dynamic>? diff}) =>
        buildWorktreeArchiveRiskReasons(
          isDirty: isDirty,
          aheadOfOrigin: ahead,
          diffStat: diff,
          label: (key, args) => '${key.split('.').last}${args['count'] ?? ''}',
        );
    expect(reasons(diff: {'additions': 1, 'deletions': 0}), ['uncommittedChangesWithDiff']);
    expect(reasons(isDirty: false, diff: {'additions': 3}), isEmpty);
    expect(reasons(isDirty: true, ahead: 2), ['uncommittedChanges', 'unpushedCommits2']);
  });
}

typedef JsonComment = Map<String, dynamic>;
