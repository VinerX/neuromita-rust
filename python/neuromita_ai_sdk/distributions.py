"""Probe installed distributions without importing inference engines or their dependencies."""

from __future__ import annotations

import importlib.metadata


def check_distributions(names: tuple[str, ...], *, exact_versions: dict[str, str] | None = None) -> list[str]:
    exact = exact_versions or {}
    missing = []
    for name in names:
        try:
            installed = importlib.metadata.version(name)
        except importlib.metadata.PackageNotFoundError:
            missing.append(f"distribution:{name}")
            continue
        version = exact.get(name)
        if version is not None and installed != version:
            missing.append(f"distribution:{name} expected {version}, found {installed}")
    return missing
