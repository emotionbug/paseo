import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:flutter/services.dart';
import 'package:path/path.dart' as p;

import 'host_config.dart';
import 'json.dart';
import 'legacy_leveldb.dart';
import 'target.dart';

typedef LegacyStateReader = Future<LegacyStateSnapshot> Function();

class LegacyStateSnapshot {
  const LegacyStateSnapshot({required this.values, required this.sources});
  final Map<String, String> values;
  final List<String> sources;
}

Future<LegacyStateSnapshot> readLegacyState() async {
  if (Platform.isAndroid || Platform.isIOS) {
    final result = await const MethodChannel('sh.paseo/legacy_storage')
        .invokeMapMethod<String, dynamic>('read');
    if (result == null || result['values'] is! Map || result['sources'] is! List) {
      throw StateError('Original settings storage is unavailable; restart to retry.');
    }
    return LegacyStateSnapshot(
      values: Map<String, String>.from(result['values'] as Map),
      sources: List<String>.from(result['sources'] as List),
    );
  }
  if (!(Platform.isMacOS || Platform.isWindows || Platform.isLinux)) {
    throw UnsupportedError('Original settings import is unavailable on this platform.');
  }
  final env = Platform.environment;
  final home = env['HOME'] ?? env['USERPROFILE'];
  if (home == null && env['PASEO_ELECTRON_USER_DATA_DIR'] == null) {
    throw StateError('Cannot locate original desktop settings: home directory is unavailable.');
  }
  final base = env['PASEO_ELECTRON_USER_DATA_DIR'] ?? (Platform.isMacOS
      ? p.join(home!, 'Library', 'Application Support', 'Paseo')
      : Platform.isWindows ? p.join(env['APPDATA'] ?? home!, 'Paseo')
      : p.join(env['XDG_CONFIG_HOME'] ?? p.join(home!, '.config'), 'Paseo'));
  final storage = Directory(p.join(base, 'Local Storage', 'leveldb'));
  final values = await readLegacyLocalStorage(storage);
  final sources = [storage.path];
  // Desktop settings have a separate native owner. Keep their original complete
  // documents here as recovery data; DesktopSettingsStore imports their behavior.
  for (final name in ['desktop-settings.json', 'window-state.json', 'skill-selection.json']) {
    final file = File(p.join(base, name));
    sources.add(file.path);
    if (await file.exists()) {
      final raw = await file.readAsString();
      if (jsonDecode(raw) is! Map) throw FormatException('Invalid original $name');
      values['desktop:$name'] = raw;
    }
  }
  return LegacyStateSnapshot(values: values, sources: sources);
}

const legacyImportVersion = 1;

/// Identity of one complete original snapshot. While it matches the stored
/// base, launch neither parses the original nor writes Flutter state.
String legacyDigest(Map<String, String> values) {
  final keys = values.keys.toList()..sort();
  return sha256.convert(utf8.encode(jsonEncode([
    for (final key in keys) [key, values[key]],
  ]))).toString();
}

/// First import (no marker): all original strings, including unknown/newer
/// stores, remain byte-for-byte recoverable under legacyImport.values. Existing
/// Flutter keys (including null/false/empty) win recursively. Existing host
/// identities/endpoints win; only missing hosts/connections/fields import.
///
/// Later launches re-sync: legacyImport.values is the base, the current
/// original snapshot is theirs and Flutter state is ours (see [_resync]).
/// Mapping never writes the source. A malformed or unreadable source throws
/// before any state or the stored base changes; an empty one changes nothing.
JsonMap mergeLegacyState(JsonMap current, LegacyStateSnapshot source, {bool? native}) {
  final mobile = native ?? (Platform.isAndroid || Platform.isIOS);
  if (source.values.isEmpty) return current;
  final digest = legacyDigest(source.values);
  final marker = jsonMap(current['legacyImport']);
  if (marker['version'] == legacyImportVersion) {
    return _resync(current, marker, source, digest, mobile);
  }
  final mapped = _mapLegacy(source, mobile);
  final existingHosts = _flutterHosts(current);
  for (final host in mapped.hosts) {
    _absorbHost(existingHosts, host);
  }
  final existingLayout = jsonMap(current['layout']);
  final layout = mapped.layout.isNotEmpty && _emptyShell(existingLayout) ? <String, dynamic>{
    ...existingLayout,
    ...mapped.layout,
    // Explicit non-default shell geometry is retained even with no open tabs.
    if (existingLayout['explorerWidth'] is num && existingLayout['explorerWidth'] != 340)
      'explorerWidth': existingLayout['explorerWidth'],
    if (existingLayout['sidebarVisible'] == false) 'sidebarVisible': false,
  } : null;
  return {
    ...current,
    'version': 1,
    'preferences': _missing(_flutterPreferences(current), mapped.preferences),
    'hosts': existingHosts,
    'layout': ?layout,
    'legacyImport': _marker(source, digest, mapped.notes, layout),
  };
}

JsonMap _marker(LegacyStateSnapshot source, String digest, List<String> notes, JsonMap? layout) => {
  'version': legacyImportVersion,
  'digest': digest,
  'sources': source.sources,
  'values': source.values,
  'retainedOnly': notes,
  // The shell layout this import last wrote; null when Flutter's own layout won.
  'layout': layout,
};

