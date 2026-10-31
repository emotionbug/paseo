import 'dart:async';
import 'dart:typed_data';

import 'package:flutter/foundation.dart' show defaultTargetPlatform;
import 'package:flutter/material.dart';
import 'package:flutter_svg/flutter_svg.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/scope.dart';
import '../../ui/common.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import '../files/pane_chrome.dart';
import '../files/remote_file_service.dart';
import 'assistant_file_links.dart' show isAbsoluteFilePath;

/// utils/assistant-image-source.ts AssistantImageSourceResolution.
sealed class AssistantImageSource {
  const AssistantImageSource();
}

/// A URI the platform loads itself (http(s), data, blob).
class DirectImageSource extends AssistantImageSource {
  const DirectImageSource(this.uri);
  final String uri;
}

/// A local file read through the daemon (`file_explorer_request` mode file).
class FileImageSource extends AssistantImageSource {
  const FileImageSource({required this.cwd, required this.path});
  final String cwd, path;
}

final _directImage = RegExp(r'^(https?:|data:|blob:)', caseSensitive: false);
final _svgImage = RegExp(r'(?:\.svg(?:[?#]|$)|^data:image/svg\+xml)', caseSensitive: false);

bool _isHomeRelative(String path) => path == '~' || path.startsWith('~/') || path.startsWith(r'~\');

String _decode(String value) {
  try {
    return Uri.decodeComponent(value);
  } catch (_) {
    return value;
  }
}

/// components/markdown/image-source.ts resolveMarkdownPreviewImageSource (an
/// assistant message has no document directory) followed by
/// utils/assistant-image-source.ts resolveAssistantImageSource and
/// file-explorer/preview-target.ts resolveFilePreviewReadTarget: direct URIs
/// load as is; a relative path reads from the workspace root, an absolute one
/// from the workspace root when inside it and the filesystem root otherwise,
/// a `~/` path from the home directory. Null when nothing can be read.
AssistantImageSource? resolveAssistantImageSource(String source, {String? workspaceRoot}) {
  final trimmed = source.trim();
  if (trimmed.isEmpty) return null;
  if (_directImage.hasMatch(trimmed)) return DirectImageSource(trimmed);
  final pathSource = trimmed.replaceFirst(RegExp(r'[?#].*$'), '');
  var path = pathSource;
  if (pathSource.startsWith('file://')) {
    path = _decode(Uri.tryParse(pathSource)?.path ?? '');
    if (RegExp(r'^/[A-Za-z]:/').hasMatch(path)) path = path.substring(1);
  } else {
    path = _decode(pathSource);
  }
  path = path.trim();
  if (path.isEmpty) return null;
  if (_isHomeRelative(path)) return FileImageSource(cwd: '~', path: path);
  final root = workspaceRoot?.trim() ?? '';
  if (!isAbsoluteFilePath(path)) {
    if (root.isEmpty || !isAbsoluteFilePath(root)) return null;
    return FileImageSource(cwd: root, path: path);
  }
  if (root.isNotEmpty && isAbsoluteFilePath(root) && _isWithin(path, root)) {
    return FileImageSource(cwd: root, path: path);
  }
  final drive = RegExp(r'^([A-Za-z]:)[\\/]').firstMatch(path);
  final unc = RegExp(r'^(\\\\[^\\]+\\[^\\]+)').firstMatch(path);
  final fsRoot = path.startsWith('/') ? '/' : drive != null ? '${drive[1]}/' : unc?[1];
  return fsRoot == null ? null : FileImageSource(cwd: fsRoot, path: path);
}

bool _isWithin(String candidate, String root) {
  String normalize(String value) {
    var normalized = value.replaceAll(r'\', '/');
    if (normalized != '/' && !RegExp(r'^[A-Za-z]:/?$').hasMatch(normalized)) {
      normalized = normalized.replaceFirst(RegExp(r'/+$'), '');
    }
    return RegExp(r'^[A-Za-z]:/').hasMatch(normalized)
        ? normalized[0].toUpperCase() + normalized.substring(1)
        : normalized;
  }

  final path = normalize(candidate), base = normalize(root);
  if (base == '/') return path.startsWith('/');
  return path == base || path.startsWith('$base/');
}

/// A decoded markdown image: a raster [provider] with its pixel [size], or
/// SVG [svg] bytes (desktop only, as on the original web renderer).
class _LoadedImage {
  const _LoadedImage.raster(ImageProvider this.provider, Size this.size) : svg = null;
  const _LoadedImage.svg(Uint8List this.svg) : provider = null, size = null;
  final ImageProvider? provider;
  final Size? size;
  final Uint8List? svg;
}

/// assistant-image/acquisition-cache.ts: at most 500 acquisitions, least
/// recently used evicted first; a failed acquisition is dropped so the next
/// mount retries.
final _acquisitions = <String, Future<_LoadedImage>>{};
const _acquisitionCapacity = 500;

Future<_LoadedImage> _acquire(String key, Future<_LoadedImage> Function() locate) {
  final cached = _acquisitions.remove(key);
  if (cached != null) {
    _acquisitions[key] = cached;
    return cached;
  }
  final future = locate();
  _acquisitions[key] = future;
  while (_acquisitions.length > _acquisitionCapacity) {
    _acquisitions.remove(_acquisitions.keys.first);
  }
  unawaited(
    future.then<void>(
      (_) {},
      onError: (Object _) {
        if (identical(_acquisitions[key], future)) _acquisitions.remove(key);
      },
    ),
  );
  return future;
}

/// image-decoding.ts decodeMarkdownImage: an image counts only once it
/// decodes to positive dimensions.
Future<Size> _decodeSize(ImageProvider provider) {
  final completer = Completer<Size>();
  final stream = provider.resolve(ImageConfiguration.empty);
  late final ImageStreamListener listener;
  listener = ImageStreamListener(
    (info, _) {
      final size = Size(info.image.width.toDouble(), info.image.height.toDouble());
      info.dispose();
      stream.removeListener(listener);
      if (completer.isCompleted) return;
      if (size.isEmpty) {
        completer.completeError(StateError('Image has no decodable dimensions'));
      } else {
        completer.complete(size);
      }
    },
    onError: (error, stack) {
      stream.removeListener(listener);
      if (!completer.isCompleted) completer.completeError(error, stack);
    },
  );
  stream.addListener(listener);
  return completer.future;
}

Future<_LoadedImage> _raster(ImageProvider provider) async =>
    _LoadedImage.raster(provider, await _decodeSize(provider));

/// One image of an assistant message (components/markdown/images.ts
/// useMarkdownSource): local files are read through the daemon, direct URIs
/// load as they are. While loading it reserves the 320px image box; an image
/// that cannot be read or decoded (or an SVG on Android/iOS) is replaced by
/// its alt text, rendered as paragraph text; a ready image renders with
/// markdown/styles.ts `image` (320 high, contain, 8px corners) and a tap opens
/// the zoomable lightbox.
class AssistantMarkdownImage extends StatefulWidget {
  const AssistantMarkdownImage({
    super.key,
    required this.source,
    required this.alt,
    required this.altStyle,
    required this.serverId,
    required this.workspaceRoot,
    required this.occurrenceKey,
  });

  final String source;
  final String alt;
  final TextStyle altStyle;
  final String serverId;
  final String? workspaceRoot;

  /// The rendered message (acquisition-cache.ts createAssistantImageOccurrenceKey):
  /// file reads are cached per message, so a later message re-reads the file.
  final String occurrenceKey;

  @override
  State<AssistantMarkdownImage> createState() => _AssistantMarkdownImageState();
}

class _AssistantMarkdownImageState extends State<AssistantMarkdownImage> {
  Future<_LoadedImage>? _image;

  static bool get _native =>
      defaultTargetPlatform == TargetPlatform.android || defaultTargetPlatform == TargetPlatform.iOS;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    _image ??= _load();
  }

  @override
  void didUpdateWidget(AssistantMarkdownImage oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.source != widget.source ||
        oldWidget.serverId != widget.serverId ||
        oldWidget.workspaceRoot != widget.workspaceRoot ||
        oldWidget.occurrenceKey != widget.occurrenceKey) {
      _image = _load();
    }
  }

  /// Null when the image is unavailable without trying (source.ts replaces
  /// SVGs with their alt text where the renderer cannot draw them).
  Future<_LoadedImage>? _load() {
    final svg = _svgImage.hasMatch(widget.source.trim());
    if (svg && _native) return null;
    final resolution = resolveAssistantImageSource(widget.source, workspaceRoot: widget.workspaceRoot);
    switch (resolution) {
      case null:
        return null;
      case DirectImageSource(:final uri):
        final parsed = Uri.tryParse(uri);
        if (parsed == null) return null;
        if (parsed.scheme == 'http' || parsed.scheme == 'https') {
          if (svg) return null;
          return _acquire('direct:$uri', () => _raster(NetworkImage(uri)));
        }
        if (parsed.scheme != 'data') return null; // blob: URLs belong to a browser page.
        return _acquire('direct:$uri', () async {
          final bytes = parsed.data!.contentAsBytes();
          return svg ? _LoadedImage.svg(bytes) : _raster(MemoryImage(bytes));
        });
      case FileImageSource(:final cwd, :final path):
        final store = PaseoScope.read(context);
        final key = '${widget.serverId}\u0000${widget.occurrenceKey}\u0000$cwd\u0000$path';
        return _acquire(key, () async {
          final file = await RemoteFileService(store.clientFor(widget.serverId)).read(cwd, path);
          if (svg || file.mime.split(';').first == 'image/svg+xml') {
            if (_native) throw StateError('SVG images are not supported here');
            return _LoadedImage.svg(file.bytes);
          }
          return _raster(MemoryImage(file.bytes));
        });
    }
  }

  @override
  Widget build(BuildContext context) {
    final image = _image;
    if (image == null) return _alt();
    return FutureBuilder<_LoadedImage>(
      future: image,
      builder: (context, snapshot) {
        if (snapshot.hasError) return _alt();
        final loaded = snapshot.data;
        // The pending placeholder keeps the image's box reserved.
        if (loaded == null) return const SizedBox(height: 320, width: double.infinity);
        return _ready(loaded);
      },
    );
  }

  Widget _alt() {
    if (widget.alt.isEmpty) return const SizedBox.shrink();
    return Text(widget.alt, style: widget.altStyle);
  }

  Widget _ready(_LoadedImage loaded) {
    final provider = loaded.provider;
    final Widget picture;
    if (provider != null) {
      final size = loaded.size!;
      picture = AspectRatio(
        aspectRatio: size.width / size.height,
        child: ClipRRect(
          borderRadius: BorderRadius.circular(8),
          child: Image(image: provider, fit: BoxFit.fill, gaplessPlayback: true),
        ),
      );
    } else {
      picture = ClipRRect(
        borderRadius: BorderRadius.circular(8),
        child: SvgPicture.memory(loaded.svg!, fit: BoxFit.contain),
      );
    }
    return SizedBox(
      height: 320,
      width: double.infinity,
      child: Center(
        child: MouseRegion(
          cursor: SystemMouseCursors.click,
          child: GestureDetector(
            onTap: () => showImageLightbox(
              context,
              image: provider,
              svg: loaded.svg,
            ),
            child: Semantics(
              image: true,
              button: true,
              label: widget.alt.isEmpty ? null : widget.alt,
              child: picture,
            ),
          ),
        ),
      ),
    );
  }
}

