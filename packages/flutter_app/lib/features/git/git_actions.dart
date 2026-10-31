import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:url_launcher/url_launcher.dart';

import '../../core/app_store.dart';
import '../../core/client.dart';
import '../../core/json.dart';
import '../../core/scope.dart';
import '../../core/target.dart';
import '../../ui/common.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import 'git_controller.dart';
import 'worktree_archive_warning.dart';

// ---------------------------------------------------------------------------
// Forge presentation (protocol/forge-manifest.ts + git/forge.ts).
// ---------------------------------------------------------------------------

/// Original `normalizeForge`: an absent forge is GitHub (old daemons never send one).
String normalizeForge(String? raw) => raw == null || raw.isEmpty ? 'github' : raw;

class ForgePresentation {
  const ForgePresentation({
    required this.forge,
    required this.brandLabel,
    required this.changeRequestAbbrev,
    required this.numberPrefix,
    this.signInCli,
    this.signInCommand,
    this.hostnameFlag,
  });
  final String forge, brandLabel, changeRequestAbbrev, numberPrefix;
  final String? signInCli, signInCommand, hostnameFlag;

  /// Original `changeRequestContext`: keys with an `_mr` variant use it for MR forges.
  bool get isMergeRequest => changeRequestAbbrev == 'MR';
}

/// Original `getForgePresentation` over `FORGE_DEFINITIONS`; an unknown forge
/// renders neutrally (its id as the brand, "PR", no sign-in CLI).
ForgePresentation forgePresentation(String? raw) {
  final forge = normalizeForge(raw);
  return switch (forge) {
    'github' => const ForgePresentation(
      forge: 'github',
      brandLabel: 'GitHub',
      changeRequestAbbrev: 'PR',
      numberPrefix: '#',
      signInCli: 'gh',
      signInCommand: 'gh auth login',
    ),
    'gitlab' => const ForgePresentation(
      forge: 'gitlab',
      brandLabel: 'GitLab',
      changeRequestAbbrev: 'MR',
      numberPrefix: '!',
      signInCli: 'glab',
      signInCommand: 'glab auth login',
      hostnameFlag: '--hostname',
    ),
    'gitea' || 'forgejo' || 'codeberg' => ForgePresentation(
      forge: forge,
      brandLabel: switch (forge) {
        'gitea' => 'Gitea',
        'forgejo' => 'Forgejo',
        _ => 'Codeberg',
      },
      changeRequestAbbrev: 'PR',
      numberPrefix: '#',
      signInCli: 'tea',
      signInCommand: 'tea login add',
    ),
    _ => ForgePresentation(
      forge: forge,
      brandLabel: forge,
      changeRequestAbbrev: 'PR',
      numberPrefix: '#',
    ),
  };
}

/// Original `buildForgeSignInCommand`.
String? forgeSignInCommand(String? forge, String? host) {
  final presentation = forgePresentation(forge);
  final command = presentation.signInCommand;
  if (command == null) return null;
  final flag = presentation.hostnameFlag;
  if (flag != null && host != null && host.isNotEmpty) return '$command $flag $host';
  return command;
}

/// `t(key, { context: changeRequestContext })`: the `_mr` variant for MR forges,
/// falling back to the base key when no variant exists.
String trForge(
  BuildContext context,
  String key,
  String? forge, {
  Map<String, Object?> args = const {},
}) {
  if (forgePresentation(forge).isMergeRequest) {
    final variant = tr(context, '${key}_mr', args: args);
    if (variant != '${key}_mr') return variant;
  }
  return tr(context, key, args: args);
}

/// Original `parseGitRemoteLocation(url).host` for the sign-in hostname.
String? parseForgeHost(String? remoteUrl) {
  if (remoteUrl == null || remoteUrl.trim().isEmpty) return null;
  final url = remoteUrl.trim();
  final scp = RegExp(r'^(?:[^@/]+@)?([^:/]+):(?!//)').firstMatch(url);
  if (scp != null && !url.contains('://')) return scp[1];
  final uri = Uri.tryParse(url);
  final host = uri?.host ?? '';
  return host.isEmpty ? null : host;
}

/// Original `computeForgeSetupAction` + `buildForgeSetupMessage`: the onboarding
/// callout naming the CLI to install or the sign-in command to run. A daemon
/// without pluggable forges gets no setup action for a non-GitHub forge.
String? forgeSetupMessage(
  BuildContext context, {
  required String? forge,
  required String? authState,
  required bool forgeProvidersSupported,
  required String? remoteUrl,
}) {
  final normalized = normalizeForge(forge);
  if (normalized != 'github' && !forgeProvidersSupported) return null;
  final action = switch (authState) {
    'cli_missing' => 'install_cli',
    'unauthenticated' => 'sign_in',
    _ => null,
  };
  if (action == null) return null;
  final presentation = forgePresentation(normalized);
  final cli = presentation.signInCli;
  if (cli == null) {
    return tr(context, 'workspace.git.forgeSetup.generic', args: {'brand': presentation.brandLabel});
  }
  if (action == 'install_cli') {
    return tr(
      context,
      'workspace.git.forgeSetup.installCli',
      args: {'cli': cli, 'brand': presentation.brandLabel},
    );
  }
  return tr(
    context,
    'workspace.git.forgeSetup.signIn',
    args: {
      'command': forgeSignInCommand(normalized, parseForgeHost(remoteUrl)),
      'brand': presentation.brandLabel,
    },
  );
}

/// Original `normalizeCheckoutPrStatusPayload`: the legacy GitHub boolean maps
/// to an auth state for daemons that predate `authState`.
JsonMap normalizePrStatusPayload(JsonMap payload) {
  const known = {'authenticated', 'unauthenticated', 'cli_missing', 'no_remote', 'error'};
  final auth = payload['authState'];
  return {
    ...payload,
    'authState': auth is String && known.contains(auth)
        ? auth
        : (payload['githubFeaturesEnabled'] == true ? 'authenticated' : 'unauthenticated'),
  };
}

/// Original `formatBaseRefLabel` / `computeBaseRefLabel`.
String formatBaseRefLabel(String? baseRef, String fallback) {
  if (baseRef == null || baseRef.isEmpty) return fallback;
  final trimmed = baseRef.replaceFirst(RegExp(r'^refs/(heads|remotes)/'), '').trim();
  return trimmed.startsWith('origin/') ? trimmed.substring('origin/'.length) : trimmed;
}

/// Original `PrHint`: the change request a checkout points at.
class PrHint {
  const PrHint({required this.url, required this.number, required this.state, required this.forge});
  final String url;
  final int number;

  /// `open`, `merged` or `closed`.
  final String state;
  final String forge;
}

/// Original `selectPrHintFromStatus`: a hint only for a status whose URL names
/// a change request (GitHub `/pull/N`, Gitea/Forgejo `/pulls/N`, GitLab
/// `/-/merge_requests/N`); merged wins over the raw state.
PrHint? selectPrHint(JsonMap? payload) {
  final status = jsonMap(payload?['status']);
  final url = status.text('url');
  if (url.isEmpty) return null;
  final path = Uri.tryParse(url)?.path;
  if (path == null) return null;
  final match = RegExp(r'/(?:pull|pulls|merge_requests)/(\d+)(?:/|$)').firstMatch(path);
  final number = match == null ? null : int.tryParse(match[1]!);
  if (number == null) return null;
  final state = status.flag('isMerged') || status['state'] == 'merged'
      ? 'merged'
      : status['state'] == 'open'
      ? 'open'
      : 'closed';
  final forge = payload?['forge'] is String ? payload!['forge'] as String : status.text('forge');
  return PrHint(url: url, number: number, state: state, forge: normalizeForge(forge));
}

// ---------------------------------------------------------------------------
// Merge capability (git/merge-capability.ts + git/forges/*.ts).
// ---------------------------------------------------------------------------