/// Three-way re-sync of an original app that kept running after the first
/// import. base = legacyImport.values, theirs = current original snapshot,
/// ours = Flutter state. An original change is adopted only where Flutter
/// still holds what the base mapped to; every Flutter change wins.
JsonMap _resync(JsonMap current, JsonMap marker, LegacyStateSnapshot source,
    String digest, bool mobile) {
  final baseValues = <String, String>{
    for (final entry in jsonMap(marker['values']).entries)
      if (entry.value is String) entry.key: entry.value as String,
  };
  final baseDigest = marker['digest'] is String ? marker['digest'] as String : legacyDigest(baseValues);
  if (baseDigest == digest) return current;
  final theirs = _mapLegacy(source, mobile);
  _Mapped base;
  try {
    base = _mapLegacy(LegacyStateSnapshot(values: baseValues, sources: const []), mobile);
  } catch (_) {
    // A base this build cannot interpret proves nothing about Flutter values:
    // only what Flutter lacks is filled, exactly like a first import.
    base = _Mapped({}, [], {}, []);
  }

  // Layout and its selected session: adopted only while the Flutter shell is
  // still exactly what the import wrote (or the untouched empty default).
  final ourLayout = jsonMap(current['layout']);
  final written = marker.containsKey('layout') ? jsonMap(marker['layout']) : base.layout;
  final layoutUntouched = written.isEmpty
      ? _emptyShell(ourLayout)
      : _same(_layoutShape(ourLayout), _layoutShape(written));
  final adoptLayout = layoutUntouched && theirs.layout.isNotEmpty &&
      !_same(_layoutShape(theirs.layout), base.layout.isEmpty ? null : _layoutShape(base.layout));
  final layout = adoptLayout ? <String, dynamic>{...ourLayout, ...theirs.layout} : null;

  final ourPreferences = _flutterPreferences(current);
  final preferences = <String, dynamic>{...ourPreferences};
  for (final key in {...base.preferences.keys, ...theirs.preferences.keys}) {
    final b = _field(base.preferences, key);
    final t = _field(theirs.preferences, key);
    final o = _field(ourPreferences, key);
    final merged = switch (key) {
      'legacyDrafts' => _mergeLegacyDrafts(b, t, o),
      'lastWorkspaceSelection' when !layoutUntouched => o,
      _ => _merge3(b, t, o),
    };
    if (identical(merged, _absent)) {
      preferences.remove(key);
    } else {
      preferences[key] = merged;
    }
  }

  final hosts = _flutterHosts(current);
  _resyncHosts(hosts, base.hosts, theirs.hosts);
  return {
    ...current,
    'version': 1,
    'preferences': preferences,
    'hosts': hosts,
    'layout': ?layout,
    'legacyImport': {
      ...marker,
      ..._marker(source, digest, theirs.notes, layout ?? (written.isEmpty ? null : written)),
    },
  };
}

class _Mapped {
  _Mapped(this.preferences, this.hosts, this.layout, this.notes);
  final JsonMap preferences;
  final List<JsonMap> hosts;
  /// The shell layout the original would open (selected workspace, else first).
  final JsonMap layout;
  final List<String> notes;
}

/// Pure mapping of one original snapshot to Flutter values; never reads state.
_Mapped _mapLegacy(LegacyStateSnapshot source, bool mobile) {
  final decoded = <String, dynamic>{};
  for (final entry in source.values.entries) {
    // Only stores this import consumes are parsed. Every other original value
    // (caches, tokens, unknown/newer stores) is retained verbatim, unparsed.
    if (_jsonStores.contains(entry.key) || entry.key.startsWith('desktop:')) {
      try {
        decoded[entry.key] = jsonDecode(entry.value);
      } catch (_) {
        // Never include source text in an error: registry values contain secrets.
        throw FormatException('Original settings contain malformed JSON (${entry.key}); no import was committed.');
      }
    }
  }
  JsonMap object(String key, {bool store = false}) {
    if (!decoded.containsKey(key)) return {};
    final value = decoded[key];
    if (value is! Map || store && value['state'] is! Map) {
      throw FormatException('Invalid original settings object ($key)');
    }
    return jsonMap(store ? value['state'] : value);
  }

  final currentSettings = source.values.containsKey('@paseo:app-settings');
  final settingsKey = currentSettings ? '@paseo:app-settings' : '@paseo:settings';
  final preferences = object(settingsKey);
  if (source.values.containsKey(settingsKey)) {
    _normalizeSettings(preferences, legacyKey: !currentSettings, native: mobile,
      applied: {...object('@paseo:settings-migrations').array('applied').whereType<String>()});
  }
  if (preferences.containsKey('terminalScrollbackLines')) {
    preferences['executionLogLines'] = preferences['terminalScrollbackLines'];
  }
  final overrides = object('@paseo:keyboard-shortcut-overrides');
  if (source.values.containsKey('@paseo:keyboard-shortcut-overrides')) {
    if (overrides.values.any((value) => value != null && value is! String)) {
      throw const FormatException('Invalid original keyboard shortcut override');
    }
    preferences['keyboardShortcuts'] = {
      for (final entry in overrides.entries) entry.key: entry.value ?? '',
    };
  }
  final panel = object('panel-state', store: true);
  final view = object(source.values.containsKey('sidebar-view') ? 'sidebar-view' : 'sidebar-group-mode', store: true);
  // migrateSidebarViewState compat: per-host group modes and single host filter.
  final modes = jsonMap(view.remove('groupModeByServerId')).values;
  if (modes.isNotEmpty) view['groupMode'] = modes.contains('status') ? 'status' : 'project';
  final hostFilter = view.remove('hostFilter');
  if ((view.isNotEmpty || hostFilter != null) && !view.containsKey('hostFilters')) {
    view['hostFilters'] = [if (hostFilter is String && hostFilter.isNotEmpty) hostFilter];
  }
  final order = object('sidebar-project-workspace-order', store: true);
  final collapsed = object('sidebar-collapsed-sections', store: true);
  preferences['navigation'] = {
    ...view,
    ...order,
    ...collapsed,
    if (view['labelFilter'] is Map) 'labelFilters': jsonMap(view['labelFilter'])['labels'],
  };
  for (final entry in panel.entries) {
    if (entry.key != 'desktop') preferences[entry.key] = entry.value;
  }
  if (source.values.containsKey('@paseo:preferred-editor')) {
    preferences['preferredEditor'] = source.values['@paseo:preferred-editor'];
  }
  if (source.values.containsKey('@paseo:client-id-v1')) {
    preferences['legacyClientId'] = source.values['@paseo:client-id-v1'];
  }
  for (final mapping in {
    '@paseo:changes-preferences': 'changesPreferences',
    '@paseo:create-agent-preferences': 'createAgentPreferences',
    'paseo:last-workspace-route-selection': 'lastWorkspaceSelection',
  }.entries) {
    if (source.values.containsKey(mapping.key)) preferences[mapping.value] = object(mapping.key);
  }
  if (source.values.containsKey('paseo-drafts')) {
    preferences['legacyDrafts'] = object('paseo-drafts', store: true);
  }
  final notes = <String>[
    'Original AsyncStorage values are retained in full. Browser/plugin/interactive-terminal surfaces are excluded.',
    'Nonselected workspace trees are retained in legacyWorkspaceLayouts; the native shell has one active global tree.',
    'Provider/icon/replica caches, review drafts, callout dismissals, favorite-profile migration markers, service routes, push tokens, client identity, desktop window geometry, and unknown stores remain recovery data unless consumed by their native feature owner.',
  ];
  final rawHosts = decoded['@paseo:daemon-registry'];
  if (decoded.containsKey('@paseo:daemon-registry') && rawHosts is! List) {
    throw const FormatException('Invalid original host registry');
  }
  final hosts = _hosts(rawHosts as List? ?? [], notes);
  final workspaces = object('workspace-layout-state', store: true);
  final selection = object('paseo:last-workspace-route-selection');
  final layouts = _layouts(workspaces, hosts, panel, notes);
  preferences['legacyWorkspaceLayouts'] = layouts;
  final selectedKey = '${selection['serverId']}:${selection['workspaceId']}';
  if (selection.isNotEmpty && !layouts.containsKey(selectedKey)) {
    final target = PanelTarget(kind: 'new_tab',
      serverId: _required(selection, 'serverId'),
      workspaceId: _required(selection, 'workspaceId'), cwd: '',
      data: const {'legacyImported': true});
    layouts[selectedKey] = {
      'tabs': [target.toJson()],
      'tree': {'kind': 'pane', 'id': 'main', 'tabs': [target.key], 'selected': target.key},
      'focusedPane': 'main', 'sidebarVisible': jsonMap(panel['desktop'])['agentListOpen'] != false,
    };
  }
  final layout = jsonMap(layouts[selectedKey] ?? layouts.values.firstOrNull);
  return _Mapped(preferences, hosts, layout, notes);
}

