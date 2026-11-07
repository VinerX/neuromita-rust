# Speech runtime: application binding and desktop observation

Base: `codex/speech-runtime-observation-20261104` (`708484dba`; GitHub tree-equivalent `59286195`).

## Application boundary

`ApplicationConfig::with_prepared_speech_runtime(plan, inventory)` attaches an **existing prepared** AI worker plan to the application. `ApplicationLifecycle::start` loads the persisted `SpeechSettings` from the canonical `SettingsOwner`, then calls `AiApplicationConfig::with_speech_settings` before any AI worker activation. That operation checks selected TTS/ASR plugin identity, asset revision, dependency layers and plugin settings against the passed catalog; a mismatch fails startup. No implicit installation, model fallback or new worker plan is created.

Other existing callers that populate `ApplicationConfig.ai_runtime` directly without an inventory retain their legacy explicit plan behavior; migration to the stricter builder is opt-in. The desktop launcher still does **not** invent a plan from model names, and starts with `ai_runtime: None` until the application has been provided with a prepared plan. Selecting a model in the settings page does not start any worker.

## Observation boundary

`RuntimeStatePresenter` now holds a weak, non-owning reference to `AiSupervisor` and supports an explicit asynchronous `refresh_status()` to detect child-process exits without requesting inference. The same supervisor-owned `watch` channel continues to deliver lifecycle snapshots. Desktop observes that channel, publishes it to Slint and rechecks process liveness every five seconds. No competing runtime state cache exists in the Presenter.

The TTS page displays installation/inventory state separately from the actual worker status. The ASR page displays the worker state and explicitly says when microphone capture is not running. `runtime-ready` remains false until a genuine, separate speech capture/session control has been connected; `ModelReady` alone is insufficient to unlock a microphone toggle. On application shutdown both views reset to stopped state.

## Verification and limits

Code-only checks: `cargo check --locked --offline --lib`, `cargo check --locked --offline --features gui --bin neuromita-gui`, `cargo fmt --all --check`, `git diff --check`. No real microphone, GPU, Python model inference or third-party API has been exercised. The separate executable startup path still needs an explicitly prepared production plan to attach to the `ApplicationConfig`; dynamic speech session start/stop is a separate future change.