class MergeCapability {
  const MergeCapability({
    required this.directMergeReady,
    required this.canEnableAutoMerge,
    required this.autoMergeEnabled,
    required this.canDisableAutoMerge,
    required this.mergeBlockedByQueue,
    required this.allowedMethods,
    this.preferredMethod,
  });
  final bool directMergeReady,
      canEnableAutoMerge,
      autoMergeEnabled,
      canDisableAutoMerge,
      mergeBlockedByQueue;
  final List<String> allowedMethods;
  final String? preferredMethod;
}

/// Original GitLab `GITLAB_ACTIVE_PIPELINE_STATUSES`.
const gitlabActivePipelineStatuses = {
  'created',
  'waiting_for_resource',
  'preparing',
  'pending',
  'running',
  'canceling',
  'scheduled',
};

class _SchemaMismatch implements Exception {}

T? _optional<T>(JsonMap facts, String key) {
  final value = facts[key];
  if (value == null) return null;
  if (value is T) return value;
  throw _SchemaMismatch();
}

/// Original `deriveMergeCapability`: the registered forge's facts arm, the legacy
/// `status.github` facts when `forgeSpecific` is absent, and null for an unknown
/// or schema-mismatched family (callers then fall back to raw git state).
MergeCapability? deriveMergeCapability(Object? forgeSpecific, [Object? legacyGithub]) {
  if (forgeSpecific == null) {
    if (legacyGithub is Map) {
      return _deriveForgeCapability({'forge': 'github', ...jsonMap(legacyGithub)});
    }
    return null;
  }
  if (forgeSpecific is! Map) return null;
  return _deriveForgeCapability(jsonMap(forgeSpecific));
}

MergeCapability? _deriveForgeCapability(JsonMap facts) {
  try {
    switch (facts['forge']) {
      case 'github':
        final mergeStateStatus = _optional<String>(facts, 'mergeStateStatus');
        final autoMergeRequest = facts['autoMergeRequest'];
        if (autoMergeRequest != null && autoMergeRequest is! Map) throw _SchemaMismatch();
        final rawRepository = facts['repository'];
        if (rawRepository != null && rawRepository is! Map) throw _SchemaMismatch();
        final repository = jsonMap(rawRepository);
        bool repo(String key) => _optional<bool>(repository, key) ?? false;
        final methods = <String>[
          if (repo('mergeCommitAllowed')) 'merge',
          if (repo('squashMergeAllowed')) 'squash',
          if (repo('rebaseMergeAllowed')) 'rebase',
        ];
        final preferred = switch (_optional<String>(repository, 'viewerDefaultMergeMethod')) {
          'SQUASH' => 'squash',
          'MERGE' => 'merge',
          'REBASE' => 'rebase',
          _ => null,
        };
        return MergeCapability(
          directMergeReady: const {'CLEAN', 'HAS_HOOKS'}.contains(mergeStateStatus ?? ''),
          canEnableAutoMerge:
              mergeStateStatus == 'BLOCKED' &&
              repo('autoMergeAllowed') &&
              (_optional<bool>(facts, 'viewerCanEnableAutoMerge') ?? false),
          autoMergeEnabled: autoMergeRequest != null,
          canDisableAutoMerge: _optional<bool>(facts, 'viewerCanDisableAutoMerge') ?? false,
          mergeBlockedByQueue:
              (_optional<bool>(facts, 'isMergeQueueEnabled') ?? false) ||
              (_optional<bool>(facts, 'isInMergeQueue') ?? false),
          allowedMethods: methods,
          preferredMethod: preferred,
        );
      case 'gitlab':
        final detailed = _optional<String>(facts, 'detailedMergeStatus');
        final legacy = _optional<String>(facts, 'mergeStatus');
        final hasConflicts = _optional<bool>(facts, 'hasConflicts') ?? false;
        final pipelineStatus = _optional<String>(facts, 'pipelineStatus');
        _optional<num>(facts, 'pipelineId');
        _optional<String>(facts, 'pipelineUrl');
        _optional<bool>(facts, 'blockingDiscussionsResolved');
        _optional<num>(facts, 'approvalsRequired');
        _optional<num>(facts, 'approvalsGiven');
        final autoMergeEnabled = _optional<bool>(facts, 'mergeWhenPipelineSucceeds') ?? false;
        final hasActivePipeline =
            pipelineStatus != null && gitlabActivePipelineStatuses.contains(pipelineStatus);
        return MergeCapability(
          directMergeReady: detailed != null
              ? detailed == 'mergeable'
              : legacy == 'can_be_merged' && !hasConflicts,
          canEnableAutoMerge: !autoMergeEnabled && hasActivePipeline,
          autoMergeEnabled: autoMergeEnabled,
          canDisableAutoMerge: autoMergeEnabled,
          mergeBlockedByQueue: false,
          allowedMethods: const ['merge', 'squash', 'rebase'],
        );
      case 'gitea':
        final mergeable = _optional<bool>(facts, 'mergeable') ?? false;
        final hasMerged = _optional<bool>(facts, 'hasMerged') ?? false;
        _optional<String>(facts, 'ciStatus');
        return MergeCapability(
          directMergeReady: mergeable && !hasMerged,
          canEnableAutoMerge: false,
          autoMergeEnabled: false,
          canDisableAutoMerge: false,
          mergeBlockedByQueue: false,
          allowedMethods: const ['merge', 'squash', 'rebase'],
        );
    }
  } on _SchemaMismatch {
    return null;
  }
  return null;
}

// ---------------------------------------------------------------------------
// Action policy (git/policy.ts).
// ---------------------------------------------------------------------------

enum GitActionStatus { idle, pending, success }

class GitActionRuntime {
  const GitActionRuntime({this.disabled = false, this.status = GitActionStatus.idle});
  final bool disabled;
  final GitActionStatus status;
}

/// An unavailable explanation as a copy key plus its arguments.
class GitActionMessage {
  const GitActionMessage(this.key, [this.args = const {}]);
  final String key;
  final Map<String, Object?> args;
  @override
  bool operator ==(Object other) => other is GitActionMessage && other.key == key;
  @override
  int get hashCode => key.hashCode;
  @override
  String toString() => key;
}

class GitAction {
  const GitAction({
    required this.id,
    required this.disabled,
    required this.status,
    required this.startsGroup,
    this.unavailable,
  });
  final String id;
  final bool disabled;
  final GitActionStatus status;
  final bool startsGroup;
  final GitActionMessage? unavailable;
}

class GitActions {
  const GitActions({this.primary, this.secondary = const [], this.menu = const []});
  final GitAction? primary;
  final List<GitAction> secondary, menu;
}

class GitActionsInput {
  const GitActionsInput({
    required this.isGit,
    this.githubFeaturesEnabled = true,
    this.forgeBrandLabel = 'GitHub',
    this.forgeChangeRequestNoun = 'PR',
    this.githubAutoMergeActionsEnabled = false,
    this.hasPullRequest = false,
    this.pullRequestUrl,
    this.pullRequestState,
    this.pullRequestIsDraft = false,
    this.pullRequestIsMerged = false,
    this.pullRequestMergeable = 'UNKNOWN',
    this.mergeCapability,
    this.hasRemote = false,
    this.isPaseoOwnedWorktree = false,
    this.isOnBaseBranch = false,
    this.hasUncommittedChanges = false,
    this.baseRefAvailable = false,
    this.baseRefLabel = 'base',
    this.aheadCount = 0,
    this.behindBaseCount = 0,
    this.aheadOfOrigin,
    this.behindOfOrigin,
    this.shouldPromoteArchive = false,
    this.shipDefault = 'pr',
    this.runtime = const {},
  });
  final bool isGit, githubFeaturesEnabled, githubAutoMergeActionsEnabled, hasPullRequest;
  final String forgeBrandLabel, forgeChangeRequestNoun;
  final String? pullRequestUrl;

  /// `open`, `closed` or null (original `narrowPullRequestState`).
  final String? pullRequestState;
  final bool pullRequestIsDraft, pullRequestIsMerged;
  final String pullRequestMergeable;
  final MergeCapability? mergeCapability;
  final bool hasRemote, isPaseoOwnedWorktree, isOnBaseBranch, hasUncommittedChanges;
  final bool baseRefAvailable;
  final String baseRefLabel;
  final int aheadCount, behindBaseCount;
  final int? aheadOfOrigin, behindOfOrigin;
  final bool shouldPromoteArchive;
  final String shipDefault;
  final Map<String, GitActionRuntime> runtime;