JsonMap _flutterPreferences(JsonMap current) {
  final preferences = {...jsonMap(current['preferences'])};
  if (!preferences.containsKey('theme') && preferences.containsKey('themeMode')) {
    preferences['theme'] = preferences['themeMode'];
  }
  if (!preferences.containsKey('language') && preferences.containsKey('locale')) {
    preferences['language'] = preferences['locale'];
  }
  return preferences;
}

List<JsonMap> _flutterHosts(JsonMap current) => jsonList(current['hosts']).map((raw) {
  final row = _object(raw, 'Flutter host');
  return <String, dynamic>{
    ...row,
    'config': {...jsonMap(row['config'] ?? row)},
    'connections': [
      for (final connection in jsonList(row['connections']))
        {..._object(connection, 'Flutter connection')},
    ],
  };
}).toList();

/// The failed first launch persists this empty default shell. It is not an
/// explicit user selection; every nonempty/custom layout remains authoritative.
bool _emptyShell(JsonMap layout) =>
    jsonList(layout['tabs']).isEmpty &&
    jsonList(layout['explorerTabs']).isEmpty &&
    (layout['tree'] == null ||
      jsonMap(layout['tree'])['kind'] == 'pane' &&
      jsonList(jsonMap(layout['tree'])['tabs']).isEmpty);

/// The Flutter host that already stands for [host]: same daemon identity, or
/// the same socket unless the Flutter host is pinned to a different daemon.
JsonMap? _matchHost(List<JsonMap> rows, JsonMap host) {
  final importedConfig = jsonMap(host['config']);
  // A routed host shares its origin's socket, so only identity can match it.
  final importedIdentities = host['originServerId'] != null ? const <String>{} :
      jsonList(host['connections']).map((c) => _endpointIdentity(jsonMap(c))).nonNulls.toSet();
  return rows.where((row) {
    final config = jsonMap(row['config'] ?? row);
    if (row['serverId'] == host['serverId'] || config['expectedServerId'] == host['serverId'] ||
        config['id'] == importedConfig['id']) {
      return true;
    }
    return (config['expectedServerId'] == null) && row['originServerId'] == null &&
        [config, ...jsonList(row['connections']).map(jsonMap)]
            .any((c) => importedIdentities.contains(_endpointIdentity(c)));
  }).firstOrNull;
}

/// First-import host rule: add an unknown host, else fill only what the
/// matching Flutter host lacks (connections, fields, identity).
void _absorbHost(List<JsonMap> existingHosts, JsonMap host) {
  final existing = _matchHost(existingHosts, host);
  if (existing == null) {
    existingHosts.add(host);
    return;
  }
  final importedConfig = jsonMap(host['config']);
  final config = jsonMap(existing['config'] ?? existing);
  final connections = [config, ...jsonList(existing['connections']).map(jsonMap)];
  for (final candidate in jsonList(host['connections']).map(jsonMap)) {
    final identity = _endpointIdentity(candidate);
    final match = connections.where((item) => item['id'] == candidate['id'] ||
        identity != null && _endpointIdentity(item) == identity).firstOrNull;
    if (match == null) { connections.add(candidate); } else {
      final merged = _missing(match, candidate);
      match.addAll(merged);
    }
  }
  existing['config'] = _missing(config, importedConfig);
  // An unpinned Flutter host only carries its connection id as identity;
  // the original registry knows the daemon's real serverId.
  if (existing['serverId'] == null || existing['serverId'] == config['id']) {
    existing['serverId'] = host['serverId'];
  }
  if (existing['originServerId'] == null && host['originServerId'] != null) {
    existing['originServerId'] = host['originServerId'];
  }
  existing['connections'] = [
    for (var i = 0; i < connections.length; i++)
      if (!connections.take(i).any((c) => c['id'] == connections[i]['id'])) connections[i],
  ];
}

/// Hosts new in the original are added with first-import de-dup rules. Hosts
/// present in base and theirs update only fields whose Flutter copy still
/// equals the base. A host removed in the original is removed from Flutter
/// only when the Flutter copy is untouched since import; a host removed in
/// Flutter stays removed.
void _resyncHosts(List<JsonMap> ours, List<JsonMap> base, List<JsonMap> theirs) {
  final baseById = {for (final host in base) host['serverId']: host};
  final theirIds = {for (final host in theirs) host['serverId']};
  for (final removed in base.where((host) => !theirIds.contains(host['serverId']))) {
    final mine = _matchHost(ours, removed);
    if (mine != null && _same(_hostShape(mine), _hostShape(removed), loose: true)) {
      ours.remove(mine);
    }
  }
  for (final host in theirs) {
    final before = baseById[host['serverId']];
    if (before == null) {
      _absorbHost(ours, host);
      continue;
    }
    if (_same(_hostShape(host), _hostShape(before), loose: true)) continue;
    final mine = _matchHost(ours, host) ?? _matchHost(ours, before);
    if (mine != null) _updateHost(mine, before, host);
  }
}

