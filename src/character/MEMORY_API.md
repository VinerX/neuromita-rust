# Character memory ownership and presenter contract

- **Character domain** (`character/memory.rs`) owns `MemoryRecord`, `MemoryCommand`, `CharacterMemories`, canonical lifecycle, validation and capacity policy. `crate::memory` remains a compatibility re-export; there is no second implementation.
- **CharacterHandle** is the existing actor/owner of user-originated changes. `memories` returns a `CharacterMemorySnapshot` with character identity, all retained records, selected branch and full Timeline HEAD. Presenter `characters.memories` and `characters.edit_memories` delegate to that same owner.
- **Timeline** owns only history, versioned selection, raw world reads and guarded event append. `character::memory_access` builds the presenter snapshot, checks the character's retained records, resolves commands with the character domain, and appends against the current Timeline HEAD. Unrelated Timeline events do not invalidate memory edits; changed character-memory records and branch selection do. No writable CharacterMemory cache exists.
- **User action** `MemoryCommand::SoftDelete` marks a record `Deleted`; the legacy physical `Delete` operation is refused at this boundary. Physical purge requires a separate confirmed backup-first administrative workflow, which is intentionally not exposed via a public presenter until there is authorization. `Forgotten` stays excluded from ordinary context/embeddings.
- **Presenter** only translates typed user intents into character-owner calls and returns the new projection. Invalid commands fail without a partial commit; stale views fail instead of overwriting newer data. A no-op returns a fresh snapshot without writing an event.
- The public presenter interface is for trusted local bindings; it is **not** a remote authorization boundary. Full UI authentication, a pushed memory change stream, Python behavioral parity and physical historical erasure are still open tasks. This refactor does not change AI Engine, Generation, or SessionTimeline wire protocols.

- LLM `memory_delete` performs `SoftDelete`, and merge marks its sources `Deleted`; neither may physically purge retained records. Existing historical `MemoryChange::Delete` remains replay-compatible.

## Read-only Presenter projections (Memory Lifecycle #21)

`CharactersPresenter::list_memories(character, MemoryListQuery)` captures **one**
character-owned snapshot and projects a read-only, bounded page (1–100 records).
Filters are optional lifecycle state, memory kind and case-insensitive substring;
ordering is the canonical stable `MemoryId` order. `total_matches` counts filtered
records while `MemorySummary` counts **all** retained records in that snapshot
(active/forgotten/deleted and fact/island/protected totals). Filtering never
changes the set available to generation or calls any embedding provider.

The page carries its complete opaque `CharacterMemorySnapshot` for a later
`edit_memories` call. `MemoryListPage::from_snapshot` can page the **same**
snapshot repeatedly without re-reading concurrent updates. The client must
explicitly refresh to observe newer memory, and edits using a stale token are
rejected by the Character owner. These types do not depend on Slint, IPC or a
particular UI toolkit. There is no separate memory cache or write endpoint.

## Observable Presenter memory list (#21)

`CharactersPresenter::observe_memories(character, query)` returns a `MemoryObserver`.
Its `next()` immediately provides the first authoritative bounded page, then waits
for committed in-process memory changes. A notification is a coalescing
**invalidation hint**, not a second source of memory data or a durable revision.
Each page is reconstructed through the same `CharacterHandle` and carries its
normal opaque edit snapshot. The observer retries a failed read on its next call.

Notifications are emitted **after a successful Timeline commit** from user edits,
LLM memory effects, TTL expiration, administrative purge/restore and branch
selection. Unrelated character changes and ordinary chat messages do not
invalidate a memory list. Failed writes and no-op TTL passes emit nothing.
The watcher closes with its owning Core. Notifications can coalesce, so clients
must not interpret them as one event per memory command.

This watch channel is **in-process only**: writes from a separate Core process,
external SQLite connection or an uninstrumented repository writer are not
pushed to this presenter. Such clients must explicitly refresh or use a
separately coordinated IPC/revision contract. It has no permission to invoke
administrative purge; that remains internal until a real authenticated client
boundary exists. No Slint or AI Engine dependency is introduced.

## Explicit capacity reduction (#21)

`CharactersPresenter::preview_memory_capacity(character, limit)` returns an immutable
`MemoryCapacityPreview` of **active fact** eviction candidates. Valid target limits
are 1..=256. Only unprotected, noncritical active facts are candidates; islands,
forgotten/deleted records, protected and Critical records are never selected.
The candidate sort is Low → Normal → High, then stable `MemoryId`. The ID is a
deterministic tie-breaker, **not** creation time or RAG access frequency. If the
requested limit cannot be reached without touching protected/Critical entries,
preview fails entirely. It never mutates the Timeline.

`apply_memory_capacity(character, preview)` commits explicit `Forget` commands
using the same `CharacterHandle::edit_memories` snapshot CAS as ordinary edits.
Stale branch/character changes reject the full batch; a no-op preview produces
no event. This does **not** alter the existing Add policy: reaching the hard
capacity still rejects an Add. This behavior differs from Python's age/access
weighted automatic eviction and is intentionally opt-in until parity is decided.
A full-capacity preview may require 255 Forget changes; character user-edit and
Timeline batch limits therefore allow up to 256 atomic changes.
No ML inference, GUI, new storage, or event bus is involved.

## Exact idempotent fact insertion (#21)

`MemoryCommand::EnsureFact { priority, content }` is an explicit action that
`CharactersPresenter::edit_memories` delegates to the canonical Character
owner. It matches a retained **fact** by exact text, preserves its `MemoryId`
when reactivating `Forgotten`/`Deleted`, and returns a no-op if already active.
Unlike regular `Add`, it never implicitly upgrades an active fact's priority.
Ambiguous matches, protection and capacity violations fail the entire guarded
edit without writing Timeline. The stored `expires_at_unix_ms` is preserved.
This operation does not require any AI Engine integration or GUI changes.

## One-time memory island seeding (#21)

`MemoryCommand::SeedIsland` creates a High-priority island only if no record
of its type is retained for that character. Repeated calls are true no-ops,
including after forgetting, soft deletion or a user edit. `SetIsland` remains
the intentional update operation. Both resolve through the existing guarded
character-memory editing boundary and require no new Presenter endpoint, GUI
component, storage or AI Engine changes.

## Explicit lexical deduplication (#21)

`MemoryCommand::AddLexicallyDeduplicated` is a pure Character domain command,
with an explicit Jaccard threshold measured in per-mille. It reuses the same
`CharactersPresenter::edit_memories` and guarded Timeline path as all other
memory edits. A matching active fact retains its ID while its content and
priority are updated; protected, hidden and island records cannot become
mutation targets. It does not activate any ML provider or alter normal Add.
