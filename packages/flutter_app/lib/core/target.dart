import 'package:flutter/foundation.dart';

import 'json.dart';

@immutable
class PanelTarget {
  const PanelTarget({
    required this.kind,
    required this.serverId,
    required this.workspaceId,
    required this.cwd,
    this.id,
    this.title,
    this.data = const {},
  });

  final String kind;
  final String serverId;
  final String workspaceId;
  final String cwd;
  final String? id;
  final String? title;
  final JsonMap data;

  String get key => [
    serverId,
    workspaceId,
    kind,
    id ?? '',
  ].map(Uri.encodeComponent).join('/');

  PanelTarget copyWith({
    String? kind,
    String? serverId,
    String? workspaceId,
    String? cwd,
    String? id,
    String? title,
    JsonMap? data,
  }) => PanelTarget(
    kind: kind ?? this.kind,
    serverId: serverId ?? this.serverId,
    workspaceId: workspaceId ?? this.workspaceId,
    cwd: cwd ?? this.cwd,
    id: id ?? this.id,
    title: title ?? this.title,
    data: data ?? this.data,
  );

  JsonMap toJson() => {
    'kind': kind,
    'serverId': serverId,
    'workspaceId': workspaceId,
    'cwd': cwd,
    if (id != null) 'id': id,
    if (title != null) 'title': title,
    if (data.isNotEmpty) 'data': data,
  };

  factory PanelTarget.fromJson(JsonMap json) => PanelTarget(
    kind: json.text('kind'),
    serverId: json.text('serverId'),
    workspaceId: json.text('workspaceId'),
    cwd: json.text('cwd'),
    id: json['id'] as String?,
    title: json['title'] as String?,
    data: json.object('data'),
  );
}
