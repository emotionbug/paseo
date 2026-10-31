import 'dart:convert';

import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/client.dart';
import '../../core/json.dart';
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/theme.dart';
import '../chat/agent_controls.dart';
import 'settings_labels.dart';

Future<JsonMap> settingsRequest(
  PaseoClient client,
  String type,
  JsonMap fields, {
  String? responseType,
}) async {
  final result = await client.request(type, fields, responseType: responseType);
  if (result['error'] != null || result['success'] == false) {
    throw SettingsRequestError(result);
  }
  return result;
}

class SettingsRequestError implements Exception {
  final JsonMap response;
  SettingsRequestError(this.response);
  @override
  String toString() => response['error'] is String
      ? response['error'] as String
      : jsonEncode(response);
}

class SettingField {
  final String key, label;
  final Object? value;
  final bool required, multiline, secret;
  final Map<String, String>? choices;
  final Object? Function(String)? parse;
  const SettingField(
    this.key,
    this.label, {
    this.value,
    this.required = false,
    this.multiline = false,
    this.secret = false,
    this.choices,
    this.parse,
  });
}

int positiveInteger(String value) {
  final number = int.tryParse(value.trim());
  if (number == null || number <= 0) {
    throw const FormatException('Enter a positive whole number');
  }
  return number;
}

List<String> textLines(String value) =>
    value.split('\n').map((s) => s.trim()).where((s) => s.isNotEmpty).toList();

/// Original settings edit modals (`AdaptiveModalSheet` + `FormField`s): muted
/// labels over surface2 fields, switches in label rows, and a stretched
/// secondary Cancel / default Save footer.
Future<JsonMap?> editSettingsForm(
  BuildContext context, {
  required String title,
  required List<SettingField> fields,
  Future<void> Function(JsonMap)? onSave,
  String? description,
}) async {
  final controllers = {
    for (final f in fields)
      if (f.value is! bool && f.choices == null)
        f.key: TextEditingController(text: f.value?.toString() ?? ''),
  };
  final values = <String, dynamic>{for (final f in fields) f.key: f.value};
  final errors = <String, String>{};
  bool busy = false;
  Object? error;
  try {
    return await showAdaptiveSheet<JsonMap>(
      context: context,
      maxWidth: 600,
      builder: (context) => StatefulBuilder(
        builder: (context, setState) {
          final theme = Theme.of(context);
          final colors = PaseoColors.of(context);
          Future<void> save() async {
            errors.clear();
            for (final f in fields) {
              if (controllers.containsKey(f.key)) {
                final text = controllers[f.key]!.text;
                if (f.required && text.trim().isEmpty) {
                  errors[f.key] = 'Required';
                  continue;
                }
                try {
                  values[f.key] = f.parse == null ? text : f.parse!(text);
                } catch (e) {
                  errors[f.key] = e is FormatException ? e.message : e.toString();
                }
              } else if (f.choices != null &&
                  f.required &&
                  stringValue(values[f.key]).isEmpty) {
                errors[f.key] = 'Required';
              }
            }
            if (errors.isNotEmpty) {
              setState(() {});
              return;
            }
            setState(() {
              busy = true;
              error = null;
            });
            try {
              await onSave?.call(values);
              if (context.mounted) Navigator.pop(context, values);
            } catch (e) {
              if (context.mounted) {
                setState(() {
                  busy = false;
                  error = e;
                });
              }
            }
          }

          Widget field(SettingField f) {
            final label = settingsLabel(context, f.label);
            if (f.value is bool) {
              return Row(
                spacing: 12,
                children: [
                  Expanded(child: Text(label, style: theme.textTheme.bodyMedium)),
                  PaseoSwitch(
                    value: values[f.key] == true,
                    semanticLabel: label,
                    onChanged: busy
                        ? null
                        : (v) => setState(() => values[f.key] = v),
                  ),
                ],
              );
            }
            if (f.choices != null) {
              final current = values[f.key] as String?;
              return PaseoFormField(
                label: label,
                error: errors[f.key],
                child: PaseoSelectField<String>(
                  title: label,
                  value: f.choices!.containsKey(current) ? current : null,
                  options: [
                    for (final e in f.choices!.entries)
                      PaseoSelectOption(e.key, settingsLabel(context, e.value)),
                  ],
                  onChanged: busy
                      ? null
                      : (v) => setState(() => values[f.key] = v),
                ),
              );
            }
            return PaseoFormField(
              label: label,
              error: errors[f.key],
              child: TextField(
                controller: controllers[f.key],
                decoration: paseoInputDecoration(
                  context,
                  multiline: f.multiline,
                ).copyWith(errorText: null),
                style: theme.textTheme.bodyMedium!.copyWith(height: 1.4),
                obscureText: f.secret,
                minLines: f.multiline ? 3 : 1,
                maxLines: f.multiline ? 8 : 1,
                enabled: !busy,
              ),
            );
          }

          return SheetContents(
            title: settingsLabel(context, title),
            stretchActions: true,
            actions: [
              PaseoButton(
                label: settingsLabel(context, 'Cancel'),
                expand: true,
                onPressed: busy ? null : () => Navigator.pop(context),
              ),
              PaseoButton(
                label: busy ? 'Saving…' : settingsLabel(context, 'Save'),
                variant: PaseoButtonVariant.primary,
                expand: true,
                loading: busy,
                onPressed: busy ? null : save,
              ),
            ],
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              spacing: 16,
              children: [
                if (description != null)
                  Text(
                    description,
                    style: theme.textTheme.bodyMedium!.copyWith(color: colors.muted),
                  ),
                for (final f in fields) field(f),
                if (error != null)
                  SelectableText(
                    error.toString(),
                    style: theme.textTheme.bodySmall!.copyWith(color: colors.danger),
                  ),
              ],
            ),
          );
        },
      ),
    );
  } finally {
    for (final controller in controllers.values) {
      controller.dispose();
    }
  }
}

