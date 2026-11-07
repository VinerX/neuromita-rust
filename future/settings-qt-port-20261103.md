# Settings: PyQt → Slint

Source is the read-only Python checkout at `D:/NeuroMitaTest/NeuroMitaOrigin/09082025/NeuroMita`, not a reconstruction from screenshots. Screenshots are visual checks and do not supply invented settings, character data or prompt metadata.

## Source map

| Python implementation | Slint counterpart | Values/behavior carried across |
|---|---|---|
| `src/styles/theme.py` | `ui/settings/theme.slint` | Named palette: canvas, panel, controls, text, borders, accent and warning colors |
| `src/styles/compose.py`, `src/styles/pages/settings.py` | SettingsTheme and settings components | Resolve page/character overrides together with the later expanded-state selectors; do not copy an earlier background rule in isolation |
| `src/ui/pages/settings/settings_page_widget.py` `_build_ui`, `_build_settings_hero`, `_build_tabs_row` | `ui/pages/settings.slint` SettingsPage | Combined outer insets 28/24; four header actions; 42px navigation tabs; flow wrapping with 10px horizontal/6px vertical spacing; panel insets/gaps 14px |
| `src/ui/pages/settings/section_registry.py`, `src/ui/widgets/settings_section_header.py` | SectionHeading | Original title/subtitle and user icon; 48px icon surface, 27px glyph, 14px text gap, 23px title and 12px subtitle |
| `src/ui/settings/character_settings/ui.py` `build_character_settings_ui` | CharacterWorkspace | 1:3 initial columns with 9px handle gap and 240/350px minimum widths; library 10px inset/6px gap; detail 12px inset/14px gap; 56px avatar/20px name |
| `src/ui/settings/character_settings/widgets.py` CharacterListDelegate and `character_settings_logic.py` `_build_character_library` | Character library rows | 48px rows, 32px circular avatars, two text lines, selected/ordinary outlines, search icon; authoritative Rust catalog remains two characters |
| `src/ui/widgets/settings_sections.py` CollapsibleSection | `ui/components/collapsible-section.slint` | 26px icon chip, 16px horizontal header padding, 20px body inset, explicit collapsed geometry; arrow and icon remain vertically centered |
| `src/styles/pages/settings.py` expanded Collapsible selectors | SettingsTheme.section / section-content | Section `rgba(9,10,22,.72)`; expanded content `rgba(8,9,19,.42)`; later rules override character background. Children are inset from the outline so content cannot cover its border |
| `src/styles/pages/settings.py` final `SettingsPageRoot QListWidget/QLineEdit/QComboBox` rules | SettingsTheme.field / field-border and library viewport | Later descendant selector overrides early transparent CharacterList: `rgba(9,10,22,.88)`, white 12% outline, 10px radius, list insets 7px vertically/10px horizontally. Item cards keep their separate panel fill and delegate outline |

The settings workspace uses Segoe UI as declared by its QSS. The shell/hero inherits the original Segoe UI Variable family. Colors are named centrally; page markup does not contain independent hex palettes. Generic Slint controls remain reusable by other views.

## Functional boundaries

- Preset assignment belongs under the character provider, not inside prompt pack metadata. Common assignment is a single typed presenter edit; external confirmed changes update the view.
- Memory remains an inline panel in character history. Its functional presenter, CharacterHandler CAS and draft acknowledgements remain independent of GUI nesting.
- Prompt catalogs/folder reload are not implemented by Rust yet. The original field/metadata/action composition is retained, with unavailable actions disabled and missing metadata shown as `—`. Do not present the Python pack `DefaultJson` or its author as loaded Rust data.
- Navigation restores the Python categories. General/API/characters/voice and language use available bindings; remaining categories explicitly show unavailable content. This is not full functional parity with all Python settings.
- Slint layout handles resizing and tab wrapping. No timer/relaunch correction or hardcoded content-position screenshot coordinates are used.

## Validation

The GUI regression renders expanded/collapsed sections and asserts that collapse releases height, reopening restores it, the expanded body differs from the header, and the body cannot paint over the outline. Larger settings/memory/Sandbox snapshots are rendered at 1560×918 and 1200×760 using local fixtures.

Current review renders: [characters, wide](gui-reference/rust-review-20261103/settings-2-1560.png), [characters, compact](gui-reference/rust-review-20261103/settings-2-1200.png), [inline memory](gui-reference/rust-review-20261103/memory-1200.png). User-supplied original: [PyQt character settings](gui-reference/08-settings-characters.png). Different character counts and unavailable prompt metadata reflect current Rust data, rather than loaded Python state.

Windows computer-use initialization fails before selecting any window (`failed to write kernel assets`, OS error 3). A separate application-level snapshot example now creates the actual Winit/FemtoVG window and captures its client area through Slint itself, without Windows input automation or a Core/network/model. [Native frame](gui-reference/rust-review-20261103/settings-native.png) verifies the production renderer's section/list backgrounds, outlines and rounded gradient button. The fixture has two Rust characters and no prompt catalog. Native DPI/minimize/restore and a pixel-difference certification remain **unverified**.

Reproduce with a new output filename: `cargo run --locked --offline --features gui --example gui_snapshot -- target/settings-native.ppm`. The example automatically closes its own window and refuses to overwrite files. PPM → PNG conversion changes encoding only; no screenshot content is retouched.

The test renderer also has a known local limitation: `vendor/i-slint-renderer-software/lib.rs::process_rectangle` submits a linear gradient before applying the rounded rectangle border, so the gradient's corners can appear square in software captures. Production selects FemtoVG. Do not redraw screenshots to hide that difference or treat these captures as native-renderer proof.

## Build storage

Removed 103.5 GiB of accumulated package build outputs using `cargo clean --package neuromita` after disk exhaustion interrupted a GUI test build. `.cargo/config.toml` disables incremental compilation to prevent accumulation of per-edit caches. Keep a final executable; clear disposable package outputs between review cycles. Do not remove vendor sources, local settings or user data when cleaning builds.

After validation, another package clean removed 17.7 GiB of test/intermediate outputs before rebuilding the final binaries. Dependency compilation caches remain reusable; no source/vendor/user settings were removed.
