# Character catalog and Qt settings integration, 2026-11-04

Base: `34250282` (main after PR 78).
Integration branch: `codex/character-gui-integration-20261104`.

## Reviewed source heads

| Head | Result |
| --- | --- |
| `4414edda`, character catalog recovery | Included. Canonical nine-character definitions belong to Core; CharactersPresenter supplies frontend membership, names and order. Old ID aliases, SQL lookup alternatives, historical settings DTOs, version envelopes and concrete dev migration steps are removed. |
| `7dfca650`, recovered GUI kit | Included with integration fixes. Shared Qt-style settings controls and General preferences; replaced page-index settings and non-atomic array updates. |
| `e47c4497`, API preset workspace | Included with integration fixes. Typed preset CRUD and supported/unsupported provider metadata; parameter translation now uses common localization descriptors and JSON. |
| `8d4652b8`, published dropdown follow-up | Included. Correct provider icons, pricing badges, popup dimensions and shared themed scrollbar. Export-only bridge workflow is excluded. |
| `31483a15`, previous review branch | Already part of main through PR 78; no additional feature merge is required. |

## Contracts and issue 75

There is one current strict settings envelope: `revision` and complete `settings`.
Missing required fields, old `schema_version` envelopes and old character IDs are
errors. Import preparation and startup use the same current-format validation.
Generic Migration/StartupMigration/Step/Store extension contracts remain available;
the Python conversion is still an explicit unimplemented operation. Incompatible
user files are not removed or rewritten automatically.

The only built-in identities are Crazy, Kind, Cappie, ShortHair, Mila, Sleepy,
Ghost, Creepy and GameMaster. The display name of Crazy is Безумная Мита.
Slint maps IDs to portraits and applies translations; it no longer defines membership.
The handlers are instantiated from the same Core catalog. Descriptions/prompts remain
minimal built-in definitions, not a completed Python prompt-pack port.

Visibility preferences use typed SettingsArea/SettingsVisibility, independent of
Slint page positions. Deserialization requires every area explicitly. The functional
InterfaceSettingsPresenter updates one area inside the settings owner's atomic edit.
The GUI owns its index-to-area mapping; future TUI navigation can use another order.
The concurrency regression updates different areas simultaneously through the real
presenter and checks that none of the accepted edits is lost.
The larger nine-character profile is owned through `SettingsSource::initial(settings)`;
the Initial variant boxes that profile once at startup rather than inflating the source enum.

Provider parameter titles/descriptions are LocalizedText descriptors. JSON catalogs
provide English and support live locale/overlay changes through the existing Slint
adapter. Localization extraction includes Core character definitions and presenter
metadata. Transport timeout is not marked as a Gemini generation parameter. Unsupported
provider controls remain unavailable rather than inventing successful operations.

## Qt source and geometry

Read-only Python sources under `D:/NeuroMitaTest/NeuroMitaOrigin/09082025/NeuroMita`:

- `src/ui/widgets/settings_sections.py`: icon badge 26x26, centered arrow 18 wide,
  header margins 16/12, gap 10, expanded content margins 20/8/20/16.
- `src/ui/gui_templates.py`: compact toggle rows with 2px top/bottom margins and
  vertically centered right-side widgets; section subtitle belongs to the header.
- `src/ui/widgets/toggle_switch.py`: 42x22 switch, 1px track inset, 16px knob,
  140ms transition and explicit enabled/disabled palette.
- `src/ui/settings/general_settings.py`: original section ordering and icons.

Integration fixes center fixed-size header badges and row controls in layout wrappers,
use the shared centered glyph primitive, restore dense toggle spacing and share heading
components across General, Characters and API. The API active indicator and active badge
are centered against the entire card/header, outside the two-line text group. Header
glyphs have an inset inside the clipped 48px badge. Removing the API page's explicit
parent-width constraint restores the enclosing workspace's right padding. The common scrollbar scrolls complete section contents;
expanded sections keep their natural height when several are open.

The popup parity follow-up is preserved. Only the three currently supported Gemini
configuration fields are editable; the presence of another provider's visual icon does
not mean its generation backend has been implemented.

## Verification

Full GUI suite: 499 passed, 0 failed, 2 ignored; default suite: 498 passed, 0 failed,
2 ignored. Default and GUI all-target Clippy with warnings denied, formatting and the
offline GUI-enabled binaries build passed. Localization checks include Core character names and presenter
parameter descriptors. Earlier tests referencing the rejected `mita`/`kind-mita` IDs
were corrected to canonical IDs rather than restoring compatibility.

Native captures reviewed:

- [General](gui-reference/rust-settings-20261104/general.png)
- [API dropdown, active indicator and badge](gui-reference/rust-settings-20261104/api-dropdown.png)
- [Nine Core-owned characters](gui-reference/rust-settings-20261104/characters.png)

Captures use the existing
Winit/FemtoVG snapshot example with explicit local fixture data, no credentials, provider
calls, production settings writes or external process launch. Such render checks do not
certify OS minimize/restore, physical DPI transitions or full Python functional parity.

## Additional branch left separate

`codex/settings-four-pages-20261104`, head `dd50806f`, adds Language/Game/Models/Images
layouts. Its Game/Models/Images controls bind to local `in-out` properties, including
hardcoded limits and booleans, and change those fields directly. They do not command a
functional presenter or update the Core owner; recreated pages lose those changes.
That is an interactive mock of settings, not a working administration contract, and
contradicts the Core-owned-data rule. The whole branch is therefore not included and
no corresponding feature issue is closed. Its visual components can be reused after
explicit bindings and unsupported-operation handling are implemented.
