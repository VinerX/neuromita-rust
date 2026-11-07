# Rust core handoff

Read HANDOFF.md, then future/implementation-checklist.md and future/minimal-contracts.md.

Work from this repository root so Cargo loads .cargo/config.toml and uses vendor/. Run cargo test --locked --offline, cargo clippy --locked --offline --all-targets -- -D warnings, and cargo fmt --all -- --check.

This is a fresh Rust implementation. Python/Unity sources are read-only context. Follow the user's architecture: static *Api, explicit typed inputs, one core per process, owned CharacterHandler instances, subjective context from the timeline, effects committed before Finished, independent presenters and typed bindings. Do not add silent preset/config fallbacks or a service locator.

This standalone repository has a local main branch. The user will create the remote repository; do not create or configure a remote unless asked. The original Python audit is in docs/ and architecture contracts are in future/. References to audit-docs/rust or codex/rust-core-contracts in historical documents describe the source repository, not this working directory.

Vendor files are third-party sources; preserve their bytes/checksums and licenses. Update dependencies with Cargo and regenerate vendor intentionally. Do not commit target/, credentials, local settings or build archives. Provider tests use local fixtures; the ignored live test needs explicit environment variables and makes a real API call.

## Core-owned data and presenter binding

Production character definitions, canonical IDs, ordering, preset configuration and real app data MUST be owned by Core. Presenters supply typed read models to Slint. GUI components MUST NOT hardcode independent domain catalogs, default generation settings, mock user data, or copy business rules. The frontend may map a stable ID to a visual asset and apply localization/theme, but cannot determine which production domain objects exist. Keep the one authoritative current Rust dev settings format: do not add historical dev migrations, ID aliases, implicit fallback or destructive overwrite of incompatible files.

## Voice GUI parity

The Telegram, Local TTS and API Voice settings tabs are Slint *views*. They must not embed provider credentials, model catalogs, client settings or per-character voice identifiers. Feed provider/model/voice data from an explicit presenter read model and Core-owned character catalog. A screenshot-only CLI `--voice-preview` may inject non-production examples through `src/bin/neuromita-gui/voice.rs`; preview values are never persisted and must not be mistaken for an operational TTS service. Keep backend commands disabled until a concrete service adapter exists.

## ASR GUI parity

ASR settings must follow the Python microphone_settings layout using nested responsive Slint layouts. Never use absolute coordinates for the ASR page or hardcode actual recognition models, devices, audio backends, microphone readings or persisted settings in GUI components. Runtime data must originate from a typed presenter read model; until the ASR runtime adapter exists, backend commands must remain disabled. The explicit `--asr-preview` mode in `src/bin/neuromita-gui/asr.rs` supplies non-persistent demonstration values only.
