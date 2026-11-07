# ASR-related Git review

Compared current initial head `d5a5e03b` against local `main` (`16fb8b7e`).
The branch includes the speech runtime/capture/autostart/reconfiguration chain,
language-catalog parity and subsequent layout/startup repairs. This report scopes
the audit to ASR-related contracts; it does not certify all 39 changed files or
other settings tabs as clean.

## Verdict

The direction is useful: capture/session ownership lives outside Slint, runtime
states are typed, installed selection is inspected, desktop transcripts go to the
draft/send path, and one JSON language catalog is shared by Rust and Python.
The complete chain is **not ready for an unconditional clean merge**.

## Findings

1. **Unresolved: implicit deployment fallback.**
   `InstalledSpeechEnvironment::for_desktop` uses the source SDK/plugin tree when
   installed paths are absent. A desktop launch must use explicit installed paths
   or an explicit development configuration, not infer one from missing files.

2. **Unresolved: duration settings disagree with capture.**
   `SpeechCaptureConfig::from_preferences` silently applies
   `.min(MAX_SPEECH_FRAMES)` with 625 × 16ms = 10 seconds. Core's default is 30s,
   and Python's widget permits 120s. The GUI also hardcodes the ten-second phrase
   in its listening status. Settings, segmentation, WAV/IPC limits and status
   metadata need one coherent bound; simply removing the clamp is insufficient.

3. **Corrected here: absent installed model still exposed cached parameters.**
   ASR render queried language capabilities from persisted `recognizer_id` even
   when the installed dropdown had no matching entry. The recognition controls
   had no availability gate. Availability now follows the selectable read model.

4. **Corrected here: preference save could clear selection indirectly.**
   GUI `read()` builds the recognizer ID from the visible dropdown. With no
   installed entries it becomes empty. `SaveAsr` previously replaced preferences
   with that value, despite model selection having a separate command. Saving
   controls now preserves Core's selected identity; language updates are optional
   when no recognition model is editable. Existing language maps remain preserved.

5. **Corrected here: layout/style ownership.**
   Generic text fields omitted numeric behavior and vertical alignment; fixed row
   heights and the generic `section` fill diverged from Qt. ASR now has explicit
   local numeric/layout/color contracts. The scrollbar viewport gets an overridable
   width while only the ASR instance reserves the stable gutter.

The startup repair in `d5a5e03b` appropriately avoids starting an uninstalled
ASR model or substituting another model. It does not resolve findings 1 and 2.

6. **Unresolved: full test run aborts in lifecycle integration.**
   `application_binds_explicit_local_embedding_retriever_without_fallback` in
   `tests/ai_application_lifecycle.rs` overflows its stack on Windows
   (`0xc00000fd`). Reproduced both in the default full run and in an isolated exact
   test run. The cause is not established; this patch does not change the lifecycle
   implementation. Do not label the current branch as fully tested or merge it
   unconditionally. No enlarged-stack workaround was used to hide the failure.

7. **Outside this patch: GUI test Clippy fails in Voice.**
   `src/bin/neuromita-gui/voice.rs` has implementation after its test module
   (`items_after_test_module`). It remains unchanged under the user's ASR-only
   instruction. ASR's corresponding ordering and the ASR capture test's initializer
   lint were corrected. Default all-target and production GUI Clippy pass.

## Validation boundaries

An earlier key-editor build was interrupted by the user switching branches and
an executable held open by Windows (`os error 5`); its logs are not evidence for
this branch. ASR validation uses its own `target/asr-audit-*` logs. Do not kill the
user's open GUI or claim it has been replaced by these source changes.
