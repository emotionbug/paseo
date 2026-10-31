# Explorer and supporting panes

The Flutter shell presents file browsing, source/Markdown views and Changes alongside session work. Ownership is `packages/flutter_app/lib/ui/shell.dart`, `core/app_store.dart`, `features/files` and `features/git`. It is not the former React panel registry or a browser split-pane renderer.

## Identity and retention

Every target carries its host identity. File and Git facts are directory-backed: equal cwd strings on different hosts remain distinct. Workspace-owned drafts/layout retain workspace identity rather than deriving it from a path. A view must not change the daemon's project membership merely because it is moved or closed.

Preserve unsaved editor text when changing width, theme, tabs or supporting-view presentation. Close guards belong to the editing session and must run before discarding a draft. Closing a chat tab is not archive; closing a script log view is not stopping a script.

## Explorer sidebar

The main navigation tree is host → directory → session. The file Explorer is a supporting surface, not a replacement workspace grouping tree. Add Project uses `remote_directory_browser.dart` to list/select a directory on the chosen host; an OS picker on the client cannot select a remote checkout.

Files are read/written through the daemon. Markdown and source editing share the same remote identity and revision. The Changes view loads summary/file-tree information first and patch bodies on demand, so a large repository does not require every diff body before navigation is usable.

## Compact views and sheets

On compact screens Back returns from the file explorer to the session, then to the sidebar. Modal tasks and menus have their own focus/dismissal lifetime. Keep host/session selection and drafts independent of transient sheet/pane geometry. A width change must not discard unsaved work or start a second subscription for the same owner.

## Side panes

Wide layouts can show supporting content beside the session. Shared target selection and data ownership remain the same as compact layouts. Persisted layout is native client state under the platform application-support directory; browser and executable React plugin targets are not restored as live surfaces.

There is no embedded browser or interactive terminal emulator. Script/setup PTYs are retained on the daemon with read-only native logs. Native editor runtime integration and platform window/tray behavior each need actual execution evidence; see [testing](testing.md).
