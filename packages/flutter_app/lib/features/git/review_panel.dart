import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:shared_preferences/shared_preferences.dart';
import 'package:uuid/uuid.dart';

import '../../core/json.dart';
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';

/// Original `ReviewCommentSheet`: an adaptive sheet titled "Review comment" with
/// the `path · ±line` context in muted small text, a multiline md field and a
/// secondary Cancel / primary Save footer (Save disabled while empty).
Future<String?> promptReviewComment(
  BuildContext context, {
  required String path,
  required String side,
  required int? line,
  String initialValue = '',
}) => showAdaptiveSheet<String>(
  context: context,
  builder: (_) => _ReviewCommentSheet(
    subtitle: '$path · ${side == 'old' ? '−' : '+'}${line ?? ''}',
    initialValue: initialValue,
  ),
);

class _ReviewCommentSheet extends StatefulWidget {
  const _ReviewCommentSheet({required this.subtitle, required this.initialValue});
  final String subtitle, initialValue;
  @override
  State<_ReviewCommentSheet> createState() => _ReviewCommentSheetState();
}

class _ReviewCommentSheetState extends State<_ReviewCommentSheet> {
  late final _controller = TextEditingController(text: widget.initialValue)
    ..addListener(() => setState(() {}));

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final type = PaseoTypography.of(context);
    final canSave = _controller.text.trim().isNotEmpty;
    return SheetContents(
      title: tr(context, 'review.comment.label'),
      subtitle: Text(widget.subtitle, maxLines: 1, overflow: TextOverflow.ellipsis),
      actions: [
        PaseoButton(
          label: tr(context, 'review.comment.cancel'),
          semanticLabel: tr(context, 'review.comment.cancelAccessibility'),
          onPressed: () => Navigator.pop(context),
        ),
        PaseoButton(
          label: tr(context, 'review.comment.save'),
          semanticLabel: tr(context, 'review.comment.saveAccessibility'),
          variant: PaseoButtonVariant.primary,
          onPressed: canSave ? () => Navigator.pop(context, _controller.text) : null,
        ),
      ],
      child: TextField(
        controller: _controller,
        autofocus: true,
        minLines: 5,
        maxLines: 12,
        textAlignVertical: TextAlignVertical.top,
        style: Theme.of(context).textTheme.bodyMedium!.copyWith(
          fontSize: type.uiSize(14),
          height: 1.4,
        ),
        decoration: paseoInputDecoration(
          context,
          hintText: tr(context, 'review.comment.placeholder'),
          multiline: true,
        ),
      ),
    );
  }
}

/// Original `buildReviewDraftKey` (review/store.ts): one private draft per host,
/// workspace (or normalized cwd), comparison, base ref and whitespace mode.
String buildReviewDraftKey({
  required String serverId,
  required String workspaceId,
  required String cwd,
  required String mode,
  required String baseRef,
  required bool ignoreWhitespace,
}) {
  String part(String value) => Uri.encodeComponent(value.trim());
  final trimmedCwd = cwd.trim();
  final normalizedCwd = trimmedCwd == '/' ? trimmedCwd : trimmedCwd.replaceFirst(RegExp(r'/+$'), '');
  final workspace = workspaceId.trim();
  return [
    'review',
    'server=${part(serverId)}',
    workspace.isNotEmpty ? 'workspace=${part(workspace)}' : 'cwd=${part(normalizedCwd)}',
    'mode=$mode',
    'base=${part(baseRef)}',
    'ignoreWhitespace=$ignoreWhitespace',
  ].join(':');
}

/// Original `useReviewDraftStore` for one draft key: every Changes view of the
/// same scope shares one live draft, persisted on the device.
class ReviewDraft extends ChangeNotifier {
  ReviewDraft._(this.key);

  static final Map<String, ReviewDraft> _shared = {};
  static final Map<String, int> _refs = {};

  static ReviewDraft acquire(String key) {
    _refs[key] = (_refs[key] ?? 0) + 1;
    return _shared.putIfAbsent(key, () => ReviewDraft._(key)..load());
  }

