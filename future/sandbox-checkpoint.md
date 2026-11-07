# Sandbox: WIP checkpoint, 2026-11-03

> **Статус checkpoint:** этот документ зафиксировал прерванное состояние на исходном HEAD `9011649`. Продолжение переноса и проверенные результаты находятся в [sandbox-ui-port-20261103.md](sandbox-ui-port-20261103.md). Утверждения ниже о том, что `ui/pages/sandbox.slint` отсутствует и GUI не собирается, относятся только к исторической точке восстановления.

Branch: `codex/sandbox-presenter-20261103`, based on main `6c7e88ca`.

## Latest user instruction

Implement Sandbox with a real presenter and visual similarity to the Python application. The user rejected the initial screenshot-based layout and explicitly requested rebuilding **the entire Sandbox page from scratch using the Python Qt widgets and their styles**. Preserve the working backend/presenter connection; this is not a request to rebuild Core or the whole GUI. The user needs to shut down and requested pushing this checkpoint.

**This checkpoint is unfinished and is not a release. Do not merge it into main as completed work.**

## Saved implementation

- [x] Library `SandboxPresenter` in `src/presentation/sandbox.rs`, composed by ApplicationLifecycle inside GuiPresenter. It delegates reply/cancel to ChatPresenter, reads a fixed-prefix character projection and actual settings/preset, and exposes existing watches.
- [x] Desktop runtime in `src/bin/neuromita-gui/runtime.rs`: separate Tokio thread, bounded command channel, async startup/observation/shutdown, persistent application configuration and actual Gemini provider. No generated demo replies.
- [x] Slint adapter in `src/bin/neuromita-gui/sandbox.rs`: typed commands and `upgrade_in_event_loop`, selected-character history and requests, provisional text/cancellation, errors, settings volume. Input clears only after successful admission.
- [x] CLI options `--sandbox`, `--model MODEL`, `--data-dir PATH`. Model is explicit; key comes from `GEMINI_API_KEY`. With no model the GUI should allow configuring/starting Core from the session pane. No key is stored or logged.
- [x] ProjectedMessage now exposes optional typed RequestId. The GUI adapter suppresses a provisional row if the canonical assistant message for that request is already visible, including when the request watch lags behind commit.
- [x] Character history comes from MitaView, not a second authoritative UI history. Selecting another character does not cancel its requests. External client requests are discovered through ChatPresenter.
- [x] RU/EN localization declarations and English overrides started; the original rejected page declared many of these IDs. Reconcile unused/new declarations after rebuilding the page.
- [x] Original Font Awesome 6.7.2 solid font copied byte-for-byte from Python QtAwesome, with source/hash/license reference in `assets/fonts/README.md`. The copied font has family `Font Awesome 6 Free Solid`; no new Cargo dependencies or vendor edits.

## Current UI rewrite state — resume here

The first rough `ui/pages/sandbox.slint` was deliberately discarded after the user's correction. **The replacement entry page does not exist yet. `ui/app.slint` still imports it, so the GUI feature currently does not build.** Do not mistake the previously built executable or earlier screenshots for this final source state.

New source-driven modules have been started under `ui/sandbox/`:

- `theme.slint`: exact theme values, 96-DPI pt-to-px font conversions, original QtAwesome codepoints/font import.
- `models.slint`: ChatStatus and ChatRow wire types used by the existing adapter.
- `controls.slint`: fresh Caption/Glyph, QSS-derived buttons, inspector strips/rows, status chips and ToggleSwitch geometry.
- `message.slint`: fresh BubbleFrame/MessageRow including Python bubble-tail geometry using a dynamic SVG path. **These new components have not yet been compiled or visually checked.**

`ui/components/avatar.slint` plus its SVG corner mask perform the circle clipping corresponding to Python's `_round_pixmap`; source PNGs remain unchanged. Check the mask radius against Python's full-size ellipse.

Next work:

- [ ] Finish fresh conversation/composer and inspector components, then recreate `ui/pages/sandbox.slint` and its AppWindow bindings. Do not revive/refine the rejected rough page.
- [ ] Port all page layout/style structure: title actions, conversation strip, message widths/tails, multiline composer, session/state/debug tabs, collapse rail, status switches/gears, capture rows and quick actions. Unsupported actions should be visibly disabled rather than falsely functional.
- [ ] State tab should use actual state bars and memory counts, not fabricated values. Extend the presenter snapshot where necessary.
- [ ] Keep send/cancel/character selection/refresh/volume connected to the real presenter. Enter sends, Shift+Enter adds a newline. TextInput supports `key-pressed`; see the vendored Slint common TextEdit implementation.
- [ ] Reconcile localization declarations/English overrides and verify live switching with the rebuilt page.
- [ ] Add a UI row projection regression test for a stale Processing snapshot after canonical commit and for Cancelled hiding provisional text. Existing core test covers admission/cancel/history isolation, but final projection refinements need verification.
- [ ] Rebuild, render at 1560×918 and 1200×760, inspect both, and run all required tests, Clippy and fmt. Native window verification remains to be done.
- [ ] Update `future/gui.md`, README and HANDOFF from preview-only claims after completion.

