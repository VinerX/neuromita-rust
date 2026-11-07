# AGENTS.md

This file provides guidance to AI coding agents when working with code in this repository.

## Project Overview

zcheapstr provides `CheapStr`, an immutable, cheaply cloneable string type with borrowed, static,
and shared (`Arc`-backed) variants, similar to `Cow<'_, str>` but specialized for strings. It's a
single-crate, `no_std` library (only requires `alloc`), extracted from and re-exported by the
zvariant crate.

## Conventions

- **Commit style**: gimoji emoji-prefixed commit messages, enforced by commitlint via
  `@gimoji/commitlint-config-gimoji`.
- **Formatting**: `cargo +nightly fmt` (the repo's `.rustfmt.toml` uses nightly-only options).
- **MSRV**: 1.87.
- **Line length**: 100 chars, in code and docs alike.
- **Changelog**: `CHANGELOG.md` is managed by [release-plz] — do not hand-edit it. Write a good
  commit message and release-plz will generate the entry at release time.
- **Changelog-skip trailer**: end a commit message with a `Changelog: skip` git trailer to
  keep it out of the user-facing changelog (use for AI-workflow artifacts such as design docs
  and implementation plans).

[release-plz]: https://release-plz.ieni.dev/
