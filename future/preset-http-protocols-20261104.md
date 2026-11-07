# HTTP preset protocols

Follow-up to merged [PR #85](https://github.com/Atm4x/neuromita-rust/pull/85).
Branch: `codex/preset-http-protocols-20261104`.

- [x] Independent typed Chat Completions and Responses codecs.
- [x] Explicit Compatible, Mistral, OpenRouter, Ai.iO, LM Studio and Responses profiles.
- [x] Shared bounded HTTP transport, system proxy behavior, redirect refusal and observations.
- [x] Core factory endpoints, explicit provider selection and provider-specific transforms.
- [x] Optional bearer authentication: `None` means explicitly no authentication.
- [x] Core/presenter template identity and request-field metadata for GUI.
- [x] Actual protocol/dialect in request observations and dataset metadata.
- [x] Six profiles checked through ConversationPresenter and local HTTP fixtures.
- [x] Reject truncated/refused replies and unsupported native tool outputs.
- [x] Final default/GUI tests, both Clippy configurations, formatting and GUI build.
- [x] Stage and install newly supported Python presets without overwriting existing edits.
- [x] Published through [PR #86](https://github.com/Atm4x/neuromita-rust/pull/86), merge `206eee8a`.

## Boundaries

Provider profiles select codecs and preparation rules; UI components do not choose
HTTP field names, provider URLs or role transformation rules. Shared HTTP code
handles transport only. Gemini retains query-key authentication. Chat/Responses
use an explicitly configured bearer credential or explicit no-auth configuration.
The GUI credential field accepts `none` for the latter, and Gemini rejects it.

All execution currently requests a non-streaming structured answer. Schema, JSON
object and prompt-only JSON modes are typed settings. No automatic downgrade or
retry with another protocol is performed on errors. Native tool-call execution,
model search, reserve-key rotation and a complete editor for advanced imported
options are outside this slice; existing advanced options survive connection edits.

The Python template protocol still wins over a stale raw preset field. OpenRouter
and standalone OpenAI-compatible presets can now be converted. ChatGPT Plan/Codex
requires its own account/OAuth/streaming contract and remains unavailable: it is
never represented as an API-key Responses preset.

The 13 normalized imported template descriptors remain an inventory. Core supplies
the executable built-in factory definitions (plus generic Compatible); arbitrary
edits to that inventory file do not dynamically reconfigure factories.

## Acceptance

HTTP fixture execution covers authorization presence/absence, Chat versus Responses
payloads, JSON output mode, model identifiers containing `/` and `:`, Mistral/Ai.iO
transforms, and acceptance through the real Presenter/Core path. Error tests cover
refusal, truncation, incomplete Responses output and unexpected native tool calls.
No live non-Gemini provider call is claimed.

Final validation: 527 default tests, 16 focused GUI/localization tests, both
all-target Clippy configurations with `-D warnings`, formatting, and the offline
GUI build passed. All Cargo commands used `--locked --offline`.
Import produced six presets with one explicit ChatGPT Plan blocker. Presets 19
and 20 were added to the local application catalog and credential store with
backups; existing presets and active selection were preserved. These local data
files and secrets are excluded from commits.

## Next checkpoint

- [ ] ChatGPT Plan/OAuth/account handling and forced streaming; preset 16 stays blocked.
- [ ] Decide the editable template-catalog contract before consuming arbitrary imported descriptors.
- [ ] Advanced per-profile parameter/transform editor and reserve routing, if needed beyond this slice.
- [ ] Review and repair the later speech chain before merging it.

The final fetch also found `codex/tts-asr-schema-forms-20261104` (`2d780ebe`),
which adds dynamic Python-schema forms over `9573deb2`. An ancestry/targeted check
confirms that its chain still contains `InstalledSpeechEnvironment::for_desktop`
falling back to the source SDK/plugin tree and the hidden ten-second capture
clamp (`.min(MAX_SPEECH_FRAMES)`). It was fetched but is not claimed fully reviewed
or merged. Keep its useful forms/language/runtime work together with explicit
installed-path handling and coherent settings/IPC/audio duration bounds, rather
than taking the chain wholesale with those unresolved contracts.
