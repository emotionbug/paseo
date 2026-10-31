import 'dart:async';

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../../core/client.dart';
import '../../../core/json.dart';
import '../../../ui/common.dart';
import '../../../ui/localization.dart';
import '../../../ui/theme.dart';
import '../chat_controller.dart' show maps, requireSuccess;
import '../../git/git_controller.dart' show checkoutPayload;
import 'attachment_files.dart' show usesLegacyForge;

/// index.tsx useCheckoutPrStatusQuery → getForgePresentation: the forge the
/// daemon resolved for the checkout (`checkout_pr_status_response.forge`); an
/// old daemon that never sends one means GitHub. Like the original query, a
/// payload carrying a domain `error` still names its forge.
Future<String> loadCheckoutForge(PaseoClient client, String cwd) async {
  final result = await checkoutPayload(client, 'checkout_pr_status_request', {'cwd': cwd});
  final forge = stringValue(result['forge']);
  return forge.isEmpty ? 'github' : forge;
}

/// Stable identity of a forge search item or forge attachment
/// (index.tsx `${kind}:${number}`).
String forgeItemKey(JsonMap item) {
  final kind = item['kind'] ?? item['type'];
  final change = kind == 'change_request' ||
      kind == 'pr' ||
      kind == 'github_pr' ||
      kind == 'forge_change_request';
  return '${change ? 'change_request' : 'issue'}:${item['number']}';
}

/// use-forge-search-query.ts buildForgeSearchQueryOptions: up to 20 issues and
/// change requests matching [query] in [cwd]'s checkout. Hosts without
/// forgeSearch answer the legacy github_search RPC, whose `pr` items normalize
/// to `change_request`. Nothing is searched without a directory.
Future<List<JsonMap>> searchForgeItems(PaseoClient client, String cwd, String query) async {
  if (cwd.trim().isEmpty) return const [];
  final legacy = usesLegacyForge(client);
  final result = requireSuccess(
    await client.request(
      legacy ? 'github_search_request' : 'forge.search.request',
      {'cwd': cwd, 'query': query.trim(), 'limit': 20},
      responseType: legacy ? 'github_search_response' : 'forge.search.response',
    ),
  );
  return [
    for (final item in maps(result['items']))
      if (!legacy) item
      else {...item, 'kind': item['kind'] == 'pr' ? 'change_request' : 'issue'},
  ];
}

/// composer/index.tsx forge picker: the searchable Combobox titled "Attach
/// issue or PR" — a popover above the attach button on desktop (surface0, 8px
/// radius, 1px border, md shadow, ≤400 tall), a bottom sheet on compact
/// layouts. Rows are "#N title" with a 14px muted GitPullRequest/CircleDot,
/// a check on items already attached; choosing one toggles it and closes.
/// Resolves with the chosen search item.
Future<JsonMap?> showForgePicker(
  BuildContext context, {
  required PaseoClient client,
  required String cwd,
  required Set<String> attachedKeys,
  Rect? anchor,
  bool mergeRequests = false,
}) {
  Widget body(BuildContext sheet) => _ForgePicker(
    client: client,
    cwd: cwd,
    attachedKeys: attachedKeys,
    mergeRequests: mergeRequests,
    showTitle: isCompactLayout(sheet),
  );
  if (isCompactLayout(context) || anchor == null) {
    return showAdaptiveSheet<JsonMap>(context: context, builder: body);
  }
  final colors = PaseoColors.of(context);
  final dark = Theme.of(context).brightness == Brightness.dark;
  return showGeneralDialog<JsonMap>(
    context: context,
    barrierDismissible: true,
    barrierLabel: MaterialLocalizations.of(context).modalBarrierDismissLabel,
    barrierColor: Colors.transparent,
    transitionDuration: const Duration(milliseconds: 120),
    transitionBuilder: (_, animation, _, child) => FadeTransition(opacity: animation, child: child),
    pageBuilder: (popover, _, _) {
      final size = MediaQuery.sizeOf(popover);
      final width = 400.0.clamp(0.0, size.width - 16);
      return Stack(
        children: [
          Positioned(
            left: anchor.left.clamp(8.0, size.width - width - 8),
            bottom: size.height - anchor.top + 8,
            width: width,
            child: Material(
              color: colors.surface0,
              shape: RoundedRectangleBorder(
                borderRadius: BorderRadius.circular(8),
                side: BorderSide(color: Theme.of(popover).colorScheme.outline),
              ),
              clipBehavior: Clip.antiAlias,
              child: DecoratedBox(
                decoration: BoxDecoration(
                  boxShadow: [
                    BoxShadow(
                      color: dark ? const Color(0x33000000) : const Color(0x0a000000),
                      offset: const Offset(0, 4),
                      blurRadius: dark ? 8 : 16,
                    ),
                  ],
                ),
                child: ConstrainedBox(
                  constraints: BoxConstraints(maxHeight: (anchor.top - 16).clamp(120.0, 400.0)),
                  child: body(popover),
                ),
              ),
            ),
          ),
        ],
      );
    },
  );
}

class _ForgePicker extends StatefulWidget {
  const _ForgePicker({
    required this.client,
    required this.cwd,
    required this.attachedKeys,
    required this.mergeRequests,
    required this.showTitle,
  });

  final PaseoClient client;
  final String cwd;
  final Set<String> attachedKeys;
  final bool mergeRequests, showTitle;

  @override
  State<_ForgePicker> createState() => _ForgePickerState();
}

class _ForgePickerState extends State<_ForgePicker> {
  final query = TextEditingController();
  Timer? debounce;
  List<JsonMap> items = [];
  Object? error;
  bool fetching = true;
  int generation = 0;

