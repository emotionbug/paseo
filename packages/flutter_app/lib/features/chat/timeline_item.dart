import 'dart:math' as math;

import 'package:flutter/foundation.dart' show defaultTargetPlatform;
import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/json.dart';
import '../../core/scope.dart';
import '../../core/target.dart';
import '../../ui/common.dart';
import '../../ui/localization.dart';
import '../../ui/indicator_clock.dart';
import '../../ui/theme.dart';
import 'assistant_file_links.dart';
import 'attachment_pill.dart';
import 'chat_controller.dart';
import 'chat_markdown.dart';
import 'markdown_image.dart';
import 'stream/message_actions.dart';
import 'stream/text_reveal.dart';
import 'stream/tool_call_display.dart';
import 'tool_call_details.dart';

export 'stream/tool_call_display.dart' show ToolGlyph;

/// Original Android/iOS renderer branch (`isNative`); desktop follows web.
bool get _isNative =>
    defaultTargetPlatform == TargetPlatform.android || defaultTargetPlatform == TargetPlatform.iOS;

/// assistant-message-render-limit.web.ts: web caps oversized messages before
/// markdown rendering; the native renderer keeps complete messages.
const assistantMessageRenderCharacterLimit = 32000;

/// One original agent-stream row (agent-stream/view.tsx renderStreamItemContent):
/// UserMessage, AssistantMessage, ThoughtSlot, ToolCall / SpeakMessage /
/// PlanCard, OverviewToolCallGroupView, TodoListCard, Notification and
/// CompactionMarker, wrapped in the StreamItemWrapper spacing.
class TimelineItem extends StatelessWidget {
  const TimelineItem({
    super.key,
    required this.entry,
    required this.target,
    this.onRewind,
    this.capabilities,
    this.expandedTools = false,
    this.previousTasks,
    this.showTaskDetails = false,
    this.gapBelow = 0,
    this.lastInToolSequence = true,
    this.footerBelow = false,
    this.streaming = false,
    this.live = false,
    this.renderFullContent = false,
    this.find,
    this.toolGroup,
    this.groupSource,
    this.resolveGroup,
    this.embedded = false,
    this.maxDetailHeight = 400,
  });
  final JsonMap entry;
  final PanelTarget target;

  /// rewind-menu.tsx: called with `conversation`, `files` or `both`.
  final Future<void> Function(JsonMap entry, String mode)? onRewind;

  /// Agent capability flags (supportsRewindConversation/Files/Both).
  final JsonMap? capabilities;
  final bool expandedTools;
  final JsonMap? previousTasks;
  final bool showTaskDetails;

  /// agent-stream/layout.ts: StreamItemWrapper marginBottom
  /// (spacing.ts getGapBetweenStreamItems; 0 above a turn footer).
  final double gapBelow;

  /// Inside an overview group: the call ends its tool sequence (16 below
  /// instead of 4).
  final bool lastInToolSequence;

  /// Assistant "compactBottom" spacing: a turn footer follows directly.
  final bool footerBelow;

  /// A reasoning row still receiving text (stream.ts thought status "loading").
  final bool streaming;

  /// layout.ts phase "streaming": the row belongs to the active turn's live
  /// head, so text growth is paced (use-revealed-text.ts).
  final bool live;

  /// AssistantMessage renderFullContent (ChatFindExpansion): skip the desktop
  /// render cap for this message.
  final bool renderFullContent;

  /// Chat find's current match inside this row (the selected find row only).
  final ChatFindTarget? find;

  /// The user bubble text with chat find's current occurrence highlighted.
  TextSpan _userText(BuildContext context, String text) {
    final target = find;
    final pattern = target == null ? null : chatFindPattern(target.query);
    final match = pattern?.allMatches(text).elementAtOrNull(target!.occurrence);
    if (match == null) return TextSpan(text: text);
    return highlightSpanRange(TextSpan(text: text), match.start, match.end, chatFindHighlightStyle(context))
        as TextSpan;
  }

  /// Overview detail level: the tool-call run this row summarizes.
  final List<JsonMap>? toolGroup;

  /// Live source of [toolGroup] for the open mobile group sheet: notifies on
  /// timeline changes; [resolveGroup] re-reads the run by its group id.
  final Listenable? groupSource;
  final List<JsonMap>? Function(String groupId)? resolveGroup;

  /// Rendered inside an overview group: no stream wrapper, only badge spacing.
  final bool embedded;

  /// ToolCall maxDetailHeight (400; 200 inside overview groups).
  final double maxDetailHeight;

  static bool _nonEmptyError(Object? error) =>
      error != null && error != false && error != '' && error != 0;

