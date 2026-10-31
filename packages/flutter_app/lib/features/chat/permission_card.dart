import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/json.dart';
import '../../ui/common.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import 'chat_controller.dart' show maps;
import 'chat_markdown.dart';
import 'tool_call_details.dart';
import 'chrome/chrome_controls.dart';

/// agent-stream/view.tsx PermissionRequestCard (and the QuestionFormCard /
/// PlanCard it delegates to). The stream places the cards in its live tail
/// (contentMaxWidth wrapper, 8px gaps); a plain permission card carries its
/// own 12px vertical margin, the question form and plan card none.
class PermissionCard extends StatefulWidget {
  const PermissionCard({
    super.key,
    required this.request,
    required this.onRespond,
  });
  final JsonMap request;
  final Future<void> Function(JsonMap response) onRespond;
  @override
  State<PermissionCard> createState() => _PermissionCardState();
}

/// question-form-card-core.ts QuestionFormQuestion.
class _Question {
  _Question(JsonMap q)
    : question = stringValue(q['question']),
      header = stringValue(q['header']),
      options = maps(q['options']),
      multiSelect = q['multiSelect'] == true,
      allowOther = q['allowOther'] == true || q['isOther'] == true,
      allowEmpty = q['allowEmpty'] == true,
      placeholder = q['placeholder'] is String ? q['placeholder'] as String : null,
      dismissLabel = q['dismissLabel'] is String ? q['dismissLabel'] as String : null;
  final String question, header;
  final List<JsonMap> options;
  final bool multiSelect, allowOther, allowEmpty;
  final String? placeholder, dismissLabel;

  bool get showsTextInput => options.isEmpty || allowOther;
}

class _PermissionCardState extends State<PermissionCard> {
  final Map<int, Set<int>> selections = {};
  final Map<int, String> otherTexts = {};
  final otherInput = TextEditingController();
  final _otherFocus = FocusNode(debugLabel: 'question-other-input');
  int activeQuestion = 0;
  bool pending = false, failed = false;
  String? respondingAction;

  /// parseQuestionFormQuestions: null unless every question is well formed.
  List<_Question>? get questions {
    final raw = jsonMap(widget.request['input'])['questions'];
    if (raw is! List || raw.isEmpty) return null;
    final parsed = <_Question>[];
    for (final item in raw) {
      if (item is! Map || item['question'] is! String || item['header'] is! String) return null;
      if (item['options'] is! List) return null;
      if ((item['options'] as List).any((o) => o is! Map || o['label'] is! String)) return null;
      parsed.add(_Question(jsonMap(item)));
    }
    return parsed;
  }

  bool isAnswered(_Question q, int index) {
    if (selections[index]?.isNotEmpty ?? false) return true;
    if (!q.showsTextInput) return false;
    if (otherTexts[index]?.trim().isNotEmpty ?? false) return true;
    return q.allowEmpty;
  }

  Map<String, String> buildAnswers(List<_Question> qs) {
    final answers = <String, String>{};
    for (var i = 0; i < qs.length; i++) {
      final q = qs[i];
      final labels = (selections[i] ?? const <int>{})
          .map((index) => stringValue(q.options[index]['label']))
          .toList();
      final text = otherTexts[i]?.trim() ?? '';
      if (q.showsTextInput) {
        if (text.isNotEmpty) {
          answers[q.header] = q.multiSelect ? [...labels, text].join(', ') : text;
          continue;
        }
        if (q.allowEmpty && q.options.isEmpty) {
          answers[q.header] = '';
          continue;
        }
      }
      if (labels.isNotEmpty) answers[q.header] = labels.join(', ');
    }
    return answers;
  }

  Future<void> respond(String action, JsonMap response) async {
    setState(() {
      pending = true;
      failed = false;
      respondingAction = action;
    });
    try {
      await widget.onRespond(response);
    } catch (_) {
      if (mounted) setState(() => failed = true);
    } finally {
      if (mounted) setState(() => pending = false);
    }
  }

