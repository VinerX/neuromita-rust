# AI Engine implementation increment — 2026-11-03

Baseline: `codex/slint-ai-runtime-formal-20261103` at `2cd201dc349595833f8a440f2a0f7848a9d2278a`.

This increment changes runtime code and tests, not merely the design. It is **not** a claim of production inference, complete AI Hub UI, or closure of issues #24/#26/#37/#42/#51/#59/#61.

## Implemented and locally verified

- Backend and model-specific overlay installation now verify installed `*.dist-info/METADATA` identities against their exact resolved locks, both before publishing and when reopening immutable revisions. A receipt alone is insufficient to claim readiness.
- The 12 built-in Python TTS descriptors explicitly declare compatible CPython minors (3.11/3.12) for the current NumPy 1.26.0 / SciPy 1.12.0 pins, along with canonical CPU/CUDA/ONNX backend choices. The installer rejects incompatible selected Python versions/backends rather than attempting an installation that cannot satisfy the declared contract.
- The common SDK implements validation of typed settings and bounded enumerations, and a host method for side-effect-free config validation. The runtime can expose model-dependent Silero speaker options *after* model initialization, while Silero and RVC have independent `silero.speaker_id` and `rvc.pitch_shift_semitones` keys.
- Explicit, allowlisted installer actions are represented as serializable data; the built-in Fish Speech install plan uses an explicit `fish_project_root` action. Rust only applies the `create_empty_marker` action to a verified, staged package after exact ID approval, and verifies the result when reopening the revision. Arbitrary Python callables are not transferred over IPC.
- A new `ModelAssetStore` stages user-authorized local/HTTPS assets with explicit SHA-256, bounded transfers and symlink/path checks, and atomically publishes immutable, named plugin revisions. Existing revisions are rehashed on selection; changed/corrupt revisions are rejected.
- A separate `VoiceProfileStore` validates profile identity, settings, referenced WAV/TXT file paths and SHA-256, and resolves references without embedding weights in profiles. `PythonVoiceProvider` maps per-voice settings and references to typed Python inference input and rejects an active model/profile revision mismatch.
- `AiSupervisor` checks named asset revisions before worker activation and passes selected immutable revision paths to the Python process. Rust→Python mock TTS→WAV integration exercises the voice profile and conversion of Rust volume 0–100 into Python gain 0–1. The Python runtime accepts per-request settings across Silero, F5, Fish Speech and OmniVoice.

## Test evidence

- Rust 1.98.1, `cargo test --locked --offline --no-default-features`: PASS (all executed tests; no real model weights).
- Rust 1.98.1, `cargo clippy --locked --offline --no-default-features --all-targets -- -D warnings`: PASS.
- `cargo fmt --all -- --check`: PASS.
- Python 3.13.5 used for **mock protocol tests only**: `PYTHONPATH=python python3 -m unittest discover -s python/tests`: 15 PASS.
- Rust local AI-specific tests: 32 PASS (scoped `--lib ai_runtime`).

## Remaining blockers and explicit non-goals of this increment

1. **Production environment composition**: bootstrap of verified `uv`, Python interpreter, SDK distribution, backend layers and overlays must be assembled into one owned application-startup flow, including authoritative lifecycle and failure recovery. The default `ApplicationConfig` still injects `UnavailableVoiceProvider`; application settings and Slint AI Hub cannot yet select/activate a real installed model.
2. **Real assets**: no trusted model download manifests with pinned hashes have been provided. The new store requires concrete source files/URLs and pinned checksums; it deliberately does not guess sources or automatically download arbitrary third-party weights. The 12 model install descriptors still contain many unpinned assets.
3. **Production inference**: no real PyTorch/ONNX/CUDA model weights, speaker lists or audio quality tests were available; F5, Fish, OmniVoice and Silero/RVC runtime code is structurally connected but not certified by actual synthesis. In particular, the backend package recipe must be tested against the real platforms and Python 3.12 environment.
4. **Streaming/cancel**: TTS currently uses bounded Base64 WAV over JSON-lines IPC. Long audio streaming/backpressure, guaranteed cancellation on GPU, process crash recovery and playback lifecycle remain independent tasks.
5. **ASR/VAD**: real speech capture, segmentation, device monitoring and Rust-native wavekat VAD are not implemented here.
6. **Security and durable state**: third-party plugin execution is not OS-sandboxed; durable selection/profiles revision history, trusted catalog, per-plugin permissions and UI confirmation still need implementation.

Do not mark a global issue or full Python parity complete based solely on mock tests or the number of plugin descriptors. Next objective should be an application-owned AI runtime composition/lifecycle and one instrumented real-model Silero+RVC inference using pinned assets and a verified Python 3.12 environment.


## Application lifecycle and canceled IPC — second increment (2026-11-03)

Base for this increment: the previously published Git commit `1cf84d6079a4a0165c17714198951e4279f0391b`.

- `AiApplicationConfig` is now an **explicit**, application-owned selection of resolved service compositions, pinned Python interpreter, chosen models and optional voice-profile store. Application startup validates topology (one shared worker or service-specific split workers), exactly one model per selected service and selected TTS readiness; no generic service locator is added.
- Startup verifies the actual Python interpreter version against every selected immutable composition *before* worker activation. A mismatch returns an explicit application startup error; it does not silently use the unavailable voice provider or another Python installation.
- `ApplicationLifecycle` injects the activated Python voice provider into the existing `VoiceProvider` port and owns worker shutdown. The normal minimal config **continues to use** `UnavailableVoiceProvider` when no AI runtime is configured. This stage does not invent a default model choice or a user-facing selection.
- Existing fallible settings, TCP binding and Timeline initialization happen **before** Python activation, so failure in these steps cannot leave a newly started AI subprocess behind. Failed multi-worker activation stops previously activated workers.
- In-flight IPC is cancellation-safe at the worker boundary: abandoning a Rust request future immediately poisons/kills its worker, so a late Python response cannot be reused as the response to a subsequent request. Restart requires a new generation. This is hard cancellation by worker termination, **not** cooperative CUDA cancellation or a complete replay/recovery mechanism.
- The new integration tests cover application-owned mock WAV, failed activation and subsequent retry, interpreter mismatch, failure before activation on invalid Timeline or occupied server port, and stale-response rejection after abort and worker restart.

**Verification:** Rust 1.98.1 `cargo test --locked --offline --no-default-features` completed: **285 passed, 2 ignored** across 35 test targets. `cargo clippy --locked --offline --all-targets -- -D warnings` and its `--no-default-features` counterpart passed; `cargo fmt --all -- --check` passed; Python SDK **15/15**. The complete default-feature test suite did not finish within the available command time; no PASS is claimed for it. Tests use a mock Python TTS, not production weights.

**Still open:** no automatic uv→Python→SDK→backend/overlay installation from `ApplicationConfig`, no durable selected-model UI state, no real model E2E, no completed Slint AI Hub, no native ASR/VAD, no safe cooperative cancellation guarantee for CUDA. Issues #24/#26/#37/#42/#51/#59/#61 remain open.
