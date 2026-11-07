"""Local Whisper ONNX encoder/decoder inference through Optimum and ONNXRuntime."""

from __future__ import annotations

from neuromita_ai_sdk.assets import selected_model_root
from neuromita_ai_sdk import validate_settings
from .gigaam import _samples, _onnx_providers
from .runtime import AsrPlugin, _decode_pcm16_wav, probe


class WhisperOnnxPlugin(AsrPlugin):
    def __init__(self):
        super().__init__("whisper_onnx")
        self.processor = None
        self.generation_config = None
        self.numpy = None

    def initialize(self, settings: dict) -> dict:
        if self.engine is not None:
            raise ValueError("Whisper ONNX is already initialized")
        options = validate_settings(self.settings_schema(), settings)
        status = probe(self.plugin_id, options)
        if not status["installed"]:
            raise RuntimeError(f"Whisper ONNX dependencies/assets missing: {status['missing']}")
        import onnxruntime as ort
        from optimum.onnxruntime import ORTModelForSpeechSeq2Seq
        from transformers import WhisperProcessor, GenerationConfig, pipeline

        provider = _onnx_providers(options["device"], ort)[0]
        root = selected_model_root(self.plugin_id)
        processor = WhisperProcessor.from_pretrained(root, local_files_only=True)
        generation = GenerationConfig.from_pretrained(root, local_files_only=True)
        generation.max_new_tokens = options["max_tokens"]
        if options["language"].strip().lower() == "auto":
            generation.forced_decoder_ids = None
        else:
            generation.forced_decoder_ids = processor.get_decoder_prompt_ids(
                language=options["language"], task="transcribe",
            )
        session_options = ort.SessionOptions()
        if provider == "DmlExecutionProvider":
            session_options.graph_optimization_level = ort.GraphOptimizationLevel.ORT_DISABLE_ALL
            session_options.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
            session_options.enable_mem_pattern = False
        model = ORTModelForSpeechSeq2Seq.from_pretrained(
            root, subfolder="onnx", encoder_file_name="encoder_model.onnx",
            decoder_file_name="decoder_model.onnx", provider=provider,
            session_options=session_options, local_files_only=True,
        )
        engine = pipeline(
            "automatic-speech-recognition", model=model,
            tokenizer=processor.tokenizer, feature_extractor=processor.feature_extractor,
            device=-1,
        )
        self.engine = engine
        self.processor = processor
        self.generation_config = generation
        self.options = options
        return {"model_ready": True, "plugin_id": self.plugin_id, "provider": provider}

    def infer(self, payload: dict) -> dict:
        if self.engine is None or self.options is None:
            raise RuntimeError("Whisper ONNX is not initialized")
        pcm, rate = _decode_pcm16_wav(payload)
        audio = _samples(pcm)
        if rate != 16000:
            from scipy.signal import resample_poly
            from math import gcd
            factor = gcd(rate, 16000)
            audio = resample_poly(audio, 16000 // factor, rate // factor).astype("float32")
        result = self.engine(
            audio, return_timestamps=False,
            generate_kwargs={"generation_config": self.generation_config},
        )
        text = result.get("text") if isinstance(result, dict) else str(result)
        if not isinstance(text, str):
            raise ValueError("Whisper ONNX returned a non-text transcript")
        return {"text": text.strip(), "language": self.options["language"]}

    def unload(self):
        self.processor = None
        self.generation_config = None
        super().unload()
