"""Export an already installed and verified GigaAM checkpoint into ONNX assets."""

from pathlib import Path
import shutil

from neuromita_ai_sdk import validate_settings
from neuromita_asr.runtime import schema, _gigaam_onnx_files


def execute(hook_id: str, source_root: Path, output_root: Path, settings: dict) -> None:
    if hook_id != "export_gigaam_onnx":
        raise ValueError("unsupported GigaAM installation hook")
    options = validate_settings(schema("gigaam_onnx"), settings)
    model = options["model"]
    checkpoint = source_root / f"{model}.ckpt"
    if checkpoint.is_symlink() or not checkpoint.is_file():
        raise FileNotFoundError("verified GigaAM source checkpoint missing")
    if "e2e" in model:
        tokenizer = source_root / f"{model}_tokenizer.model"
        if tokenizer.is_symlink() or not tokenizer.is_file():
            raise FileNotFoundError("verified GigaAM tokenizer missing")
    import gigaam
    engine = gigaam.load_model(model, device="cpu", fp16_encoder=False,
                               use_flash=False, download_root=str(source_root))
    if not hasattr(engine, "to_onnx") or not hasattr(engine, "decoding"):
        raise TypeError("GigaAM checkpoint is not an ASR export model")
    engine.to_onnx(dir_path=str(output_root))
    if "e2e" in model:
        shutil.copyfile(source_root / f"{model}_tokenizer.model",
                        output_root / f"{model}_tokenizer.model")
    for name in _gigaam_onnx_files(model):
        output = output_root / name
        if output.is_symlink() or not output.is_file() or output.stat().st_size == 0:
            raise RuntimeError(f"GigaAM exporter did not create required asset: {name}")
