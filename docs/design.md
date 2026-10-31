# Native design

Paseo is minimal, spacious and quiet. The agent workflow is primary; supporting tools help users understand, direct and review work. The native client is Flutter, with theme ownership in `packages/flutter_app/lib/ui/theme.dart`, shared widgets in `ui/common.dart`, and feature-specific UI under `features/`.

## Reuse and hierarchy

Read the existing native widget before adding another primitive. Reuse semantic colors, typography, spacing and interaction states. Do not copy retired React Native components, CSS tokens or Unistyles APIs into a parallel styling system. Hierarchy should be legible through weight, contrast and grouping, not a different arbitrary font size for every row.

Use sentence case and concrete action names. A destructive consequence needs an explicit confirmation at the operation boundary; red styling alone is not consent. Errors should say what failed and offer a relevant retry without replacing the user's work.

## Navigation and identity

The sidebar is **host → directory → session**. Keep a visible expandable host row even for one host, preserve empty directories, and scope equal paths to their host. Search reveals matching ancestors temporarily. Do not reintroduce workspace grouping controls; workspaces remain available for orchestration and Command Center.

Host, directory and session rows have distinct actions. A directory's create action must use that exact host/path. Closing a session tab does not archive it. Add Project uses a remote folder picker, not a local OS path dialog for remote data.

## Responsive interaction

Design the compact case explicitly. The sidebar, session and supporting file view must remain reachable with Back and visible controls. Keyboard insets, safe areas, text scaling and rotation must not hide primary actions or change session identity. Wider layouts may show more context without creating a second state model.

Hover is a convenience, never the only access to an action. Provide pointer, keyboard and touch paths with clear focus and semantic labels. Align visible content while keeping sufficiently large hit targets. Long paths and provider/model labels must remain distinguishable without expanding every row uncontrollably.

## Content surfaces

Markdown and source editing are native. Diffs are summary-first and load bodies lazily; do not block opening the entire Changes view on every patch. Preserve drafts and selection through transient errors. Display script/setup output as read-only logs, with real lifecycle state and exit errors. The app does not contain an interactive terminal emulator or browser.

Loading, empty, unavailable, failed, pending and successful states are separate. A disconnected host must not look like an empty host. A failed save must not look like a completed save. A permission request must not disappear merely because a different pane is selected.

## Verification

Review actual native screenshots and interaction, not mockups or a browser replica. Check light/dark theme, compact/wide sizes, keyboard, long content, loading/error states and slow networks. Measure Flutter frames and daemon work separately. Linux screenshots and smoke evidence are recorded in [testing](testing.md); unexecuted platforms remain unverified.