Future<void> showSettingsReport(
  BuildContext context,
  String title,
  Object? report,
) => showAdaptiveSheet<void>(
  context: context,
  maxWidth: 760,
  builder: (context) {
    final text = report is String
        ? report
        : const JsonEncoder.withIndent('  ').convert(report);
    return SheetContents(
      title: settingsLabel(context, title),
      actions: [
        PaseoButton(
          label: settingsLabel(context, 'Copy'),
          icon: LucideIcons.copy300,
          size: PaseoControlSize.sm,
          variant: PaseoButtonVariant.outline,
          onPressed: () => Clipboard.setData(ClipboardData(text: text)),
        ),
        PaseoButton(
          label: settingsLabel(context, 'Close'),
          size: PaseoControlSize.sm,
          onPressed: () => Navigator.pop(context),
        ),
      ],
      child: SelectableText(text, style: PaseoTypography.of(context).codeStyle),
    );
  },
);

/// Original `styles/settings.ts` metrics: muted section label, bordered
/// surface1 card, 16px rows separated by 1px top borders. [footer] sits 12px
/// under the card (the original section content gap), e.g. inline alerts.
class SettingsSection extends StatelessWidget {
  final String title;
  final List<Widget> children;
  final String? info;
  final Widget? trailing, footer;
  final bool card, flush;
  const SettingsSection({
    super.key,
    required this.title,
    required this.children,
    this.info,
    this.trailing,
    this.footer,
    this.card = true,
    this.flush = false,
  });
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final type = PaseoTypography.of(context);
    final muted = PaseoColors.of(context).muted;
    return Padding(
      padding: EdgeInsets.only(bottom: flush ? 0 : 24),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Padding(
            padding: const EdgeInsets.only(left: 4, bottom: 12),
            child: Row(
              mainAxisAlignment: MainAxisAlignment.spaceBetween,
              spacing: 8,
              children: [
                Flexible(
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    spacing: 8,
                    children: [
                      Flexible(
                        child: Text(
                          settingsLabel(context, title),
                          style: theme.textTheme.bodySmall?.copyWith(
                            color: muted,
                            fontSize: type.uiSize(12),
                          ),
                        ),
                      ),
                      if (info != null)
                        Tooltip(
                          message: info!,
                          triggerMode: TooltipTriggerMode.tap,
                          waitDuration: Duration.zero,
                          child: Icon(LucideIcons.info300, size: 14, color: muted),
                        ),
                    ],
                  ),
                ),
                ?trailing,
              ],
            ),
          ),
          if (card)
            SettingsCard(children: children)
          else
            Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              spacing: 12,
              children: children,
            ),
          if (footer != null)
            Padding(padding: const EdgeInsets.only(top: 12), child: footer),
        ],
      ),
    );
  }
}

