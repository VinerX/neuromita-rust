import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from neuromita_ai_sdk.host import PluginHost
from neuromita_rag.lm_reranker import Plugin, plan, prompt


class LmRerankerTests(unittest.TestCase):
    def test_discover_and_static_plan(self):
        host = PluginHost(Path(__file__).resolve().parents[1] / "plugins/builtin")
        manifest = next(item for item in host.discover() if item["id"] == "local_lm_reranker")
        self.assertEqual(manifest["kind"], "reranker")
        self.assertEqual(host.dispatch("install_plan", {"plugin_id": "local_lm_reranker"})["supported_python_minors"], ["3.11", "3.12"])
        self.assertTrue(any(spec.startswith("transformers") for spec in plan().requirements))

    def test_missing_revision_and_bad_configuration_fail_closed(self):
        with tempfile.TemporaryDirectory() as root, patch.dict(os.environ, {
            "NEUROMITA_AI_MODELS_DIR": root,
            "NEUROMITA_AI_MODEL_REVISIONS": json.dumps({"local_lm_reranker": "test"}),
        }):
            plugin = Plugin()
            self.assertFalse(plugin.probe_install()["installed"])
            with self.assertRaises(RuntimeError):
                plugin.initialize({"model_id": "fixture"})
            with self.assertRaises(ValueError):
                plugin.initialize({"model_id": "fixture", "device": "dml"})
            with self.assertRaises(RuntimeError):
                plugin.infer({"query": "test", "candidates": ["doc"]})

    def test_prompt_has_reference_instruction_and_explicit_query_document(self):
        value = prompt("question", "document")
        self.assertIn("<Query>: question", value)
        self.assertIn("<Document>: document", value)
        self.assertIn("answer can only be", value)
        self.assertTrue(value.endswith("</think>\n\n"))


if __name__ == "__main__":
    unittest.main()
