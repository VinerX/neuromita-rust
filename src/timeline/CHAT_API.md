# ChatApi and Unity-directed conversation contract

**Status:** implemented Rust API and TCP `ServerLifecycle` wire contract (2026-11-01).

## One timeline, one message model

Every utterance is one `ConversationMessage: Message + Event`, regardless of whether its speaker is the player, Unity-controlled character, or generated character. It is committed once with one stable `EventId`; the canonical event is not copied into per-character histories. `targets` and `participants` describe conversational routing, while `seen_by` defines who actually witnessed the utterance. A character speaker always witnesses their own message.

`MitaView` contains only directly witnessed facts available after branch selection and resets. A character hearing a retelling (event Y) **does not** acquire the event being described (X). `ReplyTo` and `ReactTo` links never transfer knowledge.

## Explicit publication, explicit generation

1. The orchestration layer (currently Unity) publishes the **existing** utterance as a canonical event using `ChatApi::publish_conversation_message`, `ApplicationLifecycle::publish_conversation_message`, or TCP `publish_message`. Publication never starts generation.
2. Unity decides which character should answer, whether this is a reply, reaction, or initiation, and who witnesses the resulting response.
3. `ChatApi::reply(ReplyRequest::new(character, message_event_id, source))` requires a visible **Message**. It cannot be called on arbitrary game events, and it never creates a synthetic player message.
4. `ChatApi::react(ReactRequest::new(character, event_id, instruction, source))` accepts **any** directly visible Event, including Unity gameplay events. It rejects unseen or reset events before generation.
5. `ChatApi::initiate(InitiateRequest::new(character, instruction, source))` has no trigger event. It uses the character's current MitaView.
6. When a queued command starts generation, the executor resolves the assigned preset once from the current settings and materializes a `GenerationPreset` for `GenerationRequest::new(context, preset, prepared_knowledge)`; queued commands observe new assignments, running commands do not. See [`../generation/PRESETS.md`](../generation/PRESETS.md). The executor generates a response using the configured LLM provider, preserving the separate structured-output processing contract. It accepts one new character Message with a `ReplyTo` causal link for replies, `ReactTo` for reactions, or no causal target for initiation. Nothing auto-schedules the next dialogue turn.

No `ReplyTarget::Input`, `.to_event`, `UserInput` guidance on Reply, generated `ProactiveOpportunity` or TTS policy inside the command is required. The internal `GenerationCommand` is an execution interface, not a domain Message. `ChatApi::send` is not public.

## ServerLifecycle TCP commands

All JSON commands are newline-delimited and contain `command_id: UUID`. A Unity client must use its own stable `event_id` and `occurred_at_unix_ms` for publication, including retry after reconnect. A successful write returns a `published` frame carrying that `event_id`. A successful generation admission returns `accepted` with a `request_id` and is followed by `request_status` frames. Repeating an identical command ID on the same connection returns the prior receipt; using a different command ID with the same stable event identity and payload can reconcile a publication through Timeline idempotency.

The `request_status` projection shares its source `RequestSnapshot` with the Sandbox presenter. `state: processing` includes `stage: text`, `text_ready`, `voice`, or `accepting`. After the LLM finishes, `text_ready` / `voice` / `accepting` include `text` and `provisional: true`: this text is **not yet** an accepted Timeline message. `can_cancel` indicates snapshot-time eligibility; `accepting` and all terminal states report `false`. `finished` reports the final accepted text, and `cancelled` has no provisional text. Clients must reconcile states by `request_id` and increasing `revision`; fast stages may be coalesced. These are completed-LLM previews, not token streaming or execution acknowledgements.

- `publish_message`: `event_id: UUID`, `occurred_at_unix_ms: u64`, `speaker`, `content: JSON`, `seen_by`, optional `targets`, optional `participants`. This publishes **one** Message regardless of speaker. It does not generate a reply.
- `reply`: `character`, `message_id: UUID`, optional `targets`, `participants`, `seen_by` for the **generated** response. `message_id` must refer to an observed Message, not an arbitrary Event.
- `react`: `character`, `event_id: UUID`, optional `instruction` and generated-response audience. Any observed Event may be a target.
- `initiate`: `character`, optional `instruction` and generated-response audience. No target.
- `cancel`, `settings`, `set_volume` retain their existing meanings. `test` remains a test-protocol convenience, not a production conversation operation.

The wire `speaker`, `targets`, and `participants` are serialized as `"player"` or `{ "character": "Crazy" }` / `{ "character": "Kind" }`. `seen_by` uses character ID strings (`"Crazy"`, `"Kind"`). New Unity roster and scene-witness mapping remain the responsibility of the Unity orchestration layer; the Rust core does not infer them.

Both `success` and `voice_error` terminal statuses include the canonical accepted Message's `event_id`. A voice error leaves the accepted text and its identity available for history reconciliation; it does not turn that text back into a provisional or failed generation. Replaying the retained command receipt returns the same `request_id` and terminal `event_id` without another generation.

The generated speaker is always included in its own `seen_by`. Other witnesses, targets and participants are supplied by the caller and are **not** inferred from each other.

## Known boundaries

- This TCP interface is the current Rust transport adapter, **not** proof that the existing Unity C# client uses the same commands yet; aligning the C# client is a separate integration step.
- Provider-specific structured output schema parity with the Python application remains incomplete and is not covered by this transport change.
- Existing historical `ProactiveOpportunity` facts remain readable for old databases, but new `reply/react/initiate` invocations no longer synthesize them.
- Current `CharacterId` is still a closed two-character enum. Dynamic roster identifiers require a separate domain migration, not ad hoc aliases in the wire adapter.
