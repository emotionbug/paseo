/// Original `file-panels/resolve.ts` + `catalog.ts`: which panel opens a file.
///
/// This client has only Paseo's built-in panels (executable plugin panels are
/// not supported), so every file has at most one candidate and Paseo never
/// asks; an association (`ext:<extension>` / `name:<filename>` → panel id)
/// applies only when it names one of the file's candidates.
library;

/// One file panel: its id, the files it claims (`null` claims every file of
/// its [fileKind]), and the only kind of file it reads.
class FilePanelCandidate {
  const FilePanelCandidate({
    required this.id,
    required this.fileKind,
    this.extensions,
    this.filenames = const [],
  });
  final String id;
  final String fileKind;
  final List<String>? extensions;
  final List<String> filenames;
  bool get claimsAll => extensions == null;
}

const markdownFilePanel = FilePanelCandidate(
  id: 'builtin:markdown',
  fileKind: 'text',
  extensions: ['md', 'markdown'],
);
const htmlFilePanel = FilePanelCandidate(
  id: 'builtin:html',
  fileKind: 'text',
  extensions: ['html', 'htm'],
);
const imageFilePanel = FilePanelCandidate(
  id: 'builtin:image',
  fileKind: 'image',
  extensions: ['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg'],
);

/// Paseo's editor, the floor for text files no other built-in claims.
const sourceFilePanel = FilePanelCandidate(id: 'builtin:source', fileKind: 'text');

/// Original `filePanelCatalog`, in resolver order: the built-ins that render a
/// file, then Source.
const builtinFilePanels = [markdownFilePanel, htmlFilePanel, imageFilePanel, sourceFilePanel];

class FilePanelResolution {
  const FilePanelResolution({
    required this.panel,
    required this.candidates,
    required this.selectorKey,
    required this.associatedPanelId,
  });
  final FilePanelCandidate? panel;
  final List<FilePanelCandidate> candidates;
  final String selectorKey;
  final String? associatedPanelId;
}

String _basename(String path) => path.trim().split(RegExp(r'[\\/]')).last;

/// Original `filePanelExtension`: lowercase, without the dot.
String filePanelExtension(String path) {
  final name = _basename(path);
  final dot = name.lastIndexOf('.');
  return dot < 0 ? '' : name.substring(dot + 1).toLowerCase();
}

bool _matches(String name, String extension, FilePanelCandidate candidate) {
  if (candidate.claimsAll) return true;
  if (candidate.filenames.contains(name)) return true;
  return candidate.extensions!.any((entry) => entry.toLowerCase() == extension);
}

/// Original `resolveFilePanel` over [candidates] (the built-in catalog).
FilePanelResolution resolveFilePanel({
  required String path,
  required String kind,
  Map<String, dynamic> associations = const {},
  String? choice,
  List<FilePanelCandidate> candidates = builtinFilePanels,
}) {
  final name = _basename(path);
  final extension = filePanelExtension(path);
  final matching = candidates
      .where((candidate) => candidate.fileKind == kind && _matches(name, extension, candidate))
      .toList();
  final claimedByBuiltin = matching.any((candidate) => !candidate.claimsAll);
  final offered = claimedByBuiltin
      ? matching.where((candidate) => !candidate.claimsAll).toList()
      : matching;
  final matchedFilename = offered.any(
    (candidate) => !candidate.claimsAll && candidate.filenames.contains(name),
  );
  final selectorKey = matchedFilename ? 'name:$name' : 'ext:$extension';
  final associated = offered.where((candidate) => candidate.id == associations[selectorKey]).firstOrNull;
  final chosen = offered.where((candidate) => candidate.id == choice).firstOrNull;
  return FilePanelResolution(
    panel: chosen ?? associated ?? offered.firstOrNull,
    candidates: offered,
    selectorKey: selectorKey,
    associatedPanelId: associated?.id,
  );
}
