import '../../core/json.dart';

/// The wire attachment for a forge search item (legacy daemons take the
/// github_pr / github_issue shapes).
JsonMap forgeAttachment(JsonMap item, {bool legacy = false}) {
  final change = item['kind'] == 'change_request' || item['kind'] == 'pr';
  return {
    'type': legacy
        ? (change ? 'github_pr' : 'github_issue')
        : (change ? 'forge_change_request' : 'forge_issue'),
    'mimeType': legacy
        ? (change ? 'application/github-pr' : 'application/github-issue')
        : (change
              ? 'application/paseo-forge-change-request'
              : 'application/paseo-forge-issue'),
    if (!legacy) 'forge': item['forge'] ?? 'github',
    'number': item['number'],
    'title': item['title'],
    'url': item['url'],
    for (final field in [
      'body',
      if (!legacy) 'projectPath',
      if (change) ...['baseRefName', 'headRefName'],
    ])
      if (item[field] != null) field: item[field],
  };
}