## Authoritative Python sources (read only)

Root: `D:/NeuroMitaTest/NeuroMitaOrigin/09082025/NeuroMita`.

| Source | What to port |
|---|---|
| `src/styles/theme.py` | Color/opacity tokens |
| `src/styles/base.py` | Segoe UI Variable, 9pt base, base controls |
| `src/styles/pages/sandbox.py` | Object-name QSS for the complete page |
| `src/ui/pages/sandbox_page.py` | Title/header, inspector sections/tabs/rail and layouts |
| `src/ui/widgets/chat_panel.py` | Conversation strip and composer |
| `src/ui/chat/chat_widget.py` | Message area margins and spacing |
| `src/ui/chat/message_widget.py` | Bubble QPainterPath, RGBA colors, fonts, avatar, content/time layout |
| `src/ui/widgets/toggle_switch.py` | Exact switch painting geometry/colors |
| `src/ui/widgets/character_state_panel.py` | State cards/bars and variable view |

Measured/source-derived facts:

- Page margins 14×12 plus shell margins 14×12 = 28×24. Shell horizontal gap 12, vertical gap 8. Title margins (4,2,4,10), spacing 18; headline spacing 10. Title font 18pt/800.
- Chat host padding 14; ChatPanel spacing 12. Conversation strip padding (14,8,14,8), spacing 14. Inspector width exactly 420 expanded / 60 collapsed, padding14, spacing10; tab-page padding (2,12,2,4), spacing12; section padding2/spacing8.
- Message list margins6/spacing4; message outer margins (0,4,0,4), spacing8; avatar36; tail8×12; radius12; bubble body padding left/right12 plus the tail's8 on its side, top/bottom8; internal spacing2.
- Chat defaults: 12pt (=16 logical px), name10pt, time9pt; max bubble width600. These same three user settings were read selectively from the Python test-build settings; no credentials were printed.
- **Measured with the actual Python MessageWidget in offscreen Qt:** for a row788px wide, both user and assistant bubble widths are372px, body width340px. The equivalent layout divides `(row_width - 36 avatar - 8 gap)` equally between the bubble and outer stretch, capped by max width600. This explains the screenshot widths; do not use an arbitrary 580px width.
- Composer padding (12,6,6,6), spacing4; attach/camera32, send42 (glyph18), editor starts36 and grows to80. Send radius14. Original icons must use QtAwesome outlines, not substitute Unicode symbols or hand-drawn approximations.
- Original venv launcher is broken (points to removed Python312). Offscreen widget measurement succeeded using system Python with the original venv's `Lib/site-packages` inserted into sys.path, `QT_QPA_PLATFORM=offscreen`, `PYTHONDONTWRITEBYTECODE=1`, and read-only imports with stubbed `main_logger`/`utils` to avoid app startup/log writes. No original files were changed.

## Verification already performed (before the full-page rewrite)

- `cargo check --locked --offline --features gui --bin neuromita-gui`: passed for the initial rough page, not the new page modules.
- `cargo test --locked --offline --features gui --test vertical_slice sandbox_reads -- --nocapture`: 1 passed. Tests real local-fixture Gemini HTTP, pending cancellation, canonical history, character isolation, settings update and external-request discovery. One later assertion about ProjectedMessage.request_id has not yet been rerun.
- GUI localization/headless render test passed for the initial page. The test saves optional PPM captures when `NEUROMITA_GUI_CAPTURE_DIR` is set. Old captures in ignored `target/gui-captures/` are rejected draft visuals, not approval of the source-driven rewrite.
- Earlier main integration test totals in HANDOFF remain valid only for that prior main revision. No full final suite/Clippy/build was run for this WIP. No live Gemini request or model downloads were made during this task.

Runtime limits still to document/test: GUI shows the last200 canonical messages; finished request observation retains100. Manual Refresh handles non-request timeline edits; this first connection watches request/settings/preset/branch changes, not every arbitrary external timeline append. Voice uses the explicit unavailable port in the current desktop composition, with the existing partial-success semantics; Python TTS workers/audio playback, microphone and capture are not connected.