/// components/attachment-lightbox.tsx: a 90% black backdrop (tap to dismiss)
/// over a zoomable image fitted inside 16px padding and at most 960×640, with
/// the zoomable-viewport toolbar — Zoom in / Zoom out / Reset view, then
/// "Close image" — 8px from the top-right corner. An image that fails to
/// load shows "Couldn't load image".
Future<void> showImageLightbox(BuildContext context, {ImageProvider? image, Uint8List? svg}) =>
    showGeneralDialog<void>(
      context: context,
      barrierDismissible: true,
      barrierLabel: tr(context, 'message.attachments.dismissImage'),
      barrierColor: const Color(0xe6000000),
      transitionDuration: const Duration(milliseconds: 200),
      transitionBuilder: (context, animation, _, child) => FadeTransition(opacity: animation, child: child),
      pageBuilder: (context, _, _) => _ImageLightbox(image: image, svg: svg),
    );

class _ImageLightbox extends StatefulWidget {
  const _ImageLightbox({this.image, this.svg});
  final ImageProvider? image;
  final Uint8List? svg;

  @override
  State<_ImageLightbox> createState() => _ImageLightboxState();
}

class _ImageLightboxState extends State<_ImageLightbox> {
  static const _minScale = 1.0, _maxScale = 8.0;
  final _transform = TransformationController();
  Size _viewport = Size.zero;