/// Connection rows by id, preferred connection first (the load order).
Map<String, JsonMap> _connectionsById(JsonMap host) {
  final result = <String, JsonMap>{};
  for (final raw in [host['config'], ...jsonList(host['connections'])]) {
    final connection = jsonMap(raw);
    final id = connection['id'];
    if (id is String) result.putIfAbsent(id, () => {...connection});
  }
  return result;
}

JsonMap _hostShape(JsonMap host) => {
  'serverId': host['serverId'],
  'originServerId': host['originServerId'],
  'preferred': jsonMap(host['config'])['id'],
  'connections': _connectionsById(host),
};

void _updateHost(JsonMap mine, JsonMap before, JsonMap theirs) {
  final ours = _connectionsById(mine);
  final base = _connectionsById(before);
  final next = _connectionsById(theirs);
  final merged = <String, JsonMap>{};
  for (final entry in ours.entries) {
    final b = base[entry.key];
    final t = next[entry.key];
    if (t == null) {
      // Removed in the original: mirrored only for an untouched Flutter copy.
      if (b == null || !_same(entry.value, b, loose: true)) merged[entry.key] = entry.value;
    } else {
      // Endpoint, password, label, color: per field, Flutter edits win.
      merged[entry.key] = b == null ? _missing(entry.value, t) : jsonMap(_merge3(b, t, entry.value, loose: true));
    }
  }
  for (final entry in next.entries) {
    // Present in base but not ours: removed in Flutter, stays removed.
    if (ours.containsKey(entry.key) || base.containsKey(entry.key)) continue;
    final identity = _endpointIdentity(entry.value);
    final twin = identity == null ? null :
        merged.keys.where((id) => _endpointIdentity(merged[id]!) == identity).firstOrNull;
    if (twin == null) {
      merged[entry.key] = entry.value;
    } else {
      merged[twin] = _missing(merged[twin]!, entry.value);
    }
  }
  final oursConfig = jsonMap(mine['config']);
  final baseConfig = jsonMap(before['config']);
  final theirConfig = jsonMap(theirs['config']);
  // The preferred connection follows the original only while Flutter's
  // choice is still the one the base imported.
  final preferred = oursConfig['id'] == baseConfig['id'] && merged.containsKey(theirConfig['id'])
      ? theirConfig['id'] : oursConfig['id'];
  final config = merged[preferred] ?? {...oursConfig};
  if (mine['originServerId'] == before['originServerId']) {
    mine['originServerId'] = theirs['originServerId'];
  }
  mine['config'] = config;
  mine['connections'] = [config, ...merged.values.where((c) => c['id'] != config['id'])];
}

/// Comparable projection of a persisted shell layout: what the Flutter shell
/// restores from it (target keys, tree, selection, explorer, geometry), so
/// Flutter's own normalization on save (resolved cwd, titles, focus default,
/// clamped ratios, int/double) is not mistaken for a user change.
JsonMap _layoutShape(JsonMap layout) {
  final keys = <String>{};
  final migrated = <String, String>{};
  for (final raw in jsonList(layout['tabs'])) {
    final row = jsonMap(raw);
    final target = PanelTarget(kind: stringValue(row['kind']),
      serverId: stringValue(row['serverId']), workspaceId: stringValue(row['workspaceId']),
      cwd: '', id: row['id'] is String ? row['id'] as String : null);
    if (target.kind == 'browser' || target.kind == 'plugin' ||
        target.kind == 'terminal' && target.id == null) {
      continue;
    }
    final key = (target.kind == 'terminal' ? target.copyWith(kind: 'execution_log') : target).key;
    migrated[target.key] = key;
    keys.add(key);
  }
  final seen = <String>{};
  List<String> placed(Object? value) => jsonList(value).whereType<String>()
      .map((key) => migrated[key] ?? key)
      .where((key) => keys.contains(key) && seen.add(key)).toList();
  String? pick(List<String> tabs, Object? selected) {
    final key = selected is String ? migrated[selected] ?? selected : null;
    return tabs.contains(key) ? key : tabs.firstOrNull;
  }
  String? firstPane;
  JsonMap node(JsonMap value) {
    if (value['kind'] == 'split') {
      final ratio = value['ratio'];
      return {
        'axis': value['axis'] == 'vertical' ? 'vertical' : 'horizontal',
        'ratio': ((ratio is num && ratio.isFinite ? ratio.toDouble().clamp(0.1, 0.9) : 0.5) * 1000).round(),
        'first': node(jsonMap(value['first'])),
        'second': node(jsonMap(value['second'])),
      };
    }
    final id = stringValue(value['id'], 'main');
    firstPane ??= id;
    final tabs = placed(value['tabs']);
    return {'id': id, 'tabs': tabs, 'selected': pick(tabs, value['selected'])};
  }
  final tree = node(jsonMap(layout['tree']));
  final explorer = placed(layout['explorerTabs']);
  final width = layout['explorerWidth'];
  return {
    'tabs': keys.toList()..sort(),
    'tree': tree,
    'focusedPane': stringValue(layout['focusedPane'], firstPane ?? 'main'),
    'explorerTabs': explorer,
    'selectedExplorer': pick(explorer, layout['selectedExplorer']),
    'explorerVisible': layout['explorerVisible'] == true,
    'explorerWidth': width is num && width.isFinite ? width.toDouble().clamp(180, 1000).round() : 340,
    'sidebarVisible': layout['sidebarVisible'] != false,
  };
}

/// Marks a key absent on one side of a three-way merge.
const _absent = _Absent();
class _Absent {
  const _Absent();
}
Object? _field(JsonMap map, String key) => map.containsKey(key) ? map[key] : _absent;

/// Three-way value merge: an original change is adopted where Flutter still
/// equals the base; any Flutter change wins. Maps merge per key.
Object? _merge3(Object? base, Object? theirs, Object? ours, {bool loose = false}) {
  if (_same(theirs, base, loose: loose)) return ours;
  if (_same(ours, base, loose: loose)) return theirs;
  if (base is Map && theirs is Map && ours is Map) {
    final b = jsonMap(base), t = jsonMap(theirs), o = jsonMap(ours);
    final result = <String, dynamic>{};
    for (final key in {...o.keys, ...t.keys, ...b.keys}) {
      final merged = _merge3(_field(b, key), _field(t, key), _field(o, key), loose: loose);
      if (!identical(merged, _absent)) result[key] = merged;
    }
    return result;
  }
  return ours;
}

