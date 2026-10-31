import '../../../core/client.dart';
import '../../../core/json.dart';

/// attachments/upload-limit.ts FILE_UPLOAD_MAX_BYTES.
const fileUploadMaxBytes = 50 * 1024 * 1024;

/// attachments/file-types.ts RASTER_IMAGE_MIME_TYPE_BY_EXTENSION.
const _rasterImageMimeTypes = {
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.jpeg': 'image/jpeg',
  '.gif': 'image/gif',
  '.webp': 'image/webp',
  '.bmp': 'image/bmp',
  '.heic': 'image/heic',
  '.heif': 'image/heif',
  '.avif': 'image/avif',
  '.tif': 'image/tiff',
  '.tiff': 'image/tiff',
};

/// The extensions an image picker accepts (RASTER_IMAGE_FILE_EXTENSIONS).
final rasterImageExtensions = [
  for (final extension in _rasterImageMimeTypes.keys) extension.substring(1),
];

/// file-types.ts getFileExtension: the lower-cased extension (with its dot)
/// of [path] without any query or fragment; empty when there is none.
String fileExtension(String path) {
  final normalized = path.split('#').first.split('?').first;
  final index = normalized.lastIndexOf('.');
  return index < 0 ? '' : normalized.substring(index).toLowerCase();
}

/// file-types.ts getRasterImageMimeTypeFromPath.
String? rasterImageMimeType(String path) => _rasterImageMimeTypes[fileExtension(path)];

/// file-types.ts getMimeTypeFromPath: raster images by extension, every other
/// file the generic octet-stream type.
String mimeTypeForPath(String path) =>
    rasterImageMimeType(path) ?? 'application/octet-stream';

/// actions.ts uploadFileAttachments: a file over [fileUploadMaxBytes].
class FileTooLargeError implements Exception {
  const FileTooLargeError(this.fileName);
  final String fileName;
  @override
  String toString() => '$fileName is too large (max 50MB)';
}

/// actions.ts uploadFileAttachments: every selected file is size-checked
/// before any upload starts, so one oversized file uploads nothing.
void checkUploadSizes(Iterable<({String name, int size})> files) {
  for (final file in files) {
    if (file.size > fileUploadMaxBytes) throw FileTooLargeError(file.name);
  }
}

/// composer/attachments/submit.ts resolveComposerAttachmentSubmitFormat over
/// `serverInfo.features.forgeSearch === true`: hosts without forge search take
/// the legacy GitHub attachment kinds and the github_search RPC.
bool usesLegacyForge(PaseoClient client) =>
    jsonMap(client.serverInfo['features'])['forgeSearch'] != true;
