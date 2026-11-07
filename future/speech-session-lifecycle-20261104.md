# Speech capture ownership and GUI bindings (2026-11-04)

The application owns the one active speech capture task. `audio::controller::SpeechSessionController` is responsible for start, stop, PTT messages, cancellation, process lifetime and a typed `watch` snapshot. The existing `SpeechInputHandler` remains the only capture → segmenter → ASR → conversation → playback pipeline. `SpeechSessionPresenter` exposes a read-only projection of the same source of truth.

## Start path

1. Slint ASR toolbar invokes `session-toggle(bool)`, delivered as a desktop command.
2. The desktop runtime checks the **actual** ASR supervisor state (`ModelReady`), reads `SettingsOwner`, and verifies the explicitly selected device ID and audio backend against the current input inventory.
3. Explicit absolute FFmpeg/FFplay paths must exist. The Core validates input mode, thresholds and segment limits. It does not download software or infer an unspecified microphone.
4. `ApplicationLifecycle::start_speech_capture` rechecks ASR readiness and creates the existing Python transcriber + chat reply handler. The application-owned controller starts the FFmpeg process with kill-on-drop, a fresh Earshot VAD and a bounded work queue.
5. For `continuous`, speech is segmented automatically. For `radio`, enabling capture starts a PTT segment until release/stop or the segment limit. For `push_to_talk`, the Slint hold control sends actual press and release events.

The current IPC/WAV limits bound **each submitted segment to 10 seconds**, even if `max_speech_ms` is larger. The ASR UI reports this operational limit. Playback uses an explicit FFplay executable; unsupported output formats and absent binaries fail closed.

## Status and shutdown

`Stopped`, `Starting`, `Listening`, `Stopping`, and `Failed` are reported by a `watch::Receiver<SpeechSessionSnapshot>` through `GuiPresenter.speech`; no UI-owned state is authoritative. Supervisor model readiness remains separate from capture readiness. The GUI can always stop a listening microphone, even if the ASR worker loses readiness. Application shutdown cancels and joins the active capture task before completion.

## Verification boundary

This code is checked with offline Core and GUI compilation, plus unit tests using only synthetic data. No live microphone, TTS synthesis, ASR inference, GPU initialization or Windows device enumeration was exercised.

This commit does not automatically install or activate models at launcher startup; an explicitly prepared, running ASR worker is still required. The input pipeline does not yet implement configurable transcription batching/delayed-send semantics; the existing handler immediately forwards completed transcripts. The user-selected model settings remain in `SettingsOwner`, not in the capture controller.
