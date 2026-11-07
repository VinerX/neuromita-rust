# Preset selection at generation execution

HTTP follow-up: [provider profiles and remaining scope](../../future/preset-http-protocols-20261104.md).
Core now also supports typed Chat Completions and Responses templates, with
Compatible, Mistral, OpenRouter, Ai.iO and LM Studio preparation profiles.
`PresetGenerationEdit` explicitly identifies the protocol and authentication
choice. `None` is an explicit no-auth request for these transports; Gemini still
requires a credential. Actual protocol/dialect accompanies provider observations.
The supported non-Gemini imports supersede the earlier blocker list below;
ChatGPT Plan/OAuth remains separate and unavailable.

## Current connection and preparation contract (2026-11-04)

Current persisted Gemini definitions use a typed `credential` reference, required
`options`, and an ordered `transforms` list. References explicitly choose
`env:NAME` or `local:NAME`; local values are resolved from the application data
directory's `preset-credentials.json`, never from a second source on failure.
The older `credential_env` description below records the original implementation.
It is not a second accepted dev storage format.

`PresetTemplate` supplies Core defaults; configuring it requires explicit model
and credential inputs. `PresetConnectionEdit` changes connection/editor fields
atomically and preserves existing preparation options and transforms. The GUI
cannot accidentally clear an imported pipeline when saving a model or URL edit.

Transforms execute after subjective context assembly and before provider wire
encoding. They never rewrite canonical history. Diagnostics and dataset capture
observe the transformed prompt and exact payload. Omitted temperature/token
parameters remain `None` in diagnostics and dataset metadata. JSON output can be
requested through native schema configuration or an explicit prompt instruction;
the application response validator remains mandatory in both modes.

`examples/import_python_presets.rs` performs an explicit staged conversion to a
**new** output directory. It never rewrites Python settings or performs a startup
migration. Gemini imports preserve enabled settings, thinking configuration and
structured-output support. Selected Python template protocol wins over stale raw
preset `protocol_id`, matching `ApiPresetService.get_full()`.

Other wire protocols, reserve-key routing and unsupported overrides are reported
as blockers rather than silently discarded. The normalized template descriptors
are an import inventory, not a claim that all template backends can execute.
The current GUI does not edit the full ordered transform/options contract yet;
existing imported settings are preserved.

Text transform parity is checked against 28 fixtures generated from the read-only
Python `src/handlers/llm_providers/message_transforms.py`, covering seven helpers.
Multimodal parts and native tool histories are outside the present text contract.

## Invariant

`ChatApi::reply`, `ChatApi::react`, and `ChatApi::initiate` express *what to do*, never a provider, model, preset or transport configuration. A pending `GenerationCommand` stores only character, operation, target/instruction, source and audience. It does **not** resolve or retain a preset at admission.

At exactly one point, **after dequeue and immediately before beginning LLM generation**, the generation executor reads the currently configured preset assignment for `(CharacterId, operation)` and the active preset ID. It resolves the ID against the configured preset catalog, **materializes/clones a fully owned `GenerationPreset`**, then supplies that value to `GenerationRequest::new(context, preset, prepared_knowledge)`. No additional preset settings reads or re-resolution occur for that attempt, including retries.

- Queued commands pick up edited assignments and active presets on their eventual execution.
- A running command retains the preset value selected at its start even if assignments or preset definitions change later.
- Different operations (`Reply`, `React`, `Initiate`) can have different assigned preset IDs for the same character.
- `Active` is a tagged selection, not a magic integer ID. A specified but missing ID is an explicit failure; never silently fall back.
- `Character` and `SessionTimeline` never own or choose presets; this is application configuration and executor policy.
- Built-in character assignment defaults use an exhaustive `match` on `CharacterId`, so adding a new variant requires compiler-visible maintenance.
- No additional version reconciliation, background synchronization or per-request settings subscriptions are required for this selection rule.

## Scope of initial implementation

The existing CLI-configured Gemini preset seeds the catalog's default ID. `AppSettings.presets.active` owns the active ID, while `AppSettings.characters.<character>.presets` stores per-operation selections. These are independent typed settings sections; preset definitions remain in the separate `PresetCatalog`. The serialized settings schema v2 migrates older `presets.characters` assignments without changing their values. Preset catalog CRUD, storage of provider credentials, alternate protocols and UI editing remain part of Issue #16; they must not be simulated by unsafe plaintext credential persistence or a second ChatApi entry point. This contract also does not alter Unity transport or structured response.

## Editable catalog (2026-11-02)

A preset *definition* is a standalone user configuration, not a field in `AppSettings`. `PresetRecord` owns a name and validated provider definition; `PresetCatalog` owns stable IDs and `create/update/delete/list`. The optional `PresetRepository` stores a versioned catalog JSON separate from application settings, and the JSON repository writes via a temporary file and rename. Failed persistence does not publish an in-memory edit. IDs are not reused after deletion.

Gemini definitions store the **name** of an environment variable (`credential_env`) rather than an API key. Credentials are resolved only when materializing the preset for a queued generation. An absent credential fails explicitly; it never becomes a fake request or an implicit fallback to another preset. `EnvironmentPresetCredentials` is the default; an injected `PresetCredentials` allows deterministic tests. Definitions are validated at CRUD boundaries. `PresetCatalog::open(runtime, repository, credentials)` loads persistent definitions and may reserve runtime-only bootstrap entries for the current CLI; runtime bootstrap entries cannot be edited through catalog CRUD.

`PresetApi` exposes catalog operations to application clients; it does not own character assignments or call `ChatApi`. A running generation continues to hold its cloned `GenerationPreset` after a catalog edit or deletion; a pending generation sees the current catalog on its one resolve point.

**2026-11-02, persistent application bootstrap:** `ApplicationConfig::persistent` opens separate `presets.json` and `settings.json` under a configured data directory; CLI uses it instead of `minimal()`. A missing settings file bootstraps an initial profile; corrupt files and read errors fail explicitly. The runtime-only Gemini default (ID 0) remains a bootstrap preset supplied via `--model` and `GEMINI_API_KEY`. Saved user definitions are assigned stable IDs above it. The CLI directory is specified with `--data-dir` or `NEUROMITA_DATA_DIR`, otherwise platform configuration directories are used. This does not make SessionTimeline persistent (its current CLI storage remains transient).

`GuiPresenter.presets` exposes typed `list/create/update/delete/observe` of standalone preset definitions; this is distinct from `GuiPresenter.settings.presets.active` and `.settings.characters.*.presets`. Observers receive a new snapshot only after successful repository save. Deleting a referenced preset does not silently change assignments: a subsequent generation reports a missing preset until the assignment is corrected.

**Remaining #16:** actual desktop UI editor for preset definitions, credentials management beyond environment references, additional protocols and real model-discovery/connection tests. The presenter is a UI-facing binding contract, not yet a rendered editor. Catalog CRUD here is not a claim that Issue #16 is complete.
