from itertools import chain

from neuromita_ai_sdk import validate_settings
from neuromita_ai_sdk.assets import selected_model_root
from neuromita_asr.gigaam import GigaAmPlugin


class Plugin(GigaAmPlugin):
    def __init__(self):
        super().__init__("gigaam_onnx")

    def runtime_capabilities(self):
        return [{"backend": "onnx_cpu", "devices": ["cpu"], "precisions": ["fp32"]}]

    def initialize_runtime(self, settings, runtime):
        if self.engine is not None:
            raise ValueError("GigaAM ONNX is already initialized")
        expected = {"backend": "onnx_cpu", "device": "cpu", "precision": "fp32"}
        if runtime != expected:
            raise ValueError("GigaAM ONNX explicit runtime supports CPU fp32 only; GPU fallback is not verified")
        options = validate_settings(self.settings_schema(), settings, include_defaults=False)
        if "device" in options and options["device"] != "cpu":
            raise ValueError("GigaAM ONNX settings device conflicts with selected runtime")
        try:
            result = super().initialize({**options, "device": "cpu"})
            self.runtime_status()
            return result
        except Exception:
            self.unload()
            raise

    def runtime_status(self):
        if self.engine is None or self.options is None:
            raise RuntimeError("GigaAM ONNX is not initialized")
        model = self.options["model"]
        names = [f"{model}_{part}.onnx" for part in ("encoder", "decoder", "joint")] if "rnnt" in model else [f"{model}.onnx"]
        if not isinstance(self.engine, list) or len(self.engine) != len(names):
            raise RuntimeError("GigaAM ONNX session set is incomplete")
        for session in self.engine:
            if session.get_providers() != ["CPUExecutionProvider"]:
                raise RuntimeError("GigaAM ONNX actual provider is not exclusively CPU")
            session.disable_fallback()
            inputs = session.get_inputs()
            outputs = session.get_outputs()
            if not inputs or not outputs or any(
                value.type not in ("tensor(float)", "tensor(int32)", "tensor(int64)", "tensor(bool)")
                for value in chain(inputs, outputs)
            ):
                raise RuntimeError("GigaAM ONNX session exposes unsupported tensor precision")
        import onnx

        root = selected_model_root(self.plugin_id)
        for name in names:
            path = root / name
            if path.is_symlink() or not path.is_file():
                raise RuntimeError(f"GigaAM ONNX model is not a regular local file: {name}")
            model_proto = onnx.load(str(path), load_external_data=False)
            if model_proto.functions:
                raise RuntimeError("GigaAM ONNX custom functions cannot confirm fp32 precision")
            self._check_fp32_graph(model_proto.graph)
        return {"backend": "onnx_cpu", "device": "cpu", "precision": "fp32"}

    @staticmethod
    def _check_fp32_graph(graph):
        def check_type(dtype):
            if dtype not in (1, 2, 3, 4, 5, 6, 7, 9, 12, 13):
                raise RuntimeError("GigaAM ONNX model contains unsupported tensor precision")

        for tensor in chain(graph.initializer, (value.values for value in graph.sparse_initializer)):
            check_type(tensor.data_type)
        for value in chain(graph.input, graph.output, graph.value_info):
            if not value.type.HasField("tensor_type"):
                raise RuntimeError("GigaAM ONNX non-tensor types are unsupported")
            check_type(value.type.tensor_type.elem_type)
        for node in graph.node:
            if node.domain not in ("", "ai.onnx"):
                raise RuntimeError("GigaAM ONNX custom operators cannot confirm fp32 precision")
            for attribute in node.attribute:
                if attribute.type == 4:
                    check_type(attribute.t.data_type)
                elif attribute.type == 9:
                    for tensor in attribute.tensors:
                        check_type(tensor.data_type)
                elif attribute.type == 5:
                    Plugin._check_fp32_graph(attribute.g)
                elif attribute.type == 10:
                    for nested in attribute.graphs:
                        Plugin._check_fp32_graph(nested)
                elif attribute.type in (11, 12, 13, 14):
                    raise RuntimeError("GigaAM ONNX sparse/type attributes cannot confirm fp32 precision")
                elif attribute.name in ("to", "dtype"):
                    check_type(attribute.i)
