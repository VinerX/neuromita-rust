# API key editor correction

The API key field displays the actual resolved key, masked by default, with an
eye toggle and direct editing. Credential references such as `local:python_preset_18`
remain internal and are not shown as the editable key. Imported secrets were not
lost and their files are not rewritten by this UI correction.

Core resolves keys for the editor through a redacted-Debug `SecretKey` read model.
Missing credentials produce an explicit editor error. User-entered keys are saved
to the local credential store; the preset catalog retains references. Saving an
unchanged key keeps its current reference. Replacement creates a fresh entry so
failure to save a preset cannot overwrite the key used by another preset.
Empty keys explicitly select no authentication for supporting protocols; Gemini
rejects that configuration.

The regression test covers reading, replacing, reopening and keeping secret
values out of catalog JSON and Debug output. No live API request is needed.

Integrated into the active audio/layout branch on 2026-11-05 from `271ee95f`.
The Python `api_settings/ui.py` uses a separate transparent 30×40 QToolButton
beside the password input. Slint now follows that structure, without the boxed
SettingsButton from the imported branch. The eye toggles password rendering;
the editable value is the resolved key, never its credential reference.

Delivery verification: 551 default tests (two ignored), 10 GUI tests, default
and GUI all-target Clippy and fmt pass. The new binary was linked successfully
as `target/debug/deps/neuromita_gui.exe`; promotion to the usual executable was
blocked by Windows because the old GUI was running. A byte-identical copy is
delivered as `target/debug/neuromita-gui-updated.exe`. The running app was not closed.
