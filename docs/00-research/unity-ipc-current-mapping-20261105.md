# Unity ↔ Python: observed wire contract and migration to GameLink (#1)

**Audit date:** 2026-11-05. **Status:** source audit, pending pinned Unity Git SHA and real client/server E2E.

## Evidence and scope

- **Unity:** user-provided `Unity.zip`, SHA-256 `381738c2d4f17bd53032ae6dc81eb1ef122962642ae6e1637b0a15f7866eb976`. Source files under `NeuroMitaScripts/Network`. The archive does not contain a `.git` repository or an authoritative branch/commit identifier. The archive checksum identifies **these bytes**, not an upstream revision.
- **Python:** `Atm4x/NeuroMita` Git bundle `sandbox-python.bundle` (verified SHA sidecar), `export/source` at **`5f3d061f523d9abefa6a264bb2953048a935fa65`**. Relevant files: `src/game_connections/server.py`, `handlers/actions/*.py`, `src/controllers/server_controller.py`.
- **Rust:** GameLink at `codex/gamelink-issue53-20261105` SHA **`b3e0ba27c1f25be4bd78e1cce1601055bbb05eb4`**, merged by PR #91 into `main` (merge `8eb024c0`). The effective protocol is documented in `docs/03-runtime/unity-game-link-integration.md` and defined by `src/game_link.rs`.
- Scope: source-visible actual command handlers and callsites. No live Unity process was launched; external Unity scripts or binary-only clients may have additional callers. No assumption that the ZIP matches an upstream commit.

## 1. Observed Unity → Python messages

The Python server uses loopback TCP with UTF-8 NDJSON and dispatches an `action` field (`server.py:197–291`). The registry is explicit (`handlers/__init__.py:11–19`). Unity serializes JSON followed by newline (`NetworkController.cs:890–901`).

| Action | Unity sender and fields | Python handler / observable effect | New GameLink operation |
|---|---|---|---|
| `hello` | `NetworkController.SendGameHello`: `client_role="game"` (`NetworkController.cs:846–853`) | `hello_ack`: `protocol_version`, connection-local `session_id`, `client_role`, `owns_player_input` (`hello.py:9–24`) | `hello`; stores returned UUID `session_id`/secret `session_token` and uses `resume` on reconnect |
| `create_task` | `NetworkController.SendCreateTaskAsync`: `type`, `character`, `sender`, `participants`, `origin_message_id`, `data`, `context`, `req_id` (`NetworkController.cs:576–615`) | `CreateTaskAction.handle` routes chat/idle/reaction/special cases; creates a task, optionally records a retry, sends `task_update` (`create_task.py:279–725`) | `publish_message` or `publish_events` as facts, `update_context` for current world state, then `reply` / `react` / `initiate` for the selected cause |
| `get_task_status` | No live sender found under supplied `NeuroMitaScripts/Network` | Python `get_task_status` returns a task dictionary by `task_uid` (`get_task_status.py:10–49`) | GameLink owns push request updates and `resume` snapshots; no periodic status RPC |
| `get_settings` | `NetworkController.SendGetSettingsAsync`: optional `context` (`NetworkController.cs:866–885`) | `loaded_settings` from controller-provided settings projection (`get_settings.py:8–18`, `server.py:97–108`) | `get_game_settings` plus unsolicited `game_settings_updated` |
| `speech_state` | `active`, `character` (`NetworkController.cs:829–842`) | `SpeechStateAction` emits a speaking-window event; also accepts optional `speech_id`/duration for multiple utterances (`speech_state.py:23–50`) | `report_playback(request_id, playback_id, phase)`; Unity maps the actual playback lifecycle |
| `asr_ptt_state` | `active`, `session_id`, `generation`, `cancelled` (`NetworkController.cs:855–868`) | Validates `session_id`, owner role and monotonic generation, emits ASR PTT state (`asr_ptt_state.py:9–25`) | `report_ptt(capture_id, generation, active, cancelled)` scoped by GameLink session |
| `get_music_beats` | `audio_path`, `track_name`, `request_id`, `context` (`NetworkController.cs:791–827`) | `music_beats_job_started`, repeated `music_beats_chunk`, then ready/error (`get_music_beats.py`) | BeatSync service under #29; not implemented in GameLink; contract/design separately required |

**Confirmed names:** Python accepts seven actions. The supplied Unity network sender visibly uses six; `get_task_status` is not found among these callers. This is a source-reachability result, not a proof that no external client uses the action.

## 2. Observed Python → Unity messages

Unity routes incoming frames by `type` (`NetworkMessageRouter.cs:8–56`). The explicit registered consumers are:

| Server `type` | Unity consumer | Semantics |
|---|---|---|
| `hello_ack` | `GameConnectionSession.cs:23–48` | Stores connection identity and `owns_player_input`. Unity resets it on disconnect. |
| `task_update` | `TaskOrchestrator.cs:27–30, 100–157` | Deserializes `body` as `TaskData`; resolves the `req_id`-associated turn, syncs UI, processes terminal status on Unity main thread. |
| `loaded_settings` | `SettingsUpdateHandler.cs:28–65` | Applies game-side settings and enabled features. |
| `asr_text` | `AsrTextHandler.cs:26–79` | Routes capture-specific or final recognized text; de-duplicates by bounded `id` cache, respects `autosend`. |
| `music_beats_job_started`, `music_beats_chunk`, `music_beats_ready`, `music_beats_error` | `MusicBeatsHandler.cs:20–29` | Streaming BeatSync progress and results. |
| `error` | `ServerErrorHandler.cs` | Displays/reports server-side rejection. |

