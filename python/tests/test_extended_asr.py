import base64
import io
import json
import sys
import tempfile
import types
import unittest
import wave
from pathlib import Path
from unittest.mock import patch

from neuromita_ai_sdk.host import PluginHost
from neuromita_asr.gigaam import GigaAmPlugin, _onnx_providers
from neuromita_asr.whisper_onnx import WhisperOnnxPlugin
from neuromita_asr.runtime import install_plan, probe, schema


PLUGINS = Path(__file__).resolve().parents[1] / "plugins/builtin"


def audio(rate=16000):
    data = io.BytesIO()
    with wave.open(data, "wb") as output:
        output.setnchannels(1)
        output.setsampwidth(2)
        output.setframerate(rate)
        output.writeframes(b"\x01\x00" * 500)
    return {"audio_base64": base64.b64encode(data.getvalue()).decode()}


class ExtendedAsrTests(unittest.TestCase):
    def test_all_reference_asr_variants_are_discovered(self):
        host = PluginHost(PLUGINS)
        discovered = {item["id"]: item for item in host.discover()}
        for name in ("google", "whisper", "gigaam", "gigaam_onnx", "whisper_onnx"):
            with self.subTest(name=name):
                self.assertEqual(discovered[name]["discovery_status"], "available")
                self.assertEqual(discovered[name]["kind"], "asr")
                self.assertEqual(host.dispatch("describe", {"plugin_id": name})["kind"], "asr")
                plan = host.dispatch("install_plan", {"plugin_id": name})
                self.assertEqual(plan["supported_python_minors"], ["3.11", "3.12"])
                self.assertTrue(plan["supported_backends"])

    def test_all_asr_backend_choices_follow_model_specific_selection(self):
        expected = {
            "google": ("remote-api",),
            "whisper": ("ctranslate2-cpu",),
            "gigaam": ("torch-cpu",),
            "gigaam_onnx": ("onnx-cpu",),
            "whisper_onnx": ("onnx-cpu",),
        }
        for plugin_id, kinds in expected.items():
            with self.subTest(plugin=plugin_id):
                self.assertEqual(install_plan(plugin_id).supported_backends, kinds)
        self.assertEqual(install_plan("whisper", {"device": "cuda"}).supported_backends, ("ctranslate2-cuda",))
        self.assertEqual(install_plan("gigaam", {"device": "cuda"}).supported_backends, ("torch-cuda",))
        self.assertEqual(install_plan("gigaam_onnx", {"device": "dml"}).supported_backends, ("onnx-directml",))
        self.assertEqual(install_plan("whisper_onnx", {"device": "cpu"}).supported_backends, ("onnx-cpu",))
        for plugin_id in ("gigaam", "gigaam_onnx", "whisper_onnx"):
            with self.assertRaises(ValueError):
                install_plan(plugin_id, {"device": "auto"})

    def test_selected_gigaam_model_changes_assets_without_rust_changes(self):
        self.assertEqual(len(install_plan("gigaam", {"model": "v2_ctc"}).assets), 1)
        self.assertEqual(len(install_plan("gigaam", {"model": "v3_e2e_rnnt"}).assets), 2)
        self.assertEqual(len(install_plan("gigaam_onnx", {"model": "v3_ctc"}).assets), 2)
        self.assertEqual(len(install_plan("gigaam_onnx", {"model": "v2_rnnt"}).assets), 4)
        self.assertEqual(len(install_plan("gigaam_onnx", {"model": "v3_e2e_rnnt"}).assets), 5)
        self.assertEqual(len(install_plan("whisper_onnx").assets), 10)
        for name in ("gigaam", "gigaam_onnx"):
            with self.assertRaises(ValueError):
                install_plan(name, {"model": "v3_ssl"})
        with self.assertRaises(ValueError):
            install_plan("whisper_onnx", {"device": "cuda"})

    def test_missing_weights_are_not_accepted(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict("os.environ", {
            "NEUROMITA_AI_MODELS_DIR": directory,
            "NEUROMITA_AI_MODEL_REVISIONS": json.dumps({name: "revision" for name in ("gigaam", "gigaam_onnx", "whisper_onnx")}),
        }):
            for name in ("gigaam", "gigaam_onnx", "whisper_onnx"):
                with self.subTest(name=name):
                    result = probe(name)
                    self.assertFalse(result["installed"])
                    self.assertTrue(any(item.startswith("asset:") for item in result["missing"]))
                    with self.assertRaises(RuntimeError):
                        (WhisperOnnxPlugin() if name == "whisper_onnx" else GigaAmPlugin(name)).initialize({})

    def test_dml_selection_never_silently_downgrades(self):
        cpu = types.SimpleNamespace(get_available_providers=lambda: ["CPUExecutionProvider"])
        self.assertEqual(_onnx_providers("auto", cpu), ["CPUExecutionProvider"])
        with self.assertRaisesRegex(RuntimeError, "DirectML"):
            _onnx_providers("dml", cpu)
        dml = types.SimpleNamespace(get_available_providers=lambda: ["DmlExecutionProvider", "CPUExecutionProvider"])
        self.assertEqual(_onnx_providers("dml", dml), ["DmlExecutionProvider"])

    def test_dml_probe_uses_directml_distribution_instead_of_cpu_onnx(self):
        recorded = []

        def check(names, exact_versions=None):
            recorded.extend(names)
            return []

        with tempfile.TemporaryDirectory() as directory, patch.dict("os.environ", {
            "NEUROMITA_AI_MODELS_DIR": directory,
            "NEUROMITA_AI_MODEL_REVISIONS": json.dumps({"whisper_onnx": "revision"}),
        }), patch("neuromita_asr.runtime.check_distributions", side_effect=check):
            self.assertFalse(probe("whisper_onnx", {"device": "dml"})["installed"])
        self.assertIn("onnxruntime-directml", recorded)
        self.assertNotIn("onnxruntime", recorded)

    def test_gigaam_onnx_infers_via_reference_utilities_without_new_process(self):
        def transcribe_sample(**kwargs):
            with wave.open(kwargs["wav_file"], "rb") as reader:
                self.assertEqual(reader.getframerate(), 16000)
            self.assertEqual(kwargs["model_cfg"], "cfg")
            return "  Проверка распознавания  "
        utils = types.ModuleType("gigaam.onnx_utils")
        utils.transcribe_sample = transcribe_sample
        gigaam = types.ModuleType("gigaam")
        gigaam.onnx_utils = utils
        plugin = GigaAmPlugin("gigaam_onnx")
        plugin.engine = {"session": "fixture"}
        plugin.model_cfg = "cfg"
        plugin.processor = "pre"
        plugin.tokenizer = "tok"
        plugin.options = {"language": "ru"}
        with patch.dict(sys.modules, {"gigaam": gigaam, "gigaam.onnx_utils": utils}):
            self.assertEqual(plugin.infer(audio()), {"text": "Проверка распознавания", "language": "ru"})
        plugin.unload()
        with self.assertRaises(RuntimeError):
            plugin.infer(audio())

    def test_whisper_onnx_model_loading_and_transcription(self):
        inference = []
        provider = types.ModuleType("onnxruntime")
        provider.get_available_providers = lambda: ["CPUExecutionProvider"]
        provider.SessionOptions = lambda: types.SimpleNamespace()
        model = types.ModuleType("optimum.onnxruntime")
        class Ort:
            @staticmethod
            def from_pretrained(path, **kwargs):
                self.assertEqual(kwargs["provider"], "CPUExecutionProvider")
                self.assertTrue(kwargs["local_files_only"])
                return "model"
        model.ORTModelForSpeechSeq2Seq = Ort
        optimum = types.ModuleType("optimum")
        optimum.onnxruntime = model
        transformers = types.ModuleType("transformers")
        class Processor:
            tokenizer = "tokenizer"
            feature_extractor = "extractor"
            @classmethod
            def from_pretrained(cls, *args, **kwargs):
                return cls()
            def get_decoder_prompt_ids(self, **kwargs):
                return [1, 2]
        class Config:
            @classmethod
            def from_pretrained(cls, *args, **kwargs):
                return cls()
        def pipeline(*args, **kwargs):
            self.assertEqual(kwargs["model"], "model")
            def run(data, **options):
                inference.append((len(data), options))
                return {"text": "  Модель ответила  "}
            return run
        transformers.WhisperProcessor = Processor
        transformers.GenerationConfig = Config
        transformers.pipeline = pipeline
        plugin = WhisperOnnxPlugin()
        modules = {"onnxruntime": provider, "optimum": optimum,
                   "optimum.onnxruntime": model, "transformers": transformers}
        with patch.dict(sys.modules, modules), patch("neuromita_asr.whisper_onnx.probe", return_value={"installed": True}), patch("neuromita_asr.whisper_onnx.selected_model_root", return_value=Path("/fixture")):
            self.assertTrue(plugin.initialize({"device": "cpu"})["model_ready"])
            self.assertEqual(plugin.infer(audio())["text"], "Модель ответила")
        self.assertEqual(inference[0][0], 500)
        self.assertEqual(plugin.options["max_tokens"], 192)
        plugin.unload()


if __name__ == "__main__":
    unittest.main()
