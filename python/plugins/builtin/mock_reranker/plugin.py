"""Deterministic reranking fixture, without model inference."""

from neuromita_ai_sdk import InstallPlan, validate_settings

class Plugin:
    def __init__(self):
        self.options = None

    def describe(self):
        return {"kind": "reranker", "protocol": "fixture-rerank-v1"}

    def settings_schema(self):
        return {"model_id": {"type": "string", "default": "rerank-fixture"}}

    def install_plan(self):
        return InstallPlan(supported_backends=("remote-api",))

    def probe_install(self):
        return {"installed": True, "missing": []}

    def initialize(self, settings):
        self.options = validate_settings(self.settings_schema(), settings)
        return {"model_ready": True, "model": self.options["model_id"]}

    def infer(self, input_data):
        if self.options is None:
            raise RuntimeError("reranker is not initialized")
        query, candidates = input_data.get("query"), input_data.get("candidates")
        if not isinstance(query, str) or not query or not isinstance(candidates, list) or not 1 <= len(candidates) <= 32 or any(not isinstance(t, str) or not t for t in candidates):
            raise ValueError("invalid reranking request")
        scores = [1.0 if query in item else 0.1 for item in candidates]
        return {"model": self.options["model_id"], "protocol": "fixture-rerank-v1", "scores": scores}

    def unload(self):
        self.options = None
