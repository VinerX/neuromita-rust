# Four settings pages and local startup repair

The user explicitly requested merging `codex/settings-four-pages-20261104`
(`dd50806f`). It is now included: Language, Game, Models and Images layouts.
Language selection reaches the existing shell localization callback. Game, Models
and Images still contain local draft controls without complete Core bindings;
merging their layouts does not complete those functional administration features.

The reported startup failure was caused by the local settings file retaining
`schema_version` and lacking `interface.visibility` and `interface.hide_private`.
The local file was backed up, those fields were explicitly corrected, and the
candidate was validated and installed through the current settings import command.
Existing character, preset, voice and other values were preserved. Neither local
file nor backup is tracked. No automatic legacy migration or fallback was added.

An empty character read model no longer invokes localization with an empty key.
It remains empty until the Core presenter supplies a catalog.

The ordinary `neuromita-gui` executable already starts ApplicationLifecycle on
its runtime thread, observes the real GuiPresenter and submits commands to it.
`examples/gui_snapshot.rs` is a separate fixture renderer used for visual checks.
The settings import validates the repaired local document; it is not a claim of
native UI acceptance or a live generation test.

The follow-up reference clarifies activity placement: the dot follows the preset
name, and the active badge follows the header title, in the same centered title
row. They no longer sit at the far right of the two-line group. Added missing
English translations for the newly merged pages.
