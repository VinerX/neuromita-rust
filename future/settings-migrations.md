# Settings: current strict Rust development format

The Rust prototype has one `StoredSettings` representation: `revision` and the complete, typed `settings` document. The required AppSettings sections are `presets`, `characters`, `voice`, `capture`, `interface`, and `reminders`.

Issue #75 intentionally removes `schema_version` and old Rust dev-schema upgrades v1→v2→v3→v4, legacy character aliases (`mita`, `kind-mita`), and alternate SQLite observer queries. Import, export, and startup accept only the current representation; they reject missing, unknown, duplicate, ill-typed, and invalid values. Incompatible persisted files are never deleted or automatically rewritten.

`SettingsRepository::migrate()` is a startup validation hook for current settings only. The independent generic `MigrationSchema`, `MigrationStep`, `MigrationStore`, and `MigrationService` interfaces remain available to other modules and future explicitly designed migrations. Python settings import is a separate `NotImplemented` extension point, not a fallback. Settings transfers preserve the whole document and implement atomic replacement; live imports keep owner revision CAS and watch notifications.

Validation: `tests/settings_transfer.rs`, `tests/settings_cli.rs`, `tests/settings_migrations.rs`, `tests/migration_system.rs`.
