import 'dart:async';

import '../../../core/json.dart';

/// git/forge-refs.ts ForgeRef: an issue or change request number referenced
/// by a pasted forge URL of the current repository.
typedef ForgeRef = ({String kind, int number});

String forgeRefKey(ForgeRef ref) => '${ref.kind}:${ref.number}';

/// protocol git-remote.ts normalizeHost.
String normalizeGitHost(String host) =>
    host.trim().replaceFirst(RegExp(r'\.+$'), '').toLowerCase();

String? _normalizeRemotePath(String path) {
  var normalized = path.trim().replaceAll(RegExp(r'^/+|/+$'), '');
  if (normalized.endsWith('.git')) normalized = normalized.substring(0, normalized.length - 4);
  return normalized.isEmpty ? null : normalized;
}

bool _validRemoteHost(String host) =>
    RegExp(r'^[a-z0-9](?:[a-z0-9._-]*[a-z0-9])?$').hasMatch(host);

/// protocol git-remote.ts parseGitRemoteLocation: the host and repository
/// path of an scp-style, ssh, http or https remote.
({String host, String path})? parseGitRemoteLocation(String remoteUrl) {
  final trimmed = remoteUrl.trim();
  if (trimmed.isEmpty) return null;
  final scp = trimmed.contains('://')
      ? null
      : RegExp(r'^[^@]+@([^:]+):(.+)$').firstMatch(trimmed);
  if (scp != null) {
    final host = normalizeGitHost(scp.group(1)!);
    final path = _normalizeRemotePath(scp.group(2)!);
    if (!_validRemoteHost(host) || path == null) return null;
    return (host: host, path: path);
  }
  final uri = Uri.tryParse(trimmed);
  if (uri == null || !const ['https', 'http', 'ssh'].contains(uri.scheme.toLowerCase())) {
    return null;
  }
  final host = normalizeGitHost(uri.host);
  final String decoded;
  try {
    decoded = Uri.decodeComponent(uri.path);
  } catch (_) {
    return null;
  }
  final path = _normalizeRemotePath(decoded);
  if (!_validRemoteHost(host) || path == null) return null;
  return (host: host, path: path);
}

/// protocol forge-manifest.ts FORGE_DEFINITIONS cloudHosts.
const forgeCloudHosts = {
  'github': ['github.com', 'ssh.github.com'],
  'gitlab': ['gitlab.com'],
  'gitea': ['gitea.com'],
  'codeberg': ['codeberg.org'],
};

/// git/forges urlGrammar.referencePaths, in CLIENT_FORGE_LOGIC_MODULES order.
const _forgeReferencePaths = [
  (forge: 'github', kind: 'change_request', infix: '/pull/'),
  (forge: 'github', kind: 'issue', infix: '/issues/'),
  (forge: 'gitlab', kind: 'change_request', infix: '/-/merge_requests/'),
  (forge: 'gitlab', kind: 'issue', infix: '/-/issues/'),
];

final _webUrlPattern = RegExp(r'''https?://[^\s<>"'`)\]]+''', caseSensitive: false);