  GitActionRuntime runtimeOf(String id) => runtime[id] ?? const GitActionRuntime();
}

const _directMergeModels = [
  ('merge-pr-squash', 'squash', true),
  ('merge-pr-merge', 'merge', false),
  ('merge-pr-rebase', 'rebase', false),
];
const _autoMergeModels = [
  ('enable-pr-auto-merge-squash', 'squash', true),
  ('enable-pr-auto-merge-merge', 'merge', false),
  ('enable-pr-auto-merge-rebase', 'rebase', false),
];
const _pullRequestActionIds = [
  'pr',
  'merge-pr-squash',
  'merge-pr-merge',
  'merge-pr-rebase',
  'enable-pr-auto-merge-squash',
  'enable-pr-auto-merge-merge',
  'enable-pr-auto-merge-rebase',
  'disable-pr-auto-merge',
];

/// Original `narrowPullRequestState`.
String? narrowPullRequestState(String? state) =>
    state == 'open' || state == 'closed' ? state : null;

/// Original `buildGitActions`.
GitActions buildGitActions(GitActionsInput input) {
  if (!input.isGit) return const GitActions();
  final all = <String, GitAction>{};
  GitAction simple(
    String id, {
    bool startsGroup = false,
    GitActionMessage? Function()? unavailable,
  }) {
    final runtime = input.runtimeOf(id);
    return GitAction(
      id: id,
      disabled: runtime.disabled,
      status: runtime.status,
      startsGroup: startsGroup,
      unavailable: runtime.disabled || unavailable == null ? null : unavailable(),
    );
  }

  all['commit'] = simple('commit');
  all['pull'] = simple('pull', unavailable: () => _pullUnavailable(input));
  all['push'] = simple('push', unavailable: () => _pushUnavailable(input));
  all['pull-and-push'] = simple('pull-and-push', unavailable: () => _pullAndPushUnavailable(input));
  all['pr'] = _prAction(input);
  for (final (id, _, startsGroup) in _directMergeModels) {
    final runtime = input.runtimeOf(id);
    all[id] = GitAction(
      id: id,
      disabled: runtime.disabled || !_canMergePr(input),
      status: runtime.status,
      startsGroup: startsGroup,
      unavailable: runtime.disabled ? null : _mergePrUnavailable(input),
    );
  }
  for (final (id, _, startsGroup) in _autoMergeModels) {
    final runtime = input.runtimeOf(id);
    all[id] = GitAction(
      id: id,
      disabled: runtime.disabled,
      status: runtime.status,
      startsGroup: startsGroup,
    );
  }
  final disableRuntime = input.runtimeOf('disable-pr-auto-merge');
  final canDisable = input.mergeCapability?.canDisableAutoMerge == true;
  all['disable-pr-auto-merge'] = GitAction(
    id: 'disable-pr-auto-merge',
    disabled: disableRuntime.disabled || !canDisable,
    status: disableRuntime.status,
    startsGroup: true,
    unavailable: disableRuntime.disabled || canDisable
        ? null
        : const GitActionMessage('workspace.git.actions.unavailable.autoMergeCannotDisable'),
  );
  all['merge-branch'] = simple('merge-branch', unavailable: () => _mergeBranchUnavailable(input));
  all['merge-from-base'] = simple(
    'merge-from-base',
    startsGroup: true,
    unavailable: () => _mergeFromBaseUnavailable(input),
  );
  all['archive-workspace'] = simple('archive-workspace', startsGroup: true);

  final primaryId = _primaryActionId(input);
  final secondaryIds = <String>[
    'pull',
    'push',
    'pull-and-push',
    if (!input.isOnBaseBranch) ...[
      'merge-from-base',
      'merge-branch',
      ..._pullRequestActionIds.where((id) => _shouldShowPullRequestAction(input, id)),
    ],
    'archive-workspace',
  ];
  return GitActions(
    primary: primaryId == null ? null : all[primaryId],
    secondary: [
      for (final id in secondaryIds)
        if (id != 'archive-workspace' || primaryId != 'archive-workspace') all[id]!,
    ],
  );
}

/// Original `actionsWithoutPrimary`.
List<GitAction> actionsWithoutPrimary(GitActions actions) => [
  for (final action in [...actions.secondary, ...actions.menu])
    if (action.id != actions.primary?.id) action,
];

GitAction _prAction(GitActionsInput input) {
  final runtime = input.runtimeOf('pr');
  final viewing = input.hasPullRequest && input.pullRequestUrl != null;
  GitActionMessage? unavailable;
  if (!runtime.disabled) {
    if (viewing) {
      unavailable = input.githubFeaturesEnabled
          ? null
          : GitActionMessage('workspace.git.actions.unavailable.viewPrNoForge', {
              'brand': input.forgeBrandLabel,
              'noun': input.forgeChangeRequestNoun,
            });
    } else {
      unavailable = _createPrUnavailable(input);
    }
  }
  return GitAction(
    id: 'pr',
    disabled: runtime.disabled,
    status: runtime.status,
    startsGroup: false,
    unavailable: unavailable,
  );
}

String? _primaryActionId(GitActionsInput input) {
  if (input.shouldPromoteArchive) return 'archive-workspace';
  if (input.hasUncommittedChanges) return 'commit';
  if (_canPull(input)) return 'pull';
  if (_canPush(input)) return 'push';
  if (_canMergePr(input)) {
    return _preferred(input, _directMergeModels)?.$1 ?? _directMergeModels.first.$1;
  }
  if (_canEnablePrAutoMerge(input)) {
    return _preferred(input, _autoMergeModels)?.$1 ?? _autoMergeModels.first.$1;
  }
  if (_hasEnabledPrAutoMerge(input)) return 'pr';
  if (input.shipDefault == 'pr' && _canUsePullRequestActionAsShipDefault(input)) return 'pr';
  if (!input.isOnBaseBranch && input.aheadCount > 0) return 'merge-branch';
  if (!input.isOnBaseBranch && _canMergeFromBase(input)) return 'merge-from-base';
  if (input.githubFeaturesEnabled && input.hasPullRequest && input.pullRequestUrl != null) {
    return 'pr';
  }
  // Only Paseo-owned worktrees get Archive as a fallback primary action.
  if (input.isPaseoOwnedWorktree) return 'archive-workspace';
  return null;
}

bool _methodAllowed(GitActionsInput input, String method) {
  final capability = input.mergeCapability;
  return capability == null || capability.allowedMethods.contains(method);
}

List<(String, String, bool)> _allowed(GitActionsInput input, List<(String, String, bool)> models) =>
    models.where((model) => _methodAllowed(input, model.$2)).toList();

(String, String, bool)? _preferred(GitActionsInput input, List<(String, String, bool)> models) {
  final allowed = _allowed(input, models);
  final preferred = input.mergeCapability?.preferredMethod;
  return allowed.where((model) => model.$2 == preferred).firstOrNull ?? allowed.firstOrNull;
}

bool _shouldShowPullRequestAction(GitActionsInput input, String id) {
  if (id == 'pr') return true;
  if (id == 'disable-pr-auto-merge') {
    return input.githubAutoMergeActionsEnabled &&
        input.mergeCapability?.autoMergeEnabled == true;
  }
  if (id.startsWith('merge-pr-')) {
    return _canMergePr(input) &&
        _allowed(input, _directMergeModels).any((model) => model.$1 == id);
  }
  if (id.startsWith('enable-pr-auto-merge-')) {
    return _canEnablePrAutoMerge(input) &&
        _allowed(input, _autoMergeModels).any((model) => model.$1 == id);
  }
  return false;
}

bool _canPull(GitActionsInput input) =>
    input.hasRemote && !input.hasUncommittedChanges && (input.behindOfOrigin ?? 0) > 0;

bool _canPush(GitActionsInput input) =>
    input.hasRemote && _hasPushableCommits(input) && (input.behindOfOrigin ?? 0) == 0;

