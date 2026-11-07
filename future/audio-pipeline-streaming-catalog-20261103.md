# AI Audio Pipeline, IPC Streaming and Model Source Catalog — 2026-11-03

Base: `codex/ai-engine-install-voice-20261103` at `6881d59716f13e49ec37289b3625fcaa0934d37b`.

## Scope delivered

1. **Application-owned audio pipeline.** `src/audio/segment.rs` implements bounded 16-kHz mono PCM16 segmentation with Earshot VAD and explicit PTT, pre-roll, minimum/maximum duration, silence hangover and WAV encoding. `src/audio/session.rs` defines an owned `SpeechInputHandler`: capture frames → VAD/PTT → single dispatch queue → typed `SpeechTranscriber` → `ChatApi::publish_message`/reply → accepted TTS audio → typed `AudioPlayback`. Work is cancelled through parent/child lifecycle tokens; PTT can interrupt inference/playback, and active playback suppresses acoustic feedback while preserving PTT pressed state. The queue is bounded and fails explicitly instead of silently discarding transcript requests. `ApplicationLifecycle::speech_session` exposes the handler *only if ASR is already activated*.

2. **Concrete device adapters.** `src/audio/device.rs` uses separately supplied FFmpeg and FFplay OS processes (no shell), supporting PulseAudio, ALSA, Windows dshow, macOS AVFoundation and an explicit synthetic Lavfi test source. Their child processes have kill-on-drop; ffplay interruption is controlled by a cancellation token. `examples/audio_io_smoke.rs` exercised actual FFmpeg Lavfi generation and FFplay with the SDL dummy output backend. **No physical microphone/device hotplug or real speaker was verified**. **The normal Slint desktop bootstrap does not yet configure a selected AI ASR runtime or expose mic/PTT controls**; this is a code integration gap, not merely a test omission.

3. **Bounded TTS IPC transport.** Python host implements `infer_stream_start` → multiple `infer_stream_next` → optional `infer_stream_cancel` with one outstanding stream per worker, bounded chunk sizes and stream IDs. Rust checks stream ID, reported size, chunk bounds and final completion under the worker lock. Dropping a partially consumed stream poisons/terminates its worker so later results cannot be misassociated. The existing `PythonVoiceProvider` switches to this path. **The model's `infer()` still produces an entire WAV first; the transport chunks it afterward. This is NOT low-latency incremental inference or true progressive speaker playback.** It also still assembles an `AudioArtifact` before the request is accepted.

4. **Pinned source catalog.** `src/ai_runtime/assets/catalog.rs` validates versioned JSON entries `(plugin ID, revision, role, relative path, SHA-256, HTTPS or absolute local source)` against the exact plugin install plan, and rejects missing/extra/duplicate or mismatched files. `ModelAssets::Catalog` invokes the existing `ModelAssetStore` atomic immutable publication and re-verification. The on-disk source catalog `assets/model-sources/catalog.v1.json` is **intentionally empty**: no verified hashes and signed source revisions were available for third-party TTS/ASR models. This is a working format and pipeline, not a prepopulated one-click model marketplace.

## Evidence

- Integration test `tests/audio_pipeline.rs`: deterministic PCM capture → gate → ASR → actual `ApplicationLifecycle`/`ChatApi`/Timeline → mock TTS → mock output, asserts both speaker messages and RIFF playback.
- `src/audio` unit tests: segmentation, minimum and maximum bounds, PTT state preserved while playback is suppressed.
- FFmpeg / FFplay real-process Lavfi-to-dummy-output smoke test passed.
- `tests/ai_streaming.rs` verifies multiple correlated IPC chunks with real Python host (mock WAV of 120044 bytes).
- Python SDK `python/tests/test_host.py` tests stream start/next/cancel and mock PCM WAV.
- Catalog tests verify malformed sources, exact role coverage, local file hash tampering. `tests/ai_production_installer.rs` exercises the catalog through an actual Rust installation coordinator with mocked `uv` and a real CPython 3.12.10 worker.
- Tests use Python 3.12.10 and NumPy 1.26.0, **not** Python 3.13.

## Remaining code requirements — not closed

- Normal desktop/UI selection and persistence of ASR/TTS models, capture device/permission controls, PTT bindings and status. The desktop currently starts without the AI runtime selection; the library's speech session must be explicitly instantiated after a selected ASR worker is activated.
- Native OS-specific audio capture/output and device hotplug policies if bundled FFmpeg/FFplay are not an accepted runtime dependency.
- True model-incremental TTS: the existing plugin contract returns an entire WAV. Frame streaming from generation through backpressure-controlled playback, early playback, per-character state and end-of-stream must be designed across Python SDK, Rust VoiceProvider/acceptance and selected models.
- Trusted, verified source manifests with actual hashes for the model variants, license/permission UI, download progress, installation errors and full third-party model source lifecycle.
- Hardware and quality parity verification, stereo echo cancellation or headphones-only safety policy, live VAD tuning and real CUDA model workloads.

Do not close broad issues #24/#25/#26/#37/#42/#51/#59/#60/#61 on this increment alone.
