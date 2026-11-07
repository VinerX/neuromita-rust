# Issue #43 — Slint Launcher wiring (2026-11-03)

Base: `codex/ai-engine-install-voice-20261103` at `1a0dfdfa0cea965d03ce6b1703f1037b39f66479`.
Python reference: `bridge/session-timeline-reexport-20261103` source at `5f3d061f523d9abefa6a264bb2953048a935fa65`.

## Integrated paths

- The Slint Home primary action calls the GUI adapter (`launcher_ui.rs`) and then the existing `LauncherSession`. It starts the configured executable or stops **only the child owned by this GUI session**. The button switches between `Играть` and `Остановить` based on the owned process state, refreshed once per second.
- The menu action `Открыть папку Unity` calls `LauncherSession::open_location()`; errors appear in the existing notice area. No second process manager or presenter-level process implementation was introduced.
- On missing install / invalid executable / already-running external process / permission denied, the launcher service's typed error is surfaced rather than bypassed. An external process is never killed by the GUI.
- The GUI accepts `--unity-dir <directory>` or `--launcher-config <file>`. They are mutually exclusive. The latter reads the existing `LauncherConfig` schema; invalid or missing explicit config fails instead of being silently overridden.
- Without either flag, Unity path follows the Python default: `NEUROMITA_BASE_DIR/NeuroMita-Unity` if set, otherwise `<GUI executable directory>/NeuroMita-Unity`. Core's strictly verified executable selection is unchanged. Translations use stable IDs and the existing localizer.

## Python parity boundary

Python reference: `src/core/unity_installation.py` and `src/controllers/gui/home_page_controller.py`. The Python Home page handles launching/stopping and can attach by PID and process creation time to an already-running Unity instance. The Rust GUI intentionally does **not** take ownership of a process started externally; `LauncherSession` rejects a duplicate start. A separate decision would be required to support stopping a previously attached game, while preserving PID + identity safety. Python can create the Unity directory on `open_unity_folder`; current Rust service requires the install root to exist. Python discovery is more permissive than Rust verified candidate selection; this change does not weaken the verified selection.

Closing the GUI drops `LauncherSession`, which terminates an owned process per the existing launcher contract. Detaching a game that should survive GUI shutdown is **not** implemented.

Update/install/verify menu actions remain explicit placeholders. The Home component/version cards are still preview data. There is no new updater, installation path editor, or live Windows E2E in this change.

## Validation

- `cargo fmt --all -- --check`
- `cargo test --locked --offline --features gui --bin neuromita-gui`: 5 passed, including a real Linux process start/stop through the new GUI adapter.
- `cargo test --locked --offline --test launcher`: 11 passed, 1 ignored helper test (existing).
- Windows GUI/process and missing-install user acceptance require validation on a real installation before closing #43.
