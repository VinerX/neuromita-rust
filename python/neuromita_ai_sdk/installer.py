"""Short-lived, explicitly approved plugin asset installation subprocess."""

import argparse
import contextlib
import importlib.util
import json
import platform
from pathlib import Path
import sys

from .host import PluginHost


def execute(plugins_dir: Path, plugin_id: str, hook_id: str, source_plugin_id: str,
            source_root: Path, output_root: Path, settings: dict) -> None:
    host = PluginHost(plugins_dir)
    host._manifest(host.plugins_dir / plugin_id)
    plan = host.dispatch("install_plan", {"plugin_id": plugin_id, "settings": settings})
    allowed = plan.get("asset_hooks", [])
    if not any(hook.get("id") == hook_id and hook.get("source_plugin_id") == source_plugin_id for hook in allowed):
        raise ValueError("asset hook is not declared by selected plugin")
    if not source_root.is_dir() or not output_root.is_dir() or source_root.is_symlink() or output_root.is_symlink():
        raise ValueError("installer requires real staged source and output directories")
    path = host.plugins_dir / plugin_id / "install.py"
    if path.is_symlink() or not path.is_file():
        raise ValueError("installer entrypoint must be a regular file")
    spec = importlib.util.spec_from_file_location("neuromita_install_hook_" + plugin_id, path)
    if spec is None or spec.loader is None:
        raise ValueError("invalid installer entrypoint")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    with contextlib.redirect_stdout(sys.stderr):
        spec.loader.exec_module(module)
        function = getattr(module, "execute", None)
        if not callable(function):
            raise ValueError("installer entrypoint has no execute method")
        function(hook_id, source_root, output_root, settings)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--plugins-dir", type=Path, required=True)
    parser.add_argument("--plugin-id", required=True)
    parser.add_argument("--hook-id", required=True)
    parser.add_argument("--source-plugin-id", required=True)
    parser.add_argument("--source-root", type=Path, required=True)
    parser.add_argument("--output-root", type=Path, required=True)
    parser.add_argument("--settings", required=True)
    parser.add_argument("--python-version", required=True)
    args = parser.parse_args()
    if platform.python_version() != args.python_version:
        raise ValueError("installer Python does not match the locked Python version")
    settings = json.loads(args.settings)
    if not isinstance(settings, dict):
        raise ValueError("install settings must be an object")
    execute(args.plugins_dir, args.plugin_id, args.hook_id, args.source_plugin_id,
            args.source_root, args.output_root, settings)


if __name__ == "__main__":
    main()
