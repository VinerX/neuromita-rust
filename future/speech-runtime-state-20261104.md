# Speech runtime observation and explicit activation binding

Base: `codex/speech-model-inspection-resolution-20261104` (`c263b8c72`, tree-equivalent GitHub `43b3d2ea`).

## Runtime state ownership

`AiSupervisor` alone owns the published worker, its generation and loaded model. It now also publishes typed snapshots through a Tokio `watch` channel. The channel is a replaceable state snapshot, not a second event stream or a model inventory.

- `Stopped`: no published worker.
- `Starting`: initial worker startup or activation, with a pending model ID when applicable.
- `WorkerRunning`: a Python worker is running but no model is confirmed loaded.
- `ModelReady`: the published worker has confirmed a loaded plugin and asset revision.
- `Failed`: the published worker is retired, has exited, or the first activation failed.

An attempted reconfiguration keeps the prior published model visible as active until the replacement has fully initialized. A candidate failure retains that active model and records the error; it does not falsely claim a stopped worker. A plugin-level request failure is recorded without retiring a healthy worker. IPC corruption, timeout and process exit invalidate its active model. For Shared topology, a fatal worker exit invalidates every service of that worker generation.

`refresh_status()` is a cheap, explicit liveness check on the child process, without calling a Python plugin or loading an inference model. It is available on `AiSupervisor` and `RunningAiApplication`. No periodic polling or real hardware probe is started automatically.

`RuntimeStatePresenter` lives in Presentation, consumes the watch receiver and provides read-only `snapshot()`/`observe()`. `ApplicationLifecycle` passes the optional runtime observer to `GuiPresenter`. If the app has no configured AI runtime, the presenter exposes stopped state, not fabricated ready workers.

## Binding saved settings to a prepared plan

`AiApplicationConfig::with_speech_settings(settings, inventory)` is an explicit pre-activation operation. It resolves the saved TTS/ASR selection with Python SDK inspection and the existing model asset store, using precisely the dependency layers in the selected worker composition. It only substitutes the validated settings for the **same** plugin ID and asset revision. A different model requires preparing its environment first. All selected speech services must agree with the worker plan; there is no implicit fallback or installer action.

The application orchestrator must explicitly invoke this method before `ApplicationLifecycle::start` with an authoritative `SpeechInventory`. Desktop GUI still has no configured production AI worker plan by default; these contracts do not imply that clicking ASR/TTS settings starts a microphone or a worker. Dynamic settings schema rendering, automatic worker monitoring, and speech-session controls are independent pending integrations.

## Code-only verification

- `cargo check --locked --offline --lib`: PASS.
- Unit tests for typed presenter snapshots, active model preserved after failed replacement, process-exit liveness and strict prepared plan matching: PASS.
- No tests with a real microphone, GPU or installed third-party TTS/ASR model.