  @override
  void didUpdateWidget(PermissionCard oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.request['id'] != widget.request['id']) {
      failed = false;
      respondingAction = null;
    }
  }

  /// A resolved card unmounts with its "Other" input focused. Flutter's
  /// FocusAttachment.detach would hand focus to the scope's previously
  /// focused child — the composer — raising the keyboard. RN just drops the
  /// unmounted input's focus (the keyboard closes, the composer stays
  /// blurred), so release it to the scope before the input detaches.
  @override
  void deactivate() {
    if (_otherFocus.hasFocus) _otherFocus.unfocus(disposition: UnfocusDisposition.scope);
    super.deactivate();
  }

  @override
  void dispose() {
    otherInput.dispose();
    _otherFocus.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final r = widget.request;
    final plan = r['kind'] == 'plan';
    final planText = _planMarkdown(r);
    if (r['kind'] == 'question') {
      final qs = questions;
      return qs == null ? const SizedBox.shrink() : _questionForm(context, qs);
    }
    if (plan && planText != null) {
      return _PlanCard(
        title: _title(context),
        description: stringValue(r['description']),
        text: planText,
        footer: _footer(context),
      );
    }
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 12),
      child: _permission(context, planText),
    );
  }

  String _title(BuildContext context) => r['kind'] == 'plan'
      ? tr(context, 'agentStream.permission.plan')
      : stringValue(
          r['title'],
          stringValue(r['name'], tr(context, 'agentStream.permission.required')),
        );

  JsonMap get r => widget.request;

  /// view.tsx planMarkdown: a non-empty metadata.planText, else a string
  /// input.plan. The original only renders it when truthy, so an empty plan
  /// counts as none.
  static String? _planMarkdown(JsonMap r) {
    final fromMetadata = jsonMap(r['metadata'])['planText'];
    if (fromMetadata is String && fromMetadata.isNotEmpty) return fromMetadata;
    final candidate = jsonMap(r['input'])['plan'];
    return candidate is String && candidate.isNotEmpty ? candidate : null;
  }

  List<JsonMap> _actions(BuildContext context) {
    final actions = maps(r['actions']);
    if (actions.isNotEmpty) return actions;
    return [
      {
        'id': 'reject',
        'label': tr(context, 'agentStream.permission.deny'),
        'behavior': 'deny',
        'variant': 'danger',
      },
      {
        'id': 'accept',
        'label': tr(
          context,
          r['kind'] == 'plan' ? 'agentStream.permission.implement' : 'agentStream.permission.accept',
        ),
        'behavior': 'allow',
        'variant': 'primary',
      },
    ];
  }

  Widget _footer(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final base = TextStyle(fontSize: PaseoTypography.of(context).uiSize(14));
    final buttons = [
      for (final action in _actions(context))
        _CardButton(
          label: stringValue(action['label']),
          icon: action['behavior'] == 'allow' ? LucideIcons.check300 : LucideIcons.x300,
          foreground: action['variant'] == 'primary' ? colors.onSurface : colors.onSurfaceVariant,
          // PermissionActionButton: the pressed action keeps its spinner
          // (respondingActionId is only reset for a new request) while the
          // buttons are disabled just for the in-flight response.
          responding: respondingAction == action['id'],
          onPressed: pending
              ? null
              : () {
                  final id = stringValue(action['id']);
                  respond(
                    id,
                    action['behavior'] == 'allow'
                        ? {'behavior': 'allow', 'selectedActionId': id}
                        : {
                            'behavior': 'deny',
                            'selectedActionId': id,
                            'message': 'Denied by user',
                          },
                  );
                },
        ),
    ];
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      spacing: 8,
      children: [
        // permissionStyles.question: base, muted, 4 above and below.
        Padding(
          padding: const EdgeInsets.symmetric(vertical: 4),
          child: Text(
            tr(context, 'agentStream.permission.question'),
            style: base.copyWith(color: colors.onSurfaceVariant),
          ),
        ),
        if (isCompactLayout(context))
          Column(crossAxisAlignment: CrossAxisAlignment.stretch, spacing: 8, children: buttons)
        else
          Wrap(spacing: 8, runSpacing: 8, children: buttons),
        // A failed permission response only re-enables the actions (the
        // original logs it); the question form is the one that shows an alert.
      ],
    );
  }

  Widget _permission(BuildContext context, String? planText) {
    final colors = Theme.of(context).colorScheme;
    final palette = PaseoColors.of(context);
    final base = PaseoTypography.of(context).uiSize(14);
    final description = stringValue(r['description']);
    final detail = r['detail'] is Map
        ? jsonMap(r['detail'])
        : {'type': 'unknown', 'input': r['input'], 'output': null};
    return Container(
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(
        color: palette.surface1,
        border: Border.all(color: colors.outline),
        borderRadius: BorderRadius.circular(8),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 8,
        children: [
          Text(
            _title(context),
            style: TextStyle(fontSize: base, height: 22 / base, color: colors.onSurface),
          ),
          if (description.isNotEmpty)
            Text(
              description,
              style: TextStyle(fontSize: base, height: 20 / base, color: colors.onSurfaceVariant),
            ),
          if (planText != null)
            _PlanCard(title: tr(context, 'agentStream.permission.proposedPlan'), text: planText),
          if (r['kind'] != 'plan') ToolCallDetailsContent(detail: detail, maxHeight: 200),
          _footer(context),
        ],
      ),
    );
  }

  Widget _questionForm(BuildContext context, List<_Question> qs) {
    final colors = Theme.of(context).colorScheme;
    final palette = PaseoColors.of(context);
    final base = PaseoTypography.of(context).uiSize(14);
    final index = activeQuestion.clamp(0, qs.length - 1);
    final q = qs[index];
    final allAnswered = [for (var i = 0; i < qs.length; i++) isAnswered(qs[i], i)].every((a) => a);
    final last = index == qs.length - 1;
    final primaryDisabled = pending || (last ? !allAnswered : !isAnswered(q, index));
    // resolveDismissLabel: the first truthy dismissLabel, else "Dismiss".
    final dismissLabel =
        qs.map((q) => q.dismissLabel).firstWhere((l) => l != null && l.isNotEmpty, orElse: () => null) ??
        tr(context, 'common.actions.dismiss');
    final selected = selections[index] ?? const <int>{};

    void submit() {
      if (!allAnswered || pending) return;
      respond('submit', {
        'behavior': 'allow',
        'updatedInput': {...jsonMap(r['input']), 'answers': buildAnswers(qs)},
      });
    }

    void primary() {
      if (!last) {
        if (!isAnswered(q, index) || pending) return;
        setState(() => activeQuestion = (index + 1).clamp(0, qs.length - 1));
        _syncOtherInput();
        return;
      }
      submit();
    }

    void dismiss() {
      if (pending) return;
      final submitEmpty = qs.every((q) => q.allowEmpty && q.options.isEmpty);
      respond(
        'dismiss',
        submitEmpty
            ? {
                'behavior': 'allow',
                'updatedInput': {...jsonMap(r['input']), 'answers': buildAnswers(qs)},
              }
            : {'behavior': 'deny', 'message': 'Dismissed by user'},
      );
    }

    void toggle(int option) {
      setState(() {
        final next = {...selected};
        if (q.multiSelect) {
          if (!next.remove(option)) next.add(option);
        } else if (next.contains(option)) {
          next.clear();
        } else {
          next
            ..clear()
            ..add(option);
        }
        selections[index] = next;
        // Single-select: an option and a custom answer replace each other.
        if (!q.multiSelect && (otherTexts[index]?.isNotEmpty ?? false)) {
          otherTexts.remove(index);
          otherInput.clear();
        }
        if (!q.multiSelect && next.isNotEmpty) {
          activeQuestion = (index + 1).clamp(0, qs.length - 1);
        }
      });
      _syncOtherInput();
    }

    return Container(
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(
        color: palette.surface1,
        border: Border.all(color: colors.outline),
        borderRadius: BorderRadius.circular(8),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 12,
        children: [
          // Titled tabs per question header, hidden for a lone question.
          if (qs.length > 1)
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 12),
              child: Wrap(
                spacing: 4,
                runSpacing: 4,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: [
                  for (var i = 0; i < qs.length; i++)
                    _QuestionTab(
                      header: qs[i].header,
                      active: i == index,
                      answered: isAnswered(qs[i], i),
                      onPressed: pending
                          ? null
                          : () {
                              setState(() => activeQuestion = i);
                              _syncOtherInput();
                            },
                    ),
                ],
              ),
            ),
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 0, 12, 4),
            child: Text(
              q.question,
              style: TextStyle(fontSize: base, height: 22 / base, color: colors.onSurface),
            ),
          ),
          Column(
            key: ValueKey(q.question),
            crossAxisAlignment: CrossAxisAlignment.stretch,
            spacing: 8,
            children: [
              if (q.options.isNotEmpty)
                Column(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  spacing: 4,
                  children: [
                    for (var j = 0; j < q.options.length; j++)
                      _QuestionOption(
                        option: q.options[j],
                        selected: selected.contains(j),
                        multiSelect: q.multiSelect,
                        onPressed: pending ? null : () => toggle(j),
                      ),
                  ],
                ),
              if (q.showsTextInput)
                TextField(
                  controller: otherInput,
                  enabled: !pending,
                  style: TextStyle(fontSize: base, color: colors.onSurface),
                  textInputAction: TextInputAction.done,
                  // blurOnSubmit={false}: the field keeps focus after Enter.
                  onSubmitted: (_) {
                    primary();
                    _otherFocus.requestFocus();
                  },
                  focusNode: _otherFocus,
                  onChanged: (text) => setState(() {
                    otherTexts[index] = text;
                    if (!q.multiSelect && text.isNotEmpty) selections[index] = <int>{};
                  }),
                  decoration: InputDecoration(
                    isDense: true,
                    filled: true,
                    fillColor: palette.surface2,
                    hintText: q.placeholder ??
                        tr(
                          context,
                          q.options.isEmpty
                              ? 'message.question.answerPlaceholder'
                              : 'message.question.otherPlaceholder',
                        ),
                    hintStyle: TextStyle(fontSize: base, color: colors.onSurfaceVariant),
                    contentPadding: const EdgeInsets.all(12),
                    border: _inputBorder(colors.outline),
                    enabledBorder: _inputBorder(
                      (otherTexts[index]?.isNotEmpty ?? false) ? palette.borderAccent : colors.outline,
                    ),
                    disabledBorder: _inputBorder(colors.outline),
                    focusedBorder: _inputBorder(
                      (otherTexts[index]?.isNotEmpty ?? false) ? palette.borderAccent : colors.outline,
                    ),
                  ),
                ),
            ],
          ),
          if (failed) ErrorAlert(message: tr(context, 'message.question.responseFailed')),
          Builder(
            builder: (context) {
              final buttons = [
                _CardButton(
                  label: dismissLabel,
                  icon: LucideIcons.x300,
                  foreground: colors.onSurfaceVariant,
                  responding: pending && respondingAction == 'dismiss',
                  onPressed: pending ? null : dismiss,
                ),
                _CardButton(
                  label: tr(context, last ? 'message.question.submit' : 'message.question.next'),
                  icon: LucideIcons.check300,
                  foreground: colors.onTertiary,
                  background: palette.accent,
                  border: palette.accent,
                  dimmed: primaryDisabled,
                  responding: pending && respondingAction == 'submit',
                  onPressed: primaryDisabled ? null : primary,
                ),
              ];
              return isCompactLayout(context)
                  ? Column(crossAxisAlignment: CrossAxisAlignment.stretch, spacing: 8, children: buttons)
                  : Row(spacing: 8, children: buttons);
            },
          ),
        ],
      ),
    );
  }

  /// The editing surface shows the active question's own custom answer.
  void _syncOtherInput() {
    final text = otherTexts[activeQuestion] ?? '';
    if (otherInput.text != text) otherInput.text = text;
  }

  static OutlineInputBorder _inputBorder(Color color) => OutlineInputBorder(
    borderRadius: BorderRadius.circular(8),
    borderSide: BorderSide(color: color),
  );
}

