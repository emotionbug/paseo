# Native capture boundary

The product no longer has an embedded browser, browser automation, Electron or Chromium. The former browser guest capture harness is not a native-client test and has no current product command. This filename remains only so existing documentation links resolve; it does not describe a retained browser feature.

For client screenshots, launch the real Flutter application against an isolated Rust daemon and capture its native window. Record the actual platform, viewport, revision and state shown. A screenshot of the website, an old browser client, or a generated image does not prove the native app rendered that state.

Use [testing](testing.md) for native smoke boundaries and [mobile testing](mobile-testing.md) for device-specific interaction. Linux evidence includes reviewed native host-tree, Markdown, large-diff and compact-sidebar captures. No equivalent Windows/macOS/iOS execution is implied.

External links open outside Paseo. There is no browser-profile persistence, guest WebContents ownership, CDP screenshot path, embedded automation toolset or fallback webview to test in the native product. Website browser tests remain website tests and must not be reported as application coverage.