  @override
  Widget build(BuildContext context) {
    final item = jsonMap(entry['item']);
    final type = stringValue(item['type']);
    final text = stringValue(item['text']);
    // The composer task pill's sheet shows the bare task list.
    if (showTaskDetails && type == 'todo') return _tasks(context, item);
    Widget content;
    var badge = false;
    // (top, bottom) outer spacing of the row. agent-stream/view.tsx wraps
    // the stream in MessageOuterSpacingProvider disableOuterSpacing, so the
    // user bubble, speak/notification containerSpacing and ExpandableBadge
    // margins collapse there; only the StreamItemWrapper gap (spacing.ts)
    // separates them. Rows inside an overview group keep the badge spacing.
    var spacing = (0.0, 0.0);
    final badgeGap = embedded ? (lastInToolSequence ? 16.0 : 4.0) : 0.0;
    switch (type) {
      case 'user_message':
        content = _userMessage(context, item, text);
      case 'assistant_message':
        // AssistantMessage: cap (web only), then pace the capped text; the
        // capped notice appears once the message is complete.
        final capped = renderFullContent ? text : _capAssistantMessage(text);
        content = RevealedText(
          text: capped,
          streaming: live,
          builder: (context, visible, _) => _assistantMessage(
            context,
            visible,
            cappedBytes: capped.length < text.length && !live ? _utf8Length(text) : null,
          ),
        );
        // assistantMessageStylesheet: 12 vertical, compactBottom above a footer.
        spacing = (12.0, footerBelow ? 0.0 : 12.0);
      case 'reasoning':
        badge = true;
        // ThoughtSlot: reasoning text is paced like assistant text.
        content = RevealedText(
          text: text,
          streaming: streaming,
          builder: (context, visible, _) => _thought(context, visible),
        );
        spacing = (0.0, badgeGap);
      case 'tool_call' when toolGroup != null:
        badge = true;
        content = _OverviewGroup(
          key: PageStorageKey('tool-group:${target.key}:${_groupId()}'),
          calls: toolGroup!,
          source: groupSource,
          resolve: resolveGroup == null ? null : () => resolveGroup!(_groupId()),
          buildCall: (context, call, last) => TimelineItem(
            entry: call,
            target: target,
            capabilities: capabilities,
            embedded: true,
            lastInToolSequence: last,
            maxDetailHeight: 200,
          ),
        );
        spacing = (0.0, badgeGap);
      case 'tool_call':
        final presentation = buildToolCallPresentation(
          item,
          cwd: target.cwd,
          targetAgentTitle: _targetAgentTitle(context, item),
        );
        final detail = jsonMap(item['detail']);
        final speakInput = detail['input'];
        if (stringValue(item['name']) == 'speak' &&
            detail['type'] == 'unknown' &&
            speakInput is String &&
            speakInput.trim().isNotEmpty) {
          // message.tsx SpeakMessage: 12 vertical padding (its 16px
          // containerSpacing is disabled in the stream).
          content = _speak(context, speakInput);
          spacing = (12.0, 12.0);
        } else if (presentation.isPlan) {
          // plan-card.tsx: 12 vertical margins.
          content = PlanCard(
            key: PageStorageKey('plan:${target.key}:${item['callId']}:${presentation.planOutcome}'),
            text: stringValue(detail['text']),
            outcome: presentation.planOutcome,
            markdown: (context, text) => _markdown(context, text),
          );
          spacing = (12.0, 12.0);
        } else {
          badge = true;
          content = _tool(context, item, presentation);
          spacing = (0.0, badgeGap);
        }
      case 'todo':
        badge = true;
        content = _tasks(context, item);
        // TodoListCard: an ExpandableBadge, without outer spacing in the stream.
      case 'compaction':
        content = _CompactionMarker(
          status: stringValue(item['status'], 'completed'),
          trigger: item['trigger'] is String ? item['trigger'] as String : null,
          preTokens: item['preTokens'] is num ? item['preTokens'] as num : null,
        );
      case 'error':
      case 'notification':
        // stream.ts: an `error` item becomes an error-level notification
        // ("Unknown error" when empty); view.tsx renders both as message.tsx
        // Notification.
        content = _notification(
          context,
          level: type == 'error' ? 'error' : stringValue(item['level'], 'info'),
          message: type == 'error'
              ? stringValue(item['message'], 'Unknown error')
              : stringValue(item['message']),
        );
      default:
        // Preserve server-authored content without loading executable client plugins.
        content = JsonDetails(value: item);
    }
    final typography = PaseoTypography.of(context);
    if (embedded) {
      return Padding(padding: EdgeInsets.only(bottom: spacing.$2), child: content);
    }
    // listContentContainer (xs 12 / md≥720 16) outside the contentMaxWidth
    // streamItemWrapper (8); badges bleed 13px so their icon rail lines up
    // with message text. The bleed is folded into the outer inset (and a
    // 10px wider max width) because Flutter padding cannot be negative.
    final listInset = MediaQuery.sizeOf(context).width < 720 ? 12.0 : 16.0;
    return Padding(
      padding: EdgeInsets.symmetric(horizontal: badge ? listInset - 5 : listInset),
      child: Align(
        alignment: Alignment.topCenter,
        child: ConstrainedBox(
          constraints: BoxConstraints(
            maxWidth: typography.contentMaxWidth + (badge ? 10 : 0),
          ),
          // streamItemWrapper `width: 100%`: every row spans the content
          // column, so short text stays flush left instead of centring.
          child: SizedBox(
            width: double.infinity,
            child: Padding(
              padding: EdgeInsets.fromLTRB(
                badge ? 0 : 8,
                spacing.$1,
                badge ? 0 : 8,
                spacing.$2 + gapBelow,
              ),
              // The user bubble keeps the natural line height on native
              // (userMessageStylesheet.text sets no lineHeight).
              child: DefaultTextStyle.merge(
                style: TextStyle(
                  fontSize: typography.contentFontSize,
                  height: type == 'user_message' && _isNative ? null : 1.4,
                ),
                child: content,
              ),
            ),
          ),
        ),
      ),
    );
  }

  String _groupId() => stringValue(jsonMap(toolGroup!.first['item'])['callId']);

  /// tool-calls/use-target-agent-title.ts: the title of the agent a Paseo
  /// call acts on, when this host's agent list knows it.
  String? _targetAgentTitle(BuildContext context, JsonMap item) {
    final agentId = paseoToolTargetAgentId(stringValue(item['name']), jsonMap(item['detail']));
    if (agentId == null) return null;
    final title = PaseoScope.select(context, (store) {
      final agents = store.hostFor(target.serverId)?.agents ?? const <JsonMap>[];
      return agents.where((agent) => agent['id'] == agentId).firstOrNull?['title'];
    });
    return title is String && title.isNotEmpty ? title : null;
  }

  // ---- UserMessage ----

