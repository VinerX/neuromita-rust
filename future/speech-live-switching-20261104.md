# Speech runtime: live selection switching

## Ownership

`ApplicationLifecycle` owns the microphone session. `ApplicationOwner` owns the active installed-speech runtime and its settings watcher. `LiveSpeechRuntime` is the sole coordinator of currently active TTS/ASR workers. The existing `SettingsOwner` remains the only source of persisted configuration; `RunningAiApplication` / `AiSupervisor` remain responsible for Python worker processes and IPC.

`LiveSpeechVoice` and `LiveSpeechTranscriber` are stable adapters provided to message handlers and the speech pipeline at application startup. They read the currently committed runtime, so changing models does not require rebuilding the message handlers or restarting Core. Both hold a read guard while inference runs; publication waits until those requests finish.

## Reconfiguration

1. Observe `SettingsOwner` revisions. A selection key contains the active local TTS model, language, model-specific overrides, and `local_auto_load`, plus the ASR recognizer, its language and overrides. Editing microphone, VAD or unrelated model options does not activate workers.
2. When the selection changes, resolve only previously installed Python, plugin, backend and overlay layers using `InstalledSpeechEnvironment`. No installation, downloads, fallback, or heuristic backend substitution.
3. Validate effective model settings through the existing Python SDK and initialize all required workers in a *candidate* `AiApplicationConfig`. Keep the old runtime, its voice provider, and speech capture usable during this stage.
4. Before publication, compare the current settings selection against the candidate. Retire superseded candidates. Synchronize publication with the microphone controller; stop an active ASR capture only if its ASR selection changes.
5. Publish the new runtime atomically, update the existing presenter `watch` status, then stop the displaced workers. Failures before publication retain the old active model and capture; errors appear in runtime status.
6. Application shutdown cancels the settings watcher, drains active message work, then releases the active workers.

## Guarantees and limits

- The latest *saved* model selection wins at commit time; an in-progress candidate with stale settings does not replace a newer choice.
- A new selection may be enabled after the application initially started with no installed models.
- Installed state is not confused with initialized/active state. An absent model reports a failed candidate, never a fake `ModelReady`.
- The watcher runs when `with_installed_speech_runtime` is selected. Explicit hardware plans remain controlled by their original activation contract.
- For now, a committed selection reactivates the complete split speech plan, including an unchanged service. In-flight inference is drained before publication; an unchanged ASR selection does not forcibly stop microphone capture, but its worker is replaced atomically with the plan. A later optimization can replace only the changed service using the same transactional boundaries.
- Updating install receipts on disk without changing settings does not automatically activate a model; the installation workflow must explicitly trigger a refresh or a later selection change.
- No real microphone, GPU, or production inference was used in the code-level checks for this change.