/// Original drafts: a record changed in the original replaces an untouched
/// or older copy (by updatedAt); a newer retained copy is never dropped.
Object? _mergeLegacyDrafts(Object? base, Object? theirs, Object? ours) {
  if (_same(theirs, base)) return ours;
  if (theirs is! Map || ours is! Map) return _merge3(base, theirs, ours);
  final b = base is Map ? jsonMap(base) : const <String, dynamic>{};
  final t = jsonMap(theirs), o = jsonMap(ours);
  final result = <String, dynamic>{...o};
  void put(JsonMap target, String key, Object? value) {
    if (identical(value, _absent)) {
      target.remove(key);
    } else {
      target[key] = value;
    }
  }
  for (final key in {...b.keys, ...t.keys}) {
    if (key == 'drafts') continue;
    put(result, key, key == 'createModalDraft'
        ? _mergeDraftRecord(_field(b, key), _field(t, key), _field(o, key))
        : _merge3(_field(b, key), _field(t, key), _field(o, key)));
  }
  final baseDrafts = b.object('drafts'), theirDrafts = t.object('drafts'), ourDrafts = o.object('drafts');
  final drafts = <String, dynamic>{...ourDrafts};
  for (final key in {...baseDrafts.keys, ...theirDrafts.keys}) {
    put(drafts, key, _mergeDraftRecord(_field(baseDrafts, key), _field(theirDrafts, key), _field(ourDrafts, key)));
  }
  result['drafts'] = drafts;
  return result;
}

Object? _mergeDraftRecord(Object? base, Object? theirs, Object? ours) {
  if (_same(theirs, base)) return ours;
  bool missing(Object? value) => value == null || identical(value, _absent);
  if (missing(theirs)) return _same(ours, base) ? theirs : ours;
  if (missing(ours) || _same(ours, base)) return theirs;
  return legacyDraftUpdatedAt(theirs) > legacyDraftUpdatedAt(ours) ? theirs : ours;
}

/// Original draft record time (epoch ms or ISO); unknown sorts oldest.
double legacyDraftUpdatedAt(Object? record) {
  final value = jsonMap(record)['updatedAt'];
  if (value is num && value.isFinite) return value.toDouble();
  final parsed = value is String ? DateTime.tryParse(value) : null;
  return parsed?.millisecondsSinceEpoch.toDouble() ?? double.negativeInfinity;
}

/// Deep JSON equality, key-order independent, integral doubles equal ints.
/// [loose] also treats null fields as absent (persisted host rows omit nulls).
bool _same(Object? a, Object? b, {bool loose = false}) {
  if (loose) {
    if (identical(a, _absent)) a = null;
    if (identical(b, _absent)) b = null;
  }
  if (identical(a, _absent) || identical(b, _absent)) return identical(a, b);
  return jsonEncode(_canonical(a, loose)) == jsonEncode(_canonical(b, loose));
}

Object? _canonical(Object? value, bool loose) {
  if (value is Map) {
    final keys = value.keys.map((key) => '$key').toList()..sort();
    return {
      for (final key in keys)
        if (!loose || value[key] != null) key: _canonical(value[key], loose),
    };
  }
  if (value is List) return [for (final item in value) _canonical(item, loose)];
  if (value is double && value.isFinite && value == value.truncateToDouble()) return value.toInt();
  return value;
}

JsonMap _object(dynamic value, String label) {
  if (value is! Map) throw FormatException('Invalid $label');
  return jsonMap(value);
}
String _required(JsonMap value, String key) {
  final result = value[key];
  if (result is! String || result.isEmpty) throw FormatException('Missing original $key');
  return result;
}
/// Socket identity of a host connection, with loopback spellings unified (the
/// same rule the native client uses before sending a local credential).
String? _endpointIdentity(JsonMap config) {
  final endpoint = config['endpoint'];
  if (endpoint is! String) return null;
  try {
    final target = HostConfig(id: '', label: '', endpoint: endpoint).resolve();
    final uri = target.uri;
    if (uri.scheme == 'unix' || uri.scheme == 'pipe') return '${uri.scheme}:${uri.path}';
    final host = const {'localhost', '127.0.0.1', '0.0.0.0', '::1', '::'}.contains(uri.host) ? 'localhost' : uri.host;
    if (target.isRelay) return 'relay:$host:${uri.port}/${target.expectedServerId}';
    return '${uri.scheme == 'ssh' ? 'ssh:' : ''}$host:${uri.port}';
  } on FormatException {
    return null;
  }
}
JsonMap _missing(JsonMap existing, JsonMap imported) => {
  ...imported,
  for (final entry in existing.entries)
    entry.key: entry.value is Map && imported[entry.key] is Map
        ? _missing(jsonMap(entry.value), jsonMap(imported[entry.key])) : entry.value,
};

const _themes = {'light', 'dark', 'auto', 'zinc', 'midnight', 'claude', 'ghostty', 'pureBlack', 'plugin'};
const _languages = {'system', 'ar', 'en', 'es', 'fr', 'ja', 'ko', 'pt-BR', 'ru', 'zh-CN'};

/// Original parseClampedFontSize: finite number or nonblank numeric string, floored and clamped.
int? _clamped(dynamic value, int min, int max) {
  final number = value is num ? value : value is String && value.trim().isNotEmpty ? num.tryParse(value.trim()) : null;
  if (number == null || !number.isFinite) return null;
  return number.floor().clamp(min, max);
}

/// Original sanitizeFontFamily; null means the stored value was rejected.
String? _fontFamily(dynamic value) {
  if (value is! String) return null;
  final trimmed = value.trim();
  if (trimmed.isEmpty) return '';
  if (trimmed.length > 200 || RegExp(r'[;{}<>]').hasMatch(trimmed) ||
      trimmed.codeUnits.any((unit) => unit <= 0x1f)) {
    return null;
  }
  String? quote;
  var escaped = false;
  for (final char in trimmed.split('')) {
    if (escaped) {
      escaped = false;
    } else if (char == r'\') {
      escaped = true;
    } else if (quote != null) {
      if (char == quote) quote = null;
    } else if (char == "'" || char == '"') {
      quote = char;
    } else if ('()[]/*'.contains(char)) {
      return null;
    }
  }
  return quote == null && !escaped ? trimmed : null;
}

