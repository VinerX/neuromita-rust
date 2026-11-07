import importlib.util
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

from neuromita_ai_sdk.installer import execute
from neuromita_asr.runtime import install_plan


ROOT = Path(__file__).resolve().parents[1]
PLUGIN = ROOT / "plugins/builtin/gigaam_onnx/install.py"


class AssetInstallHookTests(unittest.TestCase):
    def test_selected_model_declares_explicit_conversion_hook(self):
        plan = install_plan("gigaam_onnx", {"model": "v3_e2e_rnnt"})
        self.assertEqual([(x.id, x.source_plugin_id) for x in plan.asset_hooks],
                         [("export_gigaam_onnx", "gigaam")])
        self.assertIn("onnx>=1.16,<2", plan.requirements)

    def test_gigaam_converter_checks_source_and_exports_model(self):
        spec = importlib.util.spec_from_file_location("test_gigaam_install", PLUGIN)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / "source"
            output = Path(temp) / "output"
            source.mkdir()
            output.mkdir()
            with self.assertRaises(FileNotFoundError):
                module.execute("export_gigaam_onnx", source, output, {"model": "v3_e2e_ctc"})
            (source / "v3_e2e_ctc.ckpt").write_bytes(b"ckpt")
            (source / "v3_e2e_ctc_tokenizer.model").write_bytes(b"tokenizer")

            def loader(name, **kwargs):
                self.assertEqual(name, "v3_e2e_ctc")
                self.assertEqual(kwargs["device"], "cpu")
                self.assertEqual(kwargs["download_root"], str(source))

                def export(*, dir_path):
                    (Path(dir_path) / "v3_e2e_ctc.yaml").write_bytes(b"cfg")
                    (Path(dir_path) / "v3_e2e_ctc.onnx").write_bytes(b"onnx")
                return types.SimpleNamespace(to_onnx=export, decoding=True)

            with patch.dict("sys.modules", {"gigaam": types.SimpleNamespace(load_model=loader)}):
                module.execute("export_gigaam_onnx", source, output, {"model": "v3_e2e_ctc"})
            self.assertEqual((output / "v3_e2e_ctc_tokenizer.model").read_bytes(), b"tokenizer")
            with self.assertRaises(ValueError):
                module.execute("other", source, output, {"model": "v3_e2e_ctc"})

    def test_installer_requires_declared_hook_and_fails_before_import(self):
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / "input"
            output = Path(temp) / "output"
            source.mkdir()
            output.mkdir()
            with self.assertRaisesRegex(ValueError, "not declared"):
                execute(ROOT / "plugins/builtin", "gigaam_onnx", "unapproved",
                        "gigaam", source, output, {"model": "v2_ctc"})
            with self.assertRaisesRegex(ValueError, "not declared"):
                execute(ROOT / "plugins/builtin", "gigaam_onnx", "export_gigaam_onnx",
                        "whisper", source, output, {"model": "v2_ctc"})
            with self.assertRaisesRegex(ValueError, "invalid model"):
                execute(ROOT / "plugins/builtin", "gigaam_onnx", "export_gigaam_onnx",
                        "gigaam", source, output, {"model": "v3_ssl"})


if __name__ == "__main__":
    unittest.main()