  Widget _userMessage(BuildContext context, JsonMap item, String text) {
    final compact = isCompactLayout(context);
    final colors = Theme.of(context).colorScheme;
    final contentSize = PaseoTypography.of(context).contentFontSize;
    final images = maps(item['images']);
    final attachments = maps(item['attachments']);
    final hasText = text.trim().isNotEmpty;
    final timestamp = DateTime.tryParse(stringValue(entry['timestamp']));
    final messageId = item['messageId'];
    return _HoverBuilder(
      builder: (context, hovered) {
        // UserMessage isPending: an optimistic, not yet acknowledged prompt
        // keeps its trailing row hidden (and inert) until the daemon echoes it.
        final pending = entry['pending'] == true;
        final showTrailing = !pending && hasText && (compact || _isNative || hovered);
        return Align(
          alignment: Alignment.centerRight,
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.end,
            children: [
              Container(
                padding: const EdgeInsets.all(16),
                decoration: BoxDecoration(
                  color: PaseoColors.of(context).surface3,
                  borderRadius: const BorderRadius.only(
                    topLeft: Radius.circular(16),
                    topRight: Radius.circular(2),
                    bottomLeft: Radius.circular(16),
                    bottomRight: Radius.circular(16),
                  ),
                ),
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    if (images.isNotEmpty)
                      Padding(
                        padding: EdgeInsets.only(
                          bottom: hasText || attachments.isNotEmpty ? 8 : 0,
                        ),
                        child: Wrap(
                          spacing: 8,
                          runSpacing: 8,
                          children: [for (final image in images) _imagePill(context, image)],
                        ),
                      ),
                    if (attachments.isNotEmpty)
                      Padding(
                        padding: EdgeInsets.only(bottom: hasText ? 8 : 0),
                        child: Wrap(
                          spacing: 8,
                          runSpacing: 8,
                          children: [
                            for (final attachment in attachments)
                              AttachmentPill(child: AttachmentPill.label(context, attachment)),
                          ],
                        ),
                      ),
                    if (hasText)
                      SelectableText.rich(
                        _userText(context, text),
                        style: TextStyle(
                          color: colors.onSurface,
                          fontSize: contentSize,
                          height: _isNative ? null : (contentSize * 1.4).roundToDouble() / contentSize,
                        ),
                      ),
                  ],
                ),
              ),
              // trailingRow: timestamp, rewind menu and copy; always on
              // compact/native layouts, on hover otherwise.
              if (hasText)
                IgnorePointer(
                  ignoring: !showTrailing,
                  child: Opacity(
                    opacity: showTrailing ? 1 : 0,
                    child: Padding(
                      padding: const EdgeInsets.only(top: 8),
                      child: SizedBox(
                        height: 24,
                        child: Row(
                          mainAxisSize: MainAxisSize.min,
                          spacing: 8,
                          children: [
                            if (timestamp != null)
                              Text(
                                formatMessageTimestamp(context, timestamp),
                                style: TextStyle(
                                  color: colors.onSurfaceVariant,
                                  fontSize: streamMetadataFontSize,
                                  height: null,
                                ),
                              ),
                            if (capabilities != null && messageId != null && onRewind != null)
                              RewindMenuButton(
                                capabilities: capabilities!,
                                onRewind: (mode) => onRewind!(entry, mode),
                              ),
                            Transform.translate(
                              offset: const Offset(4, 0),
                              child: TurnCopyButton(
                                getContent: () => text,
                                labelKey: 'message.actions.copyMessage',
                              ),
                            ),
                          ],
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

  /// message.tsx user images open the AttachmentLightbox.
  Widget _imagePill(BuildContext context, JsonMap image) => AttachmentPill(
    tooltip: stringValue(image['fileName'], 'Image'),
    child: AttachmentPill.thumbnail(context, image),
    onOpen: () => showImageLightbox(context, image: AttachmentPill.provider(image)),
  );

  // ---- AssistantMessage ----

  /// assistant-message-render-limit.web.ts: desktop cuts at 32,000 chars
  /// without splitting a surrogate pair; mobile keeps the whole message.
  static String _capAssistantMessage(String text) {
    if (_isNative || text.length <= assistantMessageRenderCharacterLimit) return text;
    var end = assistantMessageRenderCharacterLimit;
    final last = text.codeUnitAt(end - 1);
    if (last >= 0xd800 && last <= 0xdbff) end -= 1;
    return text.substring(0, end);
  }

  Widget _assistantMessage(BuildContext context, String text, {int? cappedBytes}) {
    // A find highlight counts matches across the whole message.
    final Widget body = find != null
        ? _markdown(context, text, find: find)
        : StreamingMarkdown(
            data: text,
            streaming: live,
            markdown: (part) => _markdown(context, part),
          );
    if (cappedBytes == null) return body;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        body,
        // assistantMessageStylesheet.cappedNotice: 12 above, base italic muted.
        Padding(
          padding: const EdgeInsets.only(top: 12),
          child: Text(
            tr(context, 'agentStream.messageCapped', args: {'bytes': cappedBytes}),
            style: TextStyle(
              fontSize: PaseoTypography.of(context).uiSize(14),
              fontStyle: FontStyle.italic,
              height: null,
              color: Theme.of(context).colorScheme.onSurfaceVariant,
            ),
          ),
        ),
      ],
    );
  }

  static int _utf8Length(String text) {
    var bytes = 0;
    for (var i = 0; i < text.length; i++) {
      final unit = text.codeUnitAt(i);
      if (unit < 0x80) {
        bytes += 1;
      } else if (unit < 0x800) {
        bytes += 2;
      } else if (unit >= 0xd800 &&
          unit <= 0xdbff &&
          i + 1 < text.length &&
          text.codeUnitAt(i + 1) >= 0xdc00 &&
          text.codeUnitAt(i + 1) <= 0xdfff) {
        bytes += 4;
        i++;
      } else {
        bytes += 3;
      }
    }
    return bytes;
  }

  // ---- SpeakMessage ----

  Widget _speak(BuildContext context, String message) {
    final colors = Theme.of(context).colorScheme;
    final contentSize = PaseoTypography.of(context).contentFontSize;
    final base = PaseoTypography.of(context).uiSize(14);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Padding(
          padding: const EdgeInsets.only(bottom: 8),
          child: Row(
            spacing: 8,
            children: [
              Icon(LucideIcons.micVocal300, size: 12, color: colors.onSurfaceVariant),
              Text(
                tr(context, 'message.speak.header'),
                style: TextStyle(fontSize: base, height: null, color: colors.onSurfaceVariant),
              ),
            ],
          ),
        ),
        SelectableText(
          message,
          style: TextStyle(
            fontSize: contentSize,
            height: (contentSize * 1.4).roundToDouble() / contentSize,
            color: colors.onSurface,
          ),
        ),
      ],
    );
  }

  // ---- Notification ----

  /// message.tsx Notification: a tinted md-radius (6) card, 12×10 padding,
  /// a 16px level glyph in a 20px rail, 8 gap, then base (14/20) text.
  Widget _notification(
    BuildContext context, {
    required String level,
    required String message,
  }) {
    final theme = Theme.of(context);
    final (Color background, IconData icon, Color iconColor) = switch (level) {
      'warning' => (
        const Color(0x1af59e0b),
        LucideIcons.triangleAlert300,
        const Color(0xfff59e0b),
      ),
      'error' => (
        const Color(0x1aef4444),
        LucideIcons.circleX300,
        theme.colorScheme.error,
      ),
      _ => (
        const Color(0x1a93c5fd),
        LucideIcons.info300,
        const Color(0xff93c5fd),
      ),
    };
    final fontSize = PaseoTypography.of(context).uiSize(14);
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 10),
      decoration: BoxDecoration(
        color: background,
        borderRadius: BorderRadius.circular(6),
      ),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            height: 20,
            child: Center(child: Icon(icon, size: 16, color: iconColor)),
          ),
          const SizedBox(width: 8),
          Expanded(
            child: SelectableText(
              message,
              style: TextStyle(
                color: theme.colorScheme.onSurface,
                fontSize: fontSize,
                height: 20 / fontSize,
              ),
            ),
          ),
        ],
      ),
    );
  }

  // ---- TodoListCard ----

  static String _taskStatus(JsonMap task) {
    if (task['completed'] == true || task['status'] == 'completed') return 'completed';
    return task['status'] == 'in_progress' ? 'in_progress' : 'pending';
  }

  static String _taskKey(JsonMap task, int index) =>
      task['id'] is String ? task['id'] as String : '$index:${stringValue(task['text'])}';

  /// stream.ts deriveTaskActivities: `created` for the first list, then one
  /// activity per added / started / completed task. Empty when nothing changed
  /// (the original updates the previous card in place instead of adding one).
  static List<({String type, String task, int count})> todoActivities(
    JsonMap item,
    JsonMap? previous,
  ) {
    final tasks = maps(item['items']);
    final before = maps(previous?['items']);
    if (before.isEmpty) {
      return tasks.isEmpty ? const [] : [(type: 'created', task: '', count: tasks.length)];
    }
    final byKey = {for (final (i, task) in before.indexed) _taskKey(task, i): task};
    final activities = <({String type, String task, int count})>[];
    for (final (i, task) in tasks.indexed) {
      final prior = byKey[_taskKey(task, i)];
      final text = stringValue(task['text']);
      if (prior == null) {
        activities.add((type: 'added', task: text, count: 0));
        continue;
      }
      final from = _taskStatus(prior), to = _taskStatus(task);
      if (from == to) continue;
      if (to == 'completed') activities.add((type: 'completed', task: text, count: 0));
      if (to == 'in_progress') activities.add((type: 'started', task: text, count: 0));
    }
    return activities;
  }

  Widget _tasks(BuildContext context, JsonMap item) {
    final tasks = maps(item['items']);
    final details = _taskRows(context, tasks);
    if (showTaskDetails) return details;
    final activities = todoActivities(item, previousTasks);
    // message.tsx TodoListCard: an ExpandableBadge per activity whose details
    // list every task (8px inset, 4px apart).
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      spacing: 4,
      children: [
        for (final (index, activity) in activities.indexed)
          ToolBadge(
            key: PageStorageKey('todo:${target.key}:${entry['seqStart']}:$index'),
            icon: ToolGlyph.icon(switch (activity.type) {
              'added' => LucideIcons.plus300,
              'started' => LucideIcons.circleDot300,
              'completed' => LucideIcons.check300,
              _ => LucideIcons.checkSquare300,
            }),
            label: activity.type == 'created'
                ? tr(context, 'message.todo.activity.created', args: {'count': activity.count})
                : tr(context, 'message.todo.activity.${activity.type}'),
            secondaryLabel: activity.task.isEmpty ? null : activity.task,
            details: (context) => Padding(
              padding: const EdgeInsets.all(8),
              child: tasks.isEmpty
                  ? Text(
                      tr(context, 'message.todo.empty'),
                      style: TextStyle(
                        color: Theme.of(context).colorScheme.onSurfaceVariant,
                        fontSize: PaseoTypography.of(context).uiSize(14),
                      ),
                    )
                  : details,
            ),
          ),
      ],
    );
  }

  /// components/task-list-row.tsx: 16px status glyph 8px from base-size text;
  /// running tasks show their active form in foreground, completed ones are
  /// struck through in extra-muted.
  Widget _taskRows(BuildContext context, List<JsonMap> tasks) {
    final colors = Theme.of(context).colorScheme;
    final palette = PaseoColors.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      spacing: 4,
      children: [
        for (final task in tasks)
          Builder(builder: (context) {
            final completed = _taskStatus(task) == 'completed';
            final running = _taskStatus(task) == 'in_progress';
            final text = running && stringValue(task['activeForm']).isNotEmpty
                ? stringValue(task['activeForm'])
                : stringValue(task['text']);
            return Row(
              spacing: 8,
              children: [
                Icon(
                  completed ? LucideIcons.circleCheck300
                      : running ? LucideIcons.circleDot300 : LucideIcons.circle300,
                  size: 16,
                  color: running ? palette.running : palette.extraMuted,
                ),
                Expanded(
                  child: Text(
                    text,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(
                      fontSize: PaseoTypography.of(context).uiSize(14),
                      height: null,
                      color: completed
                          ? palette.extraMuted
                          : running ? colors.onSurface : colors.onSurfaceVariant,
                      decoration: completed ? TextDecoration.lineThrough : null,
                      decorationColor: palette.extraMuted,
                    ),
                  ),
                ),
              ],
            );
          }),
      ],
    );
  }

  // ---- Markdown / links (assistant-file-links) ----

  /// assistant-file-links tooltip-path.ts formatFileLinkTooltipPath: a file
  /// link's workspace-relative path ("." for the root) with its `:line` or
  /// `:start-end`; web links have none.
  String? _linkTooltip(String href) {
    final uri = Uri.tryParse(href);
    if (uri == null) return null;
    if (uri.scheme.isNotEmpty && uri.scheme != 'file' && !RegExp(r'^[A-Za-z]:[\\/]').hasMatch(href)) {
      return null;
    }
    final line = RegExp(r'(?:#L|:)(\d+)(?:-L?(\d+))?$').firstMatch(href);
    var path = line == null ? href : href.substring(0, line.start);
    if (path.startsWith('file://')) path = Uri.parse(path).toFilePath();
    if (path.isEmpty) return null;
    var result = stripCwdPrefix(path, target.cwd);
    if (line != null) {
      result += ':${line.group(1)}';
      final end = line.group(2);
      if (end != null && end != line.group(1)) result += '-$end';
    }
    return result;
  }

  /// AssistantMessage markdown (agent-stream/view.tsx inside its
  /// AssistantFileLinkResolverProvider): file links and paths open in this
  /// workspace, images resolve through the daemon (components/markdown/images.ts).
  Widget _markdown(BuildContext context, String text, {ChatFindTarget? find}) => PaseoMarkdown(
    data: text,
    workspaceRoot: target.cwd,
    onTapFileLink: (source) => openAssistantFileLink(context, origin: target, source: source),
    linkTooltip: _linkTooltip,
    find: find,
    imageBuilder: (uri, title, alt) => AssistantMarkdownImage(
      source: uri.toString(),
      alt: alt ?? '',
      altStyle: paseoMarkdownStyleSheet(context).p!,
      serverId: target.serverId,
      workspaceRoot: target.cwd,
      occurrenceKey: '${target.key}:${entry['seqStart']}',
    ),
  );

  // ---- ThoughtSlot / ToolCall ----

  /// view.tsx ThoughtSlot: a "thinking" ToolCall whose input is the text,
  /// loading (shimmer) while it streams; auto-expanded and forced inline
  /// with the autoExpandReasoning preference.
  Widget _thought(BuildContext context, String text) {
    final autoExpand = PaseoScope.select(
      context,
      (store) => store.preferences['autoExpandReasoning'] == true,
    );
    final detail = <String, dynamic>{'type': 'unknown', 'input': text, 'output': null};
    return _detailBadge(
      context,
      key: PageStorageKey('thinking:${target.key}:${entry['seqStart']}'),
      icon: const ToolGlyph.icon(LucideIcons.brain300),
      toolName: 'thinking',
      label: 'Thinking',
      detail: detail,
      loading: streaming,
      showLoadingSkeleton: isPendingToolCallDetail(detail, streaming ? 'executing' : 'completed', null),
      canOpenDetails: hasMeaningfulToolCallDetail(detail) ||
          isPendingToolCallDetail(detail, streaming ? 'executing' : 'completed', null),
      initiallyExpanded: autoExpand,
      forceInline: autoExpand,
    );
  }

  Widget _tool(BuildContext context, JsonMap item, ToolCallPresentation presentation) {
    final path = presentation.openFilePath;
    return _detailBadge(
      context,
      key: PageStorageKey('tool:${target.key}:${item['callId']}:$expandedTools'),
      icon: presentation.icon,
      toolName: presentation.toolName,
      label: presentation.displayName,
      summary: presentation.summary,
      detail: presentation.detail,
      errorText: presentation.errorText,
      loading: presentation.isLoading,
      error: presentation.isError,
      showLoadingSkeleton: presentation.isLoadingDetails,
      canOpenDetails: presentation.canOpenDetails || _nonEmptyError(item['error']),
      initiallyExpanded: expandedTools,
      onOpenFile: path == null ? null : () => openInlinePath(context, origin: target, path: path),
    );
  }

  /// message.tsx ToolCall: inline expansion on wide layouts, the tool-call
  /// sheet on compact ones (unless [forceInline]).
  Widget _detailBadge(
    BuildContext context, {
    required Key key,
    required ToolGlyph icon,
    required String toolName,
    required String label,
    required JsonMap detail,
    String? summary,
    String? errorText,
    bool loading = false,
    bool error = false,
    bool showLoadingSkeleton = false,
    bool canOpenDetails = true,
    bool initiallyExpanded = false,
    bool forceInline = false,
    VoidCallback? onOpenFile,
  }) {
    final inline = forceInline || !isCompactLayout(context);
    Widget content(BuildContext context) => ToolCallDetailsContent(
      toolName: toolName,
      detail: detail,
      errorText: errorText,
      maxHeight: maxDetailHeight,
      showLoadingSkeleton: showLoadingSkeleton,
    );
    return ToolBadge(
      key: key,
      icon: icon,
      label: label,
      secondaryLabel: summary == null || summary.isEmpty ? null : summary,
      loading: loading,
      error: error,
      initiallyExpanded: initiallyExpanded,
      onOpenFile: onOpenFile,
      details: canOpenDetails && inline ? content : null,
      onOpen: canOpenDetails && !inline
          ? () => showToolCallSheet(
                context,
                toolName: toolName,
                icon: icon,
                displayName: label,
                detail: detail,
                errorText: errorText,
                showLoadingSkeleton: showLoadingSkeleton,
              )
          : null,
    );
  }
}