bool _hasPushableCommits(GitActionsInput input) {
  if ((input.aheadOfOrigin ?? 0) > 0) return true;
  // No-upstream Paseo worktrees are first-pushable: the daemon push sets upstream.
  return input.isPaseoOwnedWorktree && input.aheadOfOrigin == null && input.aheadCount > 0;
}

bool _canMergeFromBase(GitActionsInput input) =>
    !input.isOnBaseBranch &&
    input.baseRefAvailable &&
    !input.hasUncommittedChanges &&
    input.behindBaseCount > 0;

bool _canUsePullRequestActionAsShipDefault(GitActionsInput input) {
  if (input.isOnBaseBranch || !input.githubFeaturesEnabled) return false;
  if (input.hasPullRequest) return input.pullRequestUrl != null;
  return input.aheadCount > 0;
}

bool _canMergePr(GitActionsInput input) {
  final fromStatus =
      input.githubFeaturesEnabled &&
      input.hasPullRequest &&
      input.pullRequestState == 'open' &&
      !input.pullRequestIsDraft &&
      !input.pullRequestIsMerged &&
      input.pullRequestMergeable != 'CONFLICTING' &&
      input.aheadCount > 0 &&
      !input.hasUncommittedChanges;
  if (!fromStatus) return false;
  final capability = input.mergeCapability;
  if (capability == null) {
    return input.pullRequestMergeable == 'MERGEABLE' &&
        input.behindOfOrigin == 0 &&
        input.aheadOfOrigin == 0 &&
        !_canMergeFromBase(input);
  }
  return capability.directMergeReady &&
      !capability.autoMergeEnabled &&
      !capability.mergeBlockedByQueue &&
      _allowed(input, _directMergeModels).isNotEmpty;
}

bool _canEnablePrAutoMerge(GitActionsInput input) {
  final capability = input.mergeCapability;
  return input.githubFeaturesEnabled &&
      input.githubAutoMergeActionsEnabled &&
      input.hasPullRequest &&
      input.pullRequestState == 'open' &&
      !input.pullRequestIsDraft &&
      !input.pullRequestIsMerged &&
      input.pullRequestMergeable != 'CONFLICTING' &&
      capability != null &&
      !capability.autoMergeEnabled &&
      capability.canEnableAutoMerge &&
      !capability.mergeBlockedByQueue &&
      _allowed(input, _autoMergeModels).isNotEmpty;
}

bool _hasEnabledPrAutoMerge(GitActionsInput input) =>
    input.githubFeaturesEnabled &&
    input.hasPullRequest &&
    input.pullRequestUrl != null &&
    input.mergeCapability?.autoMergeEnabled == true;

const _u = 'workspace.git.actions.unavailable';

GitActionMessage? _pullUnavailable(GitActionsInput input) {
  if (!input.hasRemote) return const GitActionMessage('$_u.pullNoRemote');
  if (input.hasUncommittedChanges) return const GitActionMessage('$_u.pullDirty');
  if (input.behindOfOrigin == null) return const GitActionMessage('$_u.pullNoRemote');
  if (input.behindOfOrigin == 0) return const GitActionMessage('$_u.pullUpToDate');
  return null;
}

GitActionMessage? _pushUnavailable(GitActionsInput input) {
  if (!input.hasRemote) return const GitActionMessage('$_u.pushNoRemote');
  if ((input.behindOfOrigin ?? 0) > 0) return const GitActionMessage('$_u.pushBehind');
  if (!_hasPushableCommits(input)) return const GitActionMessage('$_u.pushNothing');
  return null;
}

GitActionMessage? _pullAndPushUnavailable(GitActionsInput input) {
  if (!input.hasRemote) return const GitActionMessage('$_u.pullAndPushNoRemote');
  if (input.hasUncommittedChanges) return const GitActionMessage('$_u.pullAndPushDirty');
  if (input.behindOfOrigin == null) return const GitActionMessage('$_u.pullAndPushNoIncoming');
  if (input.behindOfOrigin == 0 && input.aheadOfOrigin == 0) {
    return const GitActionMessage('$_u.pullAndPushInSync');
  }
  if (input.behindOfOrigin == 0) return const GitActionMessage('$_u.pullAndPushNoIncoming');
  if ((input.aheadOfOrigin ?? 0) == 0) {
    return const GitActionMessage('$_u.pullAndPushNothingToPush');
  }
  return null;
}

GitActionMessage? _createPrUnavailable(GitActionsInput input) {
  if (!input.githubFeaturesEnabled) {
    return GitActionMessage('$_u.createPrNoForge', {
      'brand': input.forgeBrandLabel,
      'noun': input.forgeChangeRequestNoun,
    });
  }
  if (input.aheadCount == 0) return const GitActionMessage('$_u.createPrNoCommits');
  return null;
}

GitActionMessage? _mergeBranchUnavailable(GitActionsInput input) {
  if (!input.baseRefAvailable) return const GitActionMessage('$_u.mergeNoBase');
  if (input.hasUncommittedChanges) return const GitActionMessage('$_u.mergeDirty');
  if (input.aheadCount == 0) return const GitActionMessage('$_u.mergeNothing');
  return null;
}

GitActionMessage? _mergeFromBaseUnavailable(GitActionsInput input) {
  if (!input.baseRefAvailable) return const GitActionMessage('$_u.updateNoBase');
  if (input.hasUncommittedChanges) return const GitActionMessage('$_u.updateDirty');
  if (input.behindBaseCount == 0) {
    return GitActionMessage('$_u.updateCurrent', {'baseRef': input.baseRefLabel});
  }
  return null;
}

GitActionMessage? _mergePrUnavailable(GitActionsInput input) {
  final forgeArgs = {'brand': input.forgeBrandLabel, 'noun': input.forgeChangeRequestNoun};
  if (!input.githubFeaturesEnabled) return GitActionMessage('$_u.mergePrNoForge', forgeArgs);
  if (!input.hasPullRequest) return const GitActionMessage('$_u.mergePrMissing');
  if (input.pullRequestIsDraft) return const GitActionMessage('$_u.mergePrDraft');
  if (input.pullRequestIsMerged) return const GitActionMessage('$_u.mergePrMerged');
  if (input.pullRequestState == 'closed') return const GitActionMessage('$_u.mergePrClosed');
  if (input.pullRequestMergeable == 'CONFLICTING') {
    return const GitActionMessage('$_u.mergePrConflicts');
  }
  final capability = input.mergeCapability;
  if (capability == null) return null;
  if (capability.mergeBlockedByQueue) return const GitActionMessage('$_u.mergePrQueue');
  if (!capability.directMergeReady) return GitActionMessage('$_u.mergePrNotReady', forgeArgs);
  return null;
}

