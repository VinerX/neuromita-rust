# Preset factory, import and provider preparation

Working branch: `codex/preset-sandbox-slice-20261104`, based on main `095a4e71`.

## Implementation checklist

- [x] Review and merge speech runtime observation (`59286195`).
- [x] Move its tests below implementation to satisfy Clippy.
- [x] Core-owned Gemini factory with explicit model and credential selection.
- [x] Factory → persisted preset → active selection → ConversationPresenter → HTTP fixture.
- [x] Same route tested against real Gemini: `gemini-3.6-flash`, answer `Готово.`.
- [x] Explicit environment/local credential references, no secret-source fallback.
- [x] Ordered typed text transforms, based on Python `message_transforms.py`.
- [x] Gemini configurable sampling, thinking and structured-output options.
- [x] Stage supported Python presets and normalized template inventory, with explicit blockers.
- [x] HTTP profile follow-up implements OpenRouter/OpenAI-compatible/Responses; see the separate follow-up report.
- [ ] ChatGPT OAuth/account and forced-streaming contract.
- [x] Connect imported credentials to normal persistent bootstrap; live Main check passed.
- [x] Preserve provider preparation options during GUI connection edits.
- [x] Verify final default/GUI tests, Clippy, formatting and GUI build.
- [x] Publish reviewed Gemini changes through [PR #85](https://github.com/Atm4x/neuromita-rust/pull/85), merge `e8a199eb`.

## Contracts

Transforms prepare the outgoing prompt after subjective context assembly and before
wire encoding. They do not append Timeline events or mutate character history.
The diagnostic prompt must describe the transformed data actually sent.
Unknown transform identifiers and unknown parameters are errors.

Seven text transformations are compared against 28 independent Python-generated
edge cases. Gemini encoding separately tests that its payload and diagnostic
prompt agree after transformations and that disabled parameters are omitted.
Gemma's system-instruction restriction is captured explicitly during import,
not inferred again while the request is executing.

The supplied settings stage four Gemini presets (IDs 14, 15, 17, 18). IDs 16,
19, 20 currently have explicit unsupported-protocol blockers. Thirteen normalized
template descriptors are retained as inventory; they are not all executable
templates. This is not a claim that the complete multi-provider part of point 2
is finished.

The present prompt contract is text-only. These transforms do not claim support
for images, files or native tool-call histories. Those require typed input parts
and codecs before they can be advertised as supported.

Credential references are `env:NAME` or `local:NAME`. Local secrets live in the
ignored application data directory, separate from the preset catalog. A missing
local secret never causes an environment lookup. Source Python files remain
read-only.

Python protocol resolution follows `ApiPresetService.get_full()` and
`_effective_protocol_id_for()`: a selected template's protocol takes priority.
Only a standalone custom preset uses its own `protocol_id`. Flash (ID 17) has
a stale OpenAI-compatible field in raw JSON but effectively uses Gemini because
its selected template is Google. An initial raw-JSON interpretation was corrected
after tracing the controller-to-resolver call chain. The importer tests this case.

## Reviewed speech descendants

Subsequent HTTP protocol work and updated remaining scope are tracked in
[HTTP preset protocols](preset-http-protocols-20261104.md). Earlier blocked
OpenRouter/Compatible imports below describe the PR #85 checkpoint.

`speech-runtime-desktop` (`0b3de4bd`), `speech-session-controller` (`198074b5`),
`speech-installed-autostart` (`8e5c409f`) and `speech-live-reconfiguration`
(`7e27b45a`) are not merged by this work yet. The chain contains a silent fallback
from installed SDK/plugin paths to source-tree paths and a hidden ten-second
capture clamp. Resolve these contracts before taking the chain into main.

## Validation notes

The imported Main preset was checked through `ApplicationConfig::persistent_unconfigured`
and `ConversationPresenter`, with secrets resolved from the local store. It returned
`готово` in the explicit ignored live test. The four presets and active ID 18 are
installed in the normal application data directory; previous settings were backed
up outside the repository. No runtime migration/alias was added.

Current default test run: 525 passed. Focused GUI/localization: 16 passed.
Both default and GUI all-target Clippy runs passed with `-D warnings`;
`cargo fmt --all -- --check` and the offline GUI build passed. All Cargo
commands used `--locked --offline`. The real imported-Main test separately
passed (one explicit provider request). Executable: `target/debug/neuromita-gui.exe`.

Before the credential/transform changes, 516 default tests and 16 focused
GUI/localization tests passed; both Clippy configurations and GUI build passed.
These results do not substitute for checks of the final tree.

The newly fetched `tts-asr-python-parity` head `9573deb2` extends the deferred
speech chain with shared language catalogs and GUI draft dispatch. It does not
remove the installed-source fallback or hidden capture clamp, so it is not merged
wholesale during this preset work.