/// components/plan-card.tsx: a surface1 bordered card (12 padding, 8 radius,
/// 8 gap) with a 16px muted chevron and the outcome title; expanded unless the
/// plan was rejected or canceled. The outcome resets local expansion.
class PlanCard extends StatefulWidget {
  const PlanCard({
    super.key,
    required this.text,
    required this.markdown,
    this.outcome,
    this.title,
    this.description,
  });
  final String text;
  final String? outcome, title, description;
  final Widget Function(BuildContext context, String text) markdown;

  @override
  State<PlanCard> createState() => _PlanCardState();
}

class _PlanCardState extends State<PlanCard> {
  late bool expanded = widget.outcome != 'rejected' && widget.outcome != 'canceled';

  @override
  void didUpdateWidget(covariant PlanCard oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.outcome != widget.outcome) {
      expanded = widget.outcome != 'rejected' && widget.outcome != 'canceled';
    }
  }

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final base = PaseoTypography.of(context).uiSize(14);
    final title = switch (widget.outcome) {
      'rejected' => tr(context, 'agentStream.permission.rejectedPlan'),
      'approved' => tr(context, 'agentStream.permission.approvedPlan'),
      'canceled' => tr(context, 'agentStream.permission.canceledPlan'),
      _ => widget.title ?? tr(context, 'agentStream.permission.plan'),
    };
    return Container(
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(
        color: PaseoColors.of(context).surface1,
        border: Border.all(color: colors.outline),
        borderRadius: BorderRadius.circular(8),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 8,
        children: [
          Semantics(
            button: true,
            expanded: expanded,
            label: title,
            child: MouseRegion(
              cursor: SystemMouseCursors.click,
              child: GestureDetector(
                behavior: HitTestBehavior.opaque,
                onTap: () => setState(() => expanded = !expanded),
                child: ConstrainedBox(
                  constraints: const BoxConstraints(minHeight: 24),
                  child: Row(
                    spacing: 8,
                    children: [
                      AnimatedRotation(
                        turns: expanded ? .25 : 0,
                        duration: Duration.zero,
                        child: Icon(LucideIcons.chevronRight300, size: 16, color: colors.onSurfaceVariant),
                      ),
                      Flexible(
                        child: Text(
                          title,
                          style: TextStyle(fontSize: base, height: 22 / base, color: colors.onSurface),
                        ),
                      ),
                    ],
                  ),
                ),
              ),
            ),
          ),
          if (expanded) ...[
            if (widget.description != null && widget.description!.isNotEmpty)
              Text(
                widget.description!,
                style: TextStyle(fontSize: base, height: 20 / base, color: colors.onSurfaceVariant),
              ),
            widget.markdown(context, widget.text),
          ],
        ],
      ),
    );
  }
}

