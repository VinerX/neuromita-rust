# AI Engine — ASR families and local RAG reranking (2026-11-03)

## Scope and verified base

Parent: `ad2d78e342b18e60d63892ff5a7d5a11a571328f`. Original Python reference: `Atm4x/NeuroMita` `5f3d061f523d9abefa6a264bb2953048a935fa65`.

This increment implements the previously missing Python plugin *inference paths* for GigaAM PyTorch, GigaAM ONNX and Whisper ONNX, and exposes an optional, explicit RAG cross-encoder/causal-LM reranking step inside the existing Rust knowledge retrieval boundary. Python does not own a second process supervisor; it receives validated model assets and provides scores or transcriptions. Rust owns worker lifecycle, Timeline admission, fusion, scoring policy and failure handling. No EventBus or capability registry is introduced.

## Delivered

- Five source ASR engine families now have builtin Python SDK plugin entrypoints: Google, faster-Whisper, GigaAM PyTorch, GigaAM ONNX and Whisper ONNX. Model-specific Python schemas, install plans, assets and probes are declared by plugins, with no Rust enum of ASR implementations.
- GigaAM source package (10 original files, including MIT LICENSE) is included as `python/gigaam`, based on the pinned original repository. The obsolete `utils.native_paths` dependency was replaced and implicit model downloading during inference disabled. The original checkpoint checksum remains active.
- The GigaAM ONNX plugin uses explicit pre-exported ONNX sessions and config, with separate CTC/RNNT asset roles and E2E tokenizer requirement. Exporting checkpoints to ONNX is **not** yet implemented as a typed installation hook. It must occur as an authorized, staged install step before these sessions can start.
- Whisper ONNX uses Optimum and ONNXRuntime with separate CPU and DirectML installations and fails when explicitly requested hardware is unavailable. DirectML probes correctly require `onnxruntime-directml`, not the separate CPU `onnxruntime` distribution.
- Cross-encoder and causal-LM yes/no reranker plugins compute bounded, validated scores through the same Rust-owned worker protocol; an optional `AiRerankSelection` connects a chosen worker to `SemanticMemoryRetriever` after vector/keyword fusion. Rust restricts reranking to a candidate prefix (maximum 32), preserves source identities, and propagates failure instead of silent fallback.
- Tests include five-plugin discovery, model-specific asset plans, invalid models, missing weights, CPU/DML negative behavior, mock ASR inference, LM scoring, misconfigured reranker, and reordering actual Timeline memory selections after retrieval fusion.

## Tests performed

- `PYTHONPATH=python python3 -m unittest discover -s python/tests -q`: **33 PASS**.
- `cargo test --locked --offline --no-default-features -q`: **294 PASS, 2 ignored**, 0 failed (sequential run).
- `cargo test --locked --offline -q`: **350 PASS, 2 ignored**, 0 failed (sequential run).
- Clippy `--all-targets -- -D warnings`: **PASS** in both feature configurations.
- `cargo fmt --all -- --check`: **PASS**.

The earlier simultaneous Cargo runs generated an OS `Text file busy` failure in an existing launcher test, which passed when re-run alone; both full suites subsequently passed **sequentially**.

## Not yet complete (do not close broad issues)

1. **No real model readiness proof.** Weights were not installed in this environment, so none of the five ASR families or either local reranker was validated on real weights/audio. ABI, CUDA/DirectML and Optimum compatibility also require target-OS tests. Metadata/discovery/mock tests are not model inference benchmarks.
2. **Installables.** Actual checkpoint downloads need trustworthy pinning, full hashes and approval. GigaAM ONNX pre-export from `.ckpt` and publisher verification remain outstanding. Managed uv/CPython, resolved package locks, activation and rollback are not yet one complete production installer for these new families.
3. **Speech.** Real microphone devices, hotplug, PTT, native VAD/segmentation, silence gates, audio streaming and Unity speaking leases are not integrated end-to-end with the Rust ASR client.
4. **RAG parity.** Rust semantic/lexical retrieval and reranking are implemented for character memory, but full Python memory indexing, other RAG knowledge sources, query expansion, cross-source grouping/filters and evaluation are still unproven or missing. The configured reranker works only when a matching worker is explicitly selected and ready.
5. **TTS and UI.** Twelve existing TTS adapters still lack real per-model synthesis verification. Slint AI Hub, user-facing installation controls, model switching/readiness and full audio playback are not completed in this increment.

The acceptance target remains a real `uv bootstrap → install → verify assets → activate → transcribe/embed/rerank/synthesize → cancel/shutdown` run on supported CPU/GPU hardware. Keep issues #24/#25/#26/#37/#42/#51/#59/#60/#61 open until their full acceptance criteria are met.