/// Original `deriveGitActionsState` + the `useGitActions` input mapping from a
/// checkout status and PR status payload.
GitActionsInput gitActionsInputFrom({
  required JsonMap? status,
  required JsonMap? prPayload,
  required bool statusLoading,
  required bool postShipArchiveSuggested,
  required bool autoMergeActionsEnabled,
  required String shipDefault,
  required String baseFallbackLabel,
  required GitActionRuntime Function(String id, bool actionsDisabled) runtime,
}) {
  final gitStatus = status != null && status.flag('isGit') ? status : null;
  final isGit = gitStatus != null;
  final pr = jsonMap(prPayload?['status']);
  final prUrl = pr.text('url');
  final forge = forgePresentation(prPayload?.text('forge'));
  final baseRef = gitStatus?.text('baseRef') ?? '';
  final baseRefLabel = formatBaseRefLabel(baseRef, baseFallbackLabel);
  final aheadBehind = jsonMap(gitStatus?['aheadBehind']);
  final hasUncommittedChanges = gitStatus?.flag('isDirty') ?? false;
  final actionsDisabled = !isGit || status?['error'] != null || statusLoading;
  int? nullableInt(Object? value) => value is num ? value.toInt() : null;
  return GitActionsInput(
    isGit: isGit,
    githubFeaturesEnabled: prPayload == null ? true : prPayload.flag('githubFeaturesEnabled', true),
    forgeBrandLabel: forge.brandLabel,
    forgeChangeRequestNoun: forge.changeRequestAbbrev,
    githubAutoMergeActionsEnabled: autoMergeActionsEnabled,
    hasPullRequest: prUrl.isNotEmpty,
    pullRequestUrl: prUrl.isEmpty ? null : prUrl,
    pullRequestState: narrowPullRequestState(pr['state'] as String?),
    pullRequestIsDraft: pr.flag('isDraft'),
    pullRequestIsMerged: pr.flag('isMerged'),
    pullRequestMergeable: pr.text('mergeable', 'UNKNOWN'),
    mergeCapability: deriveMergeCapability(pr['forgeSpecific'], pr['github']),
    hasRemote: gitStatus?.flag('hasRemote') ?? false,
    isPaseoOwnedWorktree: gitStatus?.flag('isPaseoOwnedWorktree') ?? false,
    isOnBaseBranch: gitStatus?.text('currentBranch') == baseRefLabel,
    hasUncommittedChanges: hasUncommittedChanges,
    baseRefAvailable: baseRef.isNotEmpty,
    baseRefLabel: baseRefLabel,
    aheadCount: aheadBehind.integer('ahead'),
    behindBaseCount: aheadBehind.integer('behind'),
    aheadOfOrigin: nullableInt(gitStatus?['aheadOfOrigin']),
    behindOfOrigin: nullableInt(gitStatus?['behindOfOrigin']),
    shouldPromoteArchive: !hasUncommittedChanges && (postShipArchiveSuggested || pr.flag('isMerged')),
    shipDefault: shipDefault,
    runtime: {
      for (final id in [
        'commit',
        'pull',
        'push',
        'pull-and-push',
        'pr',
        ..._directMergeModels.map((model) => model.$1),
        ..._autoMergeModels.map((model) => model.$1),
        'disable-pr-auto-merge',
        'merge-branch',
        'merge-from-base',
        'archive-workspace',
      ])
        id: runtime(id, actionsDisabled),
    },
  );
}

// ---------------------------------------------------------------------------
// Shared checkout action state (git/actions-store.ts).
// ---------------------------------------------------------------------------

/// Original `useCheckoutGitActionsStore`: per checkout and action a pending /
/// success status (success shown for one second), one in-flight run per action
/// that later callers join, and the query invalidation every success triggers.
class CheckoutGitActionsStore extends ChangeNotifier {
  CheckoutGitActionsStore._();
  static final instance = CheckoutGitActionsStore._();
  static const successDisplay = Duration(milliseconds: 1000);

  final Map<String, GitActionStatus> _status = {};
  final Map<String, Future<void>> _inFlight = {};
  final Map<String, Timer> _successTimers = {};
  final Set<String> _postShipArchive = {};
  static String _checkout(String serverId, String cwd) => '$serverId::$cwd';

  GitActionStatus status(String serverId, String cwd, String actionId) =>
      _status['${_checkout(serverId, cwd)}::$actionId'] ?? GitActionStatus.idle;

  bool postShipArchiveSuggested(String serverId, String cwd) =>
      _postShipArchive.contains(_checkout(serverId, cwd));

  void suggestPostShipArchive(String serverId, String cwd) {
    if (_postShipArchive.add(_checkout(serverId, cwd))) notifyListeners();
  }

  /// Original `invalidateCheckoutGitQueries`: status, PR status and commits of
  /// the checkout reload.
  void invalidate(String serverId, String cwd) => CheckoutInvalidations.add(serverId, cwd);

  void _set(String key, GitActionStatus status) {
    final current = _status[key] ?? GitActionStatus.idle;
    if (current == status) return;
    if (status == GitActionStatus.idle) {
      _status.remove(key);
    } else {
      _status[key] = status;
    }
    notifyListeners();
  }

  Future<void> run({
    required String serverId,
    required String cwd,
    required String actionId,
    required Future<void> Function() body,
  }) async {
    final key = '${_checkout(serverId, cwd)}::$actionId';
    final existing = _inFlight[key];
    if (existing != null) {
      await existing;
      return;
    }
    _successTimers.remove(key)?.cancel();
    _set(key, GitActionStatus.pending);
    final future = () async {
      try {
        await body();
        invalidate(serverId, cwd);
        _set(key, GitActionStatus.success);
        _successTimers[key] = Timer(successDisplay, () {
          _successTimers.remove(key);
          _set(key, GitActionStatus.idle);
        });
      } catch (_) {
        _set(key, GitActionStatus.idle);
        rethrow;
      } finally {
        _inFlight.remove(key);
      }
    }();
    _inFlight[key] = future;
    await future;
  }

  /// Re-render consumers after an action-owned flag outside this store changed.
  void touch() => notifyListeners();
}

/// Original `useCheckoutStatusQuery` + `useCheckoutPrStatusQuery` for one
/// checkout: the status and PR status snapshots, kept current from
/// `checkout_status_update` pushes (including their `prStatus`), action
/// invalidations and reconnects. Shared per host and cwd.
class CheckoutGitState extends ChangeNotifier {
  CheckoutGitState._(this.client, this.serverId, this.cwd) {
    _connection = client.status;
    client.addListener(_connectionChanged);
    _events = client.events.listen(_event);
    _invalidations = CheckoutInvalidations.stream.listen((checkout) {
      if (checkout.$1 == serverId && checkout.$2 == cwd) unawaited(reload());
    });
    if (client.status == ConnectionStatus.connected) unawaited(reload());
  }

  static final Map<String, CheckoutGitState> _shared = {};
  static final Map<String, int> _refs = {};

  static CheckoutGitState acquire(PaseoClient client, String serverId, String cwd) {
    final key = '$serverId::$cwd';
    final current = _shared[key];
    if (current != null && identical(current.client, client)) {
      _refs[key] = (_refs[key] ?? 0) + 1;
      return current;
    }
    final state = CheckoutGitState._(client, serverId, cwd);
    _shared[key] = state;
    _refs[key] = 1;
    return state;
  }

  void release() {
    final key = '$serverId::$cwd';
    if (!identical(_shared[key], this)) {
      dispose();
      return;
    }
    final refs = (_refs[key] ?? 1) - 1;
    if (refs > 0) {
      _refs[key] = refs;
      return;
    }
    _refs.remove(key);
    _shared.remove(key);
    dispose();
  }

  final PaseoClient client;
  final String serverId, cwd;
  late ConnectionStatus _connection;
  StreamSubscription<JsonMap>? _events;
  StreamSubscription<(String, String)>? _invalidations;
  JsonMap? status;
  JsonMap? prPayload;
  Object? statusError;
  bool statusLoading = false;

  /// Original PR status query state: loading before the first payload, the
  /// query's own failure, and a refetch in flight.
  bool prLoading = false, prFetching = false;
  Object? prError;

  /// Bumped whenever the PR status meaningfully changes (original
  /// `hasPrStatusChanged`, ignoring the volatile `requestId`).
  int prRevision = 0;
  bool _disposed = false;
  int _statusGeneration = 0, _prGeneration = 0;

  void _setPrPayload(JsonMap next) {
    final normalized = normalizePrStatusPayload(next);
    String identity(JsonMap? payload) =>
        payload == null ? '' : jsonEncode({...payload}..remove('requestId'));
    if (identity(prPayload) != identity(normalized)) prRevision++;
    prPayload = normalized;
  }

  bool get isGit => status?.flag('isGit') == true;

  void _connectionChanged() {
    if (_disposed || _connection == client.status) return;
    _connection = client.status;
    if (_connection == ConnectionStatus.connected) unawaited(reload());
  }

  void _event(JsonMap event) {
    if (event['type'] != 'checkout_status_update') return;
    final payload = jsonMap(event['payload']);
    if (payload.text('cwd') != cwd) return;
    _statusGeneration++;
    status = {...payload}..remove('prStatus');
    statusError = null;
    statusLoading = false;
    if (payload['prStatus'] is Map) {
      _prGeneration++;
      _setPrPayload(payload.object('prStatus'));
      prError = null;
      prLoading = false;
    }
    notifyListeners();
  }

