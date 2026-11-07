"""Typed, side-effect-free configuration validation shared by the plugin host and adapters."""

from __future__ import annotations

import math
from typing import Any, Mapping


def validate_settings(schema: Mapping[str, Mapping[str, Any]], values: Mapping[str, Any], *, include_defaults: bool = True) -> dict[str, Any]:
    if not isinstance(values, dict):
        raise ValueError("settings must be an object")
    if not isinstance(schema, dict):
        raise ValueError("plugin settings schema must be an object")
    unknown = set(values) - set(schema)
    if unknown:
        raise ValueError(f"unknown settings: {sorted(unknown)}")
    output = {}
    for key, definition in schema.items():
        if not isinstance(key, str) or not isinstance(definition, dict):
            raise ValueError("invalid settings schema entry")
        if key not in values and (not include_defaults or "default" not in definition):
            continue
        value = values[key] if key in values else definition["default"]
        kind = definition.get("type")
        accepted = {
            "string": lambda v: isinstance(v, str),
            "boolean": lambda v: isinstance(v, bool),
            "integer": lambda v: isinstance(v, int) and not isinstance(v, bool),
            "number": lambda v: isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v),
        }
        if kind not in accepted or not accepted[kind](value):
            raise ValueError(f"invalid {kind} for {key}")
        if "choices" in definition and value not in definition["choices"]:
            raise ValueError(f"invalid {key}: {value}")
        if kind in ("integer", "number"):
            if "min" in definition and value < definition["min"]:
                raise ValueError(f"{key} below minimum")
            if "max" in definition and value > definition["max"]:
                raise ValueError(f"{key} above maximum")
        output[key] = value
    return output
