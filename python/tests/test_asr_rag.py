"""Dependency-free ASR/RAG contract regressions. No large inference weights are used."""

import base64
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import patch
import wave

from neuromita_ai_sdk.assets import selected_model_root
from neuromita_ai_sdk.host import PluginHost
from neuromita_asr.runtime import AsrPlugin, _decode_pcm16_wav, install_plan, probe
from neuromita_rag.embeddings import Plugin as EmbeddingPlugin
from neuromita_rag.reranker import Plugin as RerankerPlugin

PLUGINS = Path(__file__).resolve().parents[1] / "plugins/builtin"


def audio(rate=16000, channels=1, width=2):
    stream = io.BytesIO()
    with wave.open(stream, "wb") as output:
        output.setnchannels(channels)
        output.setsampwidth(width)
        output.setframerate(rate)
        output.writeframes(b"\0" * 320 * channels * width)
    return {"audio_base64": base64.b64encode(stream.getvalue()).decode("ascii")}


class InferenceContracts(unittest.TestCase):
    def test_builtin_asr_rag_discovery_does_not_import_model_packages(self):
        host = PluginHost(PLUGINS)
        found = {x["id"]: x for x in host.discover()}
        expected = {"google": "asr", "whisper": "asr", "local_transformers_embedding": "embedding", "local_cross_encoder": "reranker"}
        for plugin_id, kind in expected.items():
            self.assertEqual(found[plugin_id]["kind"], kind)
            self.assertEqual(found[plugin_id]["discovery_status"], "available")
            plan = host.dispatch("install_plan", {"plugin_id": plugin_id})
            self.assertTrue(plan["requirements"])
            self.assertEqual(plan["supported_python_minors"], ["3.11", "3.12"])

    def test_whisper_model_install_plan_is_selected_model_specific(self):
        host = PluginHost(PLUGINS)
        turbo = host.dispatch("install_plan", {"plugin_id": "whisper"})
        large = host.dispatch("install_plan", {"plugin_id": "whisper", "settings": {"model": "large-v3"}})
        self.assertEqual(len(turbo["assets"]), 5)
        self.assertEqual(len(large["assets"]), 5)
        self.assertTrue(all(a["relative_path"].startswith("large-v3-turbo/") for a in turbo["assets"]))
        self.assertTrue(all(a["relative_path"].startswith("large-v3/") for a in large["assets"]))
        with self.assertRaises(ValueError):
            host.dispatch("install_plan", {"plugin_id": "whisper", "settings": {"model": "../../bad"}})
        english = host.dispatch("install_plan", {"plugin_id": "silero_rvc_cuda", "settings": {"language": "en"}})
        self.assertTrue(any(a["relative_path"] == "silero/v3_en.pt" for a in english["assets"]))

    def test_model_storage_is_revision_scoped_and_safe(self):
        with tempfile.TemporaryDirectory() as root, patch.dict(os.environ, {"NEUROMITA_AI_MODELS_DIR": root, "NEUROMITA_AI_MODEL_REVISIONS": json.dumps({"whisper": "sha42"})}):
            self.assertEqual(selected_model_root("whisper"), Path(root) / "whisper/sha42")
            status = probe("whisper")
            self.assertFalse(status["installed"])
            self.assertTrue(any(item.startswith("asset:") for item in status["missing"]))
        with patch.dict(os.environ, {"NEUROMITA_AI_MODEL_REVISIONS": '{"whisper":"../parent"}'}, clear=False):
            with self.assertRaises(ValueError):
                selected_model_root("whisper")

    def test_wav_validation_and_audio_bounds(self):
        pcm, rate = _decode_pcm16_wav(audio())
        self.assertEqual(rate, 16000)
        self.assertEqual(len(pcm), 640)
        for invalid in (audio(channels=2), audio(width=1), {"audio_base64": "***"}, {"audio_base64": ""}):
            with self.subTest(invalid=invalid.get("audio_base64", "")[:6]), self.assertRaises(ValueError):
                _decode_pcm16_wav(invalid)

    def test_google_adapter_transcription_and_no_speech(self):
        class UnknownValueError(Exception):
            pass
        class AudioData:
            def __init__(self, pcm, sample_rate, sample_width):
                self.pcm, self.sample_rate, self.sample_width = pcm, sample_rate, sample_width
        class Recognizer:
            def recognize_google(self, data, language):
                if language != "ru-RU" or data.sample_rate != 16000:
                    raise AssertionError("incorrect audio contract")
                return "Привет"
        speech = types.SimpleNamespace(AudioData=AudioData, Recognizer=Recognizer, UnknownValueError=UnknownValueError)
        plugin = AsrPlugin("google")
        plugin.engine = speech
        plugin.options = {"language": "ru-RU"}
        self.assertEqual(plugin.infer(audio())["text"], "Привет")
        with patch.object(Recognizer, "recognize_google", side_effect=UnknownValueError()):
            self.assertEqual(plugin.infer(audio())["text"], "")
        plugin.unload()
        with self.assertRaises(RuntimeError):
            plugin.infer(audio())

    def test_whisper_adapter_filters_known_hallucinations(self):
        plugin = AsrPlugin("whisper")
        class FakeWhisper:
            def transcribe(self, pcm, **kwargs):
                self.last = (pcm, kwargs)
                return [types.SimpleNamespace(text="Спасибо за просмотр!"), types.SimpleNamespace(text=" Настоящая речь ")], None
        plugin.engine = FakeWhisper()
        plugin.options = {"language": "ru", "beam_size": 5}
        self.assertEqual(plugin.infer(audio())["text"], "Настоящая речь")
        self.assertFalse(plugin.engine.last[1]["vad_filter"])
        with self.assertRaises(ValueError):
            plugin.infer(audio(rate=8000))

    def test_numpy_2_environment_is_not_reported_installed(self):
        from neuromita_ai_sdk.distributions import check_distributions
        import importlib.metadata
        def version(name):
            if name == "numpy":
                return "2.3.5"
            return "1.0.0"
        with patch.object(importlib.metadata, "version", side_effect=version):
            result = check_distributions(("numpy", "torch"), exact_versions={"numpy": "1.26.0"})
        self.assertEqual(result, ["distribution:numpy expected 1.26.0, found 2.3.5"])

    def test_local_rag_adapters_fail_closed_without_weights(self):
        for plugin in (EmbeddingPlugin(), RerankerPlugin()):
            with self.subTest(plugin=type(plugin)):
                with tempfile.TemporaryDirectory() as root, patch.dict(os.environ, {"NEUROMITA_AI_MODELS_DIR": root, "NEUROMITA_AI_MODEL_REVISIONS": "{}"}):
                    self.assertFalse(plugin.probe_install()["installed"])
                    with self.assertRaises(RuntimeError):
                        plugin.initialize({"model_id": "fixture", **({"dimensions": 3} if isinstance(plugin, EmbeddingPlugin) else {})})
                    with self.assertRaises(RuntimeError):
                        plugin.infer({})


if __name__ == "__main__":
    unittest.main()
