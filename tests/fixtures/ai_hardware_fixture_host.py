"""Controlled worker for Rust hardware tests; not a production host.

The fixture reads ``fixture.json`` from the plugins directory on every request
so a test can change capabilities or model behaviour between calls.
"""

import json
import os
import sys
import time

API_VERSION = 1


def load(plugins_dir: str) -> dict:
    with open(os.path.join(plugins_dir, "fixture.json"), encoding="utf-8") as handle:
        return json.load(handle)


class FixtureHost:
    def __init__(self, plugins_dir: str):
        self.plugins_dir = plugins_dir
        self.active = {}

    def dispatch(self, op: str, args: dict):
        fixture = load(self.plugins_dir)
        if op == "discover":
            return fixture.get("plugins", [])
        if op == "hardware":
            if fixture.get("hardware_error"):
                raise RuntimeError(fixture["hardware_error"])
            return fixture["hardware"]
        if op == "execute":
            if fixture.get("execute_error"):
                raise RuntimeError(fixture["execute_error"])
            backend = args.get("backend")
            device = args.get("device")
            kinds = {
                "remote_api": ("remote_api", "remote"),
                "torch_cpu": ("torch", "cpu"),
                "torch_cuda": ("torch", "cuda"),
                "onnx_cpu": ("onnxruntime", "cpu"),
                "onnx_cuda": ("onnxruntime", "cuda"),
                "onnx_directml": ("onnxruntime", "dml"),
            }
            if backend not in kinds:
                raise ValueError(f"unsupported backend {backend!r}")
            family, kind = kinds[backend]
            available = any(
                item.get("backend") == family
                and item.get("kind") == kind
                and item.get("id") == device
                for item in fixture["hardware"].get("devices", [])
            )
            if not available:
                raise RuntimeError(f"device {device} is not present")
            return {"verified": True, "detail": f"fixture executed on {device}"}
        if op == "initialize":
            plugin_id = args["plugin_id"]
            config = fixture.get("initialize", {}).get(plugin_id, {})
            marker = config.get("marker")
            if marker:
                with open(marker, "w", encoding="utf-8") as handle:
                    handle.write("started")
            delay = float(config.get("delay_ms", 0)) / 1000.0
            if delay > 0:
                time.sleep(delay)
            if config.get("error"):
                raise RuntimeError(config["error"])
            runtime = args.get("runtime")
            echo = config.get("runtime", "echo")
            if echo == "echo":
                runtime = runtime
            elif isinstance(echo, dict):
                runtime = echo
            else:
                runtime = None
            kind = next(
                item["kind"]
                for item in fixture.get("plugins", [])
                if item.get("id") == plugin_id
            )
            self.active[kind] = {"plugin_id": plugin_id, "runtime": runtime}
            result = {"model_ready": bool(config.get("ready", True)), "plugin_id": plugin_id}
            if runtime is not None:
                result["runtime"] = runtime
            return result
        if op == "status":
            return {
                "services": {
                    kind: {
                        "plugin_id": entry["plugin_id"],
                        "model_ready": True,
                        "runtime": entry["runtime"],
                    }
                    for kind, entry in self.active.items()
                }
            }
        if op == "infer":
            config = fixture.get("infer", {})
            delay = float(config.get("delay_ms", 0)) / 1000.0
            if delay > 0:
                time.sleep(delay)
            if config.get("error"):
                raise RuntimeError(config["error"])
            return {"text": config.get("text", "fixture")}
        if op == "unload":
            self.active.pop(args.get("kind"), None)
            return {"unloaded": True}
        raise ValueError("unsupported fixture operation")


def serve(plugins_dir: str) -> None:
    host = FixtureHost(plugins_dir)
    print(json.dumps({"type": "hello", "api_version": API_VERSION}), flush=True)
    for line in sys.stdin:
        request_id = None
        try:
            request = json.loads(line)
            request_id = request.get("id")
            result = host.dispatch(request.get("op"), request.get("args", {}))
            response = {"id": request_id, "ok": True, "result": result}
        except Exception as error:
            response = {
                "id": request_id,
                "ok": False,
                "error": {"kind": type(error).__name__, "message": str(error)},
            }
        print(json.dumps(response, ensure_ascii=False, separators=(",", ":")), flush=True)


def main() -> None:
    arguments = sys.argv[1:]
    plugins_dir = None
    for index, argument in enumerate(arguments):
        if argument == "--plugins-dir" and index + 1 < len(arguments):
            plugins_dir = arguments[index + 1]
    if plugins_dir is None:
        raise SystemExit("--plugins-dir is required")
    serve(plugins_dir)


if __name__ == "__main__":
    main()
