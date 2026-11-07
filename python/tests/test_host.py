"""Contract tests for plugin discovery and the newline-delimited JSON host protocol."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
PLUGINS = ROOT / "plugins" / "builtin"


class HostTests(unittest.TestCase):
    def start_host(self, directory=PLUGINS):
        env = os.environ.copy()
        env["PYTHONPATH"] = str(ROOT)
        process = subprocess.Popen(
            [sys.executable, "-m", "neuromita_ai_sdk.host", "--plugins-dir", str(directory)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=env,
        )
        def cleanup():
            if process.poll() is None:
                process.terminate()
            process.communicate(timeout=5)
        self.addCleanup(cleanup)
        self.assertEqual(json.loads(process.stdout.readline()), {"type": "hello", "api_version": 1})
        return process

    def request(self, process, op, args=None, request_id="r1"):
        process.stdin.write(json.dumps({"id": request_id, "op": op, "args": args or {}}) + "\n")
        process.stdin.flush()
        return json.loads(process.stdout.readline())

    def test_discovery_and_synthesis(self):
        process = self.start_host()
        found = self.request(process, "discover")
        self.assertTrue({"mock_asr", "mock_tts"}.issubset({p["id"] for p in found["result"]}))
        self.assertTrue(self.request(process, "probe_install", {"plugin_id": "mock_tts"})["result"]["installed"])
        self.assertEqual(self.request(process, "install_plan", {"plugin_id": "mock_tts"})["result"]["packages"], [])
        self.assertTrue(self.request(process, "initialize", {"plugin_id": "mock_tts", "settings": {"speaker": "bright"}})["ok"])
        import base64
        data = self.request(process, "infer", {"kind": "tts", "input": {"text": "Привет"}})["result"]
        self.assertTrue(base64.b64decode(data["audio_base64"]).startswith(b"RIFF"))
        self.assertTrue(self.request(process, "unload", {"kind": "tts"})["ok"])
        self.assertFalse(self.request(process, "infer", {"kind": "tts", "input": {"text": "again"}})["ok"])

    def test_chunked_wav_stream_is_bounded_and_correlated(self):
        import base64
        process = self.start_host()
        self.assertTrue(self.request(process, "initialize", {"plugin_id": "mock_tts"})["ok"])
        started = self.request(process, "infer_stream_start", {"kind": "tts", "input": {"text": "a" * 200}})
        self.assertTrue(started["ok"])
        stream = started["result"]
        self.assertEqual(stream["format"], "wav")
        self.assertFalse(self.request(process, "unload", {"kind": "tts"})["ok"])
        self.assertFalse(self.request(process, "infer_stream_next", {"stream_id": "bogus"})["ok"])
        chunks = []
        while True:
            result = self.request(process, "infer_stream_next", {"stream_id": stream["stream_id"]})
            self.assertTrue(result["ok"])
            self.assertEqual(result["result"]["stream_id"], stream["stream_id"])
            chunks.append(base64.b64decode(result["result"]["chunk_base64"]))
            if result["result"]["done"]:
                break
        data = b"".join(chunks)
        self.assertEqual(len(data), stream["length"])
        self.assertTrue(data.startswith(b"RIFF"))
        self.assertFalse(self.request(process, "infer_stream_next", {"stream_id": stream["stream_id"]})["ok"])
        self.assertTrue(self.request(process, "unload", {"kind": "tts"})["ok"])

    def test_stream_cancel_releases_buffer_and_allows_reuse(self):
        process = self.start_host()
        self.assertTrue(self.request(process, "initialize", {"plugin_id": "mock_tts"})["ok"])
        args = {"kind": "tts", "input": {"text": "short"}}
        stream = self.request(process, "infer_stream_start", args)["result"]["stream_id"]
        self.assertFalse(self.request(process, "infer_stream_start", args)["ok"])
        self.assertTrue(self.request(process, "infer_stream_cancel", {"stream_id": stream})["ok"])
        self.assertTrue(self.request(process, "infer_stream_start", args)["ok"])

    def test_shared_worker_supports_independent_service_slots(self):
        process = self.start_host()
        self.assertTrue(self.request(process, "initialize", {"plugin_id": "mock_asr"})["ok"])
        self.assertTrue(self.request(process, "initialize", {"plugin_id": "mock_tts"})["ok"])
        self.assertEqual(self.request(process, "infer", {"kind": "asr", "input": {"mock_transcript": "hello"}})["result"], {"text": "hello"})
        self.assertEqual(self.request(process, "infer", {"kind": "tts", "input": {"text": "hello"}})["result"]["format"], "wav")
        self.assertFalse(self.request(process, "initialize", {"plugin_id": "mock_tts"})["ok"])
        self.assertTrue(self.request(process, "unload", {"kind": "asr"})["ok"])
        self.assertTrue(self.request(process, "infer", {"kind": "tts", "input": {"text": "still here"}})["ok"])

    def test_asr_mock_accepts_captured_pcm16_wav(self):
        import base64
        import io
        import wave
        process = self.start_host()
        self.assertTrue(self.request(process, "initialize", {"plugin_id": "mock_asr"})["ok"])
        audio = io.BytesIO()
        with wave.open(audio, "wb") as wav:
            wav.setnchannels(1)
            wav.setsampwidth(2)
            wav.setframerate(16000)
            wav.writeframes(bytes(512))
        response = self.request(process, "infer", {"kind": "asr", "input": {
            "audio_base64": base64.b64encode(audio.getvalue()).decode("ascii")
        }})
        self.assertTrue(response["ok"])
        self.assertEqual(response["result"], {"text": "", "language": "en"})

    def test_invalid_and_incompatible_discovery(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "bad"
            path.mkdir()
            (path / "plugin.json").write_text(json.dumps({"id": "bad", "name": "Old", "kind": "tts", "entrypoint": "plugin.py", "api_version": 999}))
            process = self.start_host(Path(temp))
            found = self.request(process, "discover")["result"]
            self.assertEqual(found[0]["discovery_status"], "invalid")
            self.assertFalse(self.request(process, "initialize", {"plugin_id": "../bad"})["ok"])

    def test_installplan_does_not_import_uninstalled_inference_dependencies(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp) / "third_party"
            directory.mkdir()
            (directory / "plugin.json").write_text(json.dumps({"id": "third_party", "name": "Third Party", "kind": "tts", "entrypoint": "plugin.py", "api_version": 1}))
            (directory / "plugin.py").write_text("import missing_neuromita_test_deep_learning_dependency\n")
            (directory / "installplan.py").write_text("from neuromita_ai_sdk import InstallPlan, PackageRequirement\nclass InstallContract:\n    def install_plan(self): return InstallPlan(packages=(PackageRequirement('transformers','4.45.0'),))\n    def probe_install(self): return {'installed': False}\n    def settings_schema(self): return {'speaker': {'type': 'string'}}\n")
            host = self.start_host(Path(temp))
            plan = self.request(host, "install_plan", {"plugin_id": "third_party"})
            self.assertTrue(plan["ok"])
            self.assertEqual(plan["result"]["packages"][0]["name"], "transformers")
            self.assertTrue(self.request(host, "settings_schema", {"plugin_id": "third_party"})["ok"])
            self.assertFalse(self.request(host, "initialize", {"plugin_id": "third_party"})["ok"])

    def test_validate_config_without_inference_loading(self):
        process = self.start_host()
        result = self.request(process, "validate_config", {"plugin_id": "silero_rvc_cuda", "settings": {"silero.speaker_id": "kseniya"}})
        self.assertTrue(result["ok"])
        self.assertEqual(result["result"]["rvc.pitch_shift_semitones"], 6.0)
        self.assertFalse(self.request(process, "validate_config", {"plugin_id": "silero_rvc_cuda", "settings": {"volume": float("nan")}})["ok"])
        self.assertFalse(self.request(process, "active_settings_schema", {"kind": "tts"})["ok"])

    def test_wrong_settings_not_ready(self):
        process = self.start_host()
        self.assertFalse(self.request(process, "initialize", {"plugin_id": "mock_tts", "settings": {"speaker": "unknown"}})["ok"])
        self.assertFalse(self.request(process, "infer", {"kind": "tts", "input": {"text": "x"}})["ok"])


if __name__ == "__main__":
    unittest.main()