  double get _scale => _transform.value.getMaxScaleOnAxis();

  @override
  void initState() {
    super.initState();
    _transform.addListener(_changed);
  }

  void _changed() => setState(() {});

  @override
  void dispose() {
    _transform
      ..removeListener(_changed)
      ..dispose();
    super.dispose();
  }

  /// zoomable-viewport zoomFromCenter: scales about the viewport centre.
  void _zoom(double factor) {
    final current = _scale;
    final next = (current * factor).clamp(_minScale, _maxScale);
    final step = next / current;
    final center = _viewport.center(Offset.zero);
    _transform.value = Matrix4.translationValues(center.dx, center.dy, 0)
        .multiplied(Matrix4.diagonal3Values(step, step, 1))
        .multiplied(Matrix4.translationValues(-center.dx, -center.dy, 0))
        .multiplied(_transform.value);
  }

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final scale = _scale;
    final failed = Text(
      tr(context, 'message.attachments.imageLoadFailed'),
      style: TextStyle(color: colors.muted, fontSize: PaseoTypography.of(context).uiSize(14)),
    );
    Widget content;
    if (widget.svg != null) {
      content = SvgPicture.memory(widget.svg!, fit: BoxFit.contain);
    } else if (widget.image != null) {
      content = Image(
        image: widget.image!,
        fit: BoxFit.contain,
        gaplessPlayback: true,
        errorBuilder: (_, _, _) => Center(child: failed),
      );
    } else {
      content = Center(child: failed);
    }
    return Material(
      type: MaterialType.transparency,
      child: SafeArea(
        child: LayoutBuilder(
          builder: (context, constraints) {
            _viewport = constraints.biggest;
            // geometry.ts fitContentToViewport with LIGHTBOX_FIT.
            final fit = Size(
              (constraints.maxWidth - 32).clamp(0, 960).toDouble(),
              (constraints.maxHeight - 32).clamp(0, 640).toDouble(),
            );
            return Stack(
              children: [
                Positioned.fill(
                  child: GestureDetector(
                    behavior: HitTestBehavior.opaque,
                    onTap: () => Navigator.pop(context),
                    child: InteractiveViewer(
                      transformationController: _transform,
                      minScale: _minScale,
                      maxScale: _maxScale,
                      child: Center(
                        child: GestureDetector(
                          onTap: () {},
                          child: Semantics(
                            image: true,
                            label: tr(context, 'composer.attachments.openImage'),
                            child: SizedBox.fromSize(size: fit, child: content),
                          ),
                        ),
                      ),
                    ),
                  ),
                ),
                Positioned(
                  top: 8,
                  right: 8,
                  child: Row(
                    spacing: 4,
                    children: [
                      _LightboxButton(
                        icon: LucideIcons.zoomIn300,
                        label: tr(context, 'message.diagram.zoomIn'),
                        onPressed: scale >= _maxScale ? null : () => _zoom(1.25),
                      ),
                      _LightboxButton(
                        icon: LucideIcons.zoomOut300,
                        label: tr(context, 'message.diagram.zoomOut'),
                        onPressed: scale <= _minScale ? null : () => _zoom(.8),
                      ),
                      _LightboxButton(
                        icon: LucideIcons.scan300,
                        label: tr(context, 'message.diagram.resetZoom'),
                        onPressed: scale == 1 ? null : () => _transform.value = Matrix4.identity(),
                      ),
                      _LightboxButton(
                        icon: LucideIcons.x300,
                        label: tr(context, 'message.attachments.closeImage'),
                        onPressed: () => Navigator.pop(context),
                      ),
                    ],
                  ),
                ),
              ],
            );
          },
        ),
      ),
    );
  }
}