  Future<void> reload() async {
    if (_disposed) return;
    final generation = ++_statusGeneration;
    statusLoading = status == null;
    notifyListeners();
    try {
      final next = await checkoutPayload(client, 'checkout_status_request', {'cwd': cwd});
      if (_disposed || generation != _statusGeneration) return;
      status = next;
      statusError = null;
    } catch (error) {
      if (_disposed || generation != _statusGeneration) return;
      statusError = error;
    } finally {
      if (!_disposed && generation == _statusGeneration) {
        statusLoading = false;
        notifyListeners();
      }
    }
    if (isGit) await reloadPullRequest();
  }

  Future<void> reloadPullRequest() async {
    if (_disposed) return;
    final generation = ++_prGeneration;
    prLoading = prPayload == null;
    prFetching = true;
    notifyListeners();
    try {
      final next = await checkoutPayload(client, 'checkout_pr_status_request', {'cwd': cwd});
      if (_disposed || generation != _prGeneration) return;
      _setPrPayload(next);
      prError = null;
    } catch (error) {
      // The original query keeps its last data on a failed refetch.
      if (_disposed || generation != _prGeneration) return;
      prError = error;
    } finally {
      if (!_disposed && generation == _prGeneration) {
        prLoading = false;
        prFetching = false;
        notifyListeners();
      }
    }
  }

  @override
  void dispose() {
    _disposed = true;
    client.removeListener(_connectionChanged);
    _events?.cancel();
    _invalidations?.cancel();
    super.dispose();
  }
}

// ---------------------------------------------------------------------------
// Handlers and runner (git/use-actions.tsx).
// ---------------------------------------------------------------------------

/// Binds the action policy to one checkout: runtime state, labels, handlers.
class CheckoutGitActions {
  CheckoutGitActions({
    required this.store,
    required this.serverId,
    required this.workspaceId,
    required this.cwd,
    required this.state,
  });
  final AppStore store;
  final String serverId, workspaceId, cwd;
  final CheckoutGitState state;
  bool _hiding = false;

  CheckoutGitActionsStore get _actions => CheckoutGitActionsStore.instance;
  PaseoClient get _client => store.clientFor(serverId);

  JsonMap? get _workspace {
    final workspaces = store.hostFor(serverId)?.workspaces ?? const <JsonMap>[];
    if (workspaceId.isNotEmpty) {
      final match = workspaces.where((w) => w.text('id') == workspaceId).firstOrNull;
      if (match != null) return match;
    }
    final directory = state.status?.text('cwd') ?? cwd;
    return workspaces.where((w) => w.text('workspaceDirectory') == directory).firstOrNull;
  }

  /// Original `resolveWorkspaceArchiveRisk`: checkout status first, then the
  /// workspace's git runtime.
  (bool?, int?, bool, bool) get _archiveRisk {
    final workspace = _workspace;
    final git = state.isGit ? state.status : null;
    final runtime = jsonMap(workspace?['gitRuntime']);
    final dirtyKnown = git?['isDirty'] is bool || runtime['isDirty'] is bool || runtime.containsKey('isDirty');
    final aheadKnown = (git?.containsKey('aheadOfOrigin') ?? false) || runtime.containsKey('aheadOfOrigin');
    final isDirty = git?['isDirty'] is bool ? git!['isDirty'] as bool : runtime['isDirty'] as bool?;
    final ahead = git?['aheadOfOrigin'] is num
        ? (git!['aheadOfOrigin'] as num).toInt()
        : (runtime['aheadOfOrigin'] as num?)?.toInt();
    return (isDirty, ahead, dirtyKnown, aheadKnown);
  }

  bool get _canArchive {
    final workspace = _workspace;
    if (workspace == null) return false;
    if (workspace.text('workspaceKind') != 'worktree') return true;
    final (_, _, dirtyKnown, aheadKnown) = _archiveRisk;
    return dirtyKnown && aheadKnown;
  }

  bool get _isArchiving => _workspace?['archivingAt'] != null || _hiding;

  String get shipDefault {
    final root = state.status?.text('repoRoot') ?? '';
    if (root.isEmpty) return 'pr';
    final value = jsonMap(store.preferences['changesShipDefault'])[root];
    return value == 'merge' ? 'merge' : 'pr';
  }

  Future<void> _persistShipDefault(String next) async {
    final root = state.status?.text('repoRoot') ?? '';
    if (root.isEmpty) return;
    store.preferences['changesShipDefault'] = {
      ...jsonMap(store.preferences['changesShipDefault']),
      root: next,
    };
    try {
      await store.savePreferences();
    } catch (_) {
      // Ignore persistence failures; the default resets to "pr".
    }
  }

  bool get autoMergeActionsEnabled =>
      _client.supports('checkoutForgeSetAutoMerge') ||
      _client.supports('checkoutGithubSetAutoMerge');

  GitActions build(BuildContext context) {
    final input = gitActionsInputFrom(
      status: state.status,
      prPayload: state.prPayload,
      statusLoading: state.statusLoading,
      postShipArchiveSuggested: _actions.postShipArchiveSuggested(serverId, cwd),
      autoMergeActionsEnabled: autoMergeActionsEnabled,
      shipDefault: shipDefault,
      baseFallbackLabel: tr(context, 'workspace.git.diff.base'),
      runtime: (id, actionsDisabled) {
        if (id == 'archive-workspace') {
          return GitActionRuntime(
            disabled: !_canArchive || _isArchiving,
            status: _isArchiving ? GitActionStatus.pending : GitActionStatus.idle,
          );
        }
        final storeId = id == 'pr' ? 'create-pr' : id;
        final status = _actions.status(serverId, cwd, storeId);
        final hasPullRequest = jsonMap(state.prPayload?['status']).text('url').isNotEmpty;
        return GitActionRuntime(
          disabled: actionsDisabled || status == GitActionStatus.pending,
          status: id == 'pr' && hasPullRequest ? GitActionStatus.idle : status,
        );
      },
    );
    return buildGitActions(input);
  }

  String get _forge => normalizeForge(state.prPayload?.text('forge'));

  /// The action's label for its current status (original `getActionDisplayLabel`).
  String label(BuildContext context, GitAction action) {
    final hasPullRequest = jsonMap(state.prPayload?['status']).text('url').isNotEmpty;
    final baseRefLabel = formatBaseRefLabel(
      state.status?.text('baseRef'),
      tr(context, 'workspace.git.diff.base'),
    );
    final forge = _forge;
    String t(String key) => trForge(context, key, forge);
    final (String label, String pending, String success) = switch (action.id) {
      'commit' || 'pull' || 'push' || 'pull-and-push' || 'merge-branch' => () {
        final group = switch (action.id) {
          'pull-and-push' => 'pullAndPush',
          'merge-branch' => 'mergeBranch',
          _ => action.id,
        };
        return (
          t('workspace.git.actions.$group.label'),
          t('workspace.git.actions.$group.pending'),
          t('workspace.git.actions.$group.success'),
        );
      }(),
      'pr' when hasPullRequest => (
        t('workspace.git.actions.viewPr'),
        t('workspace.git.actions.viewPr'),
        t('workspace.git.actions.viewPr'),
      ),
      'pr' => (
        t('workspace.git.actions.createPr.label'),
        t('workspace.git.actions.createPr.pending'),
        t('workspace.git.actions.createPr.success'),
      ),
      'disable-pr-auto-merge' => (
        t('workspace.git.actions.autoMerge.enabled'),
        t('workspace.git.actions.autoMerge.disabling'),
        t('workspace.git.actions.autoMerge.disabled'),
      ),
      'merge-from-base' => (
        tr(context, 'workspace.git.actions.mergeFromBase.label', args: {'baseRef': baseRefLabel}),
        t('workspace.git.actions.mergeFromBase.pending'),
        t('workspace.git.actions.mergeFromBase.success'),
      ),
      'archive-workspace' => (
        t('workspace.git.actions.archive.label'),
        t('workspace.git.actions.archive.pending'),
        t('workspace.git.actions.archive.success'),
      ),
      final id when id.startsWith('merge-pr-') => (
        t('workspace.git.actions.mergePr.${id.substring('merge-pr-'.length)}'),
        t('workspace.git.actions.mergePr.pending'),
        t('workspace.git.actions.mergePr.success'),
      ),
      final id => (
        t('workspace.git.actions.autoMerge.enable${_capitalize(id.substring('enable-pr-auto-merge-'.length))}'),
        t('workspace.git.actions.autoMerge.enabling'),
        t('workspace.git.actions.autoMerge.enabled'),
      ),
    };
    return switch (action.status) {
      GitActionStatus.pending => pending,
      GitActionStatus.success => success,
      GitActionStatus.idle => label,
    };
  }

