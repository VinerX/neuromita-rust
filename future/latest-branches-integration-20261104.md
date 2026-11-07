# Latest branches integration

Base: main `13dc29b8`. Branch: `codex/latest-ui-integration-20261104`.

Included development heads:

- Voice/ASR views through `b28e5bb1` (also includes `4bbd59a1`, `18905efc`).
- API creation/editor workflow and endpoint preview through `46e70ec2`.
- Layout parity `5a157ced`.
- Scrolling settings tabs `d03a05de`.
- Speech presenters and inventory `c3c80e33`, with corrections below.

Corrections during integration:

- Complete strict speech settings, with no serde deserialization defaults.
- Per-character voice map keyed by CharacterId, separate shared voice field.
- Typed voice-save command; UI indices translated before entering runtime commands.
- No GUI-invented generation temperature/token/timeout defaults.
- Removed obsolete and duplicate translation entries after combining branches.
- Hidden settings tabs release their layout width.
- Fade overlays use a horizontal layout with constant-width edges and an expanding
  middle. Opacity changes preserve placement; neither fade uses an x coordinate.
- Export contract includes the new speech section.

The independently probing AI Engine presenter bundled in `c3c80e33` is excluded,
as is standalone `5f8e6062`. It directly invokes nvidia-smi/PowerShell instead of
using the existing AI runtime owner. The AI Engine page remains unavailable.
Old Home launcher `9632460d` is still separate because it performs process work
synchronously on the UI thread. Draft PRs 76, 77 and 79 remain separate.
Bridge export/publish workflows are not runtime features and are not merged.

Speech settings persistence and discovery do not implement live TTS playback,
Telegram connection, ASR capture or remote provider testing. These commands remain
disabled. Newly required settings are initialized explicitly, never inferred from
an incomplete stored document. `settings_profile` exports the current initial
profile for offline tooling without credentials or a Core instance.

Validation: full default suite 503 passed, zero failed, two ignored. The GUI run
had 505 passed and two failures (missing English text and the old export section
expectation); both were corrected. Re-run GUI binding/voice tests, localization
and settings transfer: 23 passed, zero failed. Default and GUI all-target Clippy
with warnings denied and formatting pass. No live speech service, paid model call,
physical audio device or native DPI/minimize acceptance is claimed.
