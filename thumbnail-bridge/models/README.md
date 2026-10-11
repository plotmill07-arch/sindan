# models/

`scripts/setup.sh` puts the runtime and AI models here (they are not in git):

| File | What | License | SHA-256 |
|---|---|---|---|
| `libonnxruntime.so` / `.dylib` | ONNX Runtime 1.28.0 (official build, no telemetry uploader) | MIT (`LICENSE-onnxruntime.txt`) | package pinned in setup.sh |
| `PP-OCRv6_det_small.onnx` | PaddleOCR text detection | Apache-2.0 (`MODEL_LICENSES-paddleocr.md`) | `090f04ab…1ff94f` |
| `PP-OCRv6_rec_small.onnx` | PaddleOCR text recognition (Japanese) | Apache-2.0 | `6f327246…c14884` |
| `u2net_human_seg.onnx` | U²-Net human segmentation | Apache-2.0 (weights), MIT (rembg export) | `01eb6a29…d1c73c` |

Other folders can be used with `PLOTMILL_MODELS_DIR` and `PLOTMILL_ORT_LIB`.
