import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:path/path.dart' as p;
import 'package:url_launcher/url_launcher.dart';
import 'package:uuid/uuid.dart';

import '../../core/client.dart';
import 'remote_directory_browser.dart';
import 'remote_file_service.dart';

/// Serves a static, script-free preview and real relative workspace assets to
/// the system browser. The random route is never exposed to the daemon.
class RemoteHtmlPreview {
  RemoteHtmlPreview._(this._server, this.url);
  final HttpServer _server;
  final Uri url;
  String _content = '';
  bool _closed = false;
  final _cancellation = FileTransferCancellation();

  static Future<RemoteHtmlPreview> create({
    required PaseoClient client,
    required String cwd,
    required String path,
    required String content,
    void Function(Object)? onError,
  }) async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final token = const Uuid().v4();
    final windows = RegExp(r'^[A-Za-z]:|^\\\\').hasMatch(cwd);
    String portable(String value) =>
        windows ? value.replaceAll('\\', '/') : value;
    final root = p.posix.normalize(portable(cwd));
    final absolute = p.posix.normalize(portable(remoteAbsolute(cwd, path)));
    // Out-of-workspace files are confined to their own directory.
    final assetRoot = p.posix.isWithin(root, absolute)
        ? root
        : p.posix.dirname(absolute);
    final relative = p.posix.relative(absolute, from: assetRoot);
    final prefix = '/$token/';
    final url = Uri(
      scheme: 'http',
      host: '127.0.0.1',
      port: server.port,
      path: '$prefix$relative',
    );
    final preview = RemoteHtmlPreview._(server, url).._content = content;
    server.listen(
      (request) async {
        try {
          if (!['GET', 'HEAD'].contains(request.method) ||
              !request.uri.path.startsWith(prefix)) {
            request.response.statusCode = HttpStatus.notFound;
            await request.response.close();
            return;
          }
          final requested = p.posix.normalize(
            p.posix.join(
              assetRoot,
              portable(request.uri.path.substring(prefix.length)),
            ),
          );
          if (requested != assetRoot &&
              !p.posix.isWithin(assetRoot, requested)) {
            request.response.statusCode = HttpStatus.forbidden;
            await request.response.close();
            return;
          }
          request.response.headers.set('Cache-Control', 'no-store');
          request.response.headers.set('X-Content-Type-Options', 'nosniff');
          request.response.headers.set('Referrer-Policy', 'no-referrer');
          request.response.headers.set(
            'Content-Security-Policy',
            "default-src 'self' data:; script-src 'none'; object-src 'none'; frame-src 'none'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; font-src 'self' data:; form-action 'none'; base-uri 'self'; sandbox allow-same-origin",
          );
          if (requested == absolute) {
            request.response.headers.contentType = ContentType.html;
            if (request.method == 'GET') {
              request.response.add(utf8.encode(preview._content));
            }
          } else {
            final asset = await RemoteFileService(client)
                .read(cwd, requested, cancellation: preview._cancellation);
            request.response.headers.contentType = ContentType.parse(
              asset.mime,
            );
            if (request.method == 'GET') request.response.add(asset.bytes);
          }
          await request.response.close();
        } catch (error) {
          if (!preview._closed) onError?.call(error);
          try {
            request.response.statusCode = HttpStatus.notFound;
            await request.response.close();
          } catch (_) {
            /* Browser may have disconnected. */
          }
        }
      },
      onError: (Object error) {
        if (!preview._closed) onError?.call(error);
      },
    );
    return preview;
  }

  void update(String content) {
    _content = content;
  }

  Future<void> open() async {
    if (!await launchUrl(url, mode: LaunchMode.externalApplication)) {
      throw StateError('The system browser could not open the HTML preview');
    }
  }

  Future<void> close() async {
    _closed = true;
    _cancellation.cancel();
    await _server.close(force: true);
  }
}
