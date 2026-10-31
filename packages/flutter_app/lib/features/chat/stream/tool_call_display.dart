import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_svg/flutter_svg.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../../core/json.dart';
import '../tool_call_details.dart'
    show hasMeaningfulToolCallDetail, isPendingToolCallDetail, toolCallErrorText;

// Ports of protocol/src/tool-call-display.ts, tool-name-normalization.ts,
// paseo-tool-presentation/* and app/src/utils/tool-call-icon*.ts,
// extract-tool-call-file-path.ts and tool-calls/presentation.ts.

/// A tool row glyph: a Lucide icon or the Paseo logo (components/icons/paseo-logo.tsx).
class ToolGlyph {
  const ToolGlyph.icon(IconData this.icon) : svgPath = null;
  const ToolGlyph.paseo() : icon = null, svgPath = _paseoLogoPath;

  final IconData? icon;
  final String? svgPath;

  Widget build(double size, Color color) {
    if (icon != null) return Icon(icon, size: size, color: color);
    return SvgPicture.string(
      '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 700 700" fill="none">'
      '<path d="$svgPath" fill="#000000"/></svg>',
      width: size,
      height: size,
      colorFilter: ColorFilter.mode(color, BlendMode.srcIn),
    );
  }

  @override
  bool operator ==(Object other) =>
      other is ToolGlyph && other.icon == icon && other.svgPath == svgPath;

  @override
  int get hashCode => Object.hash(icon, svgPath);
}

const _paseoLogoPath =
    'M291.495 91.399C333.897 104.892 379.155 135.075 416.229 173.191C453.389 211.394 484.429 259.725 495.708 311.251C497.555 319.693 498.865 328.216 499.586 336.776C509.755 326.554 519.867 317.815 529.89 311.547C540.647 304.821 553.808 299.297 568.641 299.785C584.29 300.299 597.395 307.326 607.747 317.632C632.173 341.947 629.612 372.898 619.872 397.936C610.185 422.833 591.557 447.826 572.732 469.124C553.591 490.78 532.713 510.308 516.779 524.318C508.775 531.355 501.936 537.073 497.07 541.052C494.635 543.043 492.689 544.603 491.334 545.679C490.657 546.217 490.126 546.635 489.756 546.926C489.571 547.071 489.425 547.184 489.321 547.265C489.269 547.305 489.227 547.338 489.196 547.362C489.181 547.374 489.168 547.385 489.157 547.393C489.153 547.397 489.147 547.401 489.144 547.403C489.134 547.4 488.837 547.06 473.001 528.499L489.135 547.411C478.157 555.911 462.033 554.334 453.122 543.89C444.213 533.448 445.887 518.094 456.861 509.592C456.863 509.591 456.865 509.588 456.869 509.586C456.88 509.577 456.902 509.561 456.933 509.536C456.997 509.487 457.101 509.404 457.245 509.292C457.533 509.066 457.979 508.715 458.569 508.247C459.749 507.31 461.506 505.901 463.742 504.073C468.216 500.414 474.589 495.088 482.073 488.508C497.114 475.284 516.315 457.282 533.578 437.75C551.157 417.862 565.26 398.01 571.859 381.048C578.403 364.227 575.681 356.302 570.724 351.367C568.928 349.579 567.744 348.902 567.267 348.676C566.888 348.496 566.811 348.52 566.804 348.52C566.605 348.513 563.971 348.537 557.953 352.3C545.161 360.299 528.815 377.492 506.807 403.867C494.927 418.106 481.871 434.435 467.547 451.957C463.709 457.28 459.503 462.538 454.91 467.717L454.702 467.549C420.808 508.347 380.37 553.856 332.335 593.848C301.853 619.226 262.656 622.597 228.642 614.743C194.834 606.936 162.658 587.448 142.217 561.686C108.054 518.631 100.57 469.801 108.223 427.836C115.56 387.606 137.391 351.005 166.502 331.557C161.248 315.813 156.813 299.49 153.519 283.013C142.593 228.368 143.239 167.031 174.28 119.619C186.922 100.31 205.846 89.1535 227.387 85.2773C248.1 81.5504 270.278 84.648 291.495 91.399ZM378.642 206.356C345.773 172.563 307.463 147.917 275.208 137.654C259.096 132.527 246.171 131.514 236.828 133.195C228.314 134.727 222.227 138.497 217.721 145.38C196.712 177.468 193.858 224.004 203.82 273.827C206.532 287.394 210.127 300.834 214.345 313.817C236.45 310.276 260.156 311.463 281.22 317.11C319.621 327.403 357.501 355.419 357.501 405.654C357.501 435.255 339.111 465.136 307.278 473.815C273.211 483.103 238.854 464.822 213.105 427.541C203.716 413.947 194.443 397.766 185.947 379.89C174.028 392.223 163.08 411.953 158.673 436.118C153.128 466.518 158.514 501.286 183.085 532.253C195.993 548.522 217.742 562.031 240.771 567.349C263.594 572.619 284.147 569.24 298.664 557.154C349.383 514.927 390.709 466.547 426.366 422.952C448.879 390.86 453.195 356.06 445.578 321.265C436.703 280.718 411.425 240.06 378.642 206.356ZM306.296 405.722C306.296 384.769 292.223 370.736 267.284 364.051C256.012 361.03 244.156 360.087 233.095 360.771C240.361 375.935 248.168 389.513 255.897 400.704C275.647 429.298 289.989 427.822 293.247 426.934C298.737 425.437 306.296 418.161 306.296 405.722Z';

