# TTS/ASR settings presenters and model inventory (2026-11-04)

## Boundaries

Settings pages do not install or start TTS/ASR workers. `SettingsPresenter.tts` and `.asr` edit immutable snapshots through the existing `SettingsOwner` and persistent `AppSettings.speech` section. The desktop runtime only orchestrates snapshots, read-only inventory refreshes and GUI updates. The model catalog in Core is derived from plugin manifests, locked Python overlays and verified model asset receipts; it does not become a second model registry or source of truth.

## UI data flow

- Voice: `VoiceUi` callbacks → `voice::bind()` → `Command::SaveTts` / `SaveCharacterVoice` → `SettingsPresenter.tts.preferences` → `SettingsOwner` → `Update::Administration` → `voice::render()`.
- ASR: `AsrUi` callbacks → `asr::bind()` → `Command::SaveAsr` / `SelectAsrRecognizer` → `SettingsPresenter.asr.preferences` → `SettingsOwner` → `Update::Administration` → `asr::render()`.
- Refresh: `Command::RefreshSpeechInventory` → `SpeechModelCatalog::snapshot()` and `AudioInputCatalog::discover()` → `Update::SpeechInventory` → `voice::inventory()` and `asr::inventory()`.
- For deployed executables the built-in plugin list is embedded from actual `python/plugins/builtin/*/plugin.json` at compile time. User-provided plugin manifests are additionally read from the configured data directory. No Python script execution is required for this catalog.
- API voice provider templates follow the Python `remote_voice_templates.py` catalog; remote API/Telegram credentials are NOT persisted in general settings.

## What a status means

`package_detected` means an overlay marker for that plugin/revision was found. `verified_assets` means `ModelAssetStore::verified_revision` validated an immutable asset receipt and hashes. These are shown separately. Neither guarantees that Python dependencies can import or that a model can initialize; true runtime readiness would require a worker `probe_install` and model activation. A plugin manifest alone is **never** shown as installed. Refresh also obtains available audio inputs on supported systems, without opening a microphone.

## Python parity and deliberate limitations

The pages expose backend/model selection, Telegram wait and polling intervals, local TTS lazy/auto-load flags, API voice profile/model/speed and per-character voice IDs, ASR device/backend/input/send behavior, per-model language preference, PCM chunk size and VAD/segmentation timing. Settings are atomic and backwards-compatible with existing persisted `AppSettings` missing `speech`.

Integration correction: the current strict format requires the complete speech section and all its fields. No deserialization defaults or historical compatibility are retained. Per-character voice IDs use `BTreeMap<CharacterId, String>`; the shared voice has a separate `default_voice_id`. UI indices are translated at the adapter boundary and never stored as identities. This supersedes the earlier backward-compatibility statement above.

Initialization/inference still belongs to the AI Runtime, not presenters. ASR plugin-specific initialization schema fields beyond language (e.g. Whisper model revision, beam size, compute type, GigaAM model variant/device) are not yet exposed by the desktop Presenter: the plugin SDK provides `settings_schema` and `probe_install`, but the desktop runtime currently does not own a live schema/probe IPC worker. These should eventually be surfaced via a read-only worker inspection operation, rather than copying plugin schemas into Rust. Similarly, audio monitoring / capture, TTS playback tests, remote voice preview, Telegram connection and secure secret storage are not claimed to work. Controls for live actions remain disabled. The UI's save operations only write nonsecret preferences.

## Verification

Use `cargo check --locked --offline --features gui --bin neuromita-gui`, `cargo fmt --all --check`, `cargo test --locked --offline --lib settings::speech::tests`, `cargo test --locked --offline --lib presentation::settings::tests::tts_asr_preferences_round_trip_through_settings_owner`, `cargo test --locked --offline --lib ai_runtime::speech_catalog::tests`, and `cargo test --locked --offline --lib audio::input_catalog::tests`.