/// message.tsx CompactionMarker: border hairlines either side of a 12px
/// scissors (spinner while compacting) and the 13px muted label.
class _CompactionMarker extends StatelessWidget {
  const _CompactionMarker({required this.status, this.trigger, this.preTokens});
  final String status;
  final String? trigger;
  final num? preTokens;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    const glyph = Color(0xffa1a1aa);
    // message-compaction-label.ts getCompactionMarkerLabel.
    final label = status == 'loading'
        ? tr(context, 'message.compaction.loading')
        : trigger == 'auto'
        ? tr(context, 'message.compaction.auto')
        : trigger == 'manual'
        ? tr(context, 'message.compaction.manual')
        : preTokens != null && preTokens != 0
        ? tr(context, 'message.compaction.withTokens', args: {'tokens': (preTokens! / 1000).round()})
        : tr(context, 'message.compaction.completed');
    Widget line() => Expanded(child: Container(height: 1, color: colors.outline));
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
      child: Row(
        spacing: 8,
        children: [
          line(),
          Row(
            mainAxisSize: MainAxisSize.min,
            spacing: 8,
            children: [
              if (status == 'loading')
                const SizedBox.square(
                  dimension: 16,
                  child: CircularProgressIndicator(strokeWidth: 2, color: glyph),
                )
              else
                const Icon(LucideIcons.scissors300, size: 12, color: glyph),
              Text(
                label,
                style: TextStyle(fontSize: 13, height: null, color: colors.onSurfaceVariant),
              ),
            ],
          ),
          line(),
        ],
      ),
    );
  }
}