/// git/forge-refs.ts extractForgeRefs: the issue and change-request URLs in
/// [text] that belong to [remoteUrl]'s repository, deduplicated, in order.
/// A known cloud remote matches its forge's routes on any of its cloud hosts;
/// an unrecognized (self-hosted) host tries every registered route.
List<ForgeRef> extractForgeRefs(String? text, String? remoteUrl) {
  final remote = remoteUrl == null ? null : parseGitRemoteLocation(remoteUrl);
  final body = text?.trim() ?? '';
  if (remote == null || body.isEmpty) return const [];
  final webHosts = {remote.host};
  final forgeIds = <String>{};
  for (final entry in forgeCloudHosts.entries) {
    final hosts = entry.value.map(normalizeGitHost).toList();
    if (hosts.contains(remote.host)) {
      forgeIds.add(entry.key);
      webHosts.addAll(hosts);
    }
  }
  final refs = <ForgeRef>[];
  final seen = <String>{};
  for (final match in _webUrlPattern.allMatches(body)) {
    final raw = match.group(0)!.replaceFirst(RegExp(r'[.,;:!]+$'), '');
    final uri = Uri.tryParse(raw);
    if (uri == null || (uri.scheme != 'http' && uri.scheme != 'https')) continue;
    if (!webHosts.contains(normalizeGitHost(uri.host))) continue;
    final String pathname;
    try {
      pathname = Uri.decodeComponent(uri.path);
    } catch (_) {
      continue;
    }
    for (final route in _forgeReferencePaths) {
      if (forgeIds.isNotEmpty && !forgeIds.contains(route.forge)) continue;
      final prefix = '/${remote.path}${route.infix}';
      if (!pathname.toLowerCase().startsWith(prefix.toLowerCase())) continue;
      final digits = RegExp(r'^(\d+)(?:/|$)').firstMatch(pathname.substring(prefix.length));
      final number = digits == null ? null : int.tryParse(digits.group(1)!);
      if (number == null || number <= 0) continue;
      final ref = (kind: route.kind, number: number);
      if (seen.add(forgeRefKey(ref))) refs.add(ref);
      break;
    }
  }
  return refs;
}

/// forge-auto-attach.ts attachmentKey: the `kind:number` of a forge attachment
/// (wire or legacy kinds), null for any other attachment.
String? forgeAttachmentKey(JsonMap attachment) {
  final type = attachment['type'] ?? attachment['kind'];
  final change = type == 'forge_change_request' || type == 'github_pr';
  final issue = type == 'forge_issue' || type == 'github_issue';
  if (!change && !issue) return null;
  final number = attachment['number'] ?? jsonMap(attachment['item'])['number'];
  return '${change ? 'change_request' : 'issue'}:$number';
}

/// The composer state the auto-attach follows.
typedef ForgeAutoAttachInput = ({
  String text,
  String? remoteUrl,
  List<JsonMap> attachments,
  bool connected,
  String serverId,
  String cwd,
});

/// composer/forge-auto-attach.ts useComposerForgeAutoAttach: issue and
/// change-request URLs of the current repository typed or pasted into the
/// composer resolve (300ms after they appear) through forge search and attach
/// in source order. A removed attachment is never re-attached for that
/// reference; references carried across a server/cwd change are suppressed;
/// lookups whose reference left the text, or whose target changed, are
/// dropped. [isResolving] holds while any lookup is pending.
class ComposerForgeAutoAttach {
  ComposerForgeAutoAttach({
    required this.search,
    required this.attach,
    this.onChangeRequestDetected,
    this.onChangeRequestAdded,
    this.onResolvingChanged,
    this.debounce = const Duration(milliseconds: 300),
  });

  /// Forge search in a cwd for a query; items carry `kind` and `number`.
  final Future<List<JsonMap>> Function(String cwd, String query) search;

  /// Attach a resolved search item (the reference is still wanted).
  final void Function(JsonMap item) attach;
  final void Function()? onChangeRequestDetected;
  final void Function(JsonMap item)? onChangeRequestAdded;
  final void Function()? onResolvingChanged;
  final Duration debounce;

  ForgeAutoAttachInput? _latest;
  final Set<String> _removed = {};
  Set<String> _presentChangeRequests = {};
  final Set<_ForgeLookup> _lookups = {};
  final Map<String, int> _resolving = {};
  ({String serverId, String cwd})? _previousTarget;
  String? _relevanceDeps, _candidateDeps;
  void Function()? _cleanup;
  bool _disposed = false;

  bool get isResolving => _resolving.isNotEmpty;

  /// markForgeAttachmentRemoved.
  void markRemoved(JsonMap? attachment) {
    final key = attachment == null ? null : forgeAttachmentKey(attachment);
    if (key != null) _removed.add(key);
  }

  void update(ForgeAutoAttachInput input) {
    if (_disposed) return;
    _latest = input;
    _notifyNewChangeRequests(input);
    final target = '${input.remoteUrl}|${input.connected}|${input.serverId}|${input.cwd}';
    final relevance = '${_relevanceKey(input)}|$target';
    if (relevance != _relevanceDeps) {
      _relevanceDeps = relevance;
      for (final lookup in List.of(_lookups)) {
        lookup.invalidateIrrelevant(input);
      }
    }
    final candidates = '${_candidateRefs(input).map(forgeRefKey).join('|')}|$target';
    if (candidates == _candidateDeps) return;
    _candidateDeps = candidates;
    _cleanup?.call();
    _cleanup = _run();
  }