/// Bordered card whose children are separated by the original row border.
/// Loose, non-row children (status text, button wraps) get row padding.
class SettingsCard extends StatelessWidget {
  final List<Widget> children;
  const SettingsCard({super.key, required this.children});

  static bool _isRow(Widget child) =>
      child is SettingsRow ||
      child is SettingsSwitchRow ||
      child is SettingsSelectRow ||
      child is SettingsActionRow ||
      child is Padding;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final border = Theme.of(context).dividerColor;
    final rows = <Widget>[];
    for (final child in children) {
      if (child is Divider || child is SizedBox && child.child == null) continue;
      if (child is LinearProgressIndicator) {
        rows.add(child);
        continue;
      }
      if (rows.isNotEmpty && rows.last is! LinearProgressIndicator) {
        rows.add(Divider(height: 1, thickness: 1, color: border));
      }
      rows.add(
        _isRow(child)
            ? child
            : Padding(
                padding: const EdgeInsets.symmetric(
                  horizontal: 16,
                  vertical: 12,
                ),
                child: Align(alignment: Alignment.centerLeft, child: child),
              ),
      );
    }
    return DecoratedBox(
      decoration: BoxDecoration(
        color: colors.surface1,
        border: Border.all(color: border),
        borderRadius: BorderRadius.circular(8),
      ),
      child: ClipRRect(
        borderRadius: BorderRadius.circular(7),
        child: Material(
          type: MaterialType.transparency,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: rows,
          ),
        ),
      ),
    );
  }
}

/// Original `SettingsRow`: 16px padding, base title with a muted 12px hint
/// (4px above). The label takes a 160px basis; when the control no longer
/// fits beside it the control wraps under the label with a 12px gap.
/// [inline] keeps the original plain `settingsStyles.row` instead: the label
/// flexes beside a shrinkable control and never wraps under it.
class SettingsRow extends StatelessWidget {
  final String label;
  final String? hint, error;
  final Widget? leading, control, labelAccessory;
  final VoidCallback? onTap;
  final int? labelMaxLines;
  final double leadingGap;
  final bool inline;
  const SettingsRow({
    super.key,
    required this.label,
    this.hint,
    this.error,
    this.leading,
    this.control,
    this.labelAccessory,
    this.onTap,
    this.labelMaxLines,
    this.leadingGap = 8,
    this.inline = false,
  });
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final small = theme.textTheme.bodySmall!.copyWith(fontSize: type.uiSize(12));
    final title = Text(
      label,
      maxLines: labelMaxLines,
      overflow: labelMaxLines == null ? null : TextOverflow.ellipsis,
      style: theme.textTheme.bodyMedium!.copyWith(fontSize: type.uiSize(14)),
    );
    Widget text = Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        if (labelAccessory != null)
          Wrap(
            spacing: 8,
            runSpacing: 4,
            crossAxisAlignment: WrapCrossAlignment.center,
            children: [title, labelAccessory!],
          )
        else
          title,
        if (hint != null && hint!.isNotEmpty)
          Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Text(hint!, style: small.copyWith(color: colors.muted)),
          ),
        if (error != null && error!.isNotEmpty)
          Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Text(error!, style: small.copyWith(color: colors.danger)),
          ),
      ],
    );
    if (leading != null) {
      text = Row(
        spacing: leadingGap,
        children: [leading!, Expanded(child: text)],
      );
    }
    final row = Padding(
      padding: const EdgeInsets.all(16),
      child: control == null
          ? Align(alignment: Alignment.centerLeft, child: text)
          : inline
          ? Row(
              children: [
                Expanded(child: text),
                const SizedBox(width: 12),
                Flexible(child: control!),
              ],
            )
          : _WrappingRow(gap: 12, children: [text, control!]),
    );
    if (onTap == null) return row;
    return _PressableRow(onTap: onTap!, child: row);
  }
}

