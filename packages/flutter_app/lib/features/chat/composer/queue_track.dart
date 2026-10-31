import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../../core/json.dart';
import '../../../ui/localization.dart';
import '../../../ui/theme.dart';

/// composer/index.tsx renderQueueTrack: queued messages stacked 8px apart
/// directly above the composer (inside its 12px-gap input area).
class ComposerQueueTrack extends StatelessWidget {
  const ComposerQueueTrack({
    super.key,
    required this.queue,
    required this.onEdit,
    required this.onSendNow,
    required this.onRemove,
    required this.sending,
  });

  final List<JsonMap> queue;
  final ValueChanged<JsonMap> onEdit, onSendNow, onRemove;
  final bool sending;

  @override
  Widget build(BuildContext context) => Column(
    mainAxisSize: MainAxisSize.min,
    crossAxisAlignment: CrossAxisAlignment.stretch,
    spacing: 8,
    children: [
      for (final item in queue)
        QueuedMessageRow(
          key: ValueKey(item['messageId']),
          item: item,
          sending: sending,
          onEdit: () => onEdit(item),
          onSendNow: () => onSendNow(item),
          onRemove: () => onRemove(item),
        ),
    ],
  );
}

/// composer/index.tsx QueuedMessageRow + styles.queueItem: 12/8 insets,
/// surface1, 8px radius, 1px border, 8px gaps; two-line foreground text; a
/// 32px surface2 Pencil (edit: back into the composer) and a 32px accent
/// ArrowUp (send now), both 14px glyphs.
///
/// The client's durable delivery adds states the original never had: a
/// message that is sending, unacknowledged, blocked or rejected keeps a small
/// muted status line, and only an unacknowledged one can be dropped (it can
/// no longer be edited, since the daemon may have received it).
class QueuedMessageRow extends StatelessWidget {
  const QueuedMessageRow({
    super.key,
    required this.item,
    required this.sending,
    required this.onEdit,
    required this.onSendNow,
    required this.onRemove,
  });

  final JsonMap item;
  final bool sending;
  final VoidCallback onEdit, onSendNow, onRemove;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final palette = PaseoColors.of(context);
    final text = Theme.of(context).textTheme;
    final state = stringValue(item['state'], 'queued');
    final editable = const ['queued', 'blocked', 'rejected'].contains(state);
    final error = item['error'];
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
      decoration: BoxDecoration(
        color: palette.surface1,
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: colors.outline),
      ),
      child: Row(
        spacing: 8,
        children: [
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              mainAxisSize: MainAxisSize.min,
              children: [
                Text(
                  stringValue(item['text']),
                  maxLines: 2,
                  overflow: TextOverflow.ellipsis,
                  style: text.bodyMedium,
                ),
                if (state != 'queued')
                  Text(
                    '$state${error == null ? '' : ': $error'}',
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: text.bodySmall!.copyWith(
                      color: state == 'rejected' ? colors.error : colors.onSurfaceVariant,
                    ),
                  ),
              ],
            ),
          ),
          Row(
            mainAxisSize: MainAxisSize.min,
            spacing: 8,
            children: [
              if (state == 'unacknowledged')
                _QueueAction(
                  label: tr(context, 'common.actions.dismiss'),
                  background: palette.surface2,
                  icon: LucideIcons.x300,
                  color: colors.onSurface,
                  onPressed: onRemove,
                )
              else
                _QueueAction(
                  label: tr(context, 'composer.attachments.editQueuedMessage'),
                  background: palette.surface2,
                  icon: LucideIcons.pencil300,
                  color: colors.onSurface,
                  onPressed: editable ? onEdit : null,
                ),
              _QueueAction(
                label: tr(context, 'composer.attachments.sendQueuedMessageNow'),
                background: palette.accent,
                icon: LucideIcons.arrowUp300,
                color: colors.onTertiary,
                onPressed: sending || state == 'sending' ? null : onSendNow,
              ),
            ],
          ),
        ],
      ),
    );
  }
}

class _QueueAction extends StatelessWidget {
  const _QueueAction({
    required this.label,
    required this.background,
    required this.icon,
    required this.color,
    required this.onPressed,
  });
  final String label;
  final Color background, color;
  final IconData icon;
  final VoidCallback? onPressed;

  @override
  Widget build(BuildContext context) => Semantics(
    button: true,
    enabled: onPressed != null,
    label: label,
    excludeSemantics: true,
    child: Opacity(
      opacity: onPressed == null ? .5 : 1,
      child: Material(
        color: background,
        shape: const CircleBorder(),
        clipBehavior: Clip.antiAlias,
        child: InkWell(
          onTap: onPressed,
          child: SizedBox.square(
            dimension: 32,
            child: Icon(icon, size: 14, color: color),
          ),
        ),
      ),
    ),
  );
}
