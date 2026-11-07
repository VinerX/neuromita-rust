# Rust settings import/export

`SettingsTransfer` contains one complete current Rust development `StoredSettings` object with `revision` and all required `AppSettings` sections. No top-level `schema_version` exists in this contract. Older Rust dev schemas, obsolete character aliases, partially specified documents and unknown fields are rejected explicitly, not migrated or replaced with defaults.

`SettingsApi::prepare_import` validates without altering state or files. `SettingsApi::import` applies the transfer via revision CAS against the current owner; presenter subscriptions observe the new state. Offline JSON import/export must run with the app stopped and uses atomic file replacement and sidecar locking. Failed validation or filesystem operations do not intentionally delete the original destination. Exports do not include preset bodies, secret credentials, Timeline, or Python settings. Python import remains unimplemented.

See `future/settings-migrations.md` and the settings tests for the current contract. Existing user files are not migrated automatically by issue #75.
