# VAD evaluation dependency proposal (not yet applied)

No Cargo.toml, Cargo.lock, .cargo/config.toml or vendor changes are included in the first domain increment.

Spike status (2026-11-03): the delta was resolved and built in the separate
`codex/spike-vad-deps` worktree; see `issue-60-dependency-spike-20261103.md` for
measured results. The production manifest, lock and vendor remain unchanged.

Proposed root manifest delta:

```toml
[features]
# Existing default remains ["sqlite-system"].
vad-wavekat = ["dep:wavekat-vad"]
vad-silero = ["vad-wavekat", "wavekat-vad/silero", "dep:ort"]

[dependencies]
wavekat-vad = { version = "=0.1.17", optional = true, default-features = false, features = ["earshot"] }
ort = { version = "=2.0.0-rc.12", optional = true, default-features = false, features = ["std", "ndarray", "load-dynamic", "api-24"] }
```

The ordinary default and no-default builds retain direct Earshot and activate neither new optional crate. `vad-wavekat` activates only the pure Rust Earshot backend; no WebRTC, Silero, TEN-VAD or FireRed. Wavekat's released registry metadata reports checksum `2294cef58ceef4ff323bb282ea23be7f5154077419ec453bc12427a6e0c79c73`, archive size 91,522 bytes, Apache-2.0, with no declared rust-version. Its nonoptional dependencies are rubato ^0.16 and thiserror ^2; Earshot is ^1.2.1 (existing lock has 1.2.2). Rubato and its FFT transitive closure are an unavoidable upstream addition even for Earshot-only evaluation. No resampling feature is needed by our 16 kHz test harness.

Silero additionally activates wavekat's ndarray ^0.17, ort ^2.0.0-rc.12 (upstream default features enabled) and build ureq ^3. The direct exact ort pin unifies `load-dynamic`; ort-sys rc.12's build/main.rs returns before binary resolution/download whenever `disable-linking` is active, and ort/load-dynamic activates that flag. This must also be checked against the released vendored bytes and the resolved feature graph. Cargo feature unification does not remove upstream ort defaults: download-binaries remains in the feature graph, although the disable-linking early return prevents executing it. ORT rust-version is 1.88, below this application's 1.89 requirement; resolved transitive versions must also respect 1.89. No lowering of the project's Rust requirement and no hand patch of vendor is proposed.

Before Cargo resolution or vendoring, parent reviews this direct feature/dependency delta. Exact transitive versions, package count, rust-version and archive/unpacked sizes cannot honestly be specified until resolution; next authorized operation should be resolution only, with the existing lock preserved for unrelated packages and no build. Report that resolved delta before adding vendor sources. Keep a conservative 30 MB ceiling for new crate archives plus unpacked sources and stop if exceeded; no ORT binary distribution or speech corpus is part of that budget. Check disk free space first. Existing vendor bytes/checksums/licenses must remain intact.

## No implicit model or DLL acquisition

For Silero builds, add a non-forced Cargo environment default for `SILERO_MODEL_PATH` pointing to an explicitly absent local asset path under assets/vad. Upstream build.rs sees that variable and fails if the file is absent, instead of downloading its default v6.2.1 model. The operator explicitly overrides this variable to the verified absolute v6.2.3 path. Verify hash and size before invoking Cargo and again at runtime. This configuration has no effect on default/Earshot-only builds. Never set DOCS_RS to bypass asset validation.

Pinned local model: official snakers4/silero-vad commit `5cd7945676eb32225748052e2e6a0580e4686a08`, `src/silero_vad/data/silero_vad.onnx`, v6.2.3, MIT, 2,327,524 bytes, SHA-256 `1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3` (James's research; local file/inference not yet verified). Constructor takes verified in-memory bytes, avoiding a second model file open after verification. No embedded/default `SileroVad::new` path is used. The benchmark application explicitly initializes ORT from an absolute local DLL path before creating a session; no DLL discovery or downloading. Actual DLL availability/version still needs targeted inventory. CPU only; no GPU acceptance claimed.

## Evaluation fixture manifest contract

Each selected fixture needs a stable id, RU/EN/noise category, source URL and revision/clip id, author/license/attribution, original SHA-256 and size, conversion command, converted PCM16 mono 16 kHz WAV SHA-256 and size, and train/calibration versus held-out evaluation role. Paths are local and bounded. No downloads happen when reading a manifest. James is selecting small FLEURS and ESC clips; no acquisition is authorized by this document alone.

Speech reference intervals must be manually reviewed in integer sample offsets with annotator/method metadata. A transcript is not a boundary annotation. ESC noise must be listened to before asserting absence of speech. Missing annotations suppress accuracy/boundary metrics explicitly; raw score traces and processing timing remain available. Fixed thresholds are declared before running held-out clips; do not tune on the reported evaluation subset.

## Ticket evidence scope

Utterance UUID identifies one queued capture segment. Once-only admission applies to that ticket, not acoustic deduplication across different tickets or global transcript deduplication. A repeated PCM capture with a new ticket is a separate utterance. Generation invalidation rejects queued/late work; it does not retroactively undo admission that completed before reset acquired the short synchronous admission lock.
