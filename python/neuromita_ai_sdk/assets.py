"""Resolve a Rust-authorized, immutable model revision without path discovery."""

from __future__ import annotations

import json
import os
from pathlib import Path


def selected_model_root(plugin_id: str) -> Path:
    if not isinstance(plugin_id, str) or not plugin_id.isascii() or not plugin_id.replace("-", "").replace("_", "").isalnum():
        raise ValueError("invalid model plugin id")
    root = os.environ.get("NEUROMITA_AI_MODELS_DIR")
    if not root or not Path(root).is_absolute():
        raise ValueError("absolute model asset store is not configured")
    selected = json.loads(os.environ.get("NEUROMITA_AI_MODEL_REVISIONS", "{}"))
    if not isinstance(selected, dict):
        raise ValueError("invalid revision selection")
    revision = selected.get(plugin_id)
    if not isinstance(revision, str) or not revision.isascii() or not revision.replace("-", "").replace("_", "").isalnum():
        raise ValueError("explicit, safe model asset revision is required")
    base = Path(root)
    path = base / plugin_id / revision
    for component in (base, base / plugin_id, path):
        if component.is_symlink():
            raise ValueError("model asset symlinks are forbidden")
    return path
