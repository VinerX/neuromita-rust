# Settings layout and native audio follow-up

## Source and layout audit

The reference is the read-only Python implementation under
`D:/NeuroMitaTest/NeuroMitaOrigin/09082025/NeuroMita/src`, with screenshots used for checking discrepancies.

| Area | Python reference | Rust correction |
| --- | --- | --- |
| Game subsections | `ui/settings/game_settings.py`, `dialogue_settings.py`, `widgets/number_stepper.py`, shared `gui_templates.py` and `styles/pages/settings.py` | Shared SettingsRow derives its preferred height from both label and control layouts. The source's 40 px NumberStepper has one outlined frame, 36×38 px side buttons, zero inter-button gap, centered editable value and theme tokens from QSS. Numeric/action controls no longer paint outside fixed 26 px rows. Nested sections retain complete content height. |
| Scroll containers: Game, AI Engine and shared consumers | Qt scroll-area viewport owns the available content rectangle | Horizontal layout reserves one scrollbar column. Flickable no longer floats centered in its parent. All consumers size their content against visible-width, including API template dropdown, Characters, General, Voice, Models and Images. No content-height → scrollbar visibility → content-width cycle. |
| Settings tab strip | Scrollable navigation with natural text/icon sizing | Each icon has an unstretched 20 px slot; captions retain measured widths, 14 px horizontal padding and 6 px gap. Tabs grow to fill free width and scroll when their combined natural widths exceed the viewport. |
| Checkbox | `microphone_settings/widgets.py`, muted/accent outlined square | Shared checkbox uses SettingsTheme.muted for the unchecked outline, preserving accent fill/outline for checked state. |
| ASR | `microphone_settings/ui.py`, `widgets.py` | Existing source-mapped ASR tokens remain. Native device labels and IDs are separate read-model fields. Recognition model parameters remain gated by model availability; microphone test does not require a recognition model. |
| Sandbox session | Python session sidebar: portrait, selector, settings action; preset/model label | Avatar moved outside selector; character gear has accent background. ConversationPresenter supplies the selected generation preset name; label renders `name (model)` for named presets. Runtime-only unnamed presets display their model without inventing metadata. |
| Home | Backend component icon | Font Awesome Rust brand SVG replaces the Python icon. Source: FortAwesome/Font-Awesome `6.x/svgs/brands/rust.svg`; existing Font Awesome license applies. |

The scroll viewport explicitly overrides inferred maximum layout sizes. Otherwise Slint propagates the content's width-dependent maximum constraint back into the viewport and creates a layout binding loop. The generous maximum is a layout constraint, not a content coordinate or page-specific offset. Thumb position is the only proportional scroll coordinate; page content is laid out normally.

## Ownership and audio contracts

`ApplicationLifecycle` creates one `AudioInputOwner` and shares it with the functional `AudioInputPresenter`. The owner coordinates the recognition controller and microphone monitor; pages do not select drivers, open devices, pause workers or fabricate audio readings.

```text
ASR view → desktop command adapter → AudioInputPresenter → AudioInputOwner
                                                        ├─ native CPAL catalog
                                                        ├─ SpeechSessionController: suspend/resume
                                                        └─ MicrophoneMonitor: native input/output
                              ← watch<MonitorSnapshot> ← RMS/status/error
```

- CPAL 0.17.3 is locked and vendored. Catalog entries contain native stable endpoint ID, friendly name and actual host/backend. Windows exposes WASAPI; additional PortAudio-specific Python hosts are not fabricated in Rust.
- Exact backend + endpoint ID is required to open input. Disconnected or unknown selections fail explicitly. Native capture uses no FFmpeg or Python dependency.
- Input channels are mixed to mono, device PCM converted to 16 kHz PCM16 frames using a streaming weighted-area converter. Tests cover 16/44.1/48/96 kHz duration and constant-signal preservation. This is a voice capture converter, not a studio-quality resampling claim.
- Capture delivery is bounded. Overflow produces an error instead of silently continuing recognition with missing audio.
- Microphone test plays captured audio on the system default output, matching Python's loopback behavior. The output queue is bounded to 200 ms, callbacks never wait on its mutex, and level updates are coalesced to approximately 10 Hz. Audio is never persisted by this test.
- Core suspends the original recognition session before loopback, restores it after stop/error, and discards the suspended session when a new capture starts or the application stops. The desktop adapter only forwards commands and observations.
- Suspension carries a private controller epoch: an explicit stop or worker/model reconfiguration invalidates it. An obsolete microphone session cannot reopen after a model change.
- Native session playback accepts PCM16 WAV, including multiple channels mixed to mono. Other encodings fail explicitly; existing external FFplay adapter remains separate.
- The hidden ten-second clamp is removed. Core's configured speech duration, up to its existing 600-second validation bound, is honored. Segmenter/WAV/SDK request bounds are aligned: 19,200,044 WAV bytes and 32 MiB JSON frames. The source-parity UI still exposes the Python numeric range (up to 120 seconds); no second silent clamp exists.
- The embedding lifecycle test's Windows stack overflow was fixed by boxing the startup future at the application boundary, rather than enlarging test-thread stacks.

## Verification

CharacterVoiceTabs follow-up: source `ui/widgets/character_voice_tabs.py`
uses inactive backgrounds inset by (3,4), avatars 50 px inactive / 56 px selected
(plus a separate 3 px selection ring margin), and scroll buttons at y=26 in an
82 px strip. Slint now follows those bounds, uses an explicit Path viewBox,
clips the avatar viewport and vertically centers the arrow row through layout.
The raster regression checks the inactive-tab gap above the editor and the
enabled arrow's vertical position using the Core character catalog.

Voice-card colors follow `styles/pages/settings.py` QFrame#VoiceCard:
settings_panel_bg (#0e101f), panel_border (rgba(37,34,54,0.9)), radius 14 px.
The shared VoiceCard applies these tokens to the toolbar, provider, preview,
status and playback cards across Telegram, Local and API tabs. Profile labels
show the Core template name (Fish Audio), while settings retain its stable ID.
The voice-preview editor starts with the localized Python sample sentence;
it is editable UI state and is not reset by settings snapshot rendering.

Follow-up: the Language page also sizes its content against visible-width with
the same 2 px outer padding as General. Its locale popup has a bounded, clipped
background and a SettingsScrollView; all locale rows remain inside the popup.
Disabled SettingsField uses the darker input surface, muted text and normal
panel border, following the API editor QSS disabled state. Home's Rust icon uses
the user-selected Rust color `#d34516` and 34×34 size from `home_page.py`.

Native read-only inventory on this machine returned USB Audio, VB-Audio Virtual Cable, FIFINE K670 and Realtek inputs with distinct WASAPI endpoint IDs. `cargo run --locked --offline --example audio_devices` performs inventory only; explicit `--capture <backend> <stable-device-id>` performs a three-second capture check without saving audio.

The explicit FIFINE K670 capture check received 186 PCM16 frames in three seconds, with nonzero signal (peak 3708). Audio was not saved. Auditory loopback quality and production-model recognition were not evaluated by this capture check. Native Linux CPAL builds require the platform ALSA development library; vendoring Rust crates does not bundle operating-system audio libraries.

Final validation results and native UI acceptance are recorded in HANDOFF.md. Layout regression coverage measures row containment, zero viewport left displacement, stable viewport width across scrollbar visibility, and full expansion of multiple sections. A mock ASR IPC round trip includes a 60-second WAV, exceeding the previous transport bound. This does not claim live recognition quality from an installed production model or pixel-perfect certification at every DPI.