/// tool-calls/detail-level/overview/view.tsx OverviewToolCallGroupView: one
/// wrench badge summarizing a run ("Edited 2 files and ran 3 commands"),
/// loading while any call runs. Wide layouts expand it borderless into a
/// 400px scroll (4 top, 13 sides) of the individual calls pinned to the
/// latest; compact layouts open the group sheet instead.
class _OverviewGroup extends StatelessWidget {
  const _OverviewGroup({
    super.key,
    required this.calls,
    required this.buildCall,
    this.source,
    this.resolve,
  });
  final List<JsonMap> calls;
  final Widget Function(BuildContext context, JsonMap call, bool last) buildCall;

  /// The open sheet re-reads the run from [resolve] whenever [source] notifies.
  final Listenable? source;
  final List<JsonMap>? Function()? resolve;

  static String summaryLabel(BuildContext context, OverviewSummary summary) {
    final parts = <String>[];
    for (final (count, key) in [
      (summary.editedFileCount, 'toolCallGroup.editedFiles'),
      (summary.commandCount, 'toolCallGroup.commands'),
      (summary.readFileCount, 'toolCallGroup.readFiles'),
      (summary.searchCount, 'toolCallGroup.searches'),
      (summary.otherToolCount, 'toolCallGroup.otherTools'),
      (summary.paseoCallCount, 'toolCallGroup.paseoCalls'),
    ]) {
      if (count > 0) {
        parts.add(tr(context, '$key.${count == 1 ? 'one' : 'other'}', args: {'count': count}));
      }
    }
    if (parts.isEmpty) return '';
    final and = tr(context, 'toolCallGroup.and');
    final joined = parts.length == 1
        ? parts.first
        : parts.length == 2
        ? '${parts[0]} $and ${parts[1]}'
        : '${parts.sublist(0, parts.length - 1).join(', ')}, $and ${parts.last}';
    return joined[0].toUpperCase() + joined.substring(1);
  }

  List<Widget> _children(BuildContext context, [List<JsonMap>? current]) {
    final rows = current ?? calls;
    return [
      for (final (i, call) in rows.indexed) buildCall(context, call, i == rows.length - 1),
    ];
  }

  @override
  Widget build(BuildContext context) {
    final model = buildOverviewSummary(calls);
    final label = summaryLabel(context, model.summary);
    const icon = ToolGlyph.icon(LucideIcons.wrench300);
    if (isCompactLayout(context)) {
      return ToolBadge(
        key: key,
        icon: icon,
        label: label,
        loading: model.isLoading,
        onOpen: () => showToolGroupSheet(
          context,
          source: source,
          summary: (sheetContext) => summaryLabel(
            sheetContext,
            buildOverviewSummary(resolve?.call() ?? calls).summary,
          ),
          // overview/sheet.tsx: the children re-render with the live run and
          // the sheet scrolls to the newest call whenever they change.
          body: (sheetContext) => Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: _children(sheetContext, resolve?.call()),
          ),
        ),
      );
    }
    return ToolBadge(
      key: key,
      icon: icon,
      label: label,
      loading: model.isLoading,
      borderlessWhenExpanded: true,
      details: (context) => _PinnedToEnd(
        maxHeight: 400,
        child: Padding(
          padding: const EdgeInsets.only(top: 4),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: _children(context),
          ),
        ),
      ),
    );
  }
}