/// zoomable-viewport/toolbar.tsx ViewportToolbarButton: the small icon-button
/// chrome on surface2, a foregroundMuted glyph (foreground while hovered),
/// half opacity when disabled.
class _LightboxButton extends StatefulWidget {
  const _LightboxButton({required this.icon, required this.label, this.onPressed});
  final IconData icon;
  final String label;
  final VoidCallback? onPressed;

  @override
  State<_LightboxButton> createState() => _LightboxButtonState();
}

class _LightboxButtonState extends State<_LightboxButton> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final enabled = widget.onPressed != null;
    final size = paneToolbarButtonSize(context);
    Widget button = Semantics(
      button: true,
      enabled: enabled,
      label: widget.label,
      excludeSemantics: true,
      child: MouseRegion(
        cursor: enabled ? SystemMouseCursors.click : MouseCursor.defer,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: widget.onPressed,
          child: Opacity(
            opacity: enabled ? 1 : .5,
            child: Container(
              width: size,
              height: size,
              decoration: BoxDecoration(
                color: colors.surface2,
                borderRadius: BorderRadius.circular(6),
              ),
              child: Icon(
                widget.icon,
                size: paneToolbarIconSize(context),
                color: _hovered && enabled ? Theme.of(context).colorScheme.onSurface : colors.muted,
              ),
            ),
          ),
        ),
      ),
    );
    if (!isCompactLayout(context)) button = Tooltip(message: widget.label, child: button);
    return button;
  }
}
