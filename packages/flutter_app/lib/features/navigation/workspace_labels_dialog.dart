import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/app_store.dart';
import '../../core/json.dart';
import '../../ui/common.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import 'navigation_actions.dart';

/// Original label identity colors (`styles/identity-colors.ts`, foreground variants).
const _labelColors = [
  'violet', 'sky', 'emerald', 'orange', 'pink',
  'indigo', 'teal', 'red', 'amber', 'blue',
];
const _foregroundDark = {
  'violet': Color(0xffa392d5), 'sky': Color(0xff6aa6ce), 'emerald': Color(0xff6cae96),
  'orange': Color(0xffcc8f64), 'pink': Color(0xffd87da3), 'indigo': Color(0xff9299d5),
  'teal': Color(0xff6cacab), 'red': Color(0xffd88381), 'amber': Color(0xffb29d64),
  'blue': Color(0xff7ba1d5),
};
const _foregroundLight = {
  'violet': Color(0xff6d49b5), 'sky': Color(0xff3d6985), 'emerald': Color(0xff3e6e5d),
  'orange': Color(0xff845838), 'pink': Color(0xff974168), 'indigo': Color(0xff5251c2),
  'teal': Color(0xff3e6d6c), 'red': Color(0xff9c4243), 'amber': Color(0xff716239),
  'blue': Color(0xff39649e),
};

Color labelColor(BuildContext context, String name) =>
    (Theme.of(context).brightness == Brightness.dark ? _foregroundDark : _foregroundLight)[name] ??
    PaseoColors.of(context).muted;

/// The original workspace label menu: assign (multi-select), create, and manage pages.
Future<void> showWorkspaceLabels(
  BuildContext context, {
  required AppStore store,
  required String serverId,
  required JsonMap workspace,
}) => showAdaptiveSheet<void>(
  context: context,
  maxWidth: 400,
  builder: (_) => WorkspaceLabelsDialog(
    store: store,
    serverId: serverId,
    workspace: workspace,
  ),
);

/// Original `WorkspaceLabelManagerModal` (display menu "Manage labels…"): the label
/// catalog of one host, opened on its manage page with no workspace to assign to.
Future<void> showWorkspaceLabelManager(
  BuildContext context, {
  required AppStore store,
  required String serverId,
}) => showAdaptiveSheet<void>(
  context: context,
  maxWidth: 400,
  builder: (_) => WorkspaceLabelsDialog(store: store, serverId: serverId),
);

class WorkspaceLabelsDialog extends StatefulWidget {
  const WorkspaceLabelsDialog({
    super.key,
    required this.store,
    required this.serverId,
    this.workspace,
  });
  final AppStore store;
  final String serverId;

  /// The workspace whose labels the pick page assigns; null opens the manager alone.
  final JsonMap? workspace;
  @override
  State<WorkspaceLabelsDialog> createState() => _WorkspaceLabelsDialogState();
}

enum _Page { pick, create, manage, edit }

class _WorkspaceLabelsDialogState extends State<WorkspaceLabelsDialog> {
  List<JsonMap> _labels = [];
  late Set<String> _assigned = (widget.workspace?.array('labels') ?? const [])
      .map((item) => item.toString())
      .toSet();
  final _pending = <String>{};
  bool _busy = false;
  String? _error;
  late _Page _page = widget.workspace == null ? _Page.manage : _Page.pick;
  JsonMap? _editing;
  final _name = TextEditingController();
  String _color = 'violet';
  String _query = '';

  @override
  void initState() {
    super.initState();
    _run(_load);
  }

  @override
  void dispose() {
    _name.dispose();
    super.dispose();
  }

  bool get _offline => widget.store.hostFor(widget.serverId)?.live != true;

  Future<void> _load() async {
    final response = await navigationRequest(
      widget.store,
      widget.serverId,
      'workspace.label.list.request',
      {},
      'workspace.label.list.response',
    );
    if (mounted) {
      setState(() => _labels = response.array('labels').map(jsonMap).toList());
    }
  }