/// Original pressable settings rows (projects list): no ripple, surface2 on
/// hover and surface3 while pressed.
class _PressableRow extends StatefulWidget {
  const _PressableRow({required this.onTap, required this.child});
  final VoidCallback onTap;
  final Widget child;
  @override
  State<_PressableRow> createState() => _PressableRowState();
}

class _PressableRowState extends State<_PressableRow> {
  bool hovered = false, pressed = false;
  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    return Semantics(
      button: true,
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        onEnter: (_) => setState(() => hovered = true),
        onExit: (_) => setState(() => hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: (_) => setState(() => pressed = true),
          onTapCancel: () => setState(() => pressed = false),
          onTapUp: (_) => setState(() => pressed = false),
          onTap: widget.onTap,
          child: ColoredBox(
            color: pressed
                ? colors.surface3
                : hovered
                ? colors.surface2
                : Colors.transparent,
            child: widget.child,
          ),
        ),
      ),
    );
  }
}

/// Lays out `[label, control]` like the original flex-wrap row: the label
/// grows from a 160px basis and the control keeps its natural width; when
/// both no longer fit, the control drops under the label.
class _WrappingRow extends MultiChildRenderObjectWidget {
  const _WrappingRow({required this.gap, required super.children});
  final double gap;
  @override
  RenderObject createRenderObject(BuildContext context) => _RenderWrappingRow(gap);
  @override
  void updateRenderObject(BuildContext context, _RenderWrappingRow renderObject) {
    renderObject.gap = gap;
  }
}

class _WrappingRowParentData extends ContainerBoxParentData<RenderBox> {}

