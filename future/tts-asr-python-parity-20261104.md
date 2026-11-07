# Python ↔ Rust TTS/ASR parity — 2026-11-04

Source: Python `src/controllers/speech_controller.py`, `src/handlers/asr_models/asr_languages.py`, and Rust speech runtime on `codex/speech-live-reconfiguration-20261104`.

## Completed in this increment

- [x] Preserve Python ASR language lists (Google: 102, Whisper/Whisper ONNX: 101, GigaAM: 1) in a single embedded JSON catalog consumed by Python SDK and Rust/Slint.
- [x] Validate selected language codes through Python `settings_schema` before inference activation and reject unsupported known-model languages at Rust settings persistence.
- [x] Stop unconditional `ChatApi::reply` from desktop microphone capture. Send recognized text through the existing chat draft/GUI send path.
- [x] Respect `instant_send` and `delayed_send`/`send_delay_ms`; delayed sending is conditional on the draft still matching the recognized text (user edit cancels the scheduled send).
- [x] Respect `merge_chat` for appending ASR text to the existing desktop draft (space when enabled, newline otherwise).
- [x] Keep the existing direct `ChatReplyTarget` for explicit non-desktop API use; the desktop speech path now explicitly chooses its transcript destination.

## Still open (not claimed as complete)

- [ ] Dynamic per-plugin Slint controls driven from Python SDK `settings_schema` (`tts_model_options` / `asr_model_options` already persist in SettingsOwner).
- [ ] Runtime adapters for Telegram and remote/API TTS (GUI settings exist; local worker execution alone is not parity).
- [ ] Game-session ownership for ASR transcripts, idempotent undelivered recovery, and game radio/PTT capture ownership.
- [ ] External Mita-speaking input gate / pause-while-speaking (`SpeechInputHandler` already suppresses its own playback echo but not externally emitted character voice).
- [ ] Python-style duplicate-transcript suppression and merge/debounce when the game owns input.
- [ ] End-to-end mock-worker GUI tests validating delayed send, user editing, and service selection (no hardware/inference tests performed).

## Boundaries

Do not modify Presets API. No model installation or downloads from speech settings. Do not conflate a verified on-disk model with an initialized worker. Do not infer game recipient from the selected desktop character.
