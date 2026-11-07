# AI Engine — verified generated model assets (2026-11-03)

**Base:** `ec9c219feaaf2378952dbc43bd2ecdb8d1b28443` (`codex/ai-engine-install-voice-20261103`). Python reference: `Atm4x/NeuroMita` `5f3d061f523d9abefa6a264bb2953048a935fa65`.

## Delivered

1. **Two independent installation roles.** Existing `InstallerAction` continues to describe permitted mutations to a *package overlay* (`create_empty_marker`). New `AssetHook` declares the named *model conversion* entrypoint and its source plugin ID. These are separate contracts and the Rust Core does not contain a GigaAM-specific switch.
2. **Explicit approval and verified sources.** `ModelAssetStore::publish_generated(ModelGenerationRequest)` requires an exact approved hook ID, a hook declaration from the selected plan, installed Python and absolute runtime paths, and a verified immutable source revision. Active input files are copied into a private staging area and checked against their hashes.
3. **Isolated process role, not a second supervisor.** Rust spawns a short-lived `neuromita_ai_sdk.installer` child using a specific interpreter and selected dependency layers. The installer validates the plugin manifest and the hook declaration before importing and executing `install.py`. The installer can only publish output through the Rust caller, not mark a model ready itself. This is a subprocess boundary, **not a security sandbox** against arbitrary plugin code.
4. **Immutable publication.** Rust checks each declared output for presence, regular-file type, nonzero length, optional pinned SHA-256, and size. Symlinks and undeclared files are rejected. It computes a digest for every produced asset, writes a versioned receipt and publishes via a staging-directory rename. Existing revisions are never intentionally mutated, failed staging is removed, and both the source and target are hash-verified.
5. **GigaAM ONNX.** `gigaam_onnx` declares `export_gigaam_onnx` using the verified `gigaam` checkpoint revision as input. The Python hook calls the pinned `gigaam` implementation on CPU and exports CTC (model + YAML) or RNNT (encoder, decoder, joint + YAML), including the E2E tokenizer if required. The conversion requires the `onnx` package in addition to the existing torch/ONNXRuntime requirements. No implicit checkpoint download or worker-side export is allowed.

## Checks

- Python SDK: **36/36 PASS** (`PYTHONPATH=python python3 -m unittest discover -s python/tests -q`).
- Rust AI-runtime tests (without default-features): **33/33 PASS**, including real Python subprocess + fake exporter, source integrity checks, approval rejection, undeclared output rejection and immutable target verification.
- Full Rust test suites, Clippy and rustfmt: see current execution report; do not claim a pass unless the final run completed.

## Still open

- No real large `.ckpt` has been converted to ONNX in this environment. Actual CTC/RNNT graph exports, tokenizer paths, ORT provider compatibility and target-device latency remain unverified.
- No production orchestration from AI Hub exists yet for downloading an approved model, resolving uv/python/backend locks, deciding an asset revision, invoking conversion, activating a worker, and reporting full progress/rollback in one user operation.
- Plugin install hooks are executable Python with the user's effective OS permissions. Approval, staging, verification and digest receipts do not provide an OS-level sandbox. Packaging, provenance, hook permissions and native dependencies require further validation.
- Models, TTS 12-family inference, ASR microphones/native VAD and full RAG indexing remain separate outstanding scopes. Do not close #42, #59, #24, #25, #26 or #60 from this increment alone.
