# Native menus

Menus use Flutter/native presentation. Reuse current widgets and actions under `packages/flutter_app/lib/ui` and the owning feature; there is no React menu engine or DOM portal in the product client.

A trigger owns the entity and host it acts on. Opening a context menu on one session must not use whichever session later becomes globally selected. Keep right-click/long-press actions aligned with visible menu alternatives and keyboard access.

Distinguish selected value, keyboard/pointer focus, disabled state and pending operation. A pending asynchronous mutation must not be shown as successful merely because the menu closed. Destructive actions require explicit confirmation and must preserve the original target through that confirmation.

Choose presentation based on available space and input, not a blanket desktop/mobile OS split. Compact menus and sheets must respect keyboard and safe areas. Restore focus when closing, and avoid opening a second OS/native presenter while the first transition still owns focus. Do not transplant fixed delays from the removed React Native implementation without evidence of a native issue.

Submenus are useful when a current value matters more than the full option list. Avoid hiding essential primary actions several levels deep. Labels use sentence case and remain localized; long host/path/model labels must remain identifiable.

Test actual pointer, touch, focus, cancellation, failure and resize behavior. Desktop application/tray menus are separate native integration with their own platform verification requirement. See [testing](testing.md).