class _RenderWrappingRow extends RenderBox
    with
        ContainerRenderObjectMixin<RenderBox, _WrappingRowParentData>,
        RenderBoxContainerDefaultsMixin<RenderBox, _WrappingRowParentData> {
  _RenderWrappingRow(this._gap);
  static const _labelBasis = 160.0;
  double _gap;
  set gap(double value) {
    if (value == _gap) return;
    _gap = value;
    markNeedsLayout();
  }

  @override
  void setupParentData(RenderBox child) {
    if (child.parentData is! _WrappingRowParentData) {
      child.parentData = _WrappingRowParentData();
    }
  }

  RenderBox get _label => firstChild!;
  RenderBox get _control => lastChild!;

  @override
  double computeMinIntrinsicWidth(double height) => math.max(
    _label.getMinIntrinsicWidth(height),
    _control.getMinIntrinsicWidth(height),
  );
  @override
  double computeMaxIntrinsicWidth(double height) =>
      _label.getMaxIntrinsicWidth(height) +
      _gap +
      _control.getMaxIntrinsicWidth(height);

  @override
  double? computeDistanceToActualBaseline(TextBaseline baseline) =>
      defaultComputeDistanceToFirstActualBaseline(baseline);

  @override
  Size computeDryLayout(BoxConstraints constraints) => _layout(constraints, dry: true);

  @override
  void performLayout() => size = _layout(constraints, dry: false);

  Size _layout(BoxConstraints constraints, {required bool dry}) {
    final width = constraints.maxWidth;
    final controlWidth = math.min(_control.getMaxIntrinsicWidth(double.infinity), width);
    final inline = controlWidth <= width - _labelBasis - _gap;
    if (inline) {
      final controlConstraints = BoxConstraints(maxWidth: controlWidth);
      final labelConstraints = BoxConstraints.tightFor(width: width - controlWidth - _gap);
      final controlSize = dry
          ? _control.getDryLayout(controlConstraints)
          : (_control..layout(controlConstraints, parentUsesSize: true)).size;
      final labelSize = dry
          ? _label.getDryLayout(labelConstraints)
          : (_label..layout(labelConstraints, parentUsesSize: true)).size;
      final height = math.max(controlSize.height, labelSize.height);
      if (!dry) {
        (_label.parentData! as BoxParentData).offset =
            Offset(0, (height - labelSize.height) / 2);
        (_control.parentData! as BoxParentData).offset =
            Offset(width - controlSize.width, (height - controlSize.height) / 2);
      }
      return constraints.constrain(Size(width, height));
    }
    final labelConstraints = BoxConstraints.tightFor(width: width);
    final controlConstraints = BoxConstraints(maxWidth: width);
    final labelSize = dry
        ? _label.getDryLayout(labelConstraints)
        : (_label..layout(labelConstraints, parentUsesSize: true)).size;
    final controlSize = dry
        ? _control.getDryLayout(controlConstraints)
        : (_control..layout(controlConstraints, parentUsesSize: true)).size;
    if (!dry) {
      (_label.parentData! as BoxParentData).offset = Offset.zero;
      (_control.parentData! as BoxParentData).offset = Offset(0, labelSize.height + _gap);
    }
    return constraints.constrain(
      Size(width, labelSize.height + _gap + controlSize.height),
    );
  }

  @override
  void paint(PaintingContext context, Offset offset) => defaultPaint(context, offset);

  @override
  bool hitTestChildren(BoxHitTestResult result, {required Offset position}) =>
      defaultHitTestChildren(result, position: position);
}

/// Original `SettingsSwitch`: a settings row with the original switch.
class SettingsSwitchRow extends StatelessWidget {
  final String label;
  final String? hint;
  final bool value;
  final ValueChanged<bool>? onChanged;
  const SettingsSwitchRow({
    super.key,
    required this.label,
    this.hint,
    required this.value,
    this.onChanged,
  });
  @override
  Widget build(BuildContext context) => SettingsRow(
    label: label,
    hint: hint,
    control: PaseoSwitch(value: value, onChanged: onChanged, semanticLabel: label),
  );
}

/// Original `DropdownTrigger` (sm outline button) opening an end-aligned
/// 220px menu popover, on compact layouts too.
class SettingsDropdown<T> extends StatelessWidget {
  final T value;
  final List<(T, String)> options;
  final ValueChanged<T>? onChanged;
  final Widget Function(T value)? leading;
  final double menuWidth;
  final String? semanticLabel;
  final Set<T> separatorsBefore;
  const SettingsDropdown({
    super.key,
    required this.value,
    required this.options,
    this.onChanged,
    this.leading,
    this.menuWidth = 220,
    this.semanticLabel,
    this.separatorsBefore = const {},
  });
  @override
  Widget build(BuildContext context) => PaseoDropdownTrigger<T>(
    value: value,
    title: semanticLabel ?? '',
    menuWidth: menuWidth,
    onChanged: onChanged,
    options: [
      for (final option in options)
        PaseoSelectOption(
          option.$1,
          option.$2,
          leading: leading?.call(option.$1),
          separatorBefore: separatorsBefore.contains(option.$1),
        ),
    ],
  );
}