  void dispose() {
    _disposed = true;
    _cleanup?.call();
    _cleanup = null;
  }

  void _notifyNewChangeRequests(ForgeAutoAttachInput input) {
    final current = {
      for (final ref in extractForgeRefs(input.text, input.remoteUrl))
        if (ref.kind == 'change_request') forgeRefKey(ref),
    };
    for (final key in current) {
      if (!_presentChangeRequests.contains(key)) onChangeRequestDetected?.call();
    }
    _presentChangeRequests = current;
  }

  String _relevanceKey(ForgeAutoAttachInput input) => (extractForgeRefs(input.text, input.remoteUrl)
        .map(forgeRefKey)
        .where((key) => !_removed.contains(key))
        .toList()
      ..sort())
      .join('|');

  List<ForgeRef> _candidateRefs(ForgeAutoAttachInput input) => [
    for (final ref in extractForgeRefs(input.text, input.remoteUrl))
      if (!_removed.contains(forgeRefKey(ref)) && !_attached(input.attachments, ref)) ref,
  ];

  static bool _attached(List<JsonMap> attachments, ForgeRef ref) =>
      attachments.any((attachment) => forgeAttachmentKey(attachment) == forgeRefKey(ref));

  static bool _present(ForgeRef ref, ForgeAutoAttachInput input) =>
      extractForgeRefs(input.text, input.remoteUrl)
          .any((candidate) => forgeRefKey(candidate) == forgeRefKey(ref));

  static bool _sameTarget(ForgeAutoAttachInput a, ForgeAutoAttachInput b) =>
      a.serverId == b.serverId && a.cwd == b.cwd && a.remoteUrl == b.remoteUrl;

  bool _relevant(ForgeRef ref, ForgeAutoAttachInput initial, ForgeAutoAttachInput current) =>
      current.connected &&
      !_removed.contains(forgeRefKey(ref)) &&
      _sameTarget(initial, current) &&
      _present(ref, current);

  void _changeResolving(Iterable<String> keys, int delta) {
    final before = _resolving.isNotEmpty;
    for (final key in keys) {
      final count = (_resolving[key] ?? 0) + delta;
      if (count <= 0) {
        _resolving.remove(key);
      } else {
        _resolving[key] = count;
      }
    }
    if (before != _resolving.isNotEmpty && !_disposed) onResolvingChanged?.call();
  }

  /// One run of the lookup effect; returns its cleanup.
  void Function()? _run() {
    final initial = _latest!;
    final previous = _previousTarget;
    _previousTarget = (serverId: initial.serverId, cwd: initial.cwd);
    // suppressRefsCarriedAcrossTargets: references already in the text when
    // the composer moves to another server or directory are not resolved there.
    if (previous != null &&
        previous.cwd.trim().isNotEmpty &&
        initial.cwd.trim().isNotEmpty &&
        (previous.serverId != initial.serverId || previous.cwd != initial.cwd)) {
      for (final ref in extractForgeRefs(initial.text, initial.remoteUrl)) {
        _removed.add(forgeRefKey(ref));
      }
    }
    if (!initial.connected || initial.cwd.trim().isEmpty) return null;
    final refs = [
      for (final ref in extractForgeRefs(initial.text, initial.remoteUrl))
        if (!_removed.contains(forgeRefKey(ref)) &&
            !_lookups.any((lookup) => lookup.hasPending(forgeRefKey(ref))) &&
            !_attached(initial.attachments, ref))
          ref,
    ];
    if (refs.isEmpty) return null;
    final keys = refs.map(forgeRefKey).toList();
    _changeResolving(keys, 1);
    final unreleased = keys.toSet();
    void release(Iterable<String> released) {
      final now = released.where(unreleased.remove).toList();
      if (now.isNotEmpty) _changeResolving(now, -1);
    }

    _ForgeLookup? lookup;
    final timer = Timer(debounce, () {
      if (_disposed) return;
      lookup = _ForgeLookup(this, refs, initial, onSettled: (key) => release([key]));
      _lookups.add(lookup!);
      lookup!.start();
    });
    return () {
      timer.cancel();
      final started = lookup;
      if (started == null) {
        release(List.of(unreleased));
        return;
      }
      final current = _latest!;
      final currentKeys = _candidateRefs(current).map(forgeRefKey).toList();
      final currentSet = currentKeys.toSet();
      final retained = keys.where(currentSet.contains).toList();
      if (_disposed || currentKeys.join('|') != retained.join('|')) {
        started.invalidate(keys);
      } else {
        started.invalidateIrrelevant(current);
      }
    };
  }

