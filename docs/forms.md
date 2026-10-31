# Native forms

Forms are Flutter features. Reuse the current settings, schedule, host and creation widgets under `packages/flutter_app/lib/features`, plus shared presentation under `lib/ui`. The former TypeScript form-model hooks and React form kit are not the client implementation.

## State and data

Keep user intent, derived values, loaded server data and operation status distinct. Late provider/model discovery must not overwrite a choice the user already made. Edit forms retain the identity of the entity being edited; a host switch must not silently redirect a pending save.

Creation and editing need explicit initial data and lifecycle. A disconnected or failed catalog is not an empty successful catalog. Preserve drafts on save failure and show an actionable error. Validate user input at the boundary while retaining daemon-side validation as the authority.

Provider options are opaque provider-owned data. A provider or model change must remove incompatible dependent selections rather than sending stale options. CLI-default profiles are host-owned, not a separate local form preference.

## Presentation

Use localized labels, accessible focus order and native controls. Compact forms must remain usable with the keyboard and text scaling. Loading, pending, disabled, error and success are different states; a dismissed dialog is not proof that a mutation succeeded.

Remote directory selection uses `features/files/remote_directory_browser.dart` against the selected host. Do not populate it with a local OS file dialog when the destination is remote.

## Verification

Test slow and failed loads, out-of-order results, validation errors, duplicate submission, cancellation, retry, changing host/provider/model, and saving/read-back through the real daemon. Widget tests cover local transitions; actual native and wire behavior require their own execution evidence. See [testing](testing.md).