class SettingsSelectRow<T> extends StatelessWidget {
  final String label;
  final String? hint;
  final T value;
  final List<(T, String)> options;
  final ValueChanged<T>? onChanged;
  final double menuWidth;
  const SettingsSelectRow({
    super.key,
    required this.label,
    this.hint,
    required this.value,
    required this.options,
    this.onChanged,
    this.menuWidth = 220,
  });
  @override
  Widget build(BuildContext context) => SettingsRow(
    label: label,
    hint: hint,
    control: SettingsDropdown<T>(
      value: value,
      options: options,
      onChanged: onChanged,
      semanticLabel: label,
      menuWidth: menuWidth,
    ),
  );
}

/// Original `SettingsAction`: row with an outline small button.
class SettingsActionRow extends StatelessWidget {
  final String label, actionLabel;
  final String? hint;
  final VoidCallback? onPressed;
  final IconData? icon;
  const SettingsActionRow({
    super.key,
    required this.label,
    required this.actionLabel,
    this.hint,
    this.onPressed,
    this.icon,
  });
  @override
  Widget build(BuildContext context) => SettingsRow(
    label: label,
    hint: hint,
    control: settingsButton(context, actionLabel, onPressed, icon: icon),
  );
}

/// Original settings `<Button variant="outline" size="sm">` (ghost when
/// [ghost]): 32px tall, 12px radius, 14px icon.
Widget settingsButton(
  BuildContext context,
  String label,
  VoidCallback? onPressed, {
  IconData? icon,
  bool ghost = false,
}) => PaseoButton(
  label: label,
  icon: icon,
  onPressed: onPressed,
  size: PaseoControlSize.sm,
  variant: ghost ? PaseoButtonVariant.ghost : PaseoButtonVariant.outline,
);

List<JsonMap> providerEntries(JsonMap payload) {
  final compact = jsonMap(payload['compactSnapshot']);
  if (compact.isEmpty) {
    return jsonList(payload['entries']).map(jsonMap).toList();
  }
  final sets = jsonList(compact['thinkingSets']).map(jsonMap).toList();
  return jsonList(compact['entries']).map((value) {
    final entry = jsonMap(value);
    return <String, dynamic>{
      ...entry,
      'models': jsonList(entry['models']).map((value) {
        final model = jsonMap(value);
        final index = model['thinkingSet'];
        return <String, dynamic>{
          ...model,
          'provider': entry['provider'],
          if (index is int && index >= 0 && index < sets.length)
            'thinkingOptions': sets[index]['options'],
        };
      }).toList(),
    };
  }).toList();
}

class AgentConfigFields extends StatefulWidget {
  final PaseoClient client;
  final String? cwd;
  final JsonMap value;
  final ValueChanged<JsonMap> onChanged;
  final bool features;

  /// The composer toolbar form is the only form left; settings pages use the
  /// original agent-profile and metadata pickers instead.
  final bool compact;
  const AgentConfigFields({
    super.key,
    required this.client,
    this.cwd,
    required this.value,
    required this.onChanged,
    this.features = true,
    this.compact = true,
  }) : assert(compact, 'AgentConfigFields only renders the composer toolbar form');
  @override
  State<AgentConfigFields> createState() => _AgentConfigFieldsState();
}

class _AgentConfigFieldsState extends State<AgentConfigFields> {
  List<JsonMap> entries = [], features = [];
  Object? error;
  bool loading = true;
  int generation = 0, featureGeneration = 0;
  late ConnectionStatus connectionStatus;
  @override
  void initState() {
    super.initState();
    connectionStatus = widget.client.status;
    widget.client.addListener(connectionChanged);
    load();
  }

  void connectionChanged() {
    if (!mounted || connectionStatus == widget.client.status) return;
    connectionStatus = widget.client.status;
    load();
  }

  @override
  void didUpdateWidget(AgentConfigFields old) {
    super.didUpdateWidget(old);
    if (old.client != widget.client) {
      old.client.removeListener(connectionChanged);
      connectionStatus = widget.client.status;
      widget.client.addListener(connectionChanged);
    }
    if (old.client != widget.client || old.cwd != widget.cwd) {
      load();
    } else if ([
      'provider',
      'model',
      'modeId',
      'thinkingOptionId',
    ].any((key) => old.value[key] != widget.value[key])) {
      loadFeatures();
    }
  }

