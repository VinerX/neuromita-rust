# Review of updated branches and PRs, 2026-11-04

Base: `main` at `15710db73adea787e9ddb23a3ea2550ded96d260`.
Integration branch: `codex/review-latest-20261103`.
This review supersedes the earlier blockers for PRs 67, 68 and 70 in
[the previous review](integration-review-20261103.md).

## Included candidates

| PR | Reviewed head | What it provides | Architecture decision |
| --- | --- | --- | --- |
| [67](https://github.com/Atm4x/neuromita-rust/pull/67) | `c7c8c34f330d8c69713ce51b1c7b101afdf5e8cd` | Provider request/response diagnostics, scoped and redacted trace export; opt-in dataset capture, ratings, retention and JSONL/ShareGPT/ChatML export. | The previous blocking storage path is gone. Generation only admits records to a bounded queue. A separately owned writer performs file operations; late errors and losses are observable. Sample admission is not a durability receipt. Shutdown has a bounded drain and does not await a stuck OS write. |
| [68](https://github.com/Atm4x/neuromita-rust/pull/68) | `0286b095c638d4db9b574bf7c99ad86ef6f78716` | Sea Battle rules, typed user/model commands, role-private boards and automatic/manual reactions; historical reads, fork and SQLite recovery. | A transient owner working copy is reconstructed from canonical private Timeline facts. Public facts and role projections do not expose hidden fleets. Reply, game, memory, state and timer effects share guarded acceptance. |
| [70](https://github.com/Atm4x/neuromita-rust/pull/70) | `585a1391cb162b8b9d95c5ddc12436126506a642` | Explicit backend/device/precision selection, inventory and execution checks, startup application and guarded live worker replacement. | The old second-runtime design is gone. HardwareRuntime borrows RunningAiApplication and its existing supervisor. Candidates must confirm actual execution/model profile before publication. Failed or superseded switches preserve the previous worker, including shared-worker neighbor profiles. |

## Integration corrections

- Reconciled application configuration, imports, CLI help and dispatch so diagnostics,
  datasets, games and hardware commands coexist.
- The diagnostics fixture now includes the optional game reaction field added by PR 68.
- The GUI diagnostics adapter uses the new redacted trace export contract; its former
  `context.anchor` access no longer compiles against the compact diagnostics context.
- A game change during generation now rejects the whole proposal even when the response
  contains no game commands. Preparation captures the current game guard separately from
  the subjective historical game position. Deliberate historical reads remain available;
  a model game mutation must additionally match the subjective game position.
- GameApi captures one set of Core bindings for the owner command and its subsequent
  reaction admission. It does not resolve a potentially different global Core after await.
- Dataset capture consumes normalized `ProviderReply.answer_text`. Only the Gemini
  adapter extracts text from its wire JSON and excludes thought parts. The collector
  preserves raw bodies without parsing provider-specific response structures.
- Fixed a flaky diagnostics ownership test: matching `91,` in a complete Debug dump
  also matched timing values. It now checks that the trace retains no reference to
  the response's audio bytes, directly testing the intended ownership guarantee.

## Left separate

| Branch or PR | Reviewed revision | Purpose and reason for leaving separate |
| --- | --- | --- |
| [76](https://github.com/Atm4x/neuromita-rust/pull/76) | `1f3b89906cd7d885391d7c30fbdf00bab60479a0` | Deferred chess research/plan. Draft explicitly contains no executable implementation or acceptance evidence. |
| [77](https://github.com/Atm4x/neuromita-rust/pull/77) | `dc888e4aaecfba8d42ce3db6adf3b2f41775a4ba` | Stream tickets, stale-ASR admission protection, VAD framing and acceptance work. Author keeps it draft: acoustic A/B, optional Silero runtime and remaining lifecycle/device acceptance are pending. Do not merge the in-progress branch or claim completion of issue 60. |
| `codex/local-control-launcher-parity-20261103` | `9632460d` | Home Play/Stop and Open Folder are connected to LauncherSession. The Slint callback directly owns and operates the session: discovery/spawn and blocking child wait run on the UI thread. Move commands and ownership into a functional presenter/lifecycle, publish status to the UI, and verify responsiveness before integration. |
| `codex/api-presets-pyqt-slint-parity-20261103` | `e47c4497` | PyQt-style provider/preset workspace, typed CRUD and parameter rows. Includes the unfinished `ae046a2b` character checkpoint with a second GUI catalog and concrete dev-schema/ID compatibility that the user rejected in issue 75. Parameter titles/descriptions also duplicate RU/EN strings in PresetsPresenter and select locale inside Slint instead of using the shared JSON localization contract. `timeoutMs` is marked as a sent provider parameter although it is an HTTP transport deadline. These need reconciliation before merging the full branch. |
| `codex/gui-kit-recovered-20261103` | `7dfca650` | Reusable Qt-style controls and General preferences; appeared during the final fetch. Based on the same unfinished character checkpoint. `InterfaceSettings.visible_sections: [bool; 12]` gives the Core Slint page indices instead of functional typed preferences, contrary to the GUI/TUI-independent presenter contract. The GUI also edits the entire array by get-then-set, so independent concurrent changes can be lost; a typed owner command must update one preference atomically. New fields have serde defaults preserving an early dev document implicitly. Reconcile these contracts and issue 75 before merging. |
| `codex/character-catalog-scroll-20261103` | `ae046a2b` | Saved nine-character/accordion checkpoint. It remains WIP; this integration deliberately does not publish its incomplete catalog or compatibility layers into main. |
| Recent `bridge/*` branches | `3833a97b`, `de65da79`, `2e7a619c` | Source export/import workflows for web-agent transfer. They are transport scaffolding, not additional product features. The application head published by the API bridge is reviewed above. |

The source heads above are pinned review evidence. New commits require another review.
Issue [75](https://github.com/Atm4x/neuromita-rust/issues/75) remains open: this integration
does not claim to remove concrete early-development compatibility or finish the character
catalog correction.

## Validation

Local Windows validation from the repository root, using the locked vendor sources:

| Check | Result |
| --- | --- |
| Default full Rust suite, serial | 500 passed, 0 failed, 2 ignored |
| Full Rust suite with GUI, serial | 501 passed, 0 failed, 2 ignored |
| Full Rust suite without default features, serial | 438 passed, 0 failed, 2 ignored |
| Python SDK tests, Python 3.12.9 with `PYTHONPATH=python` | 81 passed |
| All-target Clippy, default / GUI / no-default, warnings denied | All passed |
| `cargo fmt --all -- --check`, `git diff --check` | Passed |
| Offline GUI-enabled binaries build | Passed |

The GUI full suite checked the final production changes at `46f7bfe2`. The subsequent
`0d9344cc` changes only the flaky test assertion described above; the complete default
and no-default suites and all three Clippy configurations include that correction.

An earlier no-default run received one Windows `AccessDenied` while replacing the
temporary fixture's `finetune.json`. The isolated test and complete repeated no-default
suite passed unchanged. Its external cause was not established; no permission-error
suppression, retry loop, or timeout increase was added. The failure remains recorded in
`target/review-latest-no-default.log`; final results are in the `*-green.log` files.

Vendor/dependencies are unchanged. No paid provider calls, model installations,
physical GPU inference, microphone capture, real Unity launch or native GUI interaction
were performed. GUI compilation/tests do not certify screenshot or DPI parity.