/// Applies the original StoredAppSettingsSchema transform and migrateAppSettings
/// to present fields, so Flutter renders exactly what the old app rendered.
/// Literal values (including theme names) are kept, never remapped.
void _normalizeSettings(JsonMap settings, {required bool legacyKey, required bool native,
    required Set<String> applied}) {
  void choice(String key, Set<String> allowed, String fallback) {
    if (settings.containsKey(key) && !allowed.contains(settings[key])) settings[key] = fallback;
  }
  choice('theme', _themes, 'auto');
  choice('language', _languages, 'system');
  choice('sendBehavior', const {'interrupt', 'steer', 'queue'}, 'steer');
  for (final key in ['uiFontFamily', 'monoFontFamily']) {
    if (settings.containsKey(key)) settings[key] = _fontFamily(settings[key]) ?? '';
  }
  if (settings.containsKey('terminalScrollbackLines')) {
    settings['terminalScrollbackLines'] = _clamped(settings['terminalScrollbackLines'], 0, 1000000) ?? 10000;
  }
  if (settings.containsKey('codeFontSize')) {
    settings['codeFontSize'] = _clamped(settings['codeFontSize'], 9, 22) ?? 12;
  }
  if (settings.containsKey('contentMaxWidth')) {
    settings['contentMaxWidth'] = _clamped(settings['contentMaxWidth'], 600, 4000);
  }
  final legacyScale = _clamped(settings.remove('uiFontSize'), 11, 24);
  final storedBase = _clamped(settings['uiBaseFontSize'], 10, 21);
  final base = storedBase ?? (legacyScale == null ? null : (14 * legacyScale / 16).round());
  if (base == null) {
    settings.remove('uiBaseFontSize');
  } else {
    settings['uiBaseFontSize'] = base;
  }
  // The UI base default is platform-specific (native 15, desktop 14). An absent
  // or legacy-key content size rendered on the resolved interface base size.
  final resolvedBase = base ?? (native ? 15 : 14);
  if (legacyKey || !settings.containsKey('contentFontSize')) {
    settings['contentFontSize'] = resolvedBase;
  } else {
    settings['contentFontSize'] = _clamped(settings['contentFontSize'], 10, 21) ?? (native ? 16 : 15);
  }
  final compact = settings.remove('compactToolCalls');
  final detail = settings['toolCallDetailLevel'];
  if (detail == 'concise') {
    settings['toolCallDetailLevel'] = 'overview';
  } else if (detail == null && !settings.containsKey('toolCallDetailLevel')) {
    if (compact != null) settings['toolCallDetailLevel'] = compact == true ? 'overview' : 'detailed';
  } else if (detail != 'overview' && detail != 'detailed') {
    settings['toolCallDetailLevel'] = 'detailed';
  }
  // migrateAppSettings markers live in @paseo:settings-migrations.
  if (!applied.contains('steer-default') && settings['sendBehavior'] == 'interrupt') {
    settings['sendBehavior'] = 'steer';
  }
  if (native && !applied.contains('mobile-content-16') && settings['contentFontSize'] == 15) {
    settings['contentFontSize'] = 16;
  }
}

/// IDENTITY_COLORS fill tones from the original styles/identity-colors.ts.
const _identityColors = {
  'violet': '#7a6aa8', 'sky': '#3d7ea6', 'emerald': '#388068', 'orange': '#a4673a',
  'pink': '#b05c80', 'indigo': '#6a70b8', 'teal': '#368080', 'red': '#b06260',
  'amber': '#8f7838', 'blue': '#5179b0',
};

/// Original normalizeHostPort; throws for anything the old parser rejected.
({String host, int port, bool ipv6}) _hostPort(String input) {
  final trimmed = input.trim();
  final match = trimmed.startsWith('[')
      ? RegExp(r'^\[([^\]]+)\]:(\d{1,5})$').firstMatch(trimmed)
      : RegExp(r'^(.+):(\d{1,5})$').firstMatch(trimmed);
  final host = match?.group(1)?.trim() ?? '';
  final port = int.tryParse(match?.group(2) ?? '') ?? 0;
  if (host.isEmpty || port < 1 || port > 65535) throw const FormatException('Invalid original host:port');
  return (host: host, port: port, ipv6: trimmed.startsWith('['));
}
String _normalizedHostPort(String input) {
  final parts = _hostPort(input);
  return parts.ipv6 ? '[${parts.host}]:${parts.port}' : '${parts.host}:${parts.port}';
}

