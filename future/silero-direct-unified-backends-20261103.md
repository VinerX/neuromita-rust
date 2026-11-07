# Direct Silero TTS assets and unified AI backend selection — 2026-11-03

## Scope

Branch `codex/ai-engine-install-voice-20261103`; parent commit `12f34c7c`.
All AI plugin families now declare explicit compatible backends in their Python
install plans, and the Rust production selection must carry a `LockedBackend`.
A bare package requirement, a device string, or an empty backend description
is not sufficient for a production installation.

## Silero without Torch Hub

- `silero_rvc_cuda` / `silero_rvc_onnx`: both declare a mandatory `silero_tts_model`
  asset, selected by `language`, containing the upstream package. Russian v5
  and English v3 packages were independently downloaded and hashed in successful
  GitHub Actions run `41671018677`.
- Official URLs and pinned SHA-256: see `assets/model-sources/README.md` and
  `python/neuromita_voice/plans.py`. These are real hashes of the downloaded
  files, not placeholder values.
- `VoicePlugin.initialize()` verifies presence of the selected `.pt` and loads it
  via `torch.package.PackageImporter(...).load_pickle("tts_models", "model")`;
  `silero_tts(...)` and `torch.hub.load(...)` are not called.
- The backend-specific RVC weights and estimators are still required; the source
  catalog intentionally does not claim a complete one-click model revision.

## Explicit backend contract

- `BackendKind`: Torch CPU/CUDA, ONNXRuntime CPU/CUDA/DirectML,
  CTranslate2 CPU/CUDA and `RemoteApi` (network inference without local compute
  runtime package).
- ASR: `google` = RemoteApi; faster-Whisper = CTranslate2 CPU/CUDA;
  GigaAM = Torch CPU/CUDA; GigaAM ONNX and Whisper ONNX = ONNXRuntime CPU/DirectML,
  plus explicit Torch CPU supplemental backend for their actual import/inference
  dependencies. No `auto` device mode in the new ASR install plans.
- TTS: device-specific primary backend is derived from the actual selected
  plugin settings. ONNX RVC uses CPU vs DirectML requirements explicitly;
  Silero+ONNX RVC additionally requires Torch CPU because Silero itself uses
  PyTorch, while the RVC converter uses ONNXRuntime.
- Embeddings and both local rerankers declare Torch CPU/CUDA support.
- All model selections carry a mandatory `LockedBackend` (not `Option`), plus
  exact `extra_backends` matching `InstallPlan.required_backends`.
- Rust checks selected backend compatibility before installing its layer,
  rejects missing or extra required backend layers, checks exact Python version,
  rejects conflicting Torch CPU/CUDA and multiple ONNXRuntime variants when a
  shared worker would combine them, and propagates failures without fallback.
- `RemoteApi` is a **typed external-service backend**, not a fabricated local
  PyTorch/ONNX dependency; it has an immutable manifest but no compute packages.

## Verification boundary

New tests inspect each ASR backend variant, all twelve TTS device-specific
plans, the two pinned Silero hashes, local PackageImporter use, explicit backend
validation, and co-located runtime backend incompatibility. These tests verify
orchestration/contracts, not real model GPU inference or the safety of arbitrary
pickle-based third-party code.

Next proof gates: real Silero+RVC weights on CPython 3.12, actual Torch/CTranslate2/
ONNX/DirectML wheel compatibility on target hardware, verified source catalog
for all RVC voice assets (user-specific), and UI backend selection respecting
hybrid requirements.