/// A scroll capped at [maxHeight] that jumps to its end whenever its content
/// size changes (overview view.tsx `onContentSizeChange={scrollToLatest}`).
class _PinnedToEnd extends StatefulWidget {
  const _PinnedToEnd({required this.maxHeight, required this.child});
  final double maxHeight;
  final Widget child;

  @override
  State<_PinnedToEnd> createState() => _PinnedToEndState();
}

class _PinnedToEndState extends State<_PinnedToEnd> {
  final controller = ScrollController();

  void _toEnd() {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted && controller.hasClients) {
        controller.jumpTo(controller.position.maxScrollExtent);
      }
    });
  }

  @override
  void initState() {
    super.initState();
    _toEnd();
  }

  @override
  void dispose() {
    controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => ConstrainedBox(
    constraints: BoxConstraints(maxHeight: widget.maxHeight),
    child: NotificationListener<SizeChangedLayoutNotification>(
      onNotification: (_) {
        _toEnd();
        return true;
      },
      child: SingleChildScrollView(
        controller: controller,
        child: SizeChangedLayoutNotifier(child: widget.child),
      ),
    ),
  );
}

/// Original components/message.tsx ExpandableBadge: a muted single-line row
/// (22px icon rail, 12px glyph, base label, ellipsized secondary label) that
/// expands into an attached bordered detail panel. Hover/expansion swaps the
/// glyph for a 1.3x chevron; loading rows sweep a shimmer across the labels.
/// With [onOpen] (compact layouts) a tap opens the tool-call sheet instead of
/// expanding inline; without [details] or [onOpen] the row is not interactive.
class ToolBadge extends StatefulWidget {
  const ToolBadge({
    super.key,
    required this.icon,
    required this.label,
    this.details,
    this.onOpen,
    this.secondaryLabel,
    this.loading = false,
    this.error = false,
    this.initiallyExpanded = false,
    this.borderlessWhenExpanded = false,
    this.onOpenFile,
  });

  final ToolGlyph icon;
  final String label;
  final String? secondaryLabel;
  final bool loading, error, initiallyExpanded, borderlessWhenExpanded;
  final VoidCallback? onOpenFile, onOpen;
  final WidgetBuilder? details;

  @override
  State<ToolBadge> createState() => _ToolBadgeState();
}

class _ToolBadgeState extends State<ToolBadge> {
  late bool _expanded;
  bool hovered = false, pressed = false, openFileHovered = false;

  bool get interactive => widget.onOpen != null || widget.details != null;
  bool get expanded => _expanded && widget.onOpen == null && widget.details != null;

  // Expansion is stored under an explicit identifier: the default identifier
  // is derived from ancestor PageStorageKeys, which every keyless Scrollable
  // in the details shares and would read this bool back as its scroll
  // offset, throwing during build (release builds paint a gray ErrorWidget).
  Object get _storageId => ('ToolBadge.expanded', widget.key);

  @override
  void initState() {
    super.initState();
    _expanded = PageStorage.maybeOf(context)
            ?.readState(context, identifier: _storageId) as bool? ??
        widget.initiallyExpanded;
  }

  void _tap() {
    if (widget.onOpen != null) {
      widget.onOpen!();
      return;
    }
    setState(() => _expanded = !_expanded);
    PageStorage.maybeOf(context)
        ?.writeState(context, _expanded, identifier: _storageId);
  }