/// Mirrors normalizeStoredHostProfile: connection ids are recomputed exactly as
/// the old app did (preferredConnectionId refers to them), invalid connections
/// are dropped, placeholder `local:` hosts are skipped, credentials are exact.
List<JsonMap> _hosts(List<dynamic> rows, List<String> notes) {
  final result = <JsonMap>[];
  final pendingRoutes = <JsonMap>[];
  for (final raw in rows) {
    final row = _object(raw, 'original host');
    final serverId = _required(row, 'serverId').trim();
    if (serverId.isEmpty) throw const FormatException('Missing original serverId');
    if (row['password'] != null && row['password'] is! String) throw const FormatException('Invalid original host password');
    final entries = row['connections'];
    if (entries is! List || entries.isEmpty) throw const FormatException('Original host has no connections');
    if (serverId.startsWith('local:')) {
      notes.add('Host $serverId: unresolved placeholder identity skipped, as the original app did.');
      continue;
    }
    final configs = <JsonMap>[];
    final routed = <JsonMap>[];
    final ordered = <String>[];
    // COMPAT(connectionPassword): first nonempty direct password, as the original.
    final legacyPassword = entries.map((e) => _object(e, 'original connection'))
        .where((e) => e['type'] == 'directTcp' && e['password'] is String && (e['password'] as String).isNotEmpty)
        .map((e) => e['password'] as String).firstOrNull;
    final password = row['password'] as String? ?? legacyPassword;
    final label = (row['label'] as String? ?? '').trim();
    final color = _identityColors[jsonMap(row['appearance'])['color']];
    for (final raw in entries) {
      final connection = _object(raw, 'original connection');
      final type = _required(connection, 'type');
      if (type == 'routed') {
        _required(connection, 'id');
        _required(connection, 'originServerId');
        routed.add(connection);
        ordered.add(connection['id'] as String);
        continue;
      }
      String id, endpoint;
      try {
        switch (type) {
          case 'directTcp':
            var address = _normalizedHostPort(_required(connection, 'endpoint'));
            final parts = _hostPort(address);
            if (parts.host == '127.0.0.1' || !parts.ipv6 && parts.host == '0.0.0.0' ||
                parts.ipv6 && (parts.host == '::1' || parts.host == '::')) {
              address = 'localhost:${parts.port}';
            }
            id = 'direct:$address';
            endpoint = '${connection['useTls'] == true ? 'wss' : 'ws'}://$address/ws';
          case 'directSocket':
            final path = _required(connection, 'path').trim();
            if (path.isEmpty) throw const FormatException('Empty socket path');
            id = 'socket:$path';
            endpoint = 'unix://$path';
          case 'directPipe':
            final path = _required(connection, 'path').trim();
            if (path.isEmpty) throw const FormatException('Empty pipe path');
            id = 'pipe:$path';
            endpoint = 'pipe://$path';
          case 'remoteSsh':
            final host = _required(connection, 'host').trim();
            final sshPort = connection['sshPort'] as int?;
            // DEFAULT_SSH_DAEMON_PORT (6767) is implicit in the original id.
            final daemonPort = connection['daemonPort'] == 6767 ? null : connection['daemonPort'] as int?;
            id = 'ssh:${Uri.encodeComponent(host)}:${sshPort ?? ''}:${daemonPort ?? ''}';
            endpoint = Uri.parse('ssh://$host').replace(port: sshPort, queryParameters: {
              if (daemonPort != null) 'daemonPort': '$daemonPort',
            }).toString();
          case 'relay':
            final address = _normalizedHostPort(_required(connection, 'relayEndpoint'));
            final key = _required(connection, 'daemonPublicKeyB64').trim();
            final explicitTls = connection['useTls'] as bool?;
            id = explicitTls == true ? 'relay:wss:$address' : 'relay:$address';
            // COMPAT(oldRelayOfferTls): unspecified TLS follows the hosted-relay port rule.
            final useTls = explicitTls ?? _hostPort(address).port == 443;
            endpoint = Uri.parse('relay://$address/${Uri.encodeComponent(serverId)}').replace(queryParameters: {
              'key': key,
              'ssl': '$useTls',
              if (connection['minimumSecurityMode'] == 'protected') 'security': 'protected',
            }).toString();
          default:
            throw FormatException('Unsupported original host connection type: $type');
        }
      } on FormatException catch (error) {
        if (error.message.startsWith('Unsupported original host')) rethrow;
        // normalizeStoredConnection returned null: the old app dropped it too.
        notes.add('Host $serverId: invalid $type connection dropped, as the original app did.');
        continue;
      } on TypeError {
        notes.add('Host $serverId: invalid $type connection dropped, as the original app did.');
        continue;
      }
      final config = HostConfig(
        id: id,
        label: label.isEmpty ? serverId : label,
        endpoint: endpoint,
        password: password,
        expectedServerId: serverId,
        color: color,
      );
      try {
        config.resolve();
      } on FormatException {
        // e.g. a noncanonical relay key the native transport cannot use.
        notes.add('Host $serverId: $type connection unsupported natively; retained in original registry.');
        continue;
      }
      if (configs.any((c) => c['id'] == id)) continue;
      configs.add(config.toJson());
      ordered.add(id);
    }
    if (configs.isEmpty && routed.isEmpty) {
      notes.add('Host $serverId: no usable connection; retained in original registry.');
      continue;
    }
    // Original: preferred id if it names a surviving connection, else the first.
    final preferredId = ordered.contains(row['preferredConnectionId'])
        ? row['preferredConnectionId'] as String : ordered.first;
    final preferredRoute = routed.where((c) => c['id'] == preferredId).firstOrNull;
    if (configs.isEmpty || preferredRoute != null) {
      pendingRoutes.add({...row, 'serverId': serverId, 'label': label.isEmpty ? serverId : label,
        'password': password, 'color': color, 'route': preferredRoute ?? routed.first,
        'direct': configs});
      continue;
    }
    if (routed.isNotEmpty) {
      notes.add('Host $serverId: routed fallbacks retained in original registry.');
    }
    final preferred = configs.firstWhere((c) => c['id'] == preferredId);
    result.add({'serverId': serverId, 'config': preferred, 'connections': configs});
  }
  while (pendingRoutes.isNotEmpty) {
    final count = pendingRoutes.length;
    for (final row in List<JsonMap>.of(pendingRoutes)) {
      final route = jsonMap(row['route']);
      final originId = route['originServerId'] as String;
      final origin = result.where((h) => h['serverId'] == originId).firstOrNull;
      if (origin == null) continue;
      final serverId = row['serverId'] as String;
      final config = <String, dynamic>{
        ...jsonMap(origin['config']),
        'id': route['id'],
        'label': row['label'],
        'expectedServerId': serverId,
      }..remove('password')..remove('color')..remove('localCredentialPath');
      if (row['password'] != null) config['password'] = row['password'];
      if (row['color'] != null) config['color'] = row['color'];
      result.add({'serverId': serverId, 'originServerId': originId,
        'config': config, 'connections': [config, ...jsonList(row['direct'])]});
      pendingRoutes.remove(row);
    }
    if (pendingRoutes.length == count) {
      for (final row in pendingRoutes) {
        notes.add('Host ${row['serverId']}: routed origin unavailable; retained in original registry.');
      }
      break;
    }
  }
  return result;
}

