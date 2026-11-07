"""Local Qwen3-style causal LM reranking: next-token yes/no probabilities."""

from __future__ import annotations

import math

from neuromita_ai_sdk import AssetRequirement, InstallPlan, validate_settings
from neuromita_ai_sdk.assets import selected_model_root
from neuromita_ai_sdk.distributions import check_distributions

PLUGIN_ID = "local_lm_reranker"
_INSTRUCTION = "Given a web search query, retrieve relevant passages that answer the query"
_PREFIX = (
    '<|im_start|>system\nJudge whether the Document meets the requirements based on the Query and '
    'the Instruct provided. Note that the answer can only be "yes" or "no".<|im_end|>\n'
    '<|im_start|>user\n'
)
_SUFFIX = '<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n'


def prompt(query: str, content: str) -> str:
    return f"{_PREFIX}<Instruct>: {_INSTRUCTION}\n<Query>: {query}\n<Document>: {content}{_SUFFIX}"


def schema() -> dict:
    return {
        "model_id": {"type": "string"},
        "device": {"type": "string", "default": "cpu", "choices": ["cpu", "cuda"]},
        "batch_size": {"type": "integer", "default": 8, "min": 1, "max": 16},
    }


def plan() -> InstallPlan:
    return InstallPlan(
        requirements=("numpy==1.26.0", "torch>=2.3,<3", "transformers>=4.51,<5", "safetensors>=0.4,<1"),
        assets=(AssetRequirement("hf_config", "config.json"),),
        supported_backends=("torch-cpu", "torch-cuda"),
        supported_python_minors=("3.11", "3.12"),
    )


def probe() -> dict:
    missing = check_distributions(("numpy", "torch", "transformers", "safetensors"), exact_versions={"numpy": "1.26.0"})
    try:
        root = selected_model_root(PLUGIN_ID)
        if not (root / "config.json").is_file() or (root / "config.json").is_symlink():
            missing.append("asset:config.json")
        if not any(x.is_file() and not x.is_symlink() for name in ("*.safetensors", "*.bin") for x in root.glob(name)):
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
        self.yes_id = None
        self.no_id = None

    def describe(self) -> dict:
        return {"id": PLUGIN_ID, "kind": "reranker", "protocol": "local-lm-reranker-v1"}

    def install_plan(self) -> InstallPlan:
        return plan()

    def probe_install(self) -> dict:
        return probe()

    def settings_schema(self) -> dict:
        return schema()

    def initialize(self, settings: dict) -> dict:
        if self.model is not None:
            raise RuntimeError("LM reranker already initialized")
        options = validate_settings(schema(), settings)
        if not options.get("model_id") or len(options["model_id"]) > 256:
            raise ValueError("explicit bounded model identity is required")
        status = probe()
        if not status["installed"]:
            raise RuntimeError(f"LM reranker dependencies/assets missing: {status['missing']}")
        import torch
        from transformers import AutoModelForCausalLM, AutoTokenizer

        if options["device"] == "cuda" and not torch.cuda.is_available():
            raise RuntimeError("CUDA selected but is unavailable")
        root = selected_model_root(PLUGIN_ID)
        tokenizer = AutoTokenizer.from_pretrained(root, local_files_only=True, trust_remote_code=False, padding_side="left")
        yes_id = tokenizer.convert_tokens_to_ids("yes")
        no_id = tokenizer.convert_tokens_to_ids("no")
        unk = getattr(tokenizer, "unk_token_id", None)
        if not all(isinstance(value, int) and value >= 0 and value != unk for value in (yes_id, no_id)) or yes_id == no_id:
            raise ValueError("selected LM tokenizer lacks distinct yes/no token IDs")
        dtype = torch.float16 if options["device"] == "cuda" else torch.float32
        model = AutoModelForCausalLM.from_pretrained(root, local_files_only=True, trust_remote_code=False, torch_dtype=dtype)
        model.to(options["device"])
        model.eval()
        self.model = model
        self.tokenizer = tokenizer
        self.torch = torch
        self.yes_id = yes_id
        self.no_id = no_id
        self.options = options
        return {"model_ready": True, "model": options["model_id"]}

    def infer(self, input_data: dict) -> dict:
        if self.model is None or self.options is None:
            raise RuntimeError("LM reranker is not initialized")
        query = input_data.get("query")
        candidates = input_data.get("candidates")
        if not isinstance(query, str) or not query.strip() or len(query) > 8192:
            raise ValueError("bounded query is required")
        if not isinstance(candidates, list) or not 1 <= len(candidates) <= 32 or any(
            not isinstance(value, str) or not value.strip() or len(value) > 8192 for value in candidates
        ):
            raise ValueError("one to 32 bounded candidates required")
        torch = self.torch
        scores = []
        with torch.inference_mode():
            for start in range(0, len(candidates), self.options["batch_size"]):
                batch = [prompt(query, item) for item in candidates[start:start + self.options["batch_size"]]]
                encoded = self.tokenizer(batch, padding=True, truncation=True, max_length=512, return_tensors="pt")
                encoded = {key: value.to(self.options["device"]) for key, value in encoded.items()}
                logits = self.model(**encoded).logits[:, -1, :]
                pair = torch.stack((logits[:, self.no_id], logits[:, self.yes_id]), dim=-1)
                probabilities = torch.softmax(pair.float(), dim=-1)[:, 1].cpu().tolist()
                if any(not math.isfinite(value) or not 0 <= value <= 1 for value in probabilities):
                    raise ValueError("invalid LM reranker probabilities")
                scores.extend(probabilities)
        return {"model": self.options["model_id"], "protocol": "local-lm-reranker-v1", "scores": scores}

    def unload(self) -> None:
        self.model = None
        self.tokenizer = None
        self.options = None
        self.torch = None
        self.yes_id = None
        self.no_id = None
