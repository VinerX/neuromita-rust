"""Dynamic plugin configuration restrictions; no setting is rewritten."""

from typing import Any, Literal, TypedDict
import math


class ConfigurationContext(TypedDict):
    settings: dict[str, Any]
    runtime: dict[str, Any] | None
    hardware: dict[str, Any]


class ConfigurationConstraint(TypedDict):
    field: str
    value: str | bool | int | float | None
    severity: Literal["warning", "forbidden"]
    code: str
    message: str


class DynamicConfigurationUnavailable(ValueError):
    """The plugin does not declare dynamic restrictions; this is not an empty result."""


def validate_constraints(value: Any) -> list[dict[str, Any]]:
    if not isinstance(value, list) or len(value) > 256:
        raise ValueError("configuration_constraints must return a bounded list")
    output = []
    for entry in value:
        if not isinstance(entry, dict) or set(entry) != {"field", "value", "severity", "code", "message"}:
            raise ValueError("invalid configuration constraint fields")
        if any(not isinstance(entry[key], str) or not entry[key] or len(entry[key].encode("utf-8")) > 2048
               for key in ("field", "severity", "code", "message")):
            raise ValueError("invalid configuration constraint text")
        if entry["severity"] not in {"warning", "forbidden"}:
            raise ValueError("invalid configuration constraint severity")
        scope, separator, field = entry["field"].partition(".")
        if not separator or scope not in {"settings", "runtime"} or not field or "." in field:
            raise ValueError("invalid configuration constraint field")
        if not isinstance(entry["value"], (str, bool, int, float, type(None))):
            raise ValueError("constraint value must be scalar")
        if isinstance(entry["value"], float) and not math.isfinite(entry["value"]):
            raise ValueError("constraint value must be finite")
        output.append(dict(entry))
    return output


def inspect_constraints(plugin: Any, settings: dict, runtime: dict | None, hardware: dict) -> list[dict]:
    if not isinstance(settings, dict) or (runtime is not None and not isinstance(runtime, dict)):
        raise ValueError("constraint context must contain settings/runtime objects")
    operation = getattr(plugin, "configuration_constraints", None)
    if not callable(operation):
        raise DynamicConfigurationUnavailable("plugin does not declare dynamic configuration constraints")
    return validate_constraints(operation({"settings": settings, "runtime": runtime, "hardware": hardware}))


def enforce_constraints(constraints: list[dict], settings: dict, runtime: dict | None) -> None:
    for constraint in validate_constraints(constraints):
        scope, separator, field = constraint["field"].partition(".")
        if not separator or scope not in {"settings", "runtime"} or not field or "." in field:
            raise ValueError("constraint field must identify settings.<field> or runtime.<field>")
        values = settings if scope == "settings" else runtime or {}
        if (constraint["severity"] == "forbidden" and field in values
                and type(values[field]) is type(constraint["value"])
                and values[field] == constraint["value"]):
            raise ValueError(f"{constraint['code']}: {constraint['message']}")
