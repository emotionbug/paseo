import 'dart:io';
import 'dart:typed_data';

import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';
import 'package:share_plus/share_plus.dart';
import 'package:url_launcher/url_launcher.dart';

import '../../core/client.dart';
import '../../core/json.dart';
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/theme.dart';
import 'remote_directory_browser.dart';
import 'remote_file_service.dart';

Future<File> _temporaryFile(String name) async {
  final root = await getTemporaryDirectory();
  final directory = await Directory('${root.path}/paseo-files')
      .create(recursive: true);
  final transfer = await directory.createTemp('transfer-');
  final safeName = remoteBasename(name)
      .replaceAll(RegExp(r'[<>:"/\\|?*\x00-\x1f]'), '_');
  return File('${transfer.path}/${safeName.isEmpty ? 'download' : safeName}');
}

/// Downloads into a unique staging file first; failed transfers never replace a saved file.
Future<void> saveRemoteFile(
  BuildContext context, {
  required PaseoClient client,
  required String cwd,
  required String path,
  bool share = false,
  bool open = false,
  Uint8List? currentBytes,
  String? mimeType,
}) async {
  final name = remoteBasename(path, cwd: cwd);
  FileSaveLocation? destination;
  final mobile = Platform.isAndroid || Platform.isIOS;
  if (!share && !open && !mobile) {
    destination = await getSaveLocation(suggestedName: name);
    if (destination == null) return;
  }
  if (!context.mounted) return;
  final progress = ValueNotifier<(int, int)>((0, 0));
  final cancellation = FileTransferCancellation();
  final modalContext = context;
  final navigator = Navigator.of(context, rootNavigator: true);
  final colors = PaseoColors.of(context);
  showDialog<void>(
    context: context,
    barrierDismissible: false,
    barrierColor: const Color(0x8c000000),
    builder: (dialogContext) => PopScope(
      canPop: false,
      // The original adaptive-modal card: surface1, 12px radius, surface2 border.
      child: Dialog(
        backgroundColor: colors.surface1,
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(12),
          side: BorderSide(color: colors.surface2),
        ),
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 520),
          child: Padding(
            padding: const EdgeInsets.all(24),
            child: ValueListenableBuilder<(int, int)>(
              valueListenable: progress,
              builder: (_, value, _) => Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.stretch,
                spacing: 12,
                children: [
                  Text(
                    'Downloading $name',
                    maxLines: 2,
                    overflow: TextOverflow.ellipsis,
                    style: Theme.of(dialogContext).textTheme.bodyMedium!.copyWith(
                      fontWeight: FontWeight.w500,
                    ),
                  ),
                  ClipRRect(
                    borderRadius: BorderRadius.circular(2),
                    child: LinearProgressIndicator(
                      value: value.$2 > 0 ? value.$1 / value.$2 : null,
                      minHeight: 4,
                      color: colors.accent,
                      backgroundColor: colors.surface3,
                    ),
                  ),
                  Row(children: [
                    Expanded(
                      child: Text(
                        '${value.$1} / ${value.$2} bytes',
                        style: Theme.of(dialogContext).textTheme.bodySmall!.copyWith(
                          color: colors.muted,
                        ),
                      ),
                    ),
                    PaseoButton(
                      label: 'Cancel',
                      size: PaseoControlSize.sm,
                      onPressed: cancellation.cancel,
                    ),
                  ]),
                ],
              ),
            ),
          ),
        ),
      ),
    ),
  );
  File? temporary;
  bool dialogOpen = true;
  try {
    temporary = await _temporaryFile(name);
    JsonMap metadata = {};
    if (currentBytes != null) {
      await temporary.writeAsBytes(currentBytes, flush: true);
    } else {
      metadata = await RemoteFileService(client).download(
        cwd,
        path,
        temporary,
        cancellation: cancellation,
        onProgress: (received, total) => progress.value = (received, total),
      );
    }
    if (cancellation.cancelled) throw const FileTransferCancelled();
    navigator.pop();
    dialogOpen = false;
    final mime = mimeType ?? metadata.text('mime', 'application/octet-stream');
    if (open) {
      if (!await launchUrl(
        Uri.file(temporary.path),
        mode: LaunchMode.externalApplication,
      )) {
        throw StateError('No application can open $name');
      }
    } else if (share && Platform.isLinux) {
      final result = await Process.run('xdg-email', [
        '--attach',
        temporary.path,
      ]);
      if (result.exitCode != 0) {
        throw StateError(
          'Could not open a mail composer with the downloaded attachment (${result.exitCode}): ${result.stderr}',
        );
      }
    } else if (share || mobile) {
      Rect? origin;
      if (modalContext.mounted) {
        final box = modalContext.findRenderObject();
        if (box is RenderBox && box.hasSize) {
          origin = box.localToGlobal(Offset.zero) & box.size;
        }
      }
      await SharePlus.instance.share(
        ShareParams(
          files: [XFile(temporary.path, mimeType: mime, name: name)],
          sharePositionOrigin: origin,
        ),
      );
    } else {
      await XFile(temporary.path).saveTo(destination!.path);
      await temporary.parent.delete(recursive: true);
      temporary = null;
      if (context.mounted) {
        showToast(context, 'Saved to ${destination.path}', variant: ToastVariant.success);
      }
    }
  } catch (error) {
    if (dialogOpen) navigator.pop();
    if (temporary != null && await temporary.parent.exists()) {
      await temporary.parent.delete(recursive: true);
    }
    if (context.mounted && error is! FileTransferCancelled) {
      showError(context, error);
    }
  } finally {
    progress.dispose();
  }
}
