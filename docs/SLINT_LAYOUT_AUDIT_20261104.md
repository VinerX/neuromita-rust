# Slint layout audit — 2026-11-04

Scope: first-party `ui/**/*.slint` only. Vendored Slint compiler tests are excluded.

All ordinary text fields, placeholders, page notifications, dialog content, cards, and editor controls are arranged with `HorizontalLayout` / `VerticalLayout`, padding and spacing rather than explicit `x` / `y` bindings. The Home page's main columns also use stretch weights, not formulas based on the window width.

Remaining uses of `x` / `y` are limited to specialized graphical positioning:

- `PopupWindow` anchors relative to the invoking control (menus and dropdowns).
- Animated switch thumbs, progress fill and the volume slider's track and handle. Their positions represent input values rather than layout of ordinary content.
- The custom settings scrollbar's track/handle. Its viewport width and thumb offset depend on scroll state; replacing it with a conditional layout introduced a Slint `scrollable -> layoutinfo -> height -> scroll-limit` binding loop, so it retains an explicitly positioned track.
- The Home page background image crop and its decorative edge. These are visual effects behind content, not positions of controls.

Regression criteria: no new `x` / `y` bindings for field labels, editable text, buttons, cards, ordinary navigation or text content. Use layouts for page structure and popup anchors for floating elements. Keep user input values and UI state in the presenter; layout changes must not introduce invented data.
