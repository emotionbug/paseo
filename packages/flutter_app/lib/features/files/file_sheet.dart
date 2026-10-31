import 'package:flutter/material.dart';

import '../../core/json.dart';
import '../../core/scope.dart';
import '../../core/target.dart';
import '../../ui/common.dart';
import '../../ui/theme.dart';
import 'file_icons.dart';
import 'file_panel.dart';
import 'remote_directory_browser.dart';

/// Reveals a `file` [target] the way the original workspace-layout-store
/// `openTab` does for an implicit open (workspace-tabs/open-beside.ts
/// openPreferredWorkspaceTarget): on compact layouts a file that is not
/// already open as a tab shows in the workspace sheet (panel-manifest.ts
/// `file.compactSheet: { openAsTab: false }`), so closing it — or Back —
/// returns to the pane underneath; otherwise it opens as a tab, beside the
/// current pane when the `openInSidePane[preference]` setting asks for it.
void openWorkspaceFile(BuildContext context, PanelTarget target, {required String preference}) {
  final store = PaseoScope.of(context);
  if (isCompactLayout(context) && store.targetFor(target.key) == null) {
    showWorkspaceFileSheet(context, target);
    return;
  }
  store.open(
    target,
    beside: !isCompactLayout(context) && jsonMap(store.preferences['openInSidePane']).flag(preference),
  );
}

/// screens/workspace/sheet-tabs.tsx SheetTabSheet for a file: a 92% bottom
/// sheet (45% scrim, pan-down to close) whose handle carries the header —
/// the file's icon, its name and close — over the file panel.
Future<void> showWorkspaceFileSheet(BuildContext context, PanelTarget target) {
  final colors = PaseoColors.of(context);
  final path = target.data.text('path', target.id ?? '');
  final name = target.title ?? remoteBasename(path, cwd: target.cwd);
  return showModalBottomSheet<void>(
    context: context,
    isScrollControlled: true,
    useSafeArea: true,
    enableDrag: true,
    backgroundColor: colors.surface0,
    barrierColor: const Color(0x73000000),
    shape: const RoundedRectangleBorder(
      borderRadius: BorderRadius.vertical(top: Radius.circular(16)),
    ),
    builder: (sheet) => SizedBox(
      height: MediaQuery.sizeOf(sheet).height * .92,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          // SheetHandle: the grabber above the header.
          Padding(
            padding: const EdgeInsets.only(top: 8),
            child: Center(
              child: Container(
                width: 36,
                height: 4,
                decoration: BoxDecoration(
                  color: colors.muted.withValues(alpha: .5),
                  borderRadius: BorderRadius.circular(2),
                ),
              ),
            ),
          ),
          Expanded(
            child: SheetContents(
              title: name,
              scroll: false,
              maxHeightFactor: .92,
              leading: MaterialFileIcon(fileName: name),
              child: Padding(
                padding: EdgeInsets.only(bottom: MediaQuery.paddingOf(sheet).bottom),
                child: FilePanel(target: target),
              ),
            ),
          ),
        ],
      ),
    ),
  );
}
