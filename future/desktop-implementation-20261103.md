# Desktop implementation progress, 2026-11-03

User scope: finish functional presenter connections, implement existing settings (characters/presets and related sections) from the Python widgets/QSS, integrate all GitHub work, add/vend Earshot and smoke-test without private recordings. Work until the shared 5-hour allowance approaches 6% remaining. Do not ask questions or use subagents. Check actual usage with `get_usage_limits`; initial read: 25% used / 75% remaining.

Presentation contracts are grouped by **mechanism**, not by GUI page hierarchy. GUI and a future TUI consume the same command/read/observe interfaces. Pages only compose those mechanisms.

Python is pinned to **3.12** per the user's clarification. The incoming application lifecycle tests assumed 3.13.5; fixtures now require a selected 3.12 interpreter and derive its exact patch version for the lock. The production interpreter-versus-lock check remains strict. Negative ABI tests use an explicitly incompatible version.

- [x] Fetch all remote branches.
- [x] Merge AI install/activation, application lifecycle, ASR/local embeddings/reranking (`ec9c219f`), retaining GUI additions and both goal sections.
- [x] Merge memory inspection/observer/capacity chain (`3035ed95`).
- [x] Record all bridge histories without transient bundle payloads/export jobs. The GigaAM reference branch is intentionally not an implementation override: the integrated AI branch forbids implicit downloads and uses its own canonical native paths, while the reference has the older Python-only download/helper behavior.
- [x] Add Earshot 1.2.2 through Cargo, vendoring its crate without rewriting existing third-party files.
- [x] Run `cargo run --locked --offline --example vad_smoke`: silence/synthetic sine frames, finite bounded probabilities, deterministic reset and invalid threshold rejection. No private recordings or upstream audio tests. Keep original licenses in `licenses/earshot/`; vendored bytes/checksums are intact.
- [x] Validate merged contract suites on Windows: Rust GUI 356 passed / default 355 passed (2 ignored each), Python 33 passed, GUI all-target Clippy with warnings denied and fmt passed. The pinned-Python fixture was rechecked separately: 8 passed.
- [x] Push/merge integration PR #71 into main (`302b98ce`). Subsequently integrate the new idempotent-memory branch (`ab46d9d5`) and its bridge history in the desktop settings branch.
- [x] Replace SandboxPresenter with functional ConversationPresenter; keep Characters/Settings/Presets/Chat and Connections presenters independent of page nesting.
- [x] Start Core/admin/history without a configured LLM; missing preset/credential remains a typed generation error.
- [x] Implement Settings page from Python Qt/QSS: preset CRUD/active selection, three typed per-character preset slots, voice flags/ID/volume and confirmed persistence state. Provider credentials remain environment references.
- [x] Connect history, guarded clear/reset confirmation, retained request diagnostics and live client count/server status. Add memory editor using CharactersPresenter snapshot/CAS, live observer, CRUD/forget/restore/protection, search and paging. External updates do not replace the draft or its edit token.
- [ ] Remaining: message-specific revise/restore controls, character parameter editing, AI model/install UI and actual microphone/capture/playback integration. Disabled controls must not imply those features work.
- [x] Verify software-rendered GUI at 1560×918 and 1200×760, error/disconnected states, locale changes and real presenter interactions with local fixtures. Native minimize/restore remains unverified (see below).
- [x] Commit/push completed increments and keep handoff current before stopping near the allowance threshold. PR #72 merged into main at `c4901f2c`; final save-confirmation fix is published as a follow-up PR.

Earshot reference: https://github.com/pykeio/earshot ; pinned crate 1.2.2 (`c400f15bd232dda30a297eec90235bd09066cab7`). It processes 256-sample / 16 ms frames of 16 kHz mono PCM16 in the native adapter. No microphone capture, ASR, speech-accuracy claim or model download follows from adding this component.

## Current desktop contracts

`ApplicationLifecycle` constructs mechanism-oriented presenters. The Slint bridge sends bounded typed commands to one Tokio desktop task; model/settings/request changes return through the Slint event loop. No page presenter or UI-owned canonical state was introduced. Slint globals hold view/draft data only. `ConversationChangePreview` is opaque and bound to character, branch selection and head; applying an outdated confirmation is rejected. Clear emits MessageHidden facts; context reset emits CharacterContextReset, not destructive deletion or a reset of all character parameters.