  Future<JsonMap?> _resolve(ForgeRef ref) async {
    final snapshot = _latest!;
    if (!snapshot.connected || !_present(ref, snapshot)) return null;
    List<JsonMap> items;
    try {
      items = await search(snapshot.cwd, '${ref.number}');
    } catch (_) {
      return null;
    }
    final item = items
        .where((candidate) => candidate['kind'] == ref.kind && candidate['number'] == ref.number)
        .firstOrNull;
    final current = _latest!;
    if (item == null ||
        _disposed ||
        _removed.contains(forgeRefKey(ref)) ||
        !_sameTarget(snapshot, current) ||
        !_present(ref, current) ||
        _attached(current.attachments, ref)) {
      return null;
    }
    return item;
  }
}

/// forge-auto-attach.ts attachRefs: resolved items attach strictly in the
/// order their references appear; an invalidated reference settles empty.
class _ForgeLookup {
  _ForgeLookup(this.owner, this.refs, this.initial, {required this.onSettled})
    : _settled = List.filled(refs.length, false),
      _items = List.filled(refs.length, null);

  final ComposerForgeAutoAttach owner;
  final List<ForgeRef> refs;
  final ForgeAutoAttachInput initial;
  final void Function(String key) onSettled;
  final List<bool> _settled;
  final List<JsonMap?> _items;
  int _next = 0;
  bool _complete = false;

  void start() {
    for (var index = 0; index < refs.length; index++) {
      final ref = refs[index];
      owner._resolve(ref).then((item) => _settle(index, item)).whenComplete(() {
        onSettled(forgeRefKey(ref));
      });
    }
  }

  bool hasPending(String key) {
    final index = refs.indexWhere((ref) => forgeRefKey(ref) == key);
    return index >= 0 && !_settled[index];
  }

  void _settle(int index, JsonMap? item) {
    if (_settled[index]) return;
    _settled[index] = true;
    _items[index] = item;
    _drain();
  }

  void _drain() {
    while (_next < refs.length && _settled[_next]) {
      final item = _items[_next];
      final ref = refs[_next];
      _next++;
      if (owner._disposed) continue;
      if (item != null &&
          !owner._removed.contains(forgeRefKey(ref)) &&
          !ComposerForgeAutoAttach._attached(owner._latest!.attachments, ref)) {
        owner.attach(item);
      }
      if (item?['kind'] == 'change_request') owner.onChangeRequestAdded?.call(item!);
    }
    if (!_complete && _next == refs.length) {
      _complete = true;
      owner._lookups.remove(this);
    }
  }

  void invalidate(Iterable<String> keys) {
    final invalid = keys.toSet();
    final newlySettled = <String>[];
    for (var index = 0; index < refs.length; index++) {
      final key = forgeRefKey(refs[index]);
      if (!invalid.contains(key) || index < _next) continue;
      if (!_settled[index]) newlySettled.add(key);
      _settled[index] = true;
      _items[index] = null;
    }
    newlySettled.forEach(onSettled);
    _drain();
  }

  void invalidateIrrelevant(ForgeAutoAttachInput current) {
    invalidate([
      for (var index = 0; index < refs.length; index++)
        if (!(owner._relevant(refs[index], initial, current) &&
            (_settled[index] ||
                !ComposerForgeAutoAttach._attached(current.attachments, refs[index]))))
          forgeRefKey(refs[index]),
    ]);
  }
}