  @override
  void initState() {
    super.initState();
    query.addListener(() {
      debounce?.cancel();
      debounce = Timer(const Duration(milliseconds: 250), _search);
      setState(() {});
    });
    _search();
  }

  Future<void> _search() async {
    final own = ++generation;
    setState(() {
      fetching = true;
      error = null;
    });
    try {
      final result = await searchForgeItems(widget.client, widget.cwd, query.text);
      if (mounted && own == generation) setState(() => items = result);
    } catch (e) {
      if (mounted && own == generation) setState(() => error = e);
    } finally {
      if (mounted && own == generation) setState(() => fetching = false);
    }
  }

  @override
  void dispose() {
    debounce?.cancel();
    query.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final palette = PaseoColors.of(context);
    final text = Theme.of(context).textTheme;
    final base = text.bodyMedium!;
    final suffix = widget.mergeRequests ? '_mr' : '';
    final trimmed = query.text.trim();
    final search = Container(
      padding: const EdgeInsets.symmetric(horizontal: 12),
      decoration: BoxDecoration(
        color: palette.surface1,
        border: Border(bottom: BorderSide(color: colors.outline)),
      ),
      child: Row(
        spacing: 8,
        children: [
          Icon(LucideIcons.search300, size: 16, color: colors.onSurfaceVariant),
          Expanded(
            child: TextField(
              controller: query,
              autofocus: true,
              style: base,
              decoration: InputDecoration(
                hintText: tr(context, 'composer.github.searchPlaceholder$suffix'),
                hintStyle: base.copyWith(color: colors.onSurfaceVariant),
                filled: false,
                isDense: true,
                border: InputBorder.none,
                enabledBorder: InputBorder.none,
                focusedBorder: InputBorder.none,
                contentPadding: const EdgeInsets.symmetric(vertical: 12),
              ),
            ),
          ),
        ],
      ),
    );
    Widget empty(String value) => Padding(
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
      child: Text(value, style: base.copyWith(color: colors.onSurfaceVariant)),
    );
    final list = error != null
        ? empty('$error')
        : items.isEmpty
        ? empty(tr(context, fetching ? 'composer.github.searching' : 'composer.github.noResults'))
        : ListView(
            shrinkWrap: true,
            padding: const EdgeInsets.symmetric(vertical: 4),
            children: [
              for (final item in items) _row(context, item, trimmed),
            ],
          );
    final content = Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [search, Flexible(child: list)],
    );
    if (!widget.showTitle) return content;
    return SheetContents(
      title: tr(context, 'composer.github.title$suffix'),
      scroll: false,
      child: content,
    );
  }

  Widget _row(BuildContext context, JsonMap item, String query) {
    final colors = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    final change = forgeItemKey(item).startsWith('change_request');
    final prefix = change && item['forge'] == 'gitlab' ? '!' : '#';
    return _PickerRow(
      leading: Icon(change ? LucideIcons.gitPullRequest300 : LucideIcons.circleDot300,
          size: 14, color: colors.onSurfaceVariant),
      label: '$prefix${item['number']} ${item['title'] ?? ''}',
      description: query.isEmpty ? null : query,
      labelStyle: text.bodyMedium!,
      descriptionStyle: text.bodySmall!.copyWith(color: colors.onSurfaceVariant),
      selected: widget.attachedKeys.contains(forgeItemKey(item)),
      onTap: () => Navigator.pop(context, item),
    );
  }
}

/// combobox.tsx ComboboxItem: 36px min, 12/8 insets, 8px gaps, 16px leading
/// slot, base label then inline small muted description, trailing 16px
/// muted check; hovered/pressed rows on surface1.
class _PickerRow extends StatefulWidget {
  const _PickerRow({
    required this.leading,
    required this.label,
    required this.labelStyle,
    required this.descriptionStyle,
    required this.selected,
    required this.onTap,
    this.description,
  });
  final Widget leading;
  final String label;
  final String? description;
  final TextStyle labelStyle, descriptionStyle;
  final bool selected;
  final VoidCallback onTap;

  @override
  State<_PickerRow> createState() => _PickerRowState();
}

class _PickerRowState extends State<_PickerRow> {
  bool active = false;

  @override
  Widget build(BuildContext context) {
    final palette = PaseoColors.of(context);
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => active = true),
      onExit: (_) => setState(() => active = false),
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTapDown: (_) => setState(() => active = true),
        onTapCancel: () => setState(() => active = false),
        onTap: widget.onTap,
        child: Container(
          constraints: const BoxConstraints(minHeight: 36),
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
          color: active ? palette.surface1 : null,
          child: Row(
            spacing: 8,
            children: [
              SizedBox(width: 16, child: Center(child: widget.leading)),
              Expanded(
                child: Row(
                  spacing: 8,
                  crossAxisAlignment: CrossAxisAlignment.baseline,
                  textBaseline: TextBaseline.alphabetic,
                  children: [
                    Flexible(
                      flex: 3,
                      child: Text(widget.label, maxLines: 1, overflow: TextOverflow.ellipsis,
                          style: widget.labelStyle),
                    ),
                    if (widget.description != null)
                      Flexible(
                        child: Text(widget.description!, maxLines: 1,
                            overflow: TextOverflow.ellipsis, style: widget.descriptionStyle),
                      ),
                  ],
                ),
              ),
              if (widget.selected)
                SizedBox(
                  width: 16,
                  child: Icon(LucideIcons.check300, size: 16,
                      color: Theme.of(context).colorScheme.onSurfaceVariant),
                ),
            ],
          ),
        ),
      ),
    );
  }
}