The memory bridge owns a current read snapshot and a separate immutable draft snapshot. Re-rendering live memory updates changes only the read list; save uses the draft token and CharacterHandler performs CAS. Memory observation runs in an owned abort-on-drop task so a competing desktop command cannot cancel an in-flight notification read. Pages/search are projections of one captured read snapshot, not another memory store. The native Earshot adapter is still independent of microphone capture.

Settings UI covers the implemented Rust fields, not all Python settings. Source references: `src/ui/pages/settings/settings_page_widget.py`, `src/ui/settings/api_settings/ui.py`, `src/ui/settings/character_settings/ui.py`, `src/styles/pages/settings.py`, theme/base QSS. Existing shared controls/fonts/assets supply the appearance; the volume slider uses the same pink/white palette and 5 px track / 18 px handle. Existing unsupported AI/capture switches remain disabled. No real Gemini inference, voice inference or recording is part of these UI checks.

## Final integration and validation

PR [#72](https://github.com/Atm4x/neuromita-rust/pull/72) combines this desktop work with `ab46d9d5` (idempotent facts), `1aff1241` (one-time island seeding) and `be712a87` (approved generated AI assets / ONNX export). All fetched origin heads are ancestors of the integration branch; a separate historical `bundle/` ref is not an origin development branch. Export-only workflows and bundle payloads were deliberately not copied into the application tree.

The installer subprocess now receives and checks the exact Python version from its selected lock before executing the conversion hook. Its Windows test discovers the selected 3.12 patch version rather than pretending this machine has 3.12.10. A mismatched interpreter is rejected before publication. The asset-generation future is boxed at the public boundary: the incoming conversion test otherwise overflowed the Windows test thread stack. The default stack size was not increased.

Final combined checks: **368 Rust tests with GUI / 367 default**, two explicitly ignored tests in each configuration; **36 Python SDK tests**. Both default and GUI all-target Clippy pass with warnings denied; fmt passes. After the last purely visual changes, the GUI binding/render test and offline GUI build were run again successfully. Debug executable: `target/debug/neuromita-gui.exe`; build outputs are not committed. Local provider fixtures only; no private recordings, large model conversions or live inference were used.

For Windows fixture runs, select Python 3.12 explicitly when needed:

```powershell
$env:NEUROMITA_TEST_PYTHON = (python -c 'import sys; print(sys.executable)').Trim()
cargo test --locked --offline --features gui
```

Native UI automation was attempted through the computer-use skill. Both initializations failed with `failed to write kernel assets ... os error 3`, before the application could be selected or controlled. This is an automation infrastructure failure, not evidence that native renderer/minimize/restore behavior passes or fails. Do that check manually or when the helper works.

Review renders use synthetic/local fixture data: [presets](gui-reference/rust-desktop-20261103/settings-1-1200.png), [characters](gui-reference/rust-desktop-20261103/settings-2-1200.png), [voice](gui-reference/rust-desktop-20261103/settings-3-1560.png), [memory](gui-reference/rust-desktop-20261103/memory-1200.png), [Sandbox](gui-reference/rust-desktop-20261103/sandbox-ru.png).

Next work, in order: native window restore verification; message-specific revise/restore controls and character parameter commands; typed model/install administration for the existing AI lifecycle; then real capture/microphone/ASR/playback wiring. Earshot is ready as a stream-local adapter, but adding it did not wire an audio capture pipeline.

## Save acknowledgement follow-up

Memory save commands carry a draft UUID through success/error acknowledgements. A delayed acknowledgement can only clear/unlock the same draft; switching to a different memory or character invalidates it. The submitted editor is temporarily read-only, and command-queue failure restores it without discarding text. Preset creation likewise carries a form token: opening another preset/new form prevents an older creation acknowledgement from replacing the editor's ID. GUI regression coverage exercises delayed success and error after selecting another memory and delayed preset creation after opening an existing preset. GUI test, GUI all-target Clippy/fmt and offline build pass for this follow-up; core and Python code are unchanged from the full 368/367/36 validation above.

Stopped with the requested reserve approaching 6%. Open checkboxes above are the next increment, not completed features. Native renderer restore verification requires working window automation or a manual run; no application window was controlled during this session because helper initialization failed.
