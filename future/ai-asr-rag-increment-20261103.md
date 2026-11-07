# AI Engine: ASR and RAG worker integration increment (2026-11-03)

## Scope and source fidelity

The original Python NeuroMita at `5f3d061f523d9abefa6a264bb2953048a935fa65` registers five ASR engines: `google`, `gigaam`, `gigaam_onnx`, `whisper`, and `whisper_onnx` (`src/handlers/asr_models/registry.py`). The original RAG worker provides embeddings and cross-encoder reranking, in addition to indexing, graph, lexical retrieval, and orchestration. This increment is **not full Python parity**.

This branch retains the existing Rust-owned Python process/IPC contract and Timeline authority. No event bus, generic service locator, Python subprocess supervisor, implicit ASR fallback, or silent lexical fallback was added.

## Implemented

- `google` ASR plugin: independent discovery and install plan, `SpeechRecognition` remote Google Web Speech API, bounded mono PCM16 WAV, language setting, explicit no-speech result. No microphone ownership or VAD in this plugin.
- `whisper` ASR plugin: faster-whisper/CTranslate2 local model inference, `large-v3-turbo` and `large-v3` model choices, selected-model-specific installation manifests, CPU/CUDA and precision/beam settings, no automatic runtime weights download, segment-level hallucination filter, explicit 16 kHz boundary.
- `local_transformers_embedding` plugin: offline local AutoModel/AutoTokenizer, batch embeddings, query/document prefixes, explicit model-space identity/dimensions, mean/last-token pooling, finite normalized vectors.
- `local_cross_encoder` plugin: offline local sequence-classification cross-encoder, bounded query and source-ordered candidate scoring. LM-style Qwen3 reranker is not supported by this adapter.
- `mock_embedding` and `mock_reranker` fixtures: deterministic tests only, never exposed as a real model's quality or readiness.
- Rust `PythonSpeechRecognizer` and `PythonEmbeddingProvider` typed adapters, plus source-ordered `PythonReranker` scoring. The embedding adapter implements the existing `EmbeddingProvider` port; no duplicate knowledge store is created.
- `ApplicationConfig.ai_knowledge`: explicit selection of Python embeddings and fusion mode, joined to the existing `SemanticMemoryRetriever` only after the worker initializes. Missing selected embedding worker is an error, not a fallback. An ASR- or RAG-only worker selection does not require TTS.
- Versioned model revision requirement for locally loaded weights; no arbitrary plugin path resolution. Python installs are separately probed from model initialization. NumPy 1.26.0 requirements reject installed NumPy 2.x.

## Important boundaries

- Not yet implemented: `gigaam`, `gigaam_onnx`, `whisper_onnx`; physical microphone selection, native VAD, speech boundary state machine, hotplug, ASR client routing into a user dialogue, Unity speaking leases.
- Not yet implemented: full RAG history/graph/document indexing and provenance, source-aware cross-encoder application to fused results, Qwen3 LM reranking, persistence/reindex operations, graphical RAG settings.
- TTS 12 adapters and installation infrastructure from prior commits remain; successful physical inference on actual Silero/F5/Fish/Omni weights has not been tested.
- No real Google ASR network call, Whisper/GigaAM/ONNX model loading, Transformers embedding, or cross-encoder inference was performed. Tests are contracts and mocks, not model-quality certification.
- No auto-download of untrusted HF/Python code on inference. Local Transformers model loading uses `trust_remote_code=False` and `local_files_only=True`; models requiring custom code need a separately reviewed runtime contract.
- ASR IPC currently limits a single mono PCM16 WAV segment to 700,000 bytes (16 kHz: approximately 21.8 s of samples). A scoped file/stream contract, resampling adapter and longer audio must be provided before production microphone support.
- Model asset revisions and package compositions still need to be actually installed and verified; the adapter does not silently resolve or install dependencies during `initialize`.

## Verification

Run from repository root with Rust 1.98.1, local vendored dependencies:

```
PYTHONPATH=python python3 -m unittest discover -s python/tests -q
cargo test --locked --offline --no-default-features
cargo test --locked --offline
cargo clippy --locked --offline --all-targets -- -D warnings
cargo clippy --locked --offline --no-default-features --all-targets -- -D warnings
cargo fmt --all -- --check
```

The Python tests mock external recognizers. Rust tests include split ASR/embedding/reranker mock IPC, per-request type/space checks, plus application-owned ASR-only and local embedding startup/failure contracts. No GPU and no network are required.

## Next mandatory stages

1. Bring over the exact original GigaAM / GigaAM ONNX / Whisper ONNX inference implementations, including internal model code, required checkpoint role manifests and device-specific backend validation. They are not covered by the two current ASR plugins.
2. Complete Rust-native PCM capture/segmenter and route accepted ASR transcripts to the addressed request/client, with mock devices and deterministic edge-case tests.
3. Integrate cross-encoder scores into an explicit, source-aware Rust RAG ranking phase; retain exact source identity and Timeline revision when scoring, and protect against stale results.
4. Implement source-specific history/memory/graph RAG indexing and reindex lifecycle, plus application-facing settings and AI Hub.
5. Verify production env installs, real model assets/ABI, device behavior, missing/corrupt assets and live end-to-end inference for each engine and TTS variant.

Do not close #24/#25/#26/#37/#42/#51/#59/#60/#61 or assert full AI Engine parity from this increment.
