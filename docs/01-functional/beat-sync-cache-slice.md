# Beat Sync — Rust core derived-cache slice (#29)

This is a **partial implementation** of issue #29, not full Beat Sync parity.
The current production Python/Unity path and the installed backend set must be
verified at a pinned source revision before connecting a worker to this module.
Source functional audit: [music-beats.md](music-beats.md).

`music::BeatTimeline` models the *result* of an analysis: monotonically ordered
beat times, provider-specific confidence, duration, sample rate, winning method,
and optional BPM. Validation rejects NaN/Infinity, out-of-range confidence,
non-increasing beats, beats beyond duration, and unbounded results.

`music::BeatCache` stores validated analysis results as bounded JSON envelopes.
Identity includes SHA-256 of the audio input bytes, requested backend preference,
backend/model revision, decoder revision, and analysis settings revision. These
revisions must be supplied by the analyzer integration, not guessed from the
installed/selected backend. The actual winning `method` remains on the result.
The consumer may apply `min_confidence` after cache lookup; it is not in the
key because the unfiltered confidence values are retained. Different methods'
confidence scores must not be treated as calibrated against each other.

The caller **must hash the same immutable audio bytes the analyzer uses**.
Hashing a mutable path and later asking a worker to reopen it introduces a
TOCTOU identity race and is not a valid integration. No implicit fallback,
installation, or model readiness is asserted here. Failed cache writes should
not turn an otherwise successful analysis into an analysis failure.

The cache returns `Miss`, `Hit`, or `Corrupt`; malformed entries never become
results. Writes use unique staged files with file sync and rename. This slice
has neither a cross-process writer lease nor directory-fsync durability, so
those aspects require separate review before concurrent production activation.

Not implemented: reading local audio files, performing beats inference, Python
worker lifecycle/cancellation, progressive computation, Unity status events,
GUI cache management, and batch reindex. Tests cover repeated lookup, identity
invalidation, threshold reuse, corruption recovery, and invalid fixture data.
