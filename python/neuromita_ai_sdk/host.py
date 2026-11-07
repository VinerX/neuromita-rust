"""Single-process plugin executor; OS process ownership and IPC belong to Rust.

Explicit runtime selection is optional for API v1 plugins. Opted-in plugins expose
runtime_capabilities(): a list of {backend, devices: [...], precisions: [...]},
initialize_runtime(settings, runtime): the normal model_ready result, and
runtime_status(): {backend, device, precision} read from the loaded runtime/model,
not copied from settings. All three are required before a selected model is ready.
Legacy initialize(settings) remains available when no runtime profile is supplied.
"""

import argparse
import base64
import uuid
import contextlib
import importlib.util
import json
import os
from pathlib import Path
import sys
from typing import Any

from neuromita_ai_sdk import API_VERSION, InstallPlan, validate_settings
from neuromita_ai_sdk.constraints import inspect_constraints, enforce_constraints
from neuromita_ai_sdk import hardware


class PluginHost:
    def __init__(self, plugins_dir: Path):
        self.plugins_dir = plugins_dir.resolve()
        self.active_by_kind: dict[str, Any] = {}
        self.active_ids_by_kind: dict[str, str] = {}
        self.active_runtime_by_kind: dict[str, Any] = {}
        self.audio_streams: dict[str, tuple[bytes, int]] = {}

    def _manifest(self, directory: Path) -> dict[str, Any]:
        if directory.is_symlink() or not directory.is_dir():
            raise ValueError("plugin directory is not a real directory")
        path = directory / "plugin.json"
        if path.is_symlink():
            raise ValueError("plugin manifest symlinks are forbidden")
        manifest = json.loads(path.read_text(encoding="utf-8"))
        if not isinstance(manifest, dict):
            raise ValueError("plugin manifest must be an object")
        plugin_id = manifest.get("id")
        if plugin_id != directory.name or not isinstance(plugin_id, str) or not plugin_id.isascii() or not plugin_id.replace("-", "").replace("_", "").isalnum():
            raise ValueError("plugin id must match its safe directory name")
        if manifest.get("api_version") != API_VERSION:
            raise ValueError("incompatible plugin API version")
        if manifest.get("kind") not in ("tts", "asr", "embedding", "reranker"):
            raise ValueError("unsupported plugin kind")
        if not isinstance(manifest.get("name"), str) or not manifest["name"]:
            raise ValueError("plugin name is required")
        if manifest.get("entrypoint") != "plugin.py":
            raise ValueError("entrypoint must be plugin.py")
        return manifest

    def discover(self) -> list[dict[str, Any]]:
        if not self.plugins_dir.exists():
            return []
        result = []
        for directory in sorted(self.plugins_dir.iterdir()):
            if directory.name.startswith(".") or not directory.is_dir():
                continue
            try:
                manifest = self._manifest(directory)
                result.append({**manifest, "discovery_status": "available"})
            except (ValueError, OSError, json.JSONDecodeError) as error:
                result.append({"id": directory.name, "discovery_status": "invalid", "error": str(error)})
        return result

    def _load(self, plugin_id: str) -> Any:
        if not isinstance(plugin_id, str) or not plugin_id.isascii() or not plugin_id.replace("-", "").replace("_", "").isalnum():
            raise ValueError("invalid plugin id")
        directory = self.plugins_dir / plugin_id
        self._manifest(directory)
        entrypoint = directory / "plugin.py"
        if entrypoint.is_symlink() or not entrypoint.is_file():
            raise ValueError("plugin entrypoint must be a regular file")
        spec = importlib.util.spec_from_file_location(
            "neuromita_plugin_" + plugin_id, entrypoint,
            submodule_search_locations=[str(directory)],
        )
        if spec is None or spec.loader is None:
            raise ValueError("cannot load plugin entrypoint")
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        with contextlib.redirect_stdout(sys.stderr):
            spec.loader.exec_module(module)
            plugin = module.Plugin()
        for operation in ("describe", "install_plan", "probe_install", "settings_schema", "initialize", "infer", "unload"):
            if not callable(getattr(plugin, operation, None)):
                raise ValueError(f"plugin missing {operation} contract")
        return plugin

    def _load_install_contract(self, plugin_id: str) -> Any:
        if not isinstance(plugin_id, str) or not plugin_id.isascii() or not plugin_id.replace("-", "").replace("_", "").isalnum():
            raise ValueError("invalid plugin id")
        directory = self.plugins_dir / plugin_id
        self._manifest(directory)
        entrypoint = directory / "installplan.py"
        if not entrypoint.exists():
            return self._load(plugin_id)
        if entrypoint.is_symlink() or not entrypoint.is_file():
            raise ValueError("install contract must be a regular file")
        spec = importlib.util.spec_from_file_location("neuromita_install_" + plugin_id, entrypoint)
        if spec is None or spec.loader is None:
            raise ValueError("cannot load install contract")
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        with contextlib.redirect_stdout(sys.stderr):
            spec.loader.exec_module(module)
            contract = module.InstallContract()
        for method in ("install_plan", "probe_install", "settings_schema"):
            if not callable(getattr(contract, method, None)):
                raise ValueError(f"install contract missing {method}")
        return contract

    def dispatch(self, op: str, args: dict[str, Any]) -> Any:
        if op == "configuration_constraints":
            plugin = self._load_install_contract(args["plugin_id"])
            with contextlib.redirect_stdout(sys.stderr):
                settings = validate_settings(plugin.settings_schema(), args["settings"])
                return inspect_constraints(plugin, settings, args.get("runtime"), hardware.probe())
        if op == "discover":
            return self.discover()
        if op == "hardware":
            with contextlib.redirect_stdout(sys.stderr):
                return hardware.probe()
        if op == "execute":
            with contextlib.redirect_stdout(sys.stderr):
                return hardware.execute(args["backend"], args["device"], args["precision"],
                                        model_path=args.get("model_path"), tokens=args.get("tokens"))
        if op == "status":
            return {
                "services": {
                    kind: {
                        "plugin_id": self.active_ids_by_kind[kind],
                        "model_ready": True,
                        "runtime": self.active_runtime_by_kind.get(kind),
                    }
                    for kind in self.active_by_kind
                }
            }
        if op in ("describe", "install_plan", "probe_install", "settings_schema", "validate_config"):
            plugin = (self._load_install_contract(args["plugin_id"])
                      if op in ("install_plan", "probe_install", "settings_schema", "validate_config")
                      else self._load(args["plugin_id"]))
            with contextlib.redirect_stdout(sys.stderr):
                if op == "validate_config":
                    result = validate_settings(plugin.settings_schema(), args.get("settings", {}))
                elif op in ("install_plan", "probe_install") and args.get("settings"):
                    operation = getattr(plugin, op + "_for_settings", None)
                    if not callable(operation):
                        raise ValueError("plugin does not support model-specific install settings")
                    result = operation(args["settings"])
                else:
                    result = getattr(plugin, op)()
            if op == "install_plan":
                if not isinstance(result, InstallPlan):
                    raise ValueError("install_plan must return InstallPlan")
                return result.to_wire()
            return result
        if op == "initialize":
            plugin_id = args["plugin_id"]
            kind = self._manifest(self.plugins_dir / plugin_id)["kind"]
            if kind in self.active_by_kind:
                raise ValueError("service already has an active plugin; unload first")
            plugin = self._load(plugin_id)
            settings = args.get("settings", {})
            if not isinstance(settings, dict):
                raise ValueError("settings must be an object")
            runtime = args.get("runtime")
            confirmed_runtime = None
            if runtime is not None:
                with contextlib.redirect_stdout(sys.stderr):
                    runtime = hardware.verify_runtime(runtime)
                if not all(callable(getattr(plugin, method, None))
                           for method in ("runtime_capabilities", "initialize_runtime", "runtime_status")):
                    raise ValueError("plugin does not support explicit runtime selection")
                with contextlib.redirect_stdout(sys.stderr):
                    capabilities = plugin.runtime_capabilities()
                if not isinstance(capabilities, list) or not any(
                    isinstance(item, dict) and item.get("backend") == runtime["backend"]
                    and isinstance(item.get("devices"), list)
                    and runtime["device"] in item["devices"]
                    and isinstance(item.get("precisions"), list)
                    and runtime["precision"] in item["precisions"]
                    for item in capabilities
                ):
                    raise ValueError("plugin did not declare the selected backend, device and precision")
            contract = (self._load_install_contract(plugin_id)
                        if (self.plugins_dir / plugin_id / "installplan.py").exists() else plugin)
            providers = [contract] if contract is plugin else [contract, plugin]
            for provider in providers:
                if callable(getattr(provider, "configuration_constraints", None)):
                    with contextlib.redirect_stdout(sys.stderr):
                        settings = validate_settings(provider.settings_schema(), settings)
                        restrictions = inspect_constraints(provider, settings, runtime, hardware.probe())
                        enforce_constraints(restrictions, settings, runtime)
            try:
                with contextlib.redirect_stdout(sys.stderr):
                    if runtime is None:
                        result = plugin.initialize(settings)
                    else:
                        result = plugin.initialize_runtime(dict(settings), dict(runtime))
                        observed = plugin.runtime_status()
                        if not isinstance(observed, dict) or observed != runtime:
                            raise ValueError("plugin actual runtime does not match selected runtime")
                        confirmed_runtime = dict(observed)
                if not isinstance(result, dict):
                    raise ValueError("plugin initialize must return an object")
                if result.get("model_ready") is not True:
                    raise ValueError("plugin did not confirm model readiness")
            except Exception:
                with contextlib.redirect_stdout(sys.stderr):
                    try:
                        plugin.unload()
                    except Exception:
                        pass
                raise
            self.active_by_kind[kind] = plugin
            self.active_ids_by_kind[kind] = plugin_id
            self.active_runtime_by_kind[kind] = confirmed_runtime
            if confirmed_runtime is not None:
                result = {**result, "runtime": dict(confirmed_runtime)}
            return result
        if op == "infer_stream_start":
            if self.audio_streams:
                raise ValueError("another audio stream has not been drained or cancelled")
            if args.get("kind") != "tts":
                raise ValueError("audio streaming requires initialized TTS")
            plugin = self.active_by_kind.get("tts")
            if plugin is None:
                raise ValueError("TTS is not initialized")
            input_data = args.get("input", {})
            if not isinstance(input_data, dict):
                raise ValueError("input must be an object")
            with contextlib.redirect_stdout(sys.stderr):
                value = plugin.infer(input_data)
            if not isinstance(value, dict) or value.get("format") != "wav":
                raise ValueError("stream source must return WAV")
            encoded = value.get("audio_base64")
            if not isinstance(encoded, str) or len(encoded) > 90_000_000:
                raise ValueError("stream source exceeds maximum size")
            try:
                audio = base64.b64decode(encoded, validate=True)
            except (ValueError, base64.binascii.Error) as error:
                raise ValueError("stream source returned invalid base64") from error
            if len(audio) < 44 or len(audio) > 64 * 1024 * 1024 or audio[:4] != b"RIFF" or audio[8:12] != b"WAVE":
                raise ValueError("stream source returned invalid WAV")
            token = uuid.uuid4().hex
            self.audio_streams[token] = (audio, 0)
            return {"stream_id": token, "length": len(audio), "format": "wav"}
        if op == "infer_stream_next":
            token = args.get("stream_id")
            if not isinstance(token, str) or token not in self.audio_streams:
                raise ValueError("unknown audio stream")
            audio, offset = self.audio_streams[token]
            size = 48 * 1024
            chunk = audio[offset:offset + size]
            offset += len(chunk)
            done = offset == len(audio)
            if done:
                del self.audio_streams[token]
            else:
                self.audio_streams[token] = (audio, offset)
            return {"stream_id": token, "chunk_base64": base64.b64encode(chunk).decode("ascii"), "done": done}
        if op == "infer_stream_cancel":
            token = args.get("stream_id")
            if not isinstance(token, str) or token not in self.audio_streams:
                raise ValueError("unknown audio stream")
            del self.audio_streams[token]
            return {"cancelled": True}
        if op == "infer":
            kind = args.get("kind")
            plugin = self.active_by_kind.get(kind)
            if plugin is None:
                raise ValueError("requested service is not initialized")
            input_data = args.get("input", {})
            if not isinstance(input_data, dict):
                raise ValueError("input must be an object")
            with contextlib.redirect_stdout(sys.stderr):
                return plugin.infer(input_data)
        if op == "active_settings_schema":
            kind = args.get("kind")
            plugin = self.active_by_kind.get(kind)
            if plugin is None:
                raise ValueError("requested service is not initialized")
            with contextlib.redirect_stdout(sys.stderr):
                return plugin.settings_schema()
        if op == "unload":
            if self.audio_streams:
                raise ValueError("cancel the active stream before unloading")
            kind = args.get("kind")
            plugin = self.active_by_kind.get(kind)
            if plugin is not None:
                with contextlib.redirect_stdout(sys.stderr):
                    plugin.unload()
                self.active_by_kind.pop(kind)
                self.active_ids_by_kind.pop(kind)
                self.active_runtime_by_kind.pop(kind, None)
            return {"unloaded": True}
        raise ValueError("unsupported host operation")


def serve(host: PluginHost) -> None:
    print(json.dumps({"type": "hello", "api_version": API_VERSION}), flush=True)
    for line in sys.stdin:
        request_id: Any = None
        try:
            if len(line) > 32 * 1024 * 1024:
                raise ValueError("request exceeds 32 MiB")
            request = json.loads(line)
            if not isinstance(request, dict):
                raise ValueError("request must be an object")
            request_id = request.get("id")
            if not isinstance(request_id, str) or not request_id:
                raise ValueError("request id is required")
            args = request.get("args", {})
            if not isinstance(args, dict):
                raise ValueError("args must be an object")
            result = host.dispatch(request.get("op"), args)
            response = {"id": request_id, "ok": True, "result": result}
        except Exception as error:
            response = {"id": request_id, "ok": False, "error": {"kind": type(error).__name__, "message": str(error)}}
        print(json.dumps(response, ensure_ascii=False, separators=(",", ":")), flush=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--plugins-dir", type=Path, required=True)
    args = parser.parse_args()
    serve(PluginHost(args.plugins_dir))


if __name__ == "__main__":
    main()
