# Native text selection

The Flutter client uses selectable Markdown and native selectable text. Chat Markdown is in `packages/flutter_app/lib/features/chat/timeline_item.dart`; file Markdown/source presentation is in `features/files/file_panel.dart`. See [Markdown rendering](markdown-renderer.md).

Selection must remain usable while scrolling, changing pane width and opening/closing supporting views. A selection gesture must not submit a prompt, activate a link unintentionally, or switch sessions. Preserve source text exactly for copying, including code whitespace and literal file paths.

Use Flutter's selection/focus/scroll behavior at the owning widget boundary. The removed React Native inverted-list patches and native `paseo-scroll` module are not part of the current client and must not be reinstated as a fix for a Flutter selection bug.

Test pointer and touch selection, cross-block ranges, code copy, links, text scaling, keyboard visibility and resizing on the actual platform. A Linux Markdown screenshot does not establish Android/iOS selection behavior; record that evidence separately. Embedded browser/DOM selection and executable React renderers are outside the native product scope.