  Future<void> _run(Future<void> Function() action) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await action();
    } catch (error) {
      if (mounted) setState(() => _error = error.toString());
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _assign(JsonMap label, bool assigned) async {
    final key = label.text('name').toLowerCase();
    setState(() {
      _pending.add(key);
      _error = null;
    });
    try {
      final response = await navigationRequest(
        widget.store,
        widget.serverId,
        'workspace.label.assignment.set.request',
        {
          'workspaceId': widget.workspace!.text('id'),
          'label': {'name': label.text('name'), 'color': label.text('color')},
          'assigned': assigned,
        },
        'workspace.label.assignment.set.response',
      );
      if (mounted) {
        setState(
          () => _assigned = response
              .array('workspaceLabels')
              .map((item) => item.toString())
              .toSet(),
        );
      }
      await _load();
      await widget.store.refreshHost(widget.serverId);
    } catch (error) {
      if (mounted) setState(() => _error = tr(context, 'workspaceLabels.errors.update'));
    } finally {
      if (mounted) setState(() => _pending.remove(key));
    }
  }

  void _open(_Page page, [JsonMap? label]) => setState(() {
    _page = page;
    _editing = label;
    _error = null;
    _name.text = label?.text('name') ?? '';
    _color = label?.text('color') ?? 'violet';
  });

  String get _normalizedName => _name.text.trim().replaceAll(RegExp(r'\s+'), ' ');

  Future<void> _create() async {
    if (_normalizedName.isEmpty) return;
    await _assign({'name': _normalizedName, 'color': _color}, true);
    if (mounted && _error == null) _open(_Page.pick);
  }

  Future<void> _save() async {
    final label = _editing!;
    if (_normalizedName.isEmpty) return;
    await navigationRequest(
      widget.store,
      widget.serverId,
      'workspace.label.update.request',
      {
        'name': label.text('name'),
        'newName': _normalizedName,
        'color': _color,
      },
      'workspace.label.update.response',
    );
    if (_assigned.remove(label.text('name'))) _assigned.add(_normalizedName);
    await _load();
    await widget.store.refreshHost(widget.serverId);
    if (mounted) _open(_Page.manage);
  }

  Future<void> _delete(JsonMap label) async {
    final name = label.text('name');
    final impact = await navigationRequest(
      widget.store,
      widget.serverId,
      'workspace.label.delete.inspect.request',
      {'name': name},
      'workspace.label.delete.inspect.response',
    );
    if (!mounted) return;
    final count = impact.integer('affectedWorkspaceCount');
    if (!await confirmAction(
      context,
      title: tr(context, 'workspaceLabels.manage.deleteTitle', args: {'name': name}),
      message: tr(
        context,
        count == 1
            ? 'workspaceLabels.manage.deleteMessage_one'
            : 'workspaceLabels.manage.deleteMessage_other',
        args: {'count': count},
      ),
      confirmLabel: tr(context, 'workspaceLabels.manage.delete'),
      destructive: true,
    )) {
      return;
    }
    await navigationRequest(
      widget.store,
      widget.serverId,
      'workspace.label.delete.request',
      {'name': name},
      'workspace.label.delete.response',
    );
    _assigned.remove(name);
    await _load();
    await widget.store.refreshHost(widget.serverId);
    if (mounted) _open(_Page.manage);
  }

  Widget _dot(String color) => Container(
    width: 10,
    height: 10,
    decoration: BoxDecoration(shape: BoxShape.circle, color: labelColor(context, color)),
  );

  /// A menu row: leading slot, label, trailing check or spinner.
  Widget _item({
    Widget? leading,
    required String label,
    Widget? trailing,
    VoidCallback? onTap,
    Color? color,
  }) {
    final colors = PaseoColors.of(context);
    return InkWell(
      borderRadius: BorderRadius.circular(6),
      hoverColor: colors.surface2,
      onTap: onTap,
      child: ConstrainedBox(
        constraints: BoxConstraints(minHeight: isCompactLayout(context) ? 44 : 32),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 12),
          child: Row(
            spacing: 10,
            children: [
              SizedBox(width: 16, child: Center(child: leading)),
              Expanded(
                child: Text(
                  label,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: Theme.of(context).textTheme.bodyMedium!.copyWith(
                    color: onTap == null ? colors.muted : color,
                  ),
                ),
              ),
              ?trailing,
            ],
          ),
        ),
      ),
    );
  }

  Widget _hint(String text, {bool error = false}) => Padding(
    padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
    child: Text(
      text,
      style: Theme.of(context).textTheme.bodySmall!.copyWith(
        color: error ? PaseoColors.of(context).danger : PaseoColors.of(context).muted,
      ),
    ),
  );

  Widget _separator() => Padding(
    padding: const EdgeInsets.symmetric(vertical: 4),
    child: Divider(height: 1, color: Theme.of(context).dividerColor),
  );

  Widget _swatches() => Padding(
    padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
    child: Wrap(
      spacing: 12,
      runSpacing: 12,
      children: [
        for (final color in _labelColors)
          Tooltip(
            message: tr(context, 'workspaceLabels.colors.$color'),
            child: InkWell(
              customBorder: const CircleBorder(),
              onTap: _offline || _busy ? null : () => setState(() => _color = color),
              child: Container(
                width: 20,
                height: 20,
                decoration: BoxDecoration(
                  shape: BoxShape.circle,
                  color: labelColor(context, color),
                  border: _color == color
                      ? Border.all(color: Theme.of(context).colorScheme.onSurface, width: 2)
                      : null,
                ),
              ),
            ),
          ),
      ],
    ),
  );

  Widget _nameField() => Padding(
    padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 4),
    child: TextField(
      controller: _name,
      autofocus: true,
      enabled: !_offline && !_busy,
      style: Theme.of(context).textTheme.bodyMedium,
      decoration: InputDecoration(
        hintText: tr(context, _page == _Page.edit
            ? 'workspaceLabels.manage.name'
            : 'workspaceLabels.name'),
        isDense: true,
      ),
      onChanged: (_) => setState(() {}),
      onSubmitted: (_) => _run(_page == _Page.edit ? _save : _create),
    ),
  );

  Widget _spinner() => const SizedBox.square(
    dimension: 14,
    child: CircularProgressIndicator(strokeWidth: 1.5),
  );

  List<Widget> _pickPage() {
    final colors = PaseoColors.of(context);
    return [
      for (final label in _labels)
        _item(
          leading: _dot(label.text('color')),
          label: label.text('name'),
          trailing: _pending.contains(label.text('name').toLowerCase())
              ? _spinner()
              : _assigned.contains(label.text('name'))
              ? Icon(LucideIcons.check300, size: 16, color: Theme.of(context).colorScheme.onSurface)
              : null,
          onTap: _offline
              ? null
              : () => _assign(label, !_assigned.contains(label.text('name'))),
        ),
      if (_labels.isNotEmpty) _separator(),
      _item(
        leading: Icon(LucideIcons.plus300, size: 16, color: colors.muted),
        label: tr(context, 'workspaceLabels.create'),
        trailing: Icon(LucideIcons.chevronRight300, size: 16, color: colors.muted),
        onTap: _offline ? null : () => _open(_Page.create),
      ),
      _item(
        leading: Icon(LucideIcons.slidersHorizontal300, size: 16, color: colors.muted),
        label: tr(context, 'workspaceLabels.manage.open'),
        onTap: () => _open(_Page.manage),
      ),
    ];
  }

  List<Widget> _formPage({required bool edit}) {
    final pending = _busy;
    return [
      _nameField(),
      _swatches(),
      _separator(),
      _item(
        label: pending
            ? tr(context, 'workspaceLabels.creating')
            : tr(context, edit ? 'workspaceLabels.manage.save' : 'workspaceLabels.createConfirm'),
        trailing: pending ? _spinner() : null,
        onTap: _offline || pending || _normalizedName.isEmpty
            ? null
            : () => _run(edit ? _save : _create),
      ),
      if (edit)
        _item(
          label: tr(context, 'workspaceLabels.manage.delete'),
          color: Theme.of(context).colorScheme.error,
          onTap: _offline || pending ? null : () => _run(() => _delete(_editing!)),
        ),
    ];
  }

  List<Widget> _managePage() {
    final colors = PaseoColors.of(context);
    final labels = _labels
        .where((label) => label.text('name').toLowerCase().contains(_query))
        .toList();
    return [
      Padding(
        padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 4),
        child: TextField(
          style: Theme.of(context).textTheme.bodyMedium,
          decoration: InputDecoration(
            hintText: tr(context, 'workspaceLabels.manage.search'),
            prefixIcon: Icon(LucideIcons.search300, size: 16, color: colors.muted),
            isDense: true,
          ),
          onChanged: (value) => setState(() => _query = value.trim().toLowerCase()),
        ),
      ),
      if (_labels.isEmpty) _hint(tr(context, 'workspaceLabels.manage.empty')),
      for (final label in labels)
        _item(
          leading: _dot(label.text('color')),
          label: label.text('name'),
          trailing: Icon(LucideIcons.pencil300, size: 14, color: colors.muted),
          onTap: _offline ? null : () => _open(_Page.edit, label),
        ),
    ];
  }

  @override
  Widget build(BuildContext context) {
    final (title, children) = switch (_page) {
      _Page.pick => (tr(context, 'workspaceLabels.title'), _pickPage()),
      _Page.create => (tr(context, 'workspaceLabels.create'), _formPage(edit: false)),
      _Page.manage => (tr(context, 'workspaceLabels.manage.title'), _managePage()),
      _Page.edit => (
        tr(context, 'workspaceLabels.manage.editLabel', args: {
          'name': _editing?.text('name') ?? '',
        }),
        _formPage(edit: true),
      ),
    };
    return SheetContents(
      title: title,
      subtitle: _page == _Page.pick && (widget.workspace?.text('name') ?? '').isNotEmpty
          ? Text(widget.workspace!.text('name'))
          : null,
      onBack: switch (_page) {
        _Page.pick => null,
        _Page.edit => () => _open(_Page.manage),
        // The standalone manager has no pick page to return to.
        _Page.manage when widget.workspace == null => null,
        _ => () => _open(_Page.pick),
      },
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          if (_busy && _labels.isEmpty && _page == _Page.pick)
            const LinearProgressIndicator(minHeight: 2),
          ...children,
          if (_offline) _hint(tr(context, 'workspaceLabels.manage.offline')),
          if (_error != null) _hint(_error!, error: true),
        ],
      ),
    );
  }
}