JsonMap _layouts(JsonMap state, List<JsonMap> hosts, JsonMap panel, List<String> notes) {
  final result = <String, dynamic>{};
  if (state.containsKey('layoutByWorkspace') && state['layoutByWorkspace'] is! Map) {
    throw const FormatException('Invalid original workspace layouts');
  }
  final byWorkspace = jsonMap(state['layoutByWorkspace']);
  for (final entry in byWorkspace.entries) {
    final server = hosts.map((h) => h['serverId'] as String)
        .where((id) => entry.key.startsWith('$id:')).toList()..sort((a, b) => b.length.compareTo(a.length));
    if (server.isEmpty) { notes.add('Workspace ${entry.key}: host identity unavailable; original layout retained.'); continue; }
    final serverId = server.first;
    final workspaceId = entry.key.substring(serverId.length + 1);
    final layout = _object(entry.value, 'original workspace layout');
    final targets = <PanelTarget>[];
    final keyMap = <String, String>{};
    final explorerKeys = <String>[];
    String? explorerSelected;
    // Original resolveExplorerSidebarPaneId: the registered pane when it is still in the
    // tree, else the default EXPLORER_SIDEBAR_PANE_ID ("explorer") pane. It is on screen
    // while that pane is not hidden (selectIsExplorerSidebarVisible).
    final panes = <String, JsonMap>{};
    void collect(dynamic value) {
      final row = jsonMap(value);
      if (row['kind'] == 'pane') {
        final pane = jsonMap(row['pane']);
        if (pane['id'] is String) panes[pane['id'] as String] = pane;
      } else if (row['kind'] == 'group') {
        jsonList(jsonMap(row['group'])['children']).forEach(collect);
      }
    }
    collect(layout['root']);
    final registeredId = jsonMap(state['explorerSidebarPaneIdByWorkspace'])[entry.key];
    final explorerId = panes.containsKey(registeredId)
        ? registeredId as String
        : panes.containsKey('explorer') ? 'explorer' : null;
    final explorerVisible = explorerId != null && panes[explorerId]!['hidden'] != true;
    JsonMap? node(JsonMap value) {
      if (value['kind'] == 'pane') {
        final pane = _object(value['pane'], 'original pane');
        final paneId = _required(pane, 'id');
        final tabRows = pane['tabs'] is List ? jsonList(pane['tabs']) : [
          for (final id in jsonList(pane['tabIds'])) {'tabId': id, 'target': _oldTarget(id.toString())},
        ];
        final keys = <String>[];
        for (final raw in tabRows) {
          final tab = _object(raw, 'original tab');
          final target = _object(tab['target'], 'original tab target');
          final kind = _required(target, 'kind');
          if (kind == 'browser' || kind == 'plugin' || kind == 'terminal') {
            notes.add('Workspace ${entry.key}: $kind tab ${tab['tabId']} retained only (excluded interactive surface).');
            continue;
          }
          if (!{'agent', 'draft', 'new_tab', 'file', 'files', 'changes_tree', 'working_diff', 'commit_diff', 'pull_request', 'setup', 'provider_subagent'}.contains(kind)) {
            throw FormatException('Unsupported original tab kind: $kind');
          }
          final id = switch (kind) {
            'agent' => target['agentId'], 'draft' => target['draftId'], 'file' => target['path'],
            'commit_diff' => target['sha'], 'setup' => target['workspaceId'],
            'provider_subagent' => target['subagentId'], _ => null,
          } as String?;
          final converted = PanelTarget(kind: kind, serverId: serverId, workspaceId: workspaceId,
            cwd: jsonMap(target['setup']).text('cwd'), id: id,
            data: {...target, 'legacyImported': true,
              if (target['setup'] is Map) 'config': target['setup'],
              if (tab['state'] != null) 'legacyTabState': tab['state']});
          if (!targets.any((item) => item.key == converted.key)) targets.add(converted);
          keyMap[_required(tab, 'tabId')] = converted.key;
          if (!keys.contains(converted.key)) keys.add(converted.key);
        }
        final selected = keyMap[pane['focusedTabId']] ?? keys.firstOrNull;
        if (paneId == explorerId) {
          explorerKeys.addAll(keys);
          explorerSelected = selected;
          return null;
        }
        return {'kind': 'pane', 'id': paneId, 'tabs': keys, 'selected': selected};
      }
      if (value['kind'] != 'group') throw const FormatException('Invalid original split node');
      final group = _object(value['group'], 'original split group');
      final children = jsonList(group['children']);
      final nodes = <JsonMap>[];
      final weights = <double>[];
      final sizes = jsonList(jsonMap(jsonMap(state['splitSizesByWorkspace'])[entry.key])[group['id']] ?? group['sizes']);
      for (var i = 0; i < children.length; i++) {
        final child = node(_object(children[i], 'original split child'));
        if (child == null) continue;
        nodes.add(child);
        weights.add(i < sizes.length && sizes[i] is num && (sizes[i] as num) > 0 ? (sizes[i] as num).toDouble() : 1);
      }
      if (nodes.isEmpty) return null;
      JsonMap folded = nodes.last;
      var weight = weights.last;
      for (var i = nodes.length - 2; i >= 0; i--) {
        weight += weights[i];
        folded = {'kind': 'split', 'id': '${group['id']}:$i', 'axis': group['direction'],
          'ratio': weights[i] / weight, 'first': nodes[i], 'second': folded};
      }
      return folded;
    }
    final tree = node(_object(layout['root'], 'original layout root')) ??
        {'kind': 'pane', 'id': 'main', 'tabs': <String>[]};
    result[entry.key] = {
      'tabs': targets.map((target) => target.toJson()).toList(), 'tree': tree,
      // Original keepWorkspaceFocusOutOfExplorerSidebar: focus never rests on the explorer.
      'focusedPane': layout['focusedPaneId'] == explorerId ? null : layout['focusedPaneId'],
      'explorerTabs': explorerKeys,
      'selectedExplorer': explorerSelected, 'explorerVisible': explorerVisible,
      'explorerWidth': jsonMap(state['explorerSidebarWidthByWorkspace'])[entry.key] ?? panel['explorerWidth'] ?? 340,
      'sidebarVisible': jsonMap(panel['desktop'])['agentListOpen'] != false,
    };
  }
  return result;
}

JsonMap _oldTarget(String id) {
  for (final entry in {'agent_': ('agent', 'agentId'), 'terminal_': ('terminal', 'terminalId'),
    'browser_': ('browser', 'browserId'), 'file_': ('file', 'path'),
    'commit_diff_': ('commit_diff', 'sha'), 'setup_': ('setup', 'workspaceId')}.entries) {
    if (id.startsWith(entry.key)) return {'kind': entry.value.$1, entry.value.$2: id.substring(entry.key.length)};
  }
  if (id.startsWith('draft_')) return {'kind': 'draft', 'draftId': id};
  return {'kind': id};
}

/// Original stores this import interprets. Anything else stays verbatim.
const _jsonStores = {
  '@paseo:app-settings', '@paseo:settings', '@paseo:settings-migrations',
  '@paseo:daemon-registry', '@paseo:keyboard-shortcut-overrides',
  '@paseo:changes-preferences', '@paseo:create-agent-preferences',
  'paseo:last-workspace-route-selection', 'panel-state', 'workspace-layout-state',
  'sidebar-view', 'sidebar-group-mode', 'sidebar-project-workspace-order',
  'sidebar-collapsed-sections', 'paseo-drafts',
};