// ---- summary-text.ts ----

const maxSummaryChars = 120;
const _scanWindowChars = maxSummaryChars * 4;

/// Collapses whitespace and caps at 120 chars with a trailing ellipsis.
String? toSummaryText(String value) {
  final partial = value.length > _scanWindowChars;
  final window = partial ? value.substring(0, _scanWindowChars) : value;
  final collapsed = window.replaceAll(RegExp(r'\s+'), ' ').trim();
  if (collapsed.isEmpty) return null;
  if (collapsed.length <= maxSummaryChars && !partial) return collapsed;
  var kept = collapsed.substring(0, maxSummaryChars - 1);
  final last = kept.codeUnitAt(kept.length - 1);
  if (last >= 0xd800 && last <= 0xdbff) kept = kept.substring(0, kept.length - 1);
  return '${kept.trimRight()}…';
}

// ---- path-utils.ts ----

String stripCwdPrefix(String filePath, String? cwd) {
  if (cwd == null || cwd.isEmpty || filePath.isEmpty) return filePath;
  final normalizedCwd = cwd.replaceAll(r'\', '/').replaceFirst(RegExp(r'/+$'), '');
  final normalizedPath = filePath.replaceAll(r'\', '/');
  final prefix = '$normalizedCwd/';
  if (normalizedPath.startsWith(prefix)) return normalizedPath.substring(prefix.length);
  if (normalizedPath == normalizedCwd) return '.';
  return filePath;
}

// ---- tool-name-normalization.ts ----

String _normalizeToolName(String name) => name.trim().toLowerCase();

bool _isSpeakToolName(String name) {
  final tokens = RegExp(r'[a-z0-9]+').allMatches(_normalizeToolName(name)).toList();
  return tokens.isNotEmpty && tokens.last.group(0) == 'speak';
}

bool isPaseoToolName(String name) => getPaseoToolLeafName(name) != null;

String? getPaseoToolLeafName(String name) {
  final normalized = _normalizeToolName(name);
  if (_isSpeakToolName(normalized)) return null;
  if (normalized.contains('__')) {
    final segments = normalized.split('__').where((s) => s.isNotEmpty).toList();
    if (segments.length >= 3 &&
        segments[0] == 'mcp' &&
        (segments[1] == 'paseo' || segments[1].startsWith('paseo_'))) {
      return segments.sublist(2).join('__');
    }
    return null;
  }
  if (normalized.contains('.')) {
    final parts = normalized.split('.');
    if (parts.first == 'paseo' || parts.first.startsWith('paseo_')) {
      return parts.sublist(1).join('.');
    }
  }
  return null;
}

// ---- paseo-tool-presentation/catalog.ts ----

typedef _Context = Object? Function(JsonMap input, String? targetAgent);

class _PaseoEntry {
  const _PaseoEntry(this.label, {this.context, this.promptField, this.inputOrder, this.outputFields});
  final String label;
  final _Context? context;
  final String? promptField;
  final List<String>? inputOrder, outputFields;
}

bool _isPresent(Object? value) => switch (value) {
  String() => value.trim().isNotEmpty,
  List() => value.isNotEmpty,
  num() => true,
  _ => false,
};

_Context _field(List<String> keys) =>
    (input, _) => keys.map((key) => input[key]).where(_isPresent).firstOrNull;
Object? _targetAgent(JsonMap _, String? agent) => agent;
_Context _fieldOrTargetAgent(String key) =>
    (input, agent) => _isPresent(input[key]) ? input[key] : agent;
Object? _workspaceContext(JsonMap input, String? _) {
  final path = input['path'];
  final base = path is String
      ? path.split(RegExp(r'[\\/]')).lastWhere((s) => s.isNotEmpty, orElse: () => '')
      : null;
  return [input['title'], input['branchName'], input['branch'], base].where(_isPresent).firstOrNull;
}
Object? _viewportContext(JsonMap input, String? _) =>
    input['width'] is num && input['height'] is num ? '${input['width']}×${input['height']}' : null;

const _workspaceFields = [
  'title', 'workspaceId', 'projectId', 'isolation', 'path', 'mode', 'worktreeSlug',
  'branchName', 'baseBranch', 'branch', 'prNumber', 'forge',
];
const _agentFields = [
  'title', 'agentId', 'provider', 'workspaceId', 'cwd', 'sessionMode', 'modeId',
  'background', 'notifyOnFinish', 'internal', 'settings', 'labels',
];
const _automationFields = [
  'name', 'id', 'cron', 'timezone', 'provider', 'cwd', 'isolation', 'maxRuns',
  'expiresIn', 'clearExpires',
];
const _browserFields = [
  'browserId', 'url', 'ref', 'sourceRef', 'targetRef', 'value', 'text', 'key', 'button',
  'doubleClick', 'modifiers', 'filePaths', 'fullPage', 'maxEntries', 'timeoutMs',
  'deltaX', 'deltaY', 'width', 'height', 'function',
];
const _automationOutput = ['id', 'status', 'nextRunAt', 'expiresAt'];

_PaseoEntry _browser(String label, [_Context? context]) =>
    _PaseoEntry(label, context: context, inputOrder: _browserFields);

final Map<String, _PaseoEntry> _paseoCatalog = {
  'create_agent': _PaseoEntry('Create agent',
      context: _field(['title', 'initialPrompt']),
      promptField: 'initialPrompt',
      inputOrder: _agentFields,
      outputFields: ['agentId', 'status', 'currentModeId', 'cwd']),
  'send_agent_prompt': _PaseoEntry('Send message',
      context: _field(['prompt']),
      promptField: 'prompt',
      inputOrder: _agentFields,
      outputFields: ['status', 'lastMessage', 'permission']),
  'get_agent_status': const _PaseoEntry('Check agent', context: _targetAgent, inputOrder: ['agentId']),
  'get_agent_activity': const _PaseoEntry('Read agent activity',
      context: _targetAgent, inputOrder: ['agentId', 'limit']),
  'list_agents': const _PaseoEntry('List agents',
      inputOrder: ['cwd', 'statuses', 'sinceHours', 'limit', 'includeArchived']),
  'update_agent': _PaseoEntry('Update agent',
      context: _fieldOrTargetAgent('name'), inputOrder: _agentFields),
  'set_agent_mode': _PaseoEntry('Set agent mode',
      context: _field(['modeId']), inputOrder: ['agentId', 'modeId']),
  'cancel_agent': const _PaseoEntry('Cancel agent', context: _targetAgent, inputOrder: ['agentId']),
  'archive_agent': const _PaseoEntry('Archive agent', context: _targetAgent, inputOrder: ['agentId']),
  'kill_agent': const _PaseoEntry('Kill agent', context: _targetAgent, inputOrder: ['agentId']),
  'list_pending_permissions': const _PaseoEntry('List permissions'),
  'respond_to_permission': const _PaseoEntry('Respond to permission',
      context: _targetAgent, inputOrder: ['agentId', 'requestId', 'response']),
  'create_workspace': const _PaseoEntry('Create workspace',
      context: _workspaceContext,
      inputOrder: _workspaceFields,
      outputFields: ['workspaceId', 'projectId']),
  'list_hosts': const _PaseoEntry('List hosts'),
  'list_workspaces': const _PaseoEntry('List workspaces'),
  'rename_workspace': _PaseoEntry('Rename workspace',
      context: _field(['title']), inputOrder: ['title', 'workspaceId']),
  'archive_workspace': _PaseoEntry('Archive workspace',
      context: _field(['workspaceId']),
      inputOrder: ['workspaceId'],
      outputFields: ['workspaceId', 'archivedAgentIds', 'removedDirectory']),
  'list_workspace_scripts': const _PaseoEntry('List scripts', inputOrder: ['workspaceId']),
  'start_workspace_script': _PaseoEntry('Start script',
      context: _field(['scriptName']), inputOrder: ['workspaceId', 'scriptName']),
  'stop_workspace_script': _PaseoEntry('Stop script',
      context: _field(['scriptName']), inputOrder: ['workspaceId', 'scriptName']),
  'create_terminal': _PaseoEntry('Create terminal',
      context: _field(['name', 'command']),
      inputOrder: ['cwd', 'workspaceId', 'name', 'command']),
  'list_terminals': const _PaseoEntry('List terminals', inputOrder: ['cwd', 'all']),
  'capture_terminal': _PaseoEntry('Read terminal',
      context: _field(['terminalId']), inputOrder: ['terminalId', 'lines']),
  'send_terminal_keys': _PaseoEntry('Send keys',
      context: _field(['keys']), inputOrder: ['terminalId', 'keys', 'literal']),
  'kill_terminal': _PaseoEntry('Kill terminal',
      context: _field(['terminalId']), inputOrder: ['terminalId']),
  'create_schedule': _PaseoEntry('Create schedule',
      context: _field(['name', 'cron']),
      promptField: 'prompt',
      inputOrder: _automationFields,
      outputFields: _automationOutput),
  'create_heartbeat': _PaseoEntry('Create heartbeat',
      context: _field(['name', 'cron']),
      promptField: 'prompt',
      inputOrder: _automationFields,
      outputFields: _automationOutput),
  'update_schedule': _PaseoEntry('Update schedule',
      context: _field(['name', 'id']), promptField: 'prompt', inputOrder: _automationFields),
  'list_schedules': const _PaseoEntry('List schedules'),
  'inspect_schedule': _PaseoEntry('Inspect schedule', context: _field(['id']), inputOrder: ['id']),
  'schedule_logs': _PaseoEntry('Read schedule logs', context: _field(['id']), inputOrder: ['id']),
  'run_schedule_once': _PaseoEntry('Run schedule', context: _field(['id']), inputOrder: ['id']),
  'pause_schedule': _PaseoEntry('Pause schedule', context: _field(['id']), inputOrder: ['id']),
  'resume_schedule': _PaseoEntry('Resume schedule', context: _field(['id']), inputOrder: ['id']),
  'delete_schedule': _PaseoEntry('Delete schedule', context: _field(['id']), inputOrder: ['id']),
  'delete_heartbeat': _PaseoEntry('Delete heartbeat', context: _field(['id']), inputOrder: ['id']),
  'list_providers': const _PaseoEntry('List providers'),
  'list_models': _PaseoEntry('List models', context: _field(['provider']), inputOrder: ['provider']),
  'list_profiles': const _PaseoEntry('List profiles'),
  'inspect_provider': _PaseoEntry('Inspect provider',
      context: _field(['provider']), inputOrder: ['provider', 'cwd', 'settings']),
  'browser_list_tabs': _browser('List browser tabs'),
  'browser_new_tab': _browser('Open browser tab', _field(['url'])),
  'browser_close_tab': _browser('Close browser tab', _field(['browserId'])),
  'browser_navigate': _browser('Navigate', _field(['url'])),
  'browser_back': _browser('Go back'),
  'browser_forward': _browser('Go forward'),
  'browser_reload': _browser('Reload page'),
  'browser_snapshot': _browser('Snapshot page'),
  'browser_screenshot': _browser('Take screenshot'),
  'browser_click': _browser('Click element', _field(['ref'])),
  'browser_hover': _browser('Hover element', _field(['ref'])),
  'browser_drag': _browser('Drag element', _field(['sourceRef'])),
  'browser_fill': _browser('Fill field', _field(['ref'])),
  'browser_select': _browser('Select option', _field(['value', 'ref'])),
  'browser_type': _browser('Type text', _field(['text'])),
  'browser_keypress': _browser('Press key', _field(['key'])),
  'browser_upload': _browser('Upload files', _field(['filePaths'])),
  'browser_wait': _browser('Wait for page', _field(['text', 'url'])),
  'browser_scroll': _browser('Scroll page'),
  'browser_resize': _browser('Resize browser', _viewportContext),
  'browser_logs': _browser('Read browser logs'),
  'browser_evaluate': _browser('Run JavaScript', _field(['function'])),
};

JsonMap _readToolInput(JsonMap detail) {
  if (detail['type'] != 'unknown') return const {};
  final input = detail['input'];
  return input is Map ? jsonMap(input) : const {};
}

String? _readString(Object? value) => value is String && value.isNotEmpty ? value : null;

String? _formatContext(Object? value) {
  if (value is String) return toSummaryText(value);
  if (value is num) return '$value';
  if (value is List && value.isNotEmpty) {
    final head = value.first is String ? toSummaryText(value.first as String) : null;
    if (head == null) return null;
    return value.length > 1 ? '$head +${value.length - 1}' : head;
  }
  return null;
}

String _humanizeLeafName(String leafName) {
  final sentence = leafName.replaceAll(RegExp(r'[._-]+'), ' ').trim().toLowerCase();
  return sentence.isEmpty ? '' : sentence[0].toUpperCase() + sentence.substring(1);
}

/// The agent a Paseo call acts on, so callers can resolve its title.
String? paseoToolTargetAgentId(String toolName, JsonMap detail) =>
    getPaseoToolLeafName(toolName) == null ? null : _readString(_readToolInput(detail)['agentId']);

({String displayName, String? summary})? _paseoToolDisplay(
  String toolName,
  JsonMap detail,
  String? targetAgentTitle,
) {
  final leaf = getPaseoToolLeafName(toolName);
  if (leaf == null) return null;
  final entry = _paseoCatalog[leaf];
  if (entry == null) return (displayName: _humanizeLeafName(leaf), summary: null);
  final input = _readToolInput(detail);
  final target = targetAgentTitle ?? _readString(input['agentId']);
  return (displayName: entry.label, summary: _formatContext(entry.context?.call(input, target)));
}

// ---- paseo-tool-presentation/agent-message.ts ----

const _agentMessageEvents = {
  'message': 'Message received',
  'finished': 'Agent finished',
  'errored': 'Agent errored',
  'permission-required': 'Agent needs permission',
  'closed': 'Agent closed',
};

({String displayName, String? summary, JsonMap detail}) buildAgentMessageDisplay(JsonMap message) {
  final sender = jsonMap(message['sender']);
  final senderId = stringValue(sender['id']);
  return (
    displayName: _agentMessageEvents[message['event']] ?? 'Message received',
    summary: toSummaryText(stringValue(sender['title'], senderId)),
    detail: {
      'type': 'plain_text',
      'text': 'From agent: $senderId\n\n${stringValue(message['text'])}',
    },
  );
}

// ---- paseo-tool-presentation/detail-sections.ts ----

/// A "prose" section has [text]; a "fields" section has [fields].
typedef PaseoDetailSection = ({String title, String? text, List<({String label, String value})> fields});

const _fieldLabels = {
  'agentId': 'Agent',
  'archivedAgentIds': 'Archived agents',
  'baseBranch': 'Base branch',
  'branchName': 'New branch',
  'browserId': 'Browser tab',
  'clearExpires': 'Clear expiry',
  'currentModeId': 'Current mode',
  'cwd': 'Working directory',
  'deltaX': 'Horizontal delta',
  'deltaY': 'Vertical delta',
  'doubleClick': 'Double click',
  'expiresIn': 'Expires in',
  'expiresAt': 'Expires',
  'filePaths': 'Files',
  'fullPage': 'Full page',
  'initialPrompt': 'Prompt',
  'id': 'ID',
  'lastMessage': 'Last message',
  'maxEntries': 'Maximum entries',
  'maxRuns': 'Maximum runs',
  'modeId': 'Mode',
  'newMode': 'New mode',
  'nextRunAt': 'Next run',
  'notifyOnFinish': 'Notify on finish',
  'internal': 'Internal',
  'prNumber': 'Change request',
  'projectId': 'Project',
  'removedDirectory': 'Removed directory',
  'requestId': 'Request',
  'scriptName': 'Script',
  'sessionMode': 'Session mode',
  'sinceHours': 'Since (hours)',
  'sourceRef': 'Source',
  'targetRef': 'Target',
  'terminalId': 'Terminal',
  'thinkingOptionId': 'Thinking',
  'timeoutMs': 'Timeout (ms)',
  'updateCount': 'Updates',
  'workspaceId': 'Workspace',
  'worktreeSlug': 'Worktree',
};

String _humanizeKey(String key) {
  final known = _fieldLabels[key];
  if (known != null) return known;
  final sentence = key
      .replaceAllMapped(RegExp(r'([a-z0-9])([A-Z])'), (m) => '${m[1]} ${m[2]}')
      .replaceAll(RegExp(r'[._-]+'), ' ')
      .split(' ')
      .where((w) => w.isNotEmpty)
      .join(' ')
      .toLowerCase();
  return sentence.isEmpty ? '' : sentence[0].toUpperCase() + sentence.substring(1);
}

String _indent(String value, int spaces) => value.replaceAll('\n', '\n${' ' * spaces}');

String _formatValue(Object? value) {
  if (value == null) return 'None';
  if (value is bool) return value ? 'Yes' : 'No';
  if (value is String || value is num) return '$value';
  if (value is List) {
    if (value.isEmpty) return 'None';
    return value.map((item) => '• ${_indent(_formatValue(item), 2)}').join('\n');
  }
  if (value is Map) {
    if (value.isEmpty) return 'None';
    return value.entries
        .map((e) => '${_humanizeKey('${e.key}')}: ${_indent(_formatValue(e.value), 2)}')
        .join('\n');
  }
  return '$value';
}

List<({String label, String value})> _fieldsFromValue(
  Object? value, {
  List<String>? order,
  String? omittedKey,
  List<String>? includedKeys,
}) {
  if (value == null) return const [];
  if (value is! Map) {
    final formatted = _formatValue(value);
    return formatted.isEmpty ? const [] : [(label: 'Value', value: formatted)];
  }
  final map = jsonMap(value);
  final included = includedKeys?.toSet();
  final keys = map.keys
      .where((key) => key != omittedKey && (included == null || included.contains(key)))
      .toList();
  final rank = {for (final (i, key) in (order ?? const <String>[]).indexed) key: i};
  keys.sort((a, b) {
    final byRank = (rank[a] ?? 1 << 30).compareTo(rank[b] ?? 1 << 30);
    return byRank != 0 ? byRank : a.compareTo(b);
  });
  return [for (final key in keys) (label: _humanizeKey(key), value: _formatValue(map[key]))];
}

Object? _unwrapMcpResult(Object? output) {
  if (output is! Map) return output;
  if (output.containsKey('structuredContent') && output['structuredContent'] != null) {
    return output['structuredContent'];
  }
  final content = output['content'];
  if (content is List && content.length == 1) {
    final item = content.first;
    if (item is Map && item['type'] == 'text') {
      final text = item['text'];
      if (text is String) {
        try {
          return jsonDecode(text);
        } catch (_) {
          return text;
        }
      }
      return text;
    }
  }
  return output;
}

/// Prompt/Details/Result sections for Paseo MCP tools; null for other tools.
List<PaseoDetailSection>? buildPaseoToolDetailSections(String toolName, Object? input, Object? output) {
  final leaf = getPaseoToolLeafName(toolName);
  if (leaf == null) return null;
  final spec = _paseoCatalog[leaf];
  final sections = <PaseoDetailSection>[];
  final promptField = spec?.promptField;
  if (promptField != null && input is Map) {
    final prompt = input[promptField];
    if (prompt is String && prompt.isNotEmpty) {
      sections.add((title: 'Prompt', text: prompt, fields: const []));
    }
  }
  final inputFields = _fieldsFromValue(input, order: spec?.inputOrder, omittedKey: promptField);
  if (inputFields.isNotEmpty) sections.add((title: 'Details', text: null, fields: inputFields));
  final outputFields = _fieldsFromValue(
    _unwrapMcpResult(output),
    order: spec?.outputFields,
    includedKeys: spec?.outputFields,
  );
  if (outputFields.isNotEmpty) sections.add((title: 'Result', text: null, fields: outputFields));
  return sections;
}

// ---- tool-call-display.ts ----

String humanizeToolName(String name) {
  final trimmed = name.trim();
  if (trimmed.isEmpty) return name;
  if (RegExp(r'[:./]').hasMatch(trimmed) || trimmed.contains('__')) return trimmed;
  final words = trimmed
      .replaceAll(RegExp(r'[._-]+'), ' ')
      .split(' ')
      .where((segment) => segment.isNotEmpty)
      .join(' ')
      .toLowerCase();
  return words.isEmpty ? words : words[0].toUpperCase() + words.substring(1);
}

({String displayName, String? summary}) _canonicalDisplay(JsonMap detail, String? cwd) =>
    switch (detail['type']) {
      'shell' => (displayName: 'Shell', summary: stringValue(detail['command'])),
      'read' => (displayName: 'Read', summary: stripCwdPrefix(stringValue(detail['filePath']), cwd)),
      'edit' => (displayName: 'Edit', summary: stripCwdPrefix(stringValue(detail['filePath']), cwd)),
      'write' => (displayName: 'Write', summary: stripCwdPrefix(stringValue(detail['filePath']), cwd)),
      'search' => (displayName: 'Search', summary: stringValue(detail['query'])),
      'fetch' => (displayName: 'Fetch', summary: stringValue(detail['url'])),
      'worktree_setup' => (displayName: 'Worktree setup', summary: stringValue(detail['branchName'])),
      'sub_agent' => (
        displayName: _readString(detail['subAgentType']) ?? 'Task',
        summary: _readString(detail['description']),
      ),
      'plain_text' => (displayName: '', summary: stringValue(detail['label'])),
      'plan' => (displayName: 'Plan', summary: null),
      _ => (displayName: '', summary: null),
    };

// ---- utils/tool-call-icon-name.ts ----

ToolGlyph _iconByName(String? name) => switch (name) {
  'square_terminal' => const ToolGlyph.icon(LucideIcons.squareTerminal300),
  'eye' => const ToolGlyph.icon(LucideIcons.eye300),
  'pencil' => const ToolGlyph.icon(LucideIcons.pencil300),
  'search' => const ToolGlyph.icon(LucideIcons.search300),
  'bot' => const ToolGlyph.icon(LucideIcons.bot300),
  'sparkles' => const ToolGlyph.icon(LucideIcons.sparkles300),
  'brain' => const ToolGlyph.icon(LucideIcons.brain300),
  'mic_vocal' => const ToolGlyph.icon(LucideIcons.micVocal300),
  'paseo' => const ToolGlyph.paseo(),
  _ => const ToolGlyph.icon(LucideIcons.wrench300),
};

ToolGlyph resolveToolCallIcon(String toolName, JsonMap? detail, {bool agentMessage = false}) {
  if (agentMessage) return const ToolGlyph.paseo();
  final lower = toolName.trim().toLowerCase();
  if (detail?['type'] == 'plain_text' && detail?['icon'] is String) {
    return _iconByName(detail!['icon'] as String);
  }
  if (lower == 'thinking' && (detail == null || detail['type'] == 'unknown')) {
    return _iconByName('brain');
  }
  if (lower == 'speak') return _iconByName('mic_vocal');
  if (isPaseoToolName(lower)) return const ToolGlyph.paseo();
  if (lower == 'task') return _iconByName('bot');
  if (detail == null) return _iconByName('wrench');
  return _iconByName(switch (detail['type']) {
    'shell' || 'worktree_setup' => 'square_terminal',
    'read' => 'eye',
    'edit' || 'write' => 'pencil',
    'search' || 'fetch' => 'search',
    'sub_agent' => 'bot',
    'plan' => 'brain',
    _ => 'wrench',
  });
}

// ---- utils/extract-tool-call-file-path.ts ----

const _shellFileCommands = {
  'cat', 'bat', 'less', 'more', 'head', 'tail', 'wc', 'nl', 'tac', 'od', 'xxd', 'file',
  'stat', 'column', 'md5', 'md5sum', 'sha1sum', 'sha256sum', 'shasum',
};

String? _pathFromShellCommand(String command) {
  final trimmed = command.trim();
  if (trimmed.isEmpty || RegExp(r'[|><&;`$()]').hasMatch(trimmed)) return null;
  final tokens = trimmed.split(RegExp(r'\s+'));
  if (tokens.length < 2 || !_shellFileCommands.contains(tokens.first)) return null;
  final last = tokens.last;
  if (last.startsWith('-')) return null;
  if (tokens.length > 2) {
    final previous = tokens[tokens.length - 2];
    if (!previous.startsWith('-')) {
      final beforePrevious = tokens.length > 2 ? tokens[tokens.length - 3] : null;
      if (beforePrevious == null || !RegExp(r'^-[a-zA-Z]$').hasMatch(beforePrevious)) return null;
    }
  }
  return last;
}

String? extractToolCallFilePath(JsonMap detail) => switch (detail['type']) {
  'read' || 'edit' || 'write' => _readString(detail['filePath']),
  'shell' => _pathFromShellCommand(stringValue(detail['command'])),
  _ => null,
};

// ---- tool-calls/presentation.ts ----

class ToolCallPresentation {
  const ToolCallPresentation({
    required this.toolName,
    required this.detail,
    required this.displayName,
    required this.icon,
    required this.status,
    this.summary,
    this.errorText,
    this.isLoadingDetails = false,
    this.canOpenDetails = false,
    this.openFilePath,
    this.planOutcome,
  });

  final String toolName;
  final JsonMap detail;
  final String displayName;
  final String? summary;
  final String? errorText;
  final ToolGlyph icon;
  final String status;
  final bool isLoadingDetails, canOpenDetails;
  final String? openFilePath;

  /// plan-card.tsx PlanOutcome: pending / approved / rejected / canceled.
  final String? planOutcome;

  bool get isPlan => detail['type'] == 'plan';
  bool get isLoading => status == 'running' || status == 'executing';
  bool get isError => status == 'failed';
}

/// Builds the badge model for a daemon `tool_call` timeline item.
ToolCallPresentation buildToolCallPresentation(
  JsonMap item, {
  String? cwd,
  String? targetAgentTitle,
}) {
  final name = stringValue(item['name']);
  final status = stringValue(item['status'], 'completed');
  final error = item['error'];
  final metadata = jsonMap(item['metadata']);
  final rawDetail = item['detail'] is Map
      ? jsonMap(item['detail'])
      : <String, dynamic>{'type': 'unknown', 'input': null, 'output': null};
  final agentMessage = item['agentMessage'] is Map ? jsonMap(item['agentMessage']) : null;
  final agentDisplay = agentMessage == null ? null : buildAgentMessageDisplay(agentMessage);
  final detail = agentDisplay?.detail ?? rawDetail;
  final errorText = toolCallErrorText(status, error);

  String displayName;
  String? summary;
  final paseo = agentDisplay != null
      ? (displayName: agentDisplay.displayName, summary: agentDisplay.summary)
      : _paseoToolDisplay(name, rawDetail, targetAgentTitle);
  if (paseo != null) {
    displayName = paseo.displayName;
    summary = paseo.summary;
  } else {
    final canonical = _canonicalDisplay(rawDetail, cwd);
    final lower = name.trim().toLowerCase();
    final unknown = rawDetail['type'] == 'unknown';
    String? overrideName, overrideSummary;
    if (unknown && lower == 'task') {
      overrideName = 'Task';
      overrideSummary = _readString(metadata['subAgentActivity']);
    } else if (unknown && lower == 'thinking') {
      overrideName = 'Thinking';
    } else if (lower == 'terminal') {
      overrideName = 'Terminal';
      overrideSummary =
          rawDetail['type'] == 'plain_text' ? _readString(rawDetail['label']) : null;
    }
    displayName = overrideName ??
        (canonical.displayName.isEmpty ? humanizeToolName(name) : canonical.displayName);
    summary = overrideName != null ? overrideSummary : canonical.summary;
  }
  if (summary != null && summary.isEmpty) summary = null;

  final loadingDetails = isPendingToolCallDetail(detail, status, error);
  final hasDetails = (error != null && error != false && error != '' && error != 0) ||
      hasMeaningfulToolCallDetail(detail);
  final planOutcome = rawDetail['type'] != 'plan'
      ? null
      : status == 'canceled'
      ? 'canceled'
      : metadata['approved'] == false
      ? 'rejected'
      : metadata['approved'] == true
      ? 'approved'
      : status == 'running' || status == 'executing'
      ? 'pending'
      : null;
  return ToolCallPresentation(
    toolName: name,
    detail: detail,
    displayName: displayName,
    summary: summary,
    errorText: errorText,
    icon: resolveToolCallIcon(name, detail, agentMessage: agentMessage != null),
    status: status,
    isLoadingDetails: loadingDetails,
    canOpenDetails: hasDetails || loadingDetails,
    openFilePath: extractToolCallFilePath(detail),
    planOutcome: planOutcome,
  );
}

// ---- tool-calls/detail-level/overview/model.ts ----

typedef OverviewSummary = ({
  int editedFileCount,
  int commandCount,
  int readFileCount,
  int searchCount,
  int otherToolCount,
  int paseoCallCount,
});

({OverviewSummary summary, bool isLoading}) buildOverviewSummary(List<JsonMap> calls) {
  final edited = <String>{}, read = <String>{};
  var loading = false;
  var commands = 0, searches = 0, others = 0, paseo = 0;
  for (final call in calls) {
    final item = jsonMap(call['item']);
    final name = stringValue(item['name']);
    final normalized = name.trim().toLowerCase();
    final detail = jsonMap(item['detail']);
    final status = stringValue(item['status']);
    loading = loading || status == 'running' || status == 'executing';
    if (isPaseoToolName(name) || normalized.startsWith('paseo_')) {
      paseo++;
    } else if (detail['type'] == 'edit' || detail['type'] == 'write') {
      edited.add(stringValue(detail['filePath']));
    } else if (detail['type'] == 'shell') {
      commands++;
    } else if (detail['type'] == 'read') {
      read.add(stringValue(detail['filePath']));
    } else if (detail['type'] == 'search' ||
        RegExp(r'(?:^|[_.:/])(?:web_search|llm_context)$').hasMatch(normalized)) {
      searches++;
    } else {
      others++;
    }
  }
  return (
    summary: (
      editedFileCount: edited.length,
      commandCount: commands,
      readFileCount: read.length,
      searchCount: searches,
      otherToolCount: others,
      paseoCallCount: paseo,
    ),
    isLoading: loading,
  );
}