  void release() {
    final refs = (_refs[key] ?? 1) - 1;
    if (refs > 0) {
      _refs[key] = refs;
      return;
    }
    _refs.remove(key);
    _shared.remove(key);
    // A send in flight still owns the workflow; it finishes against this draft.
    _released = true;
  }

  final String key;
  List<JsonMap> comments = [];
  Future<void> _writes = Future.value();
  bool _released = false;
  bool _loaded = false;

  /// Original `FeedbackPhase`: the sending comment count, or the failure.
  int? sendingCount;
  String? failedMessage;

  String get _storageKey => 'paseo.review.$key';

  Future<void> load() async {
    final prefs = await SharedPreferences.getInstance();
    final raw = prefs.getString(_storageKey);
    if (raw != null && !_loaded) {
      try {
        comments = [
          ...jsonList(jsonDecode(raw)).map(jsonMap).where(
            (c) =>
                c['id'] is String &&
                c['filePath'] is String &&
                (c['side'] == 'old' || c['side'] == 'new') &&
                c['lineNumber'] is int &&
                (c['lineNumber'] as int) > 0 &&
                c['body'] is String,
          ),
          ...comments,
        ];
      } on FormatException {
        comments = [...comments];
      }
    }
    _loaded = true;
    notifyListeners();
  }

  Future<void> _save() {
    final encoded = jsonEncode(comments);
    final pending = _writes.catchError((Object _) {}).then((_) async {
      final prefs = await SharedPreferences.getInstance();
      if (comments.isEmpty) {
        await prefs.remove(_storageKey);
      } else if (!await prefs.setString(_storageKey, encoded)) {
        throw StateError('Unable to save review draft.');
      }
    });
    _writes = pending;
    notifyListeners();
    return pending;
  }

  /// Original `saveReviewDraftComment`: the trimmed body updates the edited
  /// comment while it still exists, otherwise it becomes a new comment.
  Future<void> saveComment({
    String? id,
    required String filePath,
    required String side,
    required int lineNumber,
    required String body,
  }) {
    final trimmed = body.trim();
    if (trimmed.isEmpty) return Future.value();
    final now = DateTime.now().toUtc().toIso8601String();
    final index = id == null ? -1 : comments.indexWhere((c) => c['id'] == id);
    if (index >= 0) {
      comments = [
        for (final (i, comment) in comments.indexed)
          i == index ? {...comment, 'body': trimmed, 'updatedAt': now} : comment,
      ];
    } else {
      comments = [
        ...comments,
        {
          'id': const Uuid().v4(),
          'filePath': filePath,
          'side': side,
          'lineNumber': lineNumber,
          'body': trimmed,
          'createdAt': now,
          'updatedAt': now,
        },
      ];
    }
    return _save();
  }

  Future<void> delete(JsonMap comment) {
    comments = comments.where((c) => c['id'] != comment['id']).toList();
    return _save();
  }

  /// Original `clearSentReviewDraftComments`: acknowledgement removes only the
  /// versions included in that submission; a comment edited meanwhile stays.
  Future<void> clearSent(List<JsonMap> sent) {
    final byId = {for (final comment in sent) comment['id']: comment};
    final remaining = comments.where((comment) {
      final match = byId[comment['id']];
      return match == null ||
          match['updatedAt'] != comment['updatedAt'] ||
          match['body'] != comment['body'];
    }).toList();
    if (remaining.length == comments.length) return Future.value();
    comments = remaining;
    return _save();
  }

  void setPhase({int? sending, String? failed}) {
    sendingCount = sending;
    failedMessage = failed;
    notifyListeners();
  }

  @override
  void notifyListeners() {
    if (!_released || hasListeners) super.notifyListeners();
  }
}

/// One review attachment ready to send (original `ReviewComposerAttachment`).
class ReviewAttachmentSnapshot {
  const ReviewAttachmentSnapshot(this.commentCount, this.attachment);
  final int commentCount;
  final JsonMap attachment;
}

