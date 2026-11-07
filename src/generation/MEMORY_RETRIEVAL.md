# Long-term memory: deterministic context selection

**Current implementation:** lexical selection for the Gemini prompt, not semantic RAG or embeddings. Canonical memory storage and acceptance remain in `timeline::domain::memory`.

## Read path

1. `CharacterHandler::context_admitted` loads the **entire** character-specific memory projection at the selected Timeline anchor. `CharacterView.memories` and `CharacterContext.memories` are not truncated.
2. `LlmConversation::from_context` builds a topic query from request guidance, working-state focus, reaction description, the explicitly targeted event or message (including an older reply target), and the three most recent projected history messages. It does not query another character's Timeline.
3. `generation::memory_context` matches Unicode-alphanumeric words case-insensitively. Candidates are ranked by **number of distinct exact token overlaps**, then priority (`Critical > High > Normal > Low`), then stable `MemoryId`. A nonmatching high-priority memory can be selected if there is room; priority does **not** override a matching low-priority memory.
4. Render at most **12 complete records** and **6000 Unicode scalar values**, counting the heading, metadata, stable IDs and footer. Entries too large for the remaining budget are skipped, never silently truncated or rewritten. If all are too large, the memory block is omitted. The output retains the existing `[LONG-TERM MEMORY]` data-only framing.

The selection is recalculated from the captured request context. It is **not** persisted as another state or as an event, and it has no role in acceptance. The memory ID and content are unchanged in Timeline, including history, forks, deletion and SQLite reopen. Only the prompt view changes. These character bounds are not a tokenizer-specific token guarantee.

## Write path / exact duplicates

`CharacterMemories::resolve` is still the only materialization of proposed `MemoryCommand`s before atomic acceptance. An `Add` whose content matches an existing record after trimming/collapsing whitespace and Unicode lowercase **preserves the previous ID** and content. If the new priority is higher, `resolve` emits one `Upsert` upgrading priority; otherwise it emits no memory change. Within one batch, two identical Adds likewise create only one record. `Update` and `Merge` cannot create an exact-content duplicate with an unrelated existing record: they reject the **entire** proposed batch. This does not rewrite historical duplicates or constitute semantic similarity detection.

This deterministic exact-match policy intentionally does not perform fuzzy similarity, embeddings, semantic merging or speculative deletion. With no matching tokens the shortlist falls back to priority + stable ID order. A record omitted from the current prompt may still exist and be selected on a later turn; the LLM cannot reliably manipulate an ID it was not shown.

## Remaining work

Semantic retrieval/RAG, embedding indexing and revisions, user-configurable token budget, stale-index reconciliation, entities and tags, memory islands, TTL/forgetting policies, UI memory editor, and Python ordinal-ID compatibility all remain out of scope. Any future index is a *derived projection* from accepted Timeline memory facts, not an additional canonical source of truth.


## Semantic extension

The lexical implementation remains available and the CLI default. An opt-in asynchronous semantic retrieval mode now provides embedding provider contracts, Gemini 001/2 HTTP adapters, a strictly derived bounded cache, and an exact vector shortlist. The cached vectors never define membership. See [SEMANTIC_RETRIEVAL.md](SEMANTIC_RETRIEVAL.md) for the implementation, verification scope and measured synthetic baseline. Live Gemini quality is not certified.