  static String _capitalize(String value) =>
      value.isEmpty ? value : '${value[0].toUpperCase()}${value.substring(1)}';

  static IconData icon(GitAction action) => switch (action.id) {
    'commit' => LucideIcons.gitCommitHorizontal300,
    'pull' => LucideIcons.download300,
    'push' => LucideIcons.upload300,
    'pull-and-push' => LucideIcons.arrowDownUp300,
    'merge-branch' => LucideIcons.gitMerge300,
    'merge-from-base' => LucideIcons.refreshCcw300,
    'archive-workspace' => LucideIcons.archive300,
    _ => LucideIcons.gitPullRequest300,
  };

  /// Original `useGitActionRunner`: disabled actions do nothing, unavailable
  /// ones explain themselves in an info toast, the rest run their handler.
  void run(BuildContext context, GitAction action) {
    if (action.disabled) return;
    final unavailable = action.unavailable;
    if (unavailable != null) {
      showToast(
        context,
        tr(context, unavailable.key, args: unavailable.args),
        icon: LucideIcons.info300,
        duration: const Duration(milliseconds: 3200),
      );
      return;
    }
    unawaited(_handle(context, action.id));
  }

  Future<void> _guard(
    BuildContext context,
    String actionId,
    Future<void> Function() body, {
    required String failureKey,
    String? successLabel,
    VoidCallback? onSuccess,
  }) async {
    final failure = tr(context, failureKey);
    try {
      await _actions.run(serverId: serverId, cwd: cwd, actionId: actionId, body: body);
      onSuccess?.call();
      if (successLabel != null && context.mounted) {
        showToast(context, successLabel, variant: ToastVariant.success);
      }
    } catch (error) {
      if (!context.mounted) return;
      final message = errorMessage(error);
      showToast(
        context,
        message.trim().isEmpty ? failure : message,
        variant: ToastVariant.error,
        duration: const Duration(milliseconds: 3200),
      );
    }
  }

  Future<void> _request(String type, [JsonMap fields = const {}]) =>
      gitRequest(_client, type, {'cwd': cwd, ...fields});

  Future<void> _handle(BuildContext context, String id) async {
    final forge = _forge;
    String t(String key) => trForge(context, key, forge);
    final baseRef = state.status?.text('baseRef') ?? '';
    switch (id) {
      case 'commit':
        await _guard(
          context,
          'commit',
          () => _request('checkout_commit_request', {'addAll': true}),
          failureKey: 'workspace.git.actions.toasts.failedCommit',
          successLabel: t('workspace.git.actions.commit.success'),
        );
      case 'pull':
        await _guard(
          context,
          'pull',
          () => _request('checkout_pull_request'),
          failureKey: 'workspace.git.actions.toasts.failedPull',
          successLabel: t('workspace.git.actions.pull.success'),
        );
      case 'push':
        await _guard(
          context,
          'push',
          () => _request('checkout_push_request'),
          failureKey: 'workspace.git.actions.toasts.failedPush',
          successLabel: t('workspace.git.actions.push.success'),
        );
      case 'pull-and-push':
        await _guard(
          context,
          'pull-and-push',
          () async {
            await _request('checkout_pull_request');
            await _request('checkout_push_request');
          },
          failureKey: 'workspace.git.actions.toasts.failedPullAndPush',
          successLabel: t('workspace.git.actions.pullAndPush.success'),
        );
      case 'pr':
        final url = jsonMap(state.prPayload?['status']).text('url');
        if (url.isNotEmpty) {
          final uri = Uri.tryParse(url);
          if (uri != null) await launchUrl(uri, mode: LaunchMode.externalApplication);
          return;
        }
        unawaited(_persistShipDefault('pr'));
        await _guard(
          context,
          'create-pr',
          () => _request('checkout_pr_create_request'),
          failureKey: 'workspace.git.actions.toasts.failedCreatePr',
          successLabel: t('workspace.git.actions.createPr.success'),
        );
      case 'disable-pr-auto-merge':
        await _guard(
          context,
          'disable-pr-auto-merge',
          () => _request(_autoMergeRpc(), {'enabled': false}),
          failureKey: 'workspace.git.actions.toasts.failedDisableAutoMerge',
          successLabel: t('workspace.git.actions.autoMerge.disabled'),
        );
      case 'merge-branch':
        if (baseRef.isEmpty) {
          showError(context, StateError(tr(context, 'workspace.git.actions.toasts.baseRefUnavailable')));
          return;
        }
        unawaited(_persistShipDefault('merge'));
        await _guard(
          context,
          'merge-branch',
          () => _request('checkout_merge_request', {
            'baseRef': baseRef,
            'strategy': 'merge',
            'requireCleanTarget': true,
          }),
          failureKey: 'workspace.git.actions.toasts.failedMerge',
          successLabel: t('workspace.git.actions.mergeBranch.success'),
          onSuccess: () => _actions.suggestPostShipArchive(serverId, cwd),
        );
      case 'merge-from-base':
        if (baseRef.isEmpty) {
          showError(context, StateError(tr(context, 'workspace.git.actions.toasts.baseRefUnavailable')));
          return;
        }
        await _guard(
          context,
          'merge-from-base',
          () => _request('checkout_merge_from_base_request', {
            'baseRef': baseRef,
            'requireCleanTarget': true,
          }),
          failureKey: 'workspace.git.actions.toasts.failedMergeFromBase',
          successLabel: t('workspace.git.actions.mergeFromBase.success'),
        );
      case 'archive-workspace':
        await _archive(context);
      default:
        if (id.startsWith('merge-pr-')) {
          final method = id.substring('merge-pr-'.length);
          unawaited(_persistShipDefault('pr'));
          await _guard(
            context,
            id,
            () => _request('checkout_pr_merge_request', {'mergeMethod': method}),
            failureKey: 'workspace.git.actions.toasts.failedMergePr',
            successLabel: t('workspace.git.actions.mergePr.success'),
            onSuccess: () => _actions.suggestPostShipArchive(serverId, cwd),
          );
        } else if (id.startsWith('enable-pr-auto-merge-')) {
          final method = id.substring('enable-pr-auto-merge-'.length);
          unawaited(_persistShipDefault('pr'));
          String rpc;
          try {
            rpc = _autoMergeRpc();
          } catch (error) {
            showError(context, error);
            return;
          }
          await _guard(
            context,
            id,
            () => _request(rpc, {'enabled': true, 'mergeMethod': method}),
            failureKey: 'workspace.git.actions.toasts.failedEnableAutoMerge',
            successLabel: t('workspace.git.actions.autoMerge.enabled'),
          );
        }
    }
  }

  /// Original `resolveAutoMergeActionsRpc`: the forge RPC, the legacy GitHub RPC
  /// for daemons predating it, or an update prompt.
  String _autoMergeRpc() {
    if (_client.supports('checkoutForgeSetAutoMerge')) {
      return 'checkout.forge.set_auto_merge.request';
    }
    if (_client.supports('checkoutGithubSetAutoMerge')) {
      return 'checkout.github.set_auto_merge.request';
    }
    throw StateError('Update the host to use auto-merge actions.');
  }