  Widget _labelRow(BuildContext context, {required bool mask, required bool active}) {
    final theme = Theme.of(context);
    final colors = theme.colorScheme;
    final base = theme.textTheme.bodyMedium!.copyWith(fontWeight: FontWeight.normal, height: null);
    const maskColor = Color(0xff000000);
    final labelColor = mask
        ? maskColor
        : widget.loading
        ? colors.onSurface.withValues(alpha: .72)
        : active ? colors.onSurface : colors.onSurfaceVariant;
    final showOpenFile = widget.onOpenFile != null && hovered;
    Widget openFile = Padding(
      padding: const EdgeInsets.only(left: 4),
      child: Padding(
        padding: const EdgeInsets.all(4),
        child: Icon(
          LucideIcons.fileSymlink300,
          size: 14,
          color: openFileHovered ? colors.onSurface : colors.onSurfaceVariant,
        ),
      ),
    );
    if (mask) {
      openFile = Opacity(opacity: 0, child: openFile);
    } else {
      openFile = MouseRegion(
        cursor: SystemMouseCursors.click,
        onEnter: (_) => setState(() => openFileHovered = true),
        onExit: (_) => setState(() => openFileHovered = false),
        child: GestureDetector(
          onTap: widget.onOpenFile,
          child: Tooltip(message: tr(context, 'message.actions.openFile'), child: openFile),
        ),
      );
    }
    return Row(
      children: [
        Flexible(
          flex: 0,
          child: Text(
            widget.label,
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: base.copyWith(color: labelColor),
          ),
        ),
        if (widget.secondaryLabel != null)
          Flexible(
            child: Padding(
              padding: const EdgeInsets.only(left: 8),
              child: Text(
                widget.secondaryLabel!,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: base.copyWith(
                  color: mask ? maskColor : active ? colors.onSurface : colors.onSurfaceVariant,
                ),
              ),
            ),
          ),
        if (showOpenFile) openFile,
      ],
    );
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = theme.colorScheme;
    final palette = PaseoColors.of(context);
    final active = hovered || expanded;
    // LUCIDE_CHEVRON_NUDGE_LEFT (-4) / LUCIDE_TOOL_ICON_NUDGE_LEFT (-1) margins
    // inside the centered rail shift the glyph by half their width.
    final Widget glyph = interactive && active
        ? Transform.translate(
            offset: const Offset(-2, 0),
            child: Transform.scale(
              scale: 1.3,
              child: Transform.rotate(
                angle: expanded ? math.pi / 2 : 0,
                child: Icon(LucideIcons.chevronRight300, size: 12, color: colors.onSurface),
              ),
            ),
          )
        : Transform.translate(
            offset: const Offset(-.5, 0),
            child: widget.error
                ? Icon(LucideIcons.triangleAlert300, size: 12,
                    color: colors.error.withValues(alpha: .8))
                : widget.icon.build(12, active ? colors.onSurface : colors.onSurfaceVariant),
          );
    const radius = Radius.circular(8);
    final attached = expanded && !widget.borderlessWhenExpanded;
    Widget labels = _labelRow(context, mask: false, active: active);
    if (widget.loading) {
      labels = _Shimmer(
        characters: widget.label.trim().length + (widget.secondaryLabel?.trim().length ?? 0),
        mask: _labelRow(context, mask: true, active: active),
        child: labels,
      );
    }
    final header = Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
      decoration: BoxDecoration(
        color: expanded ? palette.surface1 : null,
        border: Border.all(color: attached ? colors.outline : Colors.transparent),
        borderRadius: attached
            ? const BorderRadius.vertical(top: radius)
            : const BorderRadius.all(radius),
      ),
      child: Row(
        children: [
          Padding(
            padding: const EdgeInsets.only(right: 4),
            child: SizedBox.square(dimension: 22, child: Center(child: glyph)),
          ),
          Expanded(child: labels),
        ],
      ),
    );
    return MouseRegion(
      onEnter: (_) => setState(() => hovered = true),
      onExit: (_) => setState(() {
        hovered = false;
        pressed = false;
      }),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          if (interactive)
            Semantics(
              button: true,
              expanded: widget.onOpen == null ? expanded : null,
              child: GestureDetector(
                behavior: HitTestBehavior.opaque,
                onTapDown: (_) => setState(() => pressed = true),
                onTapCancel: () => setState(() => pressed = false),
                onTapUp: (_) => setState(() => pressed = false),
                onTap: _tap,
                child: Opacity(opacity: pressed ? .9 : 1, child: header),
              ),
            )
          else
            header,
          if (expanded)
            Container(
              decoration: widget.borderlessWhenExpanded
                  ? null
                  : BoxDecoration(
                      border: Border(
                        left: BorderSide(color: colors.outline),
                        right: BorderSide(color: colors.outline),
                        bottom: BorderSide(color: colors.outline),
                      ),
                      borderRadius: const BorderRadius.vertical(bottom: radius),
                    ),
              clipBehavior: widget.borderlessWhenExpanded ? Clip.none : Clip.antiAlias,
              child: widget.details!(context),
            ),
        ],
      ),
    );
  }
}

/// ExpandableBadge loading shimmer: a white peak sweeping linearly across the
/// label row, masked by the label glyphs. Native: a 0→1→0 peak of
/// clamp(28% width, 32, 120) from -peak to width+peak; web: a plateaued peak of
/// clamp(22% width, 42, 120) from -peak to width. Duration
/// clamp(1.25s + 8ms/char (−0.25s for ≤12 chars), 1s, 2.3s).
class _Shimmer extends StatefulWidget {
  const _Shimmer({required this.characters, required this.mask, required this.child});
  final int characters;
  final Widget mask, child;

  @override
  State<_Shimmer> createState() => _ShimmerState();
}

class _ShimmerState extends State<_Shimmer> with IndicatorClockState<_Shimmer> {
  final _start = DateTime.now();
  late final _phase = ValueNotifier<double>(_currentPhase());

  Duration get _duration {
    final adjust = widget.characters <= 12 ? .25 : 0;
    final seconds = (1.25 + widget.characters * .008 - adjust).clamp(1.0, 2.3);
    return Duration(milliseconds: (seconds * 1000).round());
  }

  double _currentPhase() {
    final period = _duration.inMilliseconds;
    return (DateTime.now().difference(_start).inMilliseconds % period) / period;
  }

  /// Stepped on [IndicatorClock]; only the mask below rebuilds.
  @override
  void onIndicatorTick() => _phase.value = _currentPhase();

  @override
  void dispose() {
    _phase.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => LayoutBuilder(
    builder: (context, constraints) {
      final width = constraints.maxWidth.isFinite ? constraints.maxWidth : 0.0;
      final peak = _isNative
          ? (width * .28).clamp(32.0, 120.0)
          : (width * .22).clamp(42.0, 120.0);
      final end = _isNative ? width + peak : width;
      return Stack(
        children: [
          widget.child,
          // The sweep repaints only this overlay, not the tool row beneath it.
          Positioned.fill(
            child: RepaintBoundary(
              child: IgnorePointer(
                child: ValueListenableBuilder<double>(
                  valueListenable: _phase,
                  builder: (context, phase, mask) {
                    final x = -peak + (end + peak) * phase;
                    return ShaderMask(
                      blendMode: BlendMode.srcIn,
                      shaderCallback: (bounds) => LinearGradient(
                        colors: _isNative
                            ? const [Color(0x00ffffff), Color(0xffffffff), Color(0x00ffffff)]
                            : const [
                                Color(0x00ffffff),
                                Color(0x73ffffff),
                                Color(0xffffffff),
                                Color(0xffffffff),
                                Color(0x73ffffff),
                                Color(0x00ffffff),
                              ],
                        stops: _isNative
                            ? const [0, .5, 1]
                            : const [0, .24, .4, .6, .76, 1],
                      ).createShader(Rect.fromLTWH(x, 0, peak, bounds.height)),
                      child: mask,
                    );
                  },
                  child: widget.mask,
                ),
              ),
            ),
          ),
        ],
      );
    },
  );
}

class _HoverBuilder extends StatefulWidget {
  const _HoverBuilder({required this.builder});
  final Widget Function(BuildContext context, bool hovered) builder;

  @override
  State<_HoverBuilder> createState() => _HoverBuilderState();
}

class _HoverBuilderState extends State<_HoverBuilder> {
  bool hovered = false;

  @override
  Widget build(BuildContext context) => MouseRegion(
    onEnter: (_) => setState(() => hovered = true),
    onExit: (_) => setState(() => hovered = false),
    child: widget.builder(context, hovered),
  );
}
