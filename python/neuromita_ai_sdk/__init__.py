"""Versioned Python-side contracts for independently installed NeuroMita AI plugins."""

from dataclasses import dataclass, field
from typing import Any, Protocol

API_VERSION = 1


@dataclass(frozen=True)
class PackageRequirement:
    name: str
    version: str


@dataclass(frozen=True)
class AssetRequirement:
    role: str
    relative_path: str
    required: bool = True
    sha256: str | None = None


@dataclass(frozen=True)
class InstallerAction:
    id: str
    kind: str
    relative_path: str


@dataclass(frozen=True)
class AssetHook:
    id: str
    source_plugin_id: str


@dataclass(frozen=True)
class InstallPlan:
    packages: tuple[PackageRequirement, ...] = ()
    assets: tuple[AssetRequirement, ...] = ()
    backend: str | None = None
    supported_backends: tuple[str, ...] = ()
    required_backends: tuple[str, ...] = ()
    requirements: tuple[str, ...] = ()
    supported_python_minors: tuple[str, ...] = ()
    actions: tuple[InstallerAction, ...] = ()
    asset_hooks: tuple[AssetHook, ...] = ()

    def to_wire(self) -> dict[str, Any]:
        return {
            "packages": [vars(item) for item in self.packages],
            "assets": [vars(item) for item in self.assets],
            "backend": self.backend,
            "supported_backends": list(self.supported_backends),
            "required_backends": list(self.required_backends),
            "requirements": list(self.requirements),
            "supported_python_minors": list(self.supported_python_minors),
            "actions": [vars(action) for action in self.actions],
            "asset_hooks": [vars(hook) for hook in self.asset_hooks],
        }


class AiPlugin(Protocol):
    def describe(self) -> dict[str, Any]: ...
    def install_plan(self) -> InstallPlan: ...
    def probe_install(self) -> dict[str, Any]: ...
    def settings_schema(self) -> dict[str, Any]: ...
    def initialize(self, settings: dict[str, Any]) -> dict[str, Any]: ...
    def infer(self, input_data: dict[str, Any]) -> dict[str, Any]: ...
    def unload(self) -> None: ...

from .settings import validate_settings
from .constraints import ConfigurationContext, ConfigurationConstraint, validate_constraints, enforce_constraints


class DynamicConfigurationContract(Protocol):
    """Optional plugin capability, exposed by InstallContract or an unsplit Plugin.

    Receives settings, requested runtime and actual worker hardware. Returns
    scalar-value restrictions with field, value, severity, code and message.
    Inspection does not initialize a model. Host enforces forbidden values again
    immediately before initialization, using fresh worker facts.
    """

    def configuration_constraints(self, context: ConfigurationContext) -> list[ConfigurationConstraint]: ...
