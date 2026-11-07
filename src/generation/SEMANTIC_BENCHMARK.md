# Opt-in live semantic retrieval benchmark

A benchmark must use human-labelled representative queries and memories to establish live recall. The committed `tests/fixtures/semantic_benchmark_sample.json` is an **illustrative synthetic schema sample**, not a representative dataset and not a measured Gemini result.

Run only when the API key, dataset and expected billing are explicitly authorized:

```bash
GEMINI_API_KEY=... cargo run --locked --offline --bin semantic_benchmark -- ./corpus.json gemini-embedding-001 --execute-live
```

Alternatively use `gemini-embedding-2`. Set `NEUROMITA_MEMORY_EMBEDDING_DIMENSIONS` to override the default 768 dimensions. The corpus has `cases`, each with a `character` (a canonical `CharacterId`, e.g. `Crazy` or `Kind`), `query`, nonempty `memories` (stable UUID `id`, `content`, `priority`) and human-labelled `relevant_ids` that must refer to those memories. Up to 256 memories per case. Keep private memory corpora local; the benchmark never transmits them to GitHub, only to the chosen embedding provider.

The tool invokes the **same lexical ranking** used by production generation and the **same SemanticMemoryRetriever** used by production. It measures Hit@1 / Hit@3, elapsed semantic milliseconds, cache hits, outgoing embedding calls, text count and Unicode character count, and emits JSON to stdout. They are **not** token-billing metrics. `billing_tokens` and `cost_usd` remain null until reliably provided by the API; do not infer them from character counts. No fallback is allowed on provider errors. The cache is in-memory, so the sequence of cases affects cold/warm request totals; preserve case order when comparing repeated runs. Secrets are never included in output.

This is an opt-in measuring instrument, **not** a claim of completion of #22's full RAG, provenance/query fusion/knowledge graph requirements. No live measurements were run without explicit credentials and a representative corpus.

The underlying semantic result also exposes score provenance and explicit filter status for future evaluated retrieval policies. The existing benchmark deliberately retains unfiltered production behavior so its Hit@K statistics remain directly comparable with the existing lexical baseline. Filtering or fusion must be a separately labelled benchmark protocol, not silently mixed into those results.

Fusion is intentionally **not** included in the existing live benchmark's baseline. Offline composition tests cover fusion independently, avoiding any neural requests, costs or nonreproducible model outputs.
