# File and project icons

File presentation belongs to the Flutter files feature under `packages/flutter_app/lib/features/files`; project navigation icon rendering is in `features/navigation/project_navigation_icon.dart`. Reuse the current native widgets instead of copying the retired React/SVG file-icon component table.

Project icon discovery is a host-side operation in `packages/daemon/src/files/icons.rs`. It searches bounded candidate locations, applies file-size/type/dimension checks and returns optional icon data. A missing or unreadable decoration is not a failed project; keep a generic native fallback and the text label.

An icon is decoration, not identity or authority. Host/path/session keys must not depend on it, and fetched project content must not execute scripts or create an embedded browser. Preserve accessible labels and enough contrast in both themes; do not let icon color compete with status/error indicators.

Desktop installer icons are maintained under `packages/flutter_desktop_host/packaging/assets`. Website mockup assets belong to the website, not the native runtime. Adding an asset requires the appropriate attribution and packaging ownership, not a reference into a removed application tree.

Verify long filenames, unknown extensions, unavailable project icons, theme changes and compact rows in the actual Flutter app. Native packaging and platform execution are separate checks; see [testing](testing.md).
