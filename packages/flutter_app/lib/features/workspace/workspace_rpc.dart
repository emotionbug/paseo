import '../../core/json.dart';

JsonMap workspacePayload(JsonMap message) =>
    message['payload'] is Map ? jsonMap(message['payload']) : message;

void requireWorkspaceSuccess(JsonMap response) {
  final error = response['error'];
  if (error != null ||
      response['ok'] == false ||
      response['accepted'] == false) {
    throw StateError(
      error is Map
          ? jsonMap(error).text('code')
          : '${error ?? 'The host rejected the operation'}',
    );
  }
}