/// permissionStyles.optionButton / question actionButton: 8/12 padding,
/// radius 6, 1px border, 14px icon + base label 8 apart; surface2 on hover.
class _CardButton extends StatefulWidget {
  const _CardButton({
    required this.label,
    required this.icon,
    required this.foreground,
    required this.onPressed,
    this.background,
    this.border,
    this.responding = false,
    this.dimmed = false,
  });
  final String label;
  final IconData icon;
  final Color foreground;
  final Color? background, border;
  final VoidCallback? onPressed;
  final bool responding, dimmed;

  @override
  State<_CardButton> createState() => _CardButtonState();
}

class _CardButtonState extends State<_CardButton> {
  bool hovered = false, pressed = false;

  @override
  Widget build(BuildContext context) {
    final palette = PaseoColors.of(context);
    final base = PaseoTypography.of(context).uiSize(14);
    final background = widget.background ?? (hovered ? palette.surface2 : palette.surface1);
    return Opacity(
      opacity: widget.dimmed ? .5 : pressed ? .9 : 1,
      child: MouseRegion(
        cursor: widget.onPressed == null ? MouseCursor.defer : SystemMouseCursors.click,
        onEnter: (_) => setState(() => hovered = true),
        onExit: (_) => setState(() => hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: widget.onPressed,
          onTapDown: widget.onPressed == null ? null : (_) => setState(() => pressed = true),
          onTapUp: (_) => setState(() => pressed = false),
          onTapCancel: () => setState(() => pressed = false),
          child: Semantics(
            button: true,
            enabled: widget.onPressed != null,
            label: widget.label,
            excludeSemantics: true,
            child: Container(
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
              decoration: BoxDecoration(
                color: background,
                border: Border.all(color: widget.border ?? palette.borderAccent),
                borderRadius: BorderRadius.circular(6),
              ),
              child: widget.responding
                  ? Center(
                      widthFactor: 1,
                      heightFactor: 1,
                      child: ChromeSpinner(color: widget.foreground),
                    )
                  : Row(
                      mainAxisSize: MainAxisSize.min,
                      mainAxisAlignment: MainAxisAlignment.center,
                      spacing: 8,
                      children: [
                        Icon(widget.icon, size: 14, color: widget.foreground),
                        Flexible(
                          child: Text(
                            widget.label,
                            style: TextStyle(fontSize: base, color: widget.foreground),
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

/// question-form-card.tsx QuestionNavButton: 28 min height, 8/4 padding,
/// radius 6; active = surface2 with a muted border.
class _QuestionTab extends StatefulWidget {
  const _QuestionTab({
    required this.header,
    required this.active,
    required this.answered,
    required this.onPressed,
  });
  final String header;
  final bool active, answered;
  final VoidCallback? onPressed;

  @override
  State<_QuestionTab> createState() => _QuestionTabState();
}

class _QuestionTabState extends State<_QuestionTab> {
  bool hovered = false;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final palette = PaseoColors.of(context);
    final foreground = widget.active ? colors.onSurface : colors.onSurfaceVariant;
    return MouseRegion(
      onEnter: (_) => setState(() => hovered = true),
      onExit: (_) => setState(() => hovered = false),
      child: GestureDetector(
        onTap: widget.onPressed,
        child: Container(
          constraints: const BoxConstraints(minHeight: 28),
          padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
          decoration: BoxDecoration(
            color: widget.active || hovered ? palette.surface2 : palette.surface1,
            border: Border.all(color: widget.active ? colors.onSurfaceVariant : colors.outline),
            borderRadius: BorderRadius.circular(6),
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            spacing: 4,
            children: [
              if (widget.answered) Icon(LucideIcons.check300, size: 12, color: foreground),
              Text(
                widget.header,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(fontSize: PaseoTypography.of(context).uiSize(14), color: foreground),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// question-form-card.tsx QuestionOptionRow: 12/8 padding, radius 6,
/// surface2 when hovered or selected, 18px radio/checkbox control.
class _QuestionOption extends StatefulWidget {
  const _QuestionOption({
    required this.option,
    required this.selected,
    required this.multiSelect,
    required this.onPressed,
  });
  final JsonMap option;
  final bool selected, multiSelect;
  final VoidCallback? onPressed;

  @override
  State<_QuestionOption> createState() => _QuestionOptionState();
}

class _QuestionOptionState extends State<_QuestionOption> {
  bool hovered = false;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final palette = PaseoColors.of(context);
    final typography = PaseoTypography.of(context);
    final base = typography.uiSize(14);
    final selected = widget.selected;
    final description = widget.option['description'];
    final preview = widget.option['preview'];
    return Semantics(
      checked: selected,
      inMutuallyExclusiveGroup: !widget.multiSelect,
      label: stringValue(widget.option['label']),
      child: MouseRegion(
        onEnter: (_) => setState(() => hovered = true),
        onExit: (_) => setState(() => hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: widget.onPressed,
          child: Container(
            padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
            decoration: BoxDecoration(
              color: hovered || selected ? palette.surface2 : null,
              borderRadius: BorderRadius.circular(6),
            ),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              spacing: 8,
              children: [
                Container(
                  width: 18,
                  height: 18,
                  margin: const EdgeInsets.only(top: 2),
                  alignment: Alignment.center,
                  decoration: BoxDecoration(
                    color: selected && widget.multiSelect ? palette.accent : null,
                    border: Border.all(color: selected ? palette.accent : palette.extraMuted),
                    borderRadius: BorderRadius.circular(widget.multiSelect ? 4 : 999),
                  ),
                  child: !selected
                      ? null
                      : widget.multiSelect
                      ? Icon(LucideIcons.check300, size: 12, color: colors.onTertiary)
                      : Container(
                          width: 8,
                          height: 8,
                          decoration: BoxDecoration(color: palette.accent, shape: BoxShape.circle),
                        ),
                ),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    spacing: 4,
                    children: [
                      Text(
                        stringValue(widget.option['label']),
                        style: TextStyle(
                          fontSize: base,
                          height: 22 / base,
                          color: selected ? colors.onSurface : colors.onSurfaceVariant,
                        ),
                      ),
                      if (description is String && description.isNotEmpty)
                        Text(
                          description,
                          style: TextStyle(
                            fontSize: base,
                            height: 20 / base,
                            color: colors.onSurfaceVariant,
                          ),
                        ),
                      // ScrollableCodeSurface tone surface0, wrapped prose.
                      if (preview is String && preview.isNotEmpty)
                        Container(
                          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
                          decoration: BoxDecoration(
                            color: palette.surface0,
                            border: Border.all(color: colors.outline),
                            borderRadius: BorderRadius.circular(8),
                          ),
                          child: Text(
                            preview,
                            style: typography.codeStyle.copyWith(
                              color: colors.onSurface,
                              height: 20 / typography.codeFontSize,
                            ),
                          ),
                        ),
                    ],
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

/// components/plan-card.tsx: collapsible surface1 card with a 16px chevron,
/// base title, muted description, markdown body and optional footer.
class _PlanCard extends StatefulWidget {
  const _PlanCard({required this.title, required this.text, this.description = '', this.footer});
  final String title, text, description;
  final Widget? footer;

  @override
  State<_PlanCard> createState() => _PlanCardState();
}

class _PlanCardState extends State<_PlanCard> {
  bool expanded = true;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final palette = PaseoColors.of(context);
    final base = PaseoTypography.of(context).uiSize(14);
    return Container(
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(
        color: palette.surface1,
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
            child: GestureDetector(
              behavior: HitTestBehavior.opaque,
              onTap: () => setState(() => expanded = !expanded),
              child: ConstrainedBox(
                constraints: const BoxConstraints(minHeight: 24),
                child: Row(
                  spacing: 8,
                  children: [
                    Transform.rotate(
                      angle: expanded ? 1.5707963267948966 : 0,
                      child: Icon(LucideIcons.chevronRight300, size: 16, color: colors.onSurfaceVariant),
                    ),
                    Flexible(
                      child: Text(
                        widget.title,
                        style: TextStyle(fontSize: base, height: 22 / base, color: colors.onSurface),
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ),
          if (expanded) ...[
            if (widget.description.isNotEmpty)
              Text(
                widget.description,
                style: TextStyle(fontSize: base, height: 20 / base, color: colors.onSurfaceVariant),
              ),
            PaseoMarkdown(data: widget.text),
          ],
          ?widget.footer,
        ],
      ),
    );
  }
}