Python pushes `task_update` with `uid`, `status` and `body=task.to_dict()` (`server.py:354–359`). The task result may include `voiceover_path`, `segments`, response protocol metadata and GameMaster control fields (`get_task_status.py`, Unity `DialogueTaskResultProcessor.cs:520–581`). New GameLink pushes `request_accepted`, `request_updated`, `request_completed`, `request_failed`, `request_cancelled` directly; the client uses `request_id` and `revision` to correlate updates. Playback completion and application acknowledgments are explicit.

## 3. Identity, causality and timing

| Dimension | Source-visible old behavior | Required Rust/Unity integration behavior |
|---|---|---|
| Connection | Python `client_id = ip:port#sequence` (`server.py:197–207`); Unity resets `SessionId` on network drop (`GameConnectionSession.cs`) | GameLink UUID `session_id` and bearer `session_token` are stable within one Core process, re-established by `resume` |
| Originating turn | Unity generates `req_id` as GUID `N` (`NetworkController.cs:588–590`) | Unity generates stable `command_id` per command and `event_id` per event; Rust supplies `request_id` for generation |
| Message identity | For game-origin chat with `req_id`, Python derives `ConversationMessageIds.incoming(req_id)` and stores a retry record (`create_task.py:196–268`) | One canonical `EventId` for the published player message; `reply` refers to that event ID |
| Task identity | Python creates `task.uid` independently (`create_task.py:208`) and pushes by `client_id` | Rust `RequestId` identifies generation; request revisions support push deduplication |
| Speaker and audience | Old `sender`, `participants`, `origin_message_id` are separate string/list fields; last participants may be reused per connection (`create_task.py:293–324`) | Character visibility is explicit in semantic events. `publish_message` currently accepts `character` (target) and `content`, so multi-speaker/participants projection needs dedicated review under #13 |
| Time | Unity `create_task` does not provide an explicit `occurred_at_unix_ms`; Python TaskService stamps task lifecycle separately | Unity supplies UTC Unix milliseconds for semantic events and immutable revision for world snapshots; transport command time stays technical |
| World state | Unity sends a `context` snapshot, built on the main Unity thread (`NetworkController.cs:618–`); Python ingests task context | Unity sends `update_context` with monotonic `revision`, `scene`, `world`, `capabilities`; the origin snapshot is captured when generation is admitted |

Treat **world fact**, **character observation**, **generation trigger**, and **TCP acknowledgment** as separate categories. The canonical Timeline owns semantic facts and their witness list. `GameRuntimeContext` owns the currently valid transient world snapshot; `GameLink` owns its connection/session acknowledgments.

## 4. Delivery and recovery actually supported

- **Old Python:** `TaskService.create_task` accepts an application task. Python stores a retry record for certain game-player chat inputs (`create_task.py:196–270`), not for arbitrary commands. `task_update` is sent to an active connection; `send_json` success means the writer accepted/drained the frame, not that Unity applied it (`server.py:354–375`). The Unity turn coordinator fails a waiting turn when the connection drops (`DialogueTaskWorkflow.cs:526–539`). This does not demonstrate durable exactly-once delivery of every task.
- **New Rust:** GameLink push follows request state. Reusing the same `command_id` within retained session receipts produces a replay rather than a second generation; conflicting payload is rejected. `resume` re-sends current request snapshots. `acknowledge_result` records application outcome. The session and receipts are in-memory and expire after disconnection TTL or Core shutdown. Restart-recovery of technical states belongs to #54.
- **Unity implementation:** retain `session_id` and `session_token` through transient TCP loss; order events with `revision`; persist/correlate player EventIds according to the client lifecycle; process each terminal result once, and send `acknowledge_result` after application. A new `hello` is required after `session_denied`.

## 5. Concrete migration scope and acceptance matrix

| Gate | Required consumer change | Verification |
|---|---|---|
| A. Session | New `hello`/`resume`, secret storage for session token, disconnect handling | Reconnect while generation runs; replay terminal status; old socket cannot mutate resumed session |
| B. Timeline ingress | Typed `publish_message`/`publish_events`, speaker/observer resolution, explicit causality | Private witness; late disclosure; duplicate command; no LLM required for publication |
| C. Generation | `reply/react/initiate` with canonical EventIds and `request_id`, push results | One player message produces one eligible response; no duplicate turn after lost ack |
| D. Runtime context | Revisioned `update_context` for scene/world/capabilities | Two Unity sessions cannot share active world state; captured context stays fixed during request |
| E. Speech and delivery | `report_ptt`, `report_playback`, WAV chunks, `acknowledge_result` | Speaking lease, late playback, cancel, ASR gating, loss/reconnect, SHA-256 validation |
| F. Structured effects | Apply typed response segments on Unity main thread | Reject malformed/incompatible protocol; idempotent final application |
| G. Game settings | `get_game_settings` and push update | Dynamic update without polling, reconnect and settings revision |
| H. BeatSync | Bind a typed BeatSync operation/stream after #29 | Started/chunk/ready/error and cancellation/retry scenarios |

## 6. Exact blockers before closing #1

1. **Unity source pin:** establish an upstream Git bundle, branch and exact SHA matching the provided ZIP (or choose a newer explicitly verified Unity revision). An archive SHA-256 is insufficient to establish Git provenance.
2. **Runtime reachability:** verify actual Unity initialization and callsites for every current handler (including `get_task_status` absence in the visible network tree), and account for any external mod/third-party consumers.
3. **Unity↔Rust E2E:** perform at least one real Unity session handshake → player event → reply → typed result → audio/playback feedback → application acknowledgment, then reconnect and re-test; belongs primarily to #13/#25/#26.

The source audit establishes the **semantics and migration mapping**. New client implementation and external E2E remain separately tracked. This document is not a claim of wire compatibility between the old Unity client and new Rust server.