  @override
  void dispose() {
    ++generation;
    ++featureGeneration;
    widget.client.removeListener(connectionChanged);
    super.dispose();
  }

  Future<void> load() async {
    if (!mounted) return;
    final own = ++generation;
    ++featureGeneration;
    setState(() {
      loading = true;
      error = null;
      entries = [];
      features = [];
    });
    try {
      if (widget.client.status != ConnectionStatus.connected) {
        throw StateError(
          'Host is ${widget.client.status.name}. Provider catalogs reload when it reconnects.',
        );
      }
      if (widget.cwd?.isNotEmpty == true &&
          jsonMap(
                widget.client.serverInfo['features'],
              )['providersSnapshotCwd'] !=
              true) {
        throw StateError(
          'This host does not advertise directory-scoped provider catalogs. Update the host before selecting a project-specific configuration.',
        );
      }
      final result = await settingsRequest(
        widget.client,
        'get_providers_snapshot_request',
        {if (widget.cwd?.isNotEmpty == true) 'cwd': widget.cwd},
        responseType: 'get_providers_snapshot_response',
      );
      if (!mounted || own != generation) return;
      setState(() {
        entries = providerEntries(result);
        loading = false;
      });
      await loadFeatures();
    } catch (e) {
      if (mounted && own == generation) {
        setState(() {
          error = e;
          loading = false;
        });
      }
    }
  }

  Future<void> loadFeatures() async {
    if (!mounted) return;
    final own = ++featureGeneration;
    setState(() => features = []);
    if (widget.client.status != ConnectionStatus.connected ||
        !widget.features ||
        widget.value['provider'] == null ||
        widget.value['provider'] == '') {
      return;
    }
    try {
      final result = await settingsRequest(
        widget.client,
        'list_provider_features_request',
        {
          'draftConfig': {...widget.value, 'cwd': widget.cwd ?? '~'},
        },
        responseType: 'list_provider_features_response',
      );
      if (mounted && own == featureGeneration) {
        setState(() {
          features = jsonList(result['features']).map(jsonMap).toList();
          error = null;
        });
      }
    } catch (e) {
      if (mounted && own == featureGeneration) setState(() => error = e);
    }
  }

  @override
  Widget build(BuildContext context) => _compactControls(
    entries.where((e) => e['provider'] == widget.value['provider']).firstOrNull ?? <String, dynamic>{},
  );

  /// Composer toolbar form (new session): the same original model/thinking/
  /// mode controls the conversation composer uses, not settings dropdowns.
  Widget _compactControls(JsonMap selected) {
    final theme = Theme.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        if (error != null)
          Row(
            children: [
              Flexible(
                child: Text(
                  error.toString(),
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: theme.colorScheme.error,
                  ),
                ),
              ),
              PaseoButton(
                icon: LucideIcons.rotateCw300,
                tooltip: 'Retry',
                size: PaseoControlSize.sm,
                variant: PaseoButtonVariant.ghost,
                onPressed: load,
              ),
            ],
          ),
        AgentControls(
          providers: entries,
          config: widget.value,
          modes: jsonList(selected['modes']).map(jsonMap).toList(),
          features: features,
          profiles: const [],
          canSwitchProvider: true,
          disabled: loading,
          onChanged: widget.onChanged,
          loadFeatures: widget.features
              ? (config) async {
                  if (stringValue(config['provider']).isEmpty) return const [];
                  final result = await settingsRequest(
                    widget.client,
                    'list_provider_features_request',
                    {
                      'draftConfig': {...config, 'cwd': widget.cwd ?? '~'},
                    },
                    responseType: 'list_provider_features_response',
                  );
                  return jsonList(result['features']).map(jsonMap).toList();
                }
              : null,
        ),
      ],
    );
  }
}
