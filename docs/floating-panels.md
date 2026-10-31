# Floating panels

Anchored menus, suggestions, tooltips and dialogs are native Flutter surfaces. Use the existing feature widgets and `packages/flutter_app/lib/ui/common.dart`; the former React portals, Reanimated shared values and bottom-sheet packages are not client dependencies.

## Coordinates and ownership

Measure an anchor in the coordinate system of the overlay that owns it. Respect safe areas, viewport bounds, text direction and keyboard insets. A viewport or theme change must update geometry without changing the selected host/session or resetting a draft.

The trigger and overlay share one operation lifetime. Cancellation, row removal or navigation invalidates delayed measurements and callbacks. Never reopen a dismissed surface from a late asynchronous result. Carry the explicit host/entity target into actions rather than resolving a different global selection at callback time.

## Sizing and interaction

Keep content readable, bounded and scrollable. A searchable picker needs room for both its query and results; opening the keyboard must not cover the selected result or confirmation. Avoid first-frame flashes at an unmeasured position. Do not use offscreen placement as a substitute for an explicit opening/loading state.

Pointer travel between anchor and content must not dismiss the surface prematurely. Touch users need a deliberate trigger and dismissal path; keyboard users need predictable focus and Escape/Back behavior. Nested overlays should dismiss in order and restore focus without submitting the underlying composer.

## Verification

Test opening, closing, cancellation, rapid repeated triggers, host changes, resize/rotation, keyboard motion, safe-area insets and long labels on the actual native platform. A Linux screenshot does not prove Android/iOS native presenter behavior. See [mobile testing](mobile-testing.md), [menus](menus.md), and [testing](testing.md).
