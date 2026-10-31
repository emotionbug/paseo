# Markdown rendering

Markdown is rendered by native Flutter widgets, not React Native, a DOM or an embedded browser. Chat uses selectable `MarkdownBody` in `packages/flutter_app/lib/features/chat/timeline_item.dart`; file preview uses selectable `Markdown` in `features/files/file_panel.dart`. Syntax presentation is owned by the native chat/file feature code.

Keep source text separate from its rendering. Streaming a message must not rewrite durable history or lose the association between a displayed block and its source message. Markdown file preview and source editing use the same host-side file identity, not a second local copy with independent save behavior.

Links must retain their context: relative file links resolve against the document/session location on the selected host, while external URLs open outside the application. Equal path strings on different hosts are not the same file. Do not introduce a webview to render links or execute embedded HTML/scripts.

Use native theme/text styles and preserve copy/selection, code formatting, lists, tables and long-content scrolling. Keep error states visible for failed file/image acquisition. Do not fabricate a successful preview for unsupported content.

## Verification

Exercise chat and file preview with streaming text, code, lists, tables, links, selection/copy and a large document. Verify edit/save/read-back and failure recovery separately from preview. Keyboard, pointer and touch selection have different native behavior and must be tested on the affected platform.

Actual Linux native Markdown rendering was exercised and its screenshot reviewed during the cutover. That is not proof of Android/iOS selection or every Markdown edge case. See [testing](testing.md) and [native text selection](native-text-selection.md).
