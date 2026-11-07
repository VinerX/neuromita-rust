# Review of concurrent branches and PRs, 2026-11-03

Baseline: `origin/main` `63a565ee` (our PRs #71–73). Review branch: `codex/review-integration-20261103`. Original branch and PR heads remain available. Python 3.12, static APIs, one owned Core, character-owned state, canonical Timeline facts and functional presenters remain the acceptance criteria. Git merge conflicts are not themselves architectural defects.

## Review decisions

| Change | Purpose | Decision / remaining check |
|---|---|---|
| PR #64 `21e89095` | Correct Windows Python fixture selection | Include; test-only correction, explicit environment override retained. |
| PR #65 `51fba543` | Preserve canonical message ID when TTS fails | Include; text acceptance already occurred, receipt must retain its identity. |
| PR #66 `bd7d24df` | Retire an IPC worker when a request future is dropped | Include regression. The newer Core already has `InFlightRequest`; preserve that single guard rather than nesting two equivalent guards. |
| AI `1a0dfdfa` | Managed Python production installation, model source catalog, unified backends, PCM speech sessions and chunked WAV IPC | Include after fixing PCM read cancellation and Windows fixture portability; no live model installation claimed. |
| Lexical memory `424ff421` | Explicit Russian lexical dedup operation | Included. Explicit AddLexicallyDeduplicated preserves memory identity and highest priority; protected memories reject mutation. Ordinary Add and semantic retrieval remain independent. |
| Character GUI `ab1eca00` | Qt-style character library, accordions and inline memory | Include after fixing atomic preset assignment and confirmed split/common mode. |
| Releases/About `977e6044` | Implement the release feed and About views | Included after moving HTTP/work into an owned ReleasesLifecycle and functional ReleasesPresenter. Bounded refresh mailbox, reusable HTTP client, cancellation on shutdown; Slint is an adapter. |
| PR #67 `78a482d3` | Request diagnostics and opt-in training dataset capture/export | Keep separate as written: `MessageHandler` awaits dataset persistence before text-ready, cancellation completion and finished; store uses blocking cross-process file locking. Auxiliary storage can stall the request lifecycle and shutdown. |
| PR #68 `88b75311` | Sea Battle rules, model actions and automatic reactions | Keep separate pending contract decision: authoritative match is volatile; historical generation is rejected while a match is active. This changes the existing historical context guarantee. Atomic move preparation/privacy tests are useful, but do not resolve that limitation. |
| PR #69 `5a8771f6` | Persistent reminders/timers, timezone/DST and scheduler delivery | Included after bounding command/delivery channels (64/256). Facts remain canonical Timeline events, scheduler/monitors are lifecycle-owned, mixed effects commit atomically. Timestamp overflow now errors instead of inventing zero. |
| PR #70 `42c9a026` | Hardware/device selection, execution probe and staged activation | Keep draft: explicitly untested WIP and based on older worker/composition contracts; reconcile with the new unified backend installer before acceptance. |

## GUI responsibility

Our earlier `memory.slint` was an independent modal editor. Its CharacterHandler/CAS data boundary was valid, but the separate modal did not follow the requested character settings composition. The user's newer branch turns it into an inline `MemoryPanel` inside the character history/memory accordion, with Sandbox navigating to that section. Preserve that composition; the file's name/location does not make memory a separate owner or page presenter.

The incoming Qt branch sent three independent preset assignments from Slint and kept the split/common flag only in frontend globals. Replaced this with one typed settings edit and confirmed-assignment synchronization. A rejected/missing preset cannot partially update the three slots; the user's temporary split editing preference survives unrelated updates.

The incoming accordion hid children without releasing their intrinsic layout height, and software rendering exposed square avatar corners. Fixed collapse geometry and restored the shared circular mask. The first visual follow-up also incorrectly flattened the expanded QSS background layer; that intermediate screenshot is not the accepted UI. [Source map and current limits](settings-qt-port-20261103.md) document the resolved QSS cascade, named SettingsTheme, complete navigation structure and source-based section composition. Preset editing is under the provider section, memory remains inline. Unsupported Python operations are disabled/unavailable, not fabricated as working Rust features.

## Verification record

- Initial AI merge: Python SDK 43 passed after replacing a POSIX-only fixture path assertion with native path components.
- Interrupted PCM read regression: passed with synthetic bytes; frame data survives cancellation in the middle of a sample.
- PR #66 regression: passed against the current worker launcher after adapting the fixture to explicit model configuration.
- Combined offline Rust suites: **426 default / 427 with GUI passed**, two explicitly ignored tests each (live provider and launcher helper; the helper is exercised through launcher tests). Includes reminders acceptance/restart/DST, lexical memory, release presenter shutdown, IPC/PCM cancellation and GUI binding/layout coverage.
- Python SDK: **43 passed** under the installed Python **3.12.9**, with `PYTHONPATH=python`; no model downloads or paid API calls.
- Default and GUI all-target Clippy with warnings denied, fmt and final offline binary build passed. After the last style changes, targeted GUI/layout/localization tests passed again. The real Winit/FemtoVG snapshot example also exits successfully; [native capture](gui-reference/rust-review-20261103/settings-native.png). Native minimize/restore and exact pixel-difference certification remain unverified.

## Separate PRs: concrete follow-up

- [#67](https://github.com/Atm4x/neuromita-rust/pull/67): adds useful provider-boundary diagnostics, local training samples, ratings and export. At `78a482d3`, `src/request/handler.rs` awaits `persist_observation` before publishing text and awaits `finish_dataset_outcome` on cancellation/terminal paths. `src/finetune/store.rs::guard` uses a blocking mutex plus `File::lock`; async collector methods await the blocking writer. Storage failure is surfaced as a warning, but a stalled lock/write still prevents the request from finishing. Give the optional writer a bounded independently owned delivery path and explicit retention/drain policy before merging.
- [#68](https://github.com/Atm4x/neuromita-rust/pull/68): adds tested Sea Battle rules, private boards, typed GameApi/CLI, atomic model actions and reactions. At `88b75311`, `CharacterHandler` owns volatile `GameSlot`; canonical events contain public facts only. `context_admitted` rejects all historical context generation while a match is active. Keep the good game logic, but reconcile private authoritative state with branch/historical reads and restart before acceptance. This is a contract gap, not an objection to character ownership.
- [#70](https://github.com/Atm4x/neuromita-rust/pull/70): intends backend/device/precision selection, introspection, probe and staged apply. At `42c9a026`, `HardwareRuntime` owns another AiSupervisor/config activation path and looks in `environments/bases`, while the current production installer uses unified `backends`. Its author explicitly marks it uncompiled/untested draft. Integrate the selection with the existing production owner and run activation/rollback tests before making it ready.

Transport-only `bridge/*` branches contain export/import workflows and encoded bundles. They are provenance/delivery mechanisms, not additional application features. Do not copy temporary payloads/workflows into the product tree merely to make every ref appear merged.
