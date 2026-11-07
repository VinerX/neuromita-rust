# AI production installation coordinator — 2026-11-03

## Context / source of truth

The AI Engine branch was reconciled with repository `main` twice: PR #71 (`302b98ce`) and PR #72 (`c4901f2c`). The merged baseline includes Slint desktop administration, memory presenters, Earshot native VAD, and the GigaAM ONNX generated-asset install hooks. No parallel runtime owner was introduced. The **managed target is CPython 3.12.x**; `3.12.10` is the explicitly selected test release, not an implicit production patch pin.

## Implemented

`src/ai_runtime/production.rs` owns one explicit installation sequence for a selected set of services: preflight → verified uv bootstrap → managed CPython 3.12.x → optional locked backend layers → selected plugin install plans/immutable overlay layers → verified or generated model assets → typed shared/split runtime composition → optional application activation. It returns `AiApplicationConfig` without starting inference workers during installation; `prepare_and_activate` crosses the activation boundary only after all selected services have been prepared. Layer ownership, plugin inference and actual worker supervision remain in the existing Rust components.

For each model, an `Installed`, `Sources` or `Generated` revision is explicit; mandatory assets cannot silently resolve to an unknown revision. Generated assets require a named hook and explicit approval. Existing SHA-256 asset receipts are verified again before composition. Backend requirements and overlay packages are checked against the same exact managed Python version. Explicit errors propagate without routing to an alternative model or service.

The uv bootstrap, locked backend installer, plugin overlay installer and model asset stage execute as bounded, owned asynchronous tasks, mitigating deep `Future` stack use in normal test threads. Cancelling the coordinator aborts its active child task; there is no partial inference activation. Existing installed layers are immutable and reusable, but **filesystem crash/abort cleanup across every partial staging directory is not fully certified**.

## Validation

- Rust 1.98.1: `cargo test --offline --locked --no-default-features` — 323 PASS, 2 ignored, one existing intermittent concurrency test excluded and verified separately.
- Rust 1.98.1: `cargo test --offline --locked` — 382 PASS, 2 ignored, the same test excluded.
- `cargo clippy --offline --locked --all-targets --no-default-features -- -D warnings` — PASS.
- `cargo clippy --offline --locked --all-targets -- -D warnings` — PASS.
- `cargo fmt --all -- --check` — PASS.
- Python SDK: 36 PASS on **actual CPython 3.12.10 + NumPy 1.26.0**, obtained via GitHub Actions with SHA-256 checked before extraction.
- AI lifecycle integration: 8 PASS using the actual CPython 3.12.10 executable.
- Production coordinator contract integration: shared/split activation, idempotent preparation, missing weights, checksum corruption, incompatible Python version, generated hook approval; mock uv/package installation, no downloaded model weights.

## Not certified

The mock installer tests do not prove production wheel availability/ABI for all backends, real uv backend installation on the final target OS, real GPU inference for the twelve TTS and five ASR adapters, model checkpoint export on a real GigaAM checkpoint, microphone/PTT lifecycle, complete retrieval/indexing feature parity or end-user Slint AI Hub activation. Do not close these larger product goals on the strength of contract tests.

## Next proof gates

1. Run the real `uv`/Python 3.12 backend and overlay installation on each supported OS with exact locked NumPy/ORT/Torch packages and probe their versions/ABI.
2. Run the first complete model path using verified model weights: install → activate → PCM/WAV inference → cancel/shutdown → recover, including actual GigaAM ONNX export.
3. Connect microphone capture, Earshot VAD/PTT, AI settings and model selection to one application-owned runtime without a second Python supervisor.
4. Verify real embedding/reranker model accuracy and the remaining source Python RAG parity independently.