/// Original `buildReviewAttachmentSnapshot`: each comment resolves against the
/// current diff — its file, then the hunk line whose number on the comment's
/// side matches — and carries that hunk's header, the target line and up to
/// three lines either side. Comments whose line no longer exists are dropped;
/// null when none remain. [diffFile] returns a file's current patch.
ReviewAttachmentSnapshot? buildReviewAttachmentSnapshot({
  required String cwd,
  required String mode,
  required String? baseRef,
  required List<JsonMap> comments,
  required JsonMap? Function(String path) diffFile,
}) {
  JsonMap? contextLine(JsonMap line, int? old, int? next) => line['type'] == 'header'
      ? null
      : {
          'oldLineNumber': old,
          'newLineNumber': next,
          'type': line['type'],
          'content': line['content'],
        };
  final resolved = <JsonMap>[];
  for (final comment in comments) {
    final file = diffFile(comment.text('filePath'));
    if (file == null) continue;
    JsonMap? context;
    for (final hunk in file.array('hunks').map(jsonMap)) {
      final lines = hunk.array('lines').map(jsonMap).toList();
      final header = lines.where((l) => l['type'] == 'header').firstOrNull?.text('content') ?? '@@';
      var old = hunk.integer('oldStart'), next = hunk.integer('newStart');
      final numbered = <(JsonMap, int?, int?)>[];
      for (final line in lines) {
        switch (line['type']) {
          case 'remove':
            numbered.add((line, old++, null));
          case 'add':
            numbered.add((line, null, next++));
          case 'context':
            numbered.add((line, old++, next++));
          default:
            numbered.add((line, null, null));
        }
      }
      final index = numbered.indexWhere(
        (entry) => (comment['side'] == 'old' ? entry.$2 : entry.$3) == comment['lineNumber'],
      );
      if (index < 0) continue;
      final target = numbered[index];
      final targetLine = contextLine(target.$1, target.$2, target.$3);
      if (targetLine == null) break;
      context = {
        'hunkHeader': header,
        'targetLine': targetLine,
        'lines': [
          for (final entry in numbered.sublist(
            index - 3 < 0 ? 0 : index - 3,
            index + 4 > numbered.length ? numbered.length : index + 4,
          ))
            ?contextLine(entry.$1, entry.$2, entry.$3),
        ],
      };
      break;
    }
    if (context == null) continue;
    resolved.add({
      'filePath': comment['filePath'],
      'side': comment['side'],
      'lineNumber': comment['lineNumber'],
      'body': comment['body'],
      'context': context,
    });
  }
  if (resolved.isEmpty) return null;
  final base = baseRef?.trim() ?? '';
  return ReviewAttachmentSnapshot(resolved.length, {
    'type': 'review',
    'mimeType': 'application/paseo-review',
    'cwd': cwd,
    'mode': mode,
    'baseRef': base.isEmpty ? null : base,
    'comments': resolved,
  });
}

/// Original `FLOATING_ACTION_BUTTON_CLEARANCE`: the 56px action plus its 16px inset.
const gitFloatingActionClearance = 56.0 + 16.0;

/// Original `FloatingActionButton`: a 56px surface3 circle (md shadow) 16px from the
/// trailing and bottom edges with a 20px foreground glyph; a labelled action grows
/// to a 16px-padded pill with an 8px gap. Interaction highlight while pressed, 50%
/// opacity when disabled.
class GitFloatingActionButton extends StatefulWidget {
  const GitFloatingActionButton({
    super.key,
    required this.icon,
    required this.semanticLabel,
    required this.onPressed,
    this.label,
    this.bottomInset = 0,
  });
  final IconData icon;
  final String semanticLabel;
  final String? label;
  final VoidCallback? onPressed;
  final double bottomInset;

  @override
  State<GitFloatingActionButton> createState() => _GitFloatingActionButtonState();
}

