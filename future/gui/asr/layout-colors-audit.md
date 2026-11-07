# ASR: layout and colors audit

Scope: **ASR only**, branch `codex/settings-unified-asr-runtime-repair-20261104`,
initial head `d5a5e03b`. Other settings pages are not being ported in this pass.

## Authoritative reference

Read-only Python repository: `D:/NeuroMitaTest/NeuroMitaOrigin/09082025/NeuroMita`.

- `src/ui/settings/microphone_settings/ui.py`: `_card`, `_row`, `_top_field`,
  `params`, `ResponsiveColumns` composition.
- `src/ui/settings/microphone_settings/widgets.py`: `RecognitionStatusBadge`.
- `src/styles/pages/settings.py`: `MicrophoneSettingsWorkspace` QSS.
- `src/styles/theme.py`: actual token values, including alpha.

Screenshots are acceptance references; they do not replace these sources.

## Declarative layout contract

```text
Settings page / ASR viewport (stable scrollbar gutter)
└─ AsrSettingsPage: vertical, spacing 14, transparent, no extra page padding
   ├─ toolbar card
   ├─ responsive columns, breakpoint 850, gap 14
   │  ├─ behavior card
   │  └─ recognition card
   └─ microphone monitor card
```

| Element | Python contract | Slint implementation/acceptance |
|---|---|---|
| Card | padding 16/14/16/14, gap 9, radius 14, border 1 | `AsrTheme` + `AsrCard` layouts |
| Card heading | gap 8, title 15px/600, icon 18px | fixed icon wrapper; text centered vertically |
| Setting row | padding 10/8/10/8, gap 10, radius 10 | content-derived height, no fixed 70px crop |
| Row text | title 13px/600; description 12px; gap 3 | wrapping changes required row height |
| Numeric field | width 110; QSS minimum content height 24, padding 5/8, radius 8 | centered text, explicit step controls in nested layouts |
| Status badge | height 38, padding 12/0/12/0, gap 8, radius 9 | idle `—`, real non-idle/error states retained |
| Columns | breakpoint 850 | one column below breakpoint, two above |
| Scroll content | viewport owns usable width | ASR binds to visible width and reserves a stable gutter |

Do not size a child to a surrounding panel when it is inside the smaller viewport.
Do not make responsive page width depend on scrollbar visibility while scrollbar
visibility depends on the page's preferred height. That creates a real binding
cycle, not a rendering-quality issue. The ASR instance reserves scrollbar width
independently of content height. Other scroll-view instances retain their defaults.

## Colors (resolved from Python tokens)

| Role | Python token | Value |
|---|---|---|
| Card | `settings_panel_bg` | `#0e101f` |
| Card border | `panel_border` | `rgba(37,34,54,0.9)` |
| Row | `card_alt_bg` | `rgba(20,22,40,0.92)` / `#141628eb` |
| Row border | `border_soft` | `rgba(255,255,255,0.07)` |
| Numeric input | `control_bg` | `#080912`, opaque |
| Text / muted | `text` / `muted` | `#f3edf6` / `#bca9bb` |
| Status | state color | background alpha .10, border alpha .35 |

Rows must not use the unrelated generic `section` background. ASR-specific
tokens are declared in `AsrTheme`; shared theme tokens are reused where identical.

## Numeric editor contract

| Field | Min / max | Step | Decimals |
|---|---|---|---|
| Send delay | .5 / 30 | .5 | 1 |
| Sample rate | 16000 / 16000 | 1000 | 0; disabled |
| Chunk | 128 / 4096 | 128 | 0 |
| VAD | 0 / 1 | .05 | 2 |
| Silence | .05 / 10 | .05 | 2 |
| Prebuffer | 0 / 5 | .05 | 2 |
| Max speech | 1 / 120 | 1 | 1 |
| Min speech | 0 / 3 | .05 | 2 |

These are the original widget's editing constraints, not backend activation
capabilities. Core still validates persisted settings. The separate ten-second
runtime clamp is an unresolved contract violation, documented in the Git review.

## Availability and status

- Recognition controls require an actually selectable installed model, not merely
  a persisted ID whose files may be missing.
- Without that model: no language list/default hint for a previously saved model;
  language, numeric recognition controls and Apply are disabled.
- Behavior preferences remain independently editable.
- Saving behavior/preferences must not clear the canonical recognizer ID because
  the display dropdown has no installed entries. Model selection has its own command.
- Idle capture + idle worker displays `—`; errors/loading/listening remain visible.

## Acceptance checklist

- [x] Trace sizes, colors and ranges to Python source.
- [x] Remove fixed setting-row height and unrelated row colors.
- [x] Add vertically centered numeric editors and functional up/down buttons.
- [x] Break the scrollbar-width/preferred-height dependency cycle for ASR.
- [x] Gate recognition editing on installed selection and preserve idle placeholder.
- [x] Complete ASR/GUI checks and record branch-level blockers.
- [ ] Native visual comparison at equal window size/DPI, including scroll and long labels.

Do not claim pixel-perfect/native visual acceptance from successful compilation.

Validation: 10 GUI tests passed, including the regression that a preference save
with an empty installed dropdown preserves the selected recognizer. Default
all-target Clippy, production GUI Clippy, formatting and GUI build passed. Full
GUI all-target Clippy still fails on the unchanged `voice.rs` test-module ordering;
the full Core test run hits the lifecycle stack overflow described in the Git
review. Logs: `target/asr-audit-*`.

The user chose to perform native visual acceptance. Computer Use only checked
whether NeuroMita was running; no application was launched or UI action performed.