  /// Original `useWorkspaceArchive`: worktrees confirm their risks first; the
  /// workspace's tabs close and the host list refreshes once archived.
  Future<void> _archive(BuildContext context) async {
    final workspace = _workspace;
    if (workspace == null) return;
    if (workspace.text('workspaceKind') == 'worktree') {
      final (isDirty, ahead, _, _) = _archiveRisk;
      final confirmed = await confirmRiskyWorktreeArchive(
        context,
        workspaceName: workspace.text(
          'name',
          state.status?.text('currentBranch') ?? workspace.text('id'),
        ),
        isDirty: isDirty,
        aheadOfOrigin: ahead,
        diffStat: workspace['diffStat'] is Map ? workspace.object('diffStat') : null,
      );
      if (!confirmed || !context.mounted) return;
    }
    if (_client.status != ConnectionStatus.connected) {
      showError(context, StateError(tr(context, 'sidebar.workspace.toasts.hostDisconnected')));
      return;
    }
    final failure = tr(context, 'sidebar.workspace.toasts.archiveFailed');
    _hiding = true;
    _actions.touch();
    try {
      final id = workspace.text('id');
      final response = await checkoutPayload(_client, 'archive_workspace_request', {'workspaceId': id});
      if (response['error'] != null || response['success'] == false || response['accepted'] == false) {
        throw StateError(checkoutPayloadError(response) ?? failure);
      }
      for (final tab in List<PanelTarget>.of(store.tabs)) {
        if (tab.serverId == serverId && tab.workspaceId == id) await store.closeTab(tab.key);
      }
      await store.refreshHost(serverId);
    } catch (error) {
      if (context.mounted) {
        final message = errorMessage(error);
        showError(context, StateError(message.trim().isEmpty ? failure : message));
      }
    } finally {
      _hiding = false;
      _actions.touch();
    }
  }
}

// ---------------------------------------------------------------------------
// Widgets.
// ---------------------------------------------------------------------------

/// Builds the menu entries of [actions] for a `GitActionsSplitButton` menu:
/// separators before group starters (never first), unavailable actions muted. As in
/// the original `MenuItem`, a pending or succeeded row is disabled too.
List<PaseoMenuEntry<String>> gitActionMenuEntries(
  BuildContext context,
  CheckoutGitActions binding,
  List<GitAction> actions,
) => [
  for (final (index, action) in actions.indexed) ...[
    if (action.startsGroup && index > 0) const PaseoMenuEntry<String>.separator(),
    PaseoMenuEntry.item(
      value: action.id,
      label: binding.label(context, action),
      icon: CheckoutGitActions.icon(action),
      enabled: !action.disabled && action.status == GitActionStatus.idle,
      muted: action.unavailable != null,
    ),
  ],
];

/// Original `WorkspaceActions` (git/workspace-actions.tsx) rendering the
/// `GitActionsSplitButton`: the primary action (spinner while pending) and a
/// caret menu of the secondary actions; nothing outside a git checkout.
class WorkspaceGitActionsButton extends StatefulWidget {
  const WorkspaceGitActionsButton({
    super.key,
    required this.serverId,
    required this.workspaceId,
    required this.cwd,
  });
  final String serverId, workspaceId, cwd;
  @override
  State<WorkspaceGitActionsButton> createState() => _WorkspaceGitActionsButtonState();
}

class _WorkspaceGitActionsButtonState extends State<WorkspaceGitActionsButton> {
  CheckoutGitState? _state;
  CheckoutGitActions? _binding;
  bool _hovered = false, _caretHovered = false, _open = false;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    _bind();
  }

  @override
  void didUpdateWidget(WorkspaceGitActionsButton old) {
    super.didUpdateWidget(old);
    if (old.serverId != widget.serverId || old.cwd != widget.cwd || old.workspaceId != widget.workspaceId) {
      _unbind();
      _bind();
    }
  }

  void _bind() {
    if (_state != null) return;
    final store = PaseoScope.of(context);
    final state = CheckoutGitState.acquire(store.clientFor(widget.serverId), widget.serverId, widget.cwd)
      ..addListener(_changed);
    _state = state;
    _binding = CheckoutGitActions(
      store: store,
      serverId: widget.serverId,
      workspaceId: widget.workspaceId,
      cwd: widget.cwd,
      state: state,
    );
    CheckoutGitActionsStore.instance.addListener(_changed);
  }

  void _unbind() {
    _state?.removeListener(_changed);
    _state?.release();
    _state = null;
    _binding = null;
    CheckoutGitActionsStore.instance.removeListener(_changed);
  }

  void _changed() {
    if (mounted) setState(() {});
  }

  @override
  void dispose() {
    _unbind();
    super.dispose();
  }

  Future<void> _menu(BuildContext anchor, GitActions actions) async {
    final binding = _binding!;
    setState(() => _open = true);
    try {
      final id = await showPaseoMenu<String>(
        context,
        anchor: paseoAnchorRect(anchor),
        alignEnd: true,
        title: tr(context, 'workspace.git.actions.moreOptions'),
        entries: gitActionMenuEntries(context, binding, actions.secondary),
      );
      final action = actions.secondary.where((a) => a.id == id).firstOrNull;
      if (action != null && mounted) binding.run(context, action);
    } finally {
      if (mounted) setState(() => _open = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final binding = _binding;
    if (binding == null) return const SizedBox.shrink();
    final actions = binding.build(context);
    final primary = actions.primary;
    if (primary == null) return const SizedBox.shrink();
    final colors = PaseoColors.of(context);
    final theme = Theme.of(context);
    final compact = isCompactLayout(context);
    const height = 28.0;
    final label = binding.label(context, primary);
    return Container(
      height: height,
      clipBehavior: Clip.antiAlias,
      decoration: BoxDecoration(
        border: Border.all(color: colors.borderAccent),
        borderRadius: BorderRadius.circular(6),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Semantics(
            button: true,
            label: label,
            enabled: !primary.disabled,
            excludeSemantics: true,
            child: MouseRegion(
              cursor: primary.disabled ? MouseCursor.defer : SystemMouseCursors.click,
              onEnter: (_) => setState(() => _hovered = true),
              onExit: (_) => setState(() => _hovered = false),
              child: GestureDetector(
                behavior: HitTestBehavior.opaque,
                onTap: primary.disabled ? null : () => binding.run(context, primary),
                child: Opacity(
                  opacity: primary.disabled ? .6 : 1,
                  child: Container(
                    color: _hovered && !primary.disabled ? colors.surface2 : null,
                    padding: EdgeInsets.symmetric(horizontal: compact ? 12 : 8),
                    alignment: Alignment.center,
                    child: primary.status == GitActionStatus.pending
                        ? SizedBox.square(
                            dimension: 14,
                            child: CircularProgressIndicator(strokeWidth: 1.5, color: theme.colorScheme.onSurface),
                          )
                        : Row(
                            mainAxisSize: MainAxisSize.min,
                            spacing: compact ? 8 : 4,
                            children: [
                              Icon(CheckoutGitActions.icon(primary), size: 16, color: colors.muted),
                              Text(label, style: theme.textTheme.bodyMedium),
                            ],
                          ),
                  ),
                ),
              ),
            ),
          ),
          if (actions.secondary.isNotEmpty)
            Builder(
              builder: (anchor) => Semantics(
                button: true,
                label: tr(context, 'workspace.git.actions.moreOptions'),
                excludeSemantics: true,
                child: MouseRegion(
                  cursor: SystemMouseCursors.click,
                  onEnter: (_) => setState(() => _caretHovered = true),
                  onExit: (_) => setState(() => _caretHovered = false),
                  child: GestureDetector(
                    behavior: HitTestBehavior.opaque,
                    onTap: () => _menu(anchor, actions),
                    child: Container(
                      width: height,
                      decoration: BoxDecoration(
                        color: _caretHovered || _open ? colors.surface2 : null,
                        border: Border(left: BorderSide(color: colors.borderAccent)),
                      ),
                      alignment: Alignment.center,
                      child: Icon(LucideIcons.chevronDown300, size: 16, color: colors.extraMuted),
                    ),
                  ),
                ),
              ),
            ),
        ],
      ),
    );
  }
}