class _GitFloatingActionButtonState extends State<GitFloatingActionButton> {
  bool _pressed = false;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final enabled = widget.onPressed != null;
    final dark = theme.brightness == Brightness.dark;
    return Positioned(
      right: 16,
      bottom: 16 + widget.bottomInset,
      child: Semantics(
        button: true,
        enabled: enabled,
        label: widget.semanticLabel,
        excludeSemantics: true,
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: enabled ? (_) => setState(() => _pressed = true) : null,
          onTapCancel: () => setState(() => _pressed = false),
          onTapUp: (_) => setState(() => _pressed = false),
          onTap: widget.onPressed,
          child: Opacity(
            opacity: enabled ? 1 : .5,
            child: Container(
              height: 56,
              constraints: const BoxConstraints(minWidth: 56),
              padding: widget.label == null ? null : const EdgeInsets.symmetric(horizontal: 16),
              alignment: Alignment.center,
              decoration: BoxDecoration(
                color: _pressed ? Color.alphaBlend(paseoInteractionHighlight(context), colors.surface3) : colors.surface3,
                borderRadius: BorderRadius.circular(28),
                boxShadow: [
                  BoxShadow(
                    color: dark ? const Color(0x33000000) : const Color(0x0a000000),
                    offset: const Offset(0, 4),
                    blurRadius: dark ? 8 : 16,
                  ),
                ],
              ),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                spacing: 8,
                children: [
                  Icon(widget.icon, size: 20, color: theme.colorScheme.onSurface),
                  if (widget.label != null)
                    Text(
                      widget.label!,
                      style: theme.textTheme.bodyMedium!.copyWith(
                        color: theme.colorScheme.onSurface,
                      ),
                    ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// An agent tab of the workspace that can receive review feedback.
class FeedbackRecipient {
  const FeedbackRecipient(this.agentId, this.label);
  final String agentId, label;
}

/// Original `deriveState` disabled reason (review/feedback/index.ts).
String? reviewFeedbackDisabledReason({
  required int recipientCount,
  required bool connected,
  required bool hasAttachment,
}) {
  if (recipientCount == 0) return 'no-agents';
  if (!connected) return 'disconnected';
  if (!hasAttachment) return 'no-context';
  return null;
}

/// Original `ReviewFeedbackAction` + `createReviewFeedback`: once the draft holds
/// comments, a labelled floating "Send feedback (N)" action on compact layouts
/// (stacked above [bottomOffset]) and a centred primary small button elsewhere.
/// One open agent tab receives the feedback directly; several open a chooser.
/// Without a recipient, a connection or a still-matching diff the action is
/// disabled and a notice explains why; a failed send shows its error. Place it
/// in the Changes body's [Stack].
class ReviewFeedbackAction extends StatefulWidget {
  const ReviewFeedbackAction({
    super.key,
    required this.draft,
    required this.recipients,
    required this.connected,
    required this.attachment,
    required this.onSend,
    this.bottomOffset = 0,
  });
  final ReviewDraft draft;
  final List<FeedbackRecipient> recipients;
  final bool connected;
  final ReviewAttachmentSnapshot? attachment;
  final Future<void> Function(FeedbackRecipient recipient, JsonMap attachment) onSend;
  final double bottomOffset;

  @override
  State<ReviewFeedbackAction> createState() => _ReviewFeedbackActionState();
}

class _ReviewFeedbackActionState extends State<ReviewFeedbackAction> {
  String? get _disabledReason => reviewFeedbackDisabledReason(
    recipientCount: widget.recipients.length,
    connected: widget.connected,
    hasAttachment: widget.attachment != null,
  );

  bool get _canSend =>
      widget.draft.comments.isNotEmpty &&
      _disabledReason == null &&
      widget.draft.sendingCount == null;

  Future<void> _press(BuildContext anchor) async {
    if (!_canSend) return;
    if (widget.recipients.length == 1) {
      await _send(widget.recipients.single);
      return;
    }
    final muted = PaseoColors.of(context).muted;
    final chosen = await showPaseoMenu<String>(
      anchor,
      anchor: paseoAnchorRect(anchor),
      title: tr(context, 'review.feedback.chooseAgent'),
      entries: [
        for (final recipient in widget.recipients)
          PaseoMenuEntry.item(
            value: recipient.agentId,
            label: recipient.label,
            leading: Icon(LucideIcons.bot300, size: 16, color: muted),
          ),
      ],
    );
    final recipient = widget.recipients.where((r) => r.agentId == chosen).firstOrNull;
    if (recipient != null && mounted) await _send(recipient);
  }

  Future<void> _send(FeedbackRecipient recipient) async {
    final draft = widget.draft;
    final snapshot = widget.attachment;
    if (!_canSend || snapshot == null) return;
    final sentComments = draft.comments.where((comment) {
      return snapshot.attachment.array('comments').map(jsonMap).any(
        (sent) =>
            sent['filePath'] == comment['filePath'] &&
            sent['side'] == comment['side'] &&
            sent['lineNumber'] == comment['lineNumber'] &&
            sent['body'] == comment['body'],
      );
    }).toList();
    final failedLabel = tr(context, 'review.feedback.failed');
    final sentLabel = tr(context, 'review.feedback.sent', args: {'recipient': recipient.label});
    draft.setPhase(sending: snapshot.commentCount);
    try {
      await widget.onSend(recipient, snapshot.attachment);
      if (mounted) showToast(context, sentLabel, variant: ToastVariant.success);
      await draft.clearSent(sentComments);
      draft.setPhase();
    } catch (error) {
      final message = errorMessage(error);
      draft.setPhase(failed: message.trim().isEmpty ? failedLabel : message);
    }
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: widget.draft,
    builder: (context, _) {
      final draft = widget.draft;
      final count = draft.comments.length;
      final theme = Theme.of(context);
      final colors = PaseoColors.of(context);
      final compact = isCompactLayout(context);
      final bottom = MediaQuery.paddingOf(context).bottom + widget.bottomOffset;
      final sending = draft.sendingCount != null;
      final label = tr(
        context,
        sending ? 'review.feedback.sending' : 'review.feedback.send',
        args: {'count': '${draft.sendingCount ?? widget.attachment?.commentCount ?? 0}'},
      );
      final reason = _disabledReason;
      final failed = draft.failedMessage;
      final notice = failed ?? (reason != null && count > 0 ? tr(context, 'review.feedback.$reason') : null);
      final enabled = _canSend;
      return Positioned.fill(
        child: Stack(
          children: [
            if (count > 0)
              Builder(
                builder: (anchor) => compact
                    ? GitFloatingActionButton(
                        icon: LucideIcons.send300,
                        label: label,
                        semanticLabel: label,
                        bottomInset: bottom,
                        onPressed: enabled ? () => _press(anchor) : null,
                      )
                    : Positioned(
                        left: 0,
                        right: 0,
                        bottom: bottom + 16,
                        child: Center(
                          child: PaseoButton(
                            label: label,
                            icon: LucideIcons.send300,
                            variant: PaseoButtonVariant.primary,
                            size: PaseoControlSize.sm,
                            semanticLabel: label,
                            onPressed: enabled ? () => _press(anchor) : null,
                          ),
                        ),
                      ),
              ),
            if (notice != null)
              Positioned(
                left: 16,
                right: 16,
                bottom: bottom + (compact ? 88 : 32 + 16 + 8),
                child: Semantics(
                  liveRegion: true,
                  child: Align(
                    alignment: compact ? Alignment.centerRight : Alignment.center,
                    child: Container(
                      padding: const EdgeInsets.all(8),
                      decoration: BoxDecoration(
                        color: colors.surface1,
                        borderRadius: BorderRadius.circular(6),
                      ),
                      child: Text(
                        notice,
                        style: theme.textTheme.bodySmall!.copyWith(
                          color: failed != null ? colors.danger : theme.colorScheme.onSurface,
                        ),
                      ),
                    ),
                  ),
                ),
              ),
          ],
        ),
      );
    },
  );
}
