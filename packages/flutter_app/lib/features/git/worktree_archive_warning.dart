import 'package:flutter/widgets.dart';

import '../../core/json.dart';
import '../../ui/common.dart';
import '../../ui/localization.dart';

/// Original `buildWorktreeArchiveRiskReasons` (git/worktree-archive-warning.ts):
/// uncommitted changes (from `isDirty`, or the diff stat when dirtiness is unknown)
/// with the added/deleted line counts, then the unpushed commit count. Each
/// reason is produced through [label] so tests can pin the copy keys.
List<String> buildWorktreeArchiveRiskReasons({
  bool? isDirty,
  int? aheadOfOrigin,
  JsonMap? diffStat,
  required String Function(String key, Map<String, Object?> args) label,
}) {
  final reasons = <String>[];
  final additions = diffStat?.integer('additions') ?? 0;
  final deletions = diffStat?.integer('deletions') ?? 0;
  final hasDiffStatChanges = diffStat != null && (additions > 0 || deletions > 0);
  final hasUncommittedChanges = isDirty == true || (isDirty == null && hasDiffStatChanges);
  if (hasUncommittedChanges) {
    final parts = <String>[
      if (additions > 0)
        label(
          additions == 1
              ? 'workspace.git.actions.archiveWarning.addedLine'
              : 'workspace.git.actions.archiveWarning.addedLines',
          {'count': additions},
        ),
      if (deletions > 0)
        label(
          deletions == 1
              ? 'workspace.git.actions.archiveWarning.deletedLine'
              : 'workspace.git.actions.archiveWarning.deletedLines',
          {'count': deletions},
        ),
    ];
    reasons.add(
      parts.isEmpty
          ? label('workspace.git.actions.archiveWarning.uncommittedChanges', const {})
          : label('workspace.git.actions.archiveWarning.uncommittedChangesWithDiff', {
              'diffStat': parts.join(', '),
            }),
    );
  }
  final ahead = aheadOfOrigin ?? 0;
  if (ahead > 0) {
    reasons.add(
      label(
        ahead == 1
            ? 'workspace.git.actions.archiveWarning.unpushedCommit'
            : 'workspace.git.actions.archiveWarning.unpushedCommits',
        {'count': ahead},
      ),
    );
  }
  return reasons;
}

/// Original `confirmRiskyWorktreeArchive`: no dialog (true) without a risk;
/// otherwise a destructive confirmation titled with the workspace name listing
/// every reason on its own line.
Future<bool> confirmRiskyWorktreeArchive(
  BuildContext context, {
  required String workspaceName,
  bool? isDirty,
  int? aheadOfOrigin,
  JsonMap? diffStat,
}) async {
  final reasons = buildWorktreeArchiveRiskReasons(
    isDirty: isDirty,
    aheadOfOrigin: aheadOfOrigin,
    diffStat: diffStat,
    label: (key, args) => tr(context, key, args: args),
  );
  if (reasons.isEmpty) return true;
  return confirmAction(
    context,
    title: tr(
      context,
      'workspace.git.actions.archiveWarning.title',
      args: {'workspaceName': workspaceName},
    ),
    message: reasons.join('\n'),
    confirmLabel: tr(context, 'workspace.git.actions.archiveWarning.confirm'),
    destructive: true,
  );
}
