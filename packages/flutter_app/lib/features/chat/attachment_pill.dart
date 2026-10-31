import 'dart:convert';

import 'package:flutter/foundation.dart' show defaultTargetPlatform;
import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/json.dart';
import '../../ui/common.dart';
import '../../ui/theme.dart';
import '../../ui/localization.dart';

/// components/attachment-pill.tsx: every attachment body is 48px high inside
/// a 6px-radius accent-bordered frame; removable pills carry a 24px close disc
/// overhanging the top-left corner (always shown on native or compact
/// layouts, otherwise while the body or disc is hovered; no transition).
class AttachmentPill extends StatefulWidget {
  const AttachmentPill({
    super.key,
    required this.child,
    this.onOpen,
    this.onRemove,
    this.tooltip,
    this.openLabel,
    this.removeLabel,
    this.disabled = false,
  });

  final Widget child;
  final VoidCallback? onOpen, onRemove;
  final String? tooltip;

  /// openAccessibilityLabel / removeAccessibilityLabel.
  final String? openLabel, removeLabel;
  final bool disabled;

  /// How far the remove disc overhangs the frame's top-left corner; a
  /// removable pill includes this band in its own layout box.
  static const double removableOverhang = 8;

  /// AttachmentLabel: 48px high, max 260 wide, 12px insets, 8px gap, surface1;
  /// an 18px icon slot (14px muted glyph), then the one-line base title over
  /// the one-line small muted subtitle.
  static Widget label(BuildContext context, JsonMap attachment) {
    final colors = Theme.of(context).colorScheme;
    final theme = Theme.of(context).textTheme;
    final (title, subtitle, icon) = describe(attachment);
    return Container(
      height: 48,
      constraints: const BoxConstraints(maxWidth: 260),
      padding: const EdgeInsets.symmetric(horizontal: 12),
      color: PaseoColors.of(context).surface1,
      child: Row(
        mainAxisSize: MainAxisSize.min,
        spacing: 8,
        children: [
          SizedBox(width: 18, child: Icon(icon, size: 14, color: colors.onSurfaceVariant)),
          Flexible(
            child: Column(
              mainAxisAlignment: MainAxisAlignment.center,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(title, maxLines: 1, overflow: TextOverflow.ellipsis, style: theme.bodyMedium),
                Text(
                  subtitle ?? tr(context, 'message.attachments.file'),
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: theme.bodySmall!.copyWith(color: colors.onSurfaceVariant),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }

  /// index.tsx pendingFilePillIcon: an uploading file's label body with an
  /// 18px muted spinner in place of the file glyph.
  static Widget pendingLabel(BuildContext context, String fileName) {
    final colors = Theme.of(context).colorScheme;
    final theme = Theme.of(context).textTheme;
    return Container(
      height: 48,
      constraints: const BoxConstraints(maxWidth: 260),
      padding: const EdgeInsets.symmetric(horizontal: 12),
      color: PaseoColors.of(context).surface1,
      child: Row(
        mainAxisSize: MainAxisSize.min,
        spacing: 8,
        children: [
          SizedBox.square(
            dimension: 18,
            child: CircularProgressIndicator(strokeWidth: 2, color: colors.onSurfaceVariant),
          ),
          Flexible(
            child: Column(
              mainAxisAlignment: MainAxisAlignment.center,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(fileName, maxLines: 1, overflow: TextOverflow.ellipsis, style: theme.bodyMedium),
                Text(
                  fileTypeLabel(fileName) ?? '',
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: theme.bodySmall!.copyWith(color: colors.onSurfaceVariant),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }

  /// git/forge.ts getForgePresentation: GitLab says "MR" and numbers merge
  /// requests with "!"; every other forge says "PR" and uses "#".
  static (String abbrev, String prefix) forgeChangeRequest(Object? forge) =>
      forge == 'gitlab' ? ('MR', '!') : ('PR', '#');

  /// Accessibility kind and number label for a forge attachment
  /// (composer.attachments.openGithub / removeGithub arguments).
  static ({String kind, String number})? forgeLabels(JsonMap attachment) {
    final type = attachment['kind'] ?? attachment['type'];
    final change = type == 'github_pr' || type == 'forge_change_request';
    if (!change && type != 'github_issue' && type != 'forge_issue') return null;
    final (abbrev, prefix) = forgeChangeRequest(attachment['forge']);
    return (
      kind: change ? abbrev : 'issue',
      number: '${change ? prefix : '#'}${attachment['number'] ?? ''}',
    );
  }

  /// Image source for an inline (base64 or data URI) or remote image; null
  /// when the payload carries neither or its bytes are not valid base64.
  static ImageProvider? provider(JsonMap image) {
    var data = stringValue(image['data']);
    if (data.startsWith('data:')) {
      final comma = data.indexOf(',');
      data = comma < 0 ? '' : data.substring(comma + 1);
    }
    if (data.isNotEmpty) {
      final cached = _inline.remove(data);
      if (cached != null) return _inline[data] = cached;
      try {
        final provider = MemoryImage(base64Decode(base64.normalize(data.replaceAll(RegExp(r'\s'), ''))));
        if (_inline.length >= _inlineLimit) _inline.remove(_inline.keys.first);
        return _inline[data] = provider;
      } on FormatException {
        return null;
      }
    }
    final url = stringValue(image['url'], stringValue(image['uri']));
    if (url.startsWith('http://') || url.startsWith('https://')) return NetworkImage(url);
    return null;
  }

  /// One provider per inline payload, least recently used first out. A fresh
  /// MemoryImage is a new image-cache key, so every rebuild of the composer
  /// or a sent message decoded each thumbnail again and blanked it meanwhile.
  static final Map<String, ImageProvider> _inline = {};
  static const _inlineLimit = 32;

  /// 48px square thumbnail for an inline (base64) or remote image.
  static Widget thumbnail(BuildContext context, JsonMap image) {
    Widget fallback() => Container(
      width: 48,
      height: 48,
      color: PaseoColors.of(context).surface1,
      child: Icon(LucideIcons.image300, size: 16,
          color: Theme.of(context).colorScheme.onSurfaceVariant),
    );
    final source = provider(image);
    if (source == null) return fallback();
    return Image(image: source, width: 48, height: 48, fit: BoxFit.cover,
        gaplessPlayback: true, errorBuilder: (_, _, _) => fallback());
  }

  /// Title, type subtitle (null: the localized "File") and glyph for each
  /// attachment format the client sends (composer/index.tsx *AttachmentPill).
  static (String, String?, IconData) describe(JsonMap attachment) {
    final path = stringValue(attachment['path']);
    final type = attachment['kind'] ?? attachment['type'];
    switch (type) {
      case 'workspace_file':
        // attachments/workspace-file.ts getWorkspaceFileAttachmentSubtitle.
        final selection = jsonMap(attachment['selection']);
        return (
          path.split('/').last,
          selection['kind'] == 'line_range'
              ? '$path · ${selection['startLine']}-${selection['endLine']}'
              : path,
          LucideIcons.fileText300,
        );
      case 'github_issue':
      case 'forge_issue':
        return (
          stringValue(attachment['title'], '#${attachment['number'] ?? ''}'),
          'Issue #${attachment['number'] ?? ''}',
          LucideIcons.circleDot300,
        );
      case 'github_pr':
      case 'forge_change_request':
        final (abbrev, prefix) = forgeChangeRequest(attachment['forge']);
        return (
          stringValue(attachment['title'], '$prefix${attachment['number'] ?? ''}'),
          '$abbrev $prefix${attachment['number'] ?? ''}',
          LucideIcons.gitPullRequest300,
        );
    }
    final name = stringValue(attachment['fileName'],
        stringValue(attachment['title'], path.isEmpty ? 'Attachment' : path.split('/').last));
    return (name, fileTypeLabel(name), LucideIcons.fileText300);
  }

  /// attachments/file-types.ts getFileTypeLabel: the upper-cased extension.
  static String? fileTypeLabel(String path) {
    final normalized = path.split('#').first.split('?').first;
    final dot = normalized.lastIndexOf('.');
    if (dot < 0 || dot == normalized.length - 1) return null;
    return normalized.substring(dot + 1).toUpperCase();
  }

  @override
  State<AttachmentPill> createState() => _AttachmentPillState();
}

class _AttachmentPillState extends State<AttachmentPill> {
  bool bodyHovered = false, closeHovered = false;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final palette = PaseoColors.of(context);
    final native = defaultTargetPlatform == TargetPlatform.android ||
        defaultTargetPlatform == TargetPlatform.iOS;
    final showRemove = widget.onRemove != null &&
        (native || isCompactLayout(context) || bodyHovered || closeHovered);
    Widget frame = Semantics(
      button: widget.onOpen != null,
      label: widget.openLabel,
      child: Container(
        decoration: BoxDecoration(
          border: Border.all(color: palette.borderAccent),
          borderRadius: BorderRadius.circular(6),
        ),
        clipBehavior: Clip.antiAlias,
        child: Material(
          type: MaterialType.transparency,
          child: InkWell(
            onTap: widget.disabled ? null : widget.onOpen,
            onHover: (value) => setState(() => bodyHovered = value),
            child: widget.child,
          ),
        ),
      ),
    );
    if (widget.tooltip != null) frame = Tooltip(message: widget.tooltip!, child: frame);
    if (widget.onRemove == null) return frame;
    // The disc overhangs the frame by 8px; Flutter only hit-tests inside a
    // box, so a removable pill reserves that band itself (top and left) and
    // its tray offsets the band back out (see removableOverhang).
    return Stack(
      children: [
        Padding(
          padding: const EdgeInsets.only(
            top: AttachmentPill.removableOverhang,
            left: AttachmentPill.removableOverhang,
          ),
          child: frame,
        ),
        Positioned(
          top: 0,
          left: 0,
          child: IgnorePointer(
            ignoring: !showRemove,
            child: Opacity(
              opacity: showRemove ? 1 : 0,
              child: Semantics(
                button: true,
                label: widget.removeLabel ?? tr(context, 'composer.attachments.removeFile'),
                excludeSemantics: true,
                child: MouseRegion(
                  cursor: SystemMouseCursors.click,
                  onEnter: (_) => setState(() => closeHovered = true),
                  onExit: (_) => setState(() => closeHovered = false),
                  child: GestureDetector(
                    behavior: HitTestBehavior.opaque,
                    onTap: widget.disabled ? null : widget.onRemove,
                    child: Container(
                      width: 24,
                      height: 24,
                      decoration: BoxDecoration(
                        color: palette.surface2,
                        shape: BoxShape.circle,
                        border: Border.all(color: colors.outline),
                      ),
                      child: Icon(LucideIcons.x300, size: 12, color: colors.onSurfaceVariant),
                    ),
                  ),
                ),
              ),
            ),
          ),
        ),
      ],
    );
  }
}
