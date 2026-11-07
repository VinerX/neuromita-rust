"""Offline sequence-classifier cross-encoder scores; rank fusion stays in Rust."""

from __future__ import annotations

import math

from neuromita_ai_sdk import AssetRequirement, InstallPlan, validate_settings
from neuromita_ai_sdk.assets import selected_model_root
from neuromita_ai_sdk.distributions import check_distributions

PLUGIN_ID = "local_cross_encoder"


def schema():
    return {
        "model_id": {"type": "string"},
        "device": {"type": "string", "default": "cpu"},
        "batch_size": {"type": "integer", "default": 8, "min": 1, "max": 32},
    }


def plan():
    return InstallPlan(
        requirements=("numpy==1.26.0", "transformers>=4.40,<5", "torch>=2.3,<3", "safetensors>=0.4,<1"),
        assets=(AssetRequirement("hf_config", "config.json"),),
        supported_backends=("torch-cpu", "torch-cuda"),
        supported_python_minors=("3.11", "3.12"),
    )


def probe():
    missing = check_distributions(("numpy", "transformers", "torch", "safetensors"), exact_versions={"numpy": "1.26.0"})
    try:
        root = selected_model_root(PLUGIN_ID)
        if not (root / "config.json").is_file() or (root / "config.json").is_symlink():
            missing.append("asset:config.json")
        if not any(p.is_file() and not p.is_symlink() for p in root.glob("*.safetensors")) and not any(
            p.is_file() and not p.is_symlink() for p in root.glob("*.bin")
        ):
            missing.append("asset:model_weights")
    except (ValueError, OSError) as error:
        missing.append(f"model:{error}")
    return {"installed": not missing, "missing": missing}


class Plugin:
    def __init__(self):
        self.model = None
        self.tokenizer = None
        self.torch = None
        self.options = None

    def describe(self):
        return {"id": PLUGIN_ID, "kind": "reranker", "protocol": "local-cross-encoder-v1"}

    def install_plan(self):
        return plan()

    def probe_install(self):
        return probe()

    def settings_schema(self):
        return schema()

    def initialize(self, settings):
        options = validate_settings(schema(), settings)
        if not options.get("model_id") or len(options["model_id"]) > 256 or len(options["device"]) > 256:
            raise ValueError("explicit bounded model identity and device are required")
        if self.model is not None:
            raise RuntimeError("reranker already initialized")
        status = probe()
        if not status["installed"]:
            raise RuntimeError(f"reranker dependencies/assets missing: {status['missing']}")
        import torch
        from transformers import AutoTokenizer, AutoModelForSequenceClassification
        root = selected_model_root(PLUGIN_ID)
        tokenizer = AutoTokenizer.from_pretrained(root, local_files_only=True, trust_remote_code=False)
        model = AutoModelForSequenceClassification.from_pretrained(root, local_files_only=True, trust_remote_code=False)
        model.to(options["device"])
        model.eval()
        self.model = model
        self.tokenizer = tokenizer
        self.torch = torch
        self.options = options
        return {"model_ready": True, "model": options["model_id"]}

    def infer(self, payload):
        if self.model is None or self.options is None:
            raise RuntimeError("reranker is not initialized")
        query = payload.get("query")
        candidates = payload.get("candidates")
        if not isinstance(query, str) or not query.strip() or len(query) > 8192:
            raise ValueError("bounded query is required")
        if not isinstance(candidates, list) or not 1 <= len(candidates) <= 32 or any(
            not isinstance(t, str) or not t.strip() or len(t) > 8192 for t in candidates
        ):
            raise ValueError("one to 32 bounded candidates required")
        scores = []
        torch = self.torch
        with torch.inference_mode():
            for index in range(0, len(candidates), self.options["batch_size"]):
                batch = candidates[index:index + self.options["batch_size"]]
                tokens = self.tokenizer([query] * len(batch), batch, padding=True, truncation=True,
                                        max_length=512, return_tensors="pt")
                tokens = {key: value.to(self.options["device"]) for key, value in tokens.items()}
                logits = self.model(**tokens).logits.float()
                if logits.ndim != 2 or logits.shape[1] not in (1, 2):
                    raise ValueError("cross-encoder needs one or two classification logits")
                if logits.shape[1] == 1:
                    batch_scores = torch.sigmoid(logits[:, 0])
                else:
                    batch_scores = torch.softmax(logits, dim=-1)[:, 1]
                for score in batch_scores.cpu().tolist():
                    if not math.isfinite(score) or not 0 <= score <= 1:
                        raise ValueError("nonfinite cross-encoder score")
                    scores.append(score)
        return {"model": self.options["model_id"], "protocol": "local-cross-encoder-v1", "scores": scores}

    def unload(self):
        self.model = None
        self.tokenizer = None
        self.options = None
        self.torch = None
