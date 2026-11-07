"""Deterministic finite-vector fixture; not a real semantic embedding model."""

from neuromita_ai_sdk import InstallPlan, validate_settings


class Plugin:
    def __init__(self):
        self.options = None

    def describe(self):
        return {"kind": "embedding", "protocol": "fixture-v1"}

    def settings_schema(self):
        return {"model_id": {"type": "string", "default": "fixture"}, "dimensions": {"type": "integer", "default": 3, "choices": [3]}}

    def install_plan(self):
        return InstallPlan(supported_backends=("remote-api",))

    def probe_install(self):
        return {"installed": True, "missing": []}

    def initialize(self, settings):
        self.options = validate_settings(self.settings_schema(), settings)
        return {"model_ready": True, "model": self.options["model_id"], "dimensions": 3}

    def infer(self, input_data):
        if self.options is None:
            raise RuntimeError("not initialized")
        texts = input_data.get("texts")
        if input_data.get("purpose") not in ("query", "document") or not isinstance(texts, list) or not 1 <= len(texts) <= 32:
            raise ValueError("bad embedding request")
        if any(not isinstance(t, str) or not t.strip() for t in texts):
            raise ValueError("nonempty text is required")
        vectors = [[1.0, 0.0, 0.0] if "чай" in t else [0.0, 1.0, 0.0] for t in texts]
        return {"model": self.options["model_id"], "dimensions": 3, "protocol": "fixture-v1", "vectors": vectors}

    def unload(self):
        self.options = None
