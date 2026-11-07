"""Offline Transformer embeddings for Rust's snapshot-aware knowledge retriever."""

from __future__ import annotations

import math
from pathlib import Path

from neuromita_ai_sdk import AssetRequirement, InstallPlan, validate_settings
from neuromita_ai_sdk.assets import selected_model_root
from neuromita_ai_sdk.distributions import check_distributions


PLUGIN_ID = "local_transformers_embedding"


def schema() -> dict:
    return {
        "model_id": {"type": "string"},
        "dimensions": {"type": "integer", "min": 1, "max": 3072},
        "device": {"type": "string", "default": "cpu"},
        "pooling": {"type": "string", "default": "mean", "choices": ["mean", "last_token"]},
        "query_prefix": {"type": "string", "default": ""},
        "document_prefix": {"type": "string", "default": ""},
        "batch_size": {"type": "integer", "default": 8, "min": 1, "max": 32},
    }


def plan() -> InstallPlan:
    return InstallPlan(
        requirements=("numpy==1.26.0", "transformers>=4.40,<5", "torch>=2.3,<3", "safetensors>=0.4,<1"),
        supported_backends=("torch-cpu", "torch-cuda"),
        supported_python_minors=("3.11", "3.12"),
        assets=(AssetRequirement("hf_config", "config.json"),),
    )


def probe() -> dict:
    missing = check_distributions(("numpy", "transformers", "torch", "safetensors"), exact_versions={"numpy": "1.26.0"})
    try:
        root = selected_model_root(PLUGIN_ID)
        config = root / "config.json"
        if config.is_symlink() or not config.is_file() or config.stat().st_size == 0:
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
        self.options = None
        self.torch = None

    def describe(self):
        return {"id": PLUGIN_ID, "kind": "embedding", "protocol": "local-transformers-v1"}

    def settings_schema(self):
        return schema()

    def install_plan(self):
        return plan()

    def probe_install(self):
        return probe()

    def initialize(self, settings: dict):
        options = validate_settings(schema(), settings)
        if not options.get("model_id") or not options.get("dimensions"):
            raise ValueError("model_id and dimensions must be explicitly configured")
        if any(len(options[key]) > 256 for key in ("model_id", "query_prefix", "document_prefix", "device")):
            raise ValueError("embedding identity/prefix/device exceeds limit")
        if self.model is not None:
            raise ValueError("embedding model already initialized")
        status = self.probe_install()
        if not status["installed"]:
            raise RuntimeError(f"embedding dependencies/assets missing: {status['missing']}")
        from transformers import AutoTokenizer, AutoModel
        import torch
        root = selected_model_root(PLUGIN_ID)
        tokenizer = AutoTokenizer.from_pretrained(root, local_files_only=True, trust_remote_code=False)
        model = AutoModel.from_pretrained(root, local_files_only=True, trust_remote_code=False)
        model.to(options["device"])
        model.eval()
        self.tokenizer = tokenizer
        self.model = model
        self.torch = torch
        self.options = options
        return {"model_ready": True, "model": options["model_id"], "dimensions": options["dimensions"]}

    def infer(self, input_data):
        if self.model is None or self.options is None:
            raise RuntimeError("embedding model is not initialized")
        texts = input_data.get("texts")
        purpose = input_data.get("purpose")
        if purpose not in ("query", "document") or not isinstance(texts, list) or not 1 <= len(texts) <= 32:
            raise ValueError("expected bounded texts and query/document purpose")
        if any(not isinstance(t, str) or not t.strip() or len(t) > 8192 for t in texts):
            raise ValueError("invalid embedding text")
        prefix = self.options["query_prefix" if purpose == "query" else "document_prefix"]
        vectors = []
        torch = self.torch
        with torch.inference_mode():
            for start in range(0, len(texts), self.options["batch_size"]):
                batch = [prefix + t for t in texts[start:start + self.options["batch_size"]]]
                tokens = self.tokenizer(batch, padding=True, truncation=True, return_tensors="pt", max_length=512)
                tokens = {key: value.to(self.options["device"]) for key, value in tokens.items()}
                output = self.model(**tokens)
                hidden = output.last_hidden_state
                mask = tokens["attention_mask"]
                if self.options["pooling"] == "last_token":
                    index = mask.sum(dim=1).long().clamp(min=1) - 1
                    pooled = hidden[torch.arange(hidden.shape[0], device=hidden.device), index]
                else:
                    weights = mask.unsqueeze(-1).to(hidden.dtype)
                    pooled = (hidden * weights).sum(1) / weights.sum(1).clamp(min=1)
                pooled = torch.nn.functional.normalize(pooled.float(), p=2, dim=1)
                values = pooled.cpu().tolist()
                for row in values:
                    if len(row) != self.options["dimensions"] or not all(math.isfinite(v) for v in row):
                        raise ValueError("embedding dimensions or values mismatch")
                vectors.extend(values)
        return {"model": self.options["model_id"], "dimensions": self.options["dimensions"],
                "protocol": "local-transformers-v1", "vectors": vectors}

    def unload(self):
        self.tokenizer = None
        self.model = None
        self.options = None
        self.torch = None
