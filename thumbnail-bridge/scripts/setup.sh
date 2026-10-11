#!/usr/bin/env bash
# Plotmill AI Thumbnail Bridge — one-time setup (Linux / macOS).
#
#   scripts/setup.sh            fetch PhotoCraft + fonts + AI models, apply the patch, build (release)
#   scripts/setup.sh --no-build fetch and patch only
#
# Everything is pinned: PhotoCraft and craft-fonts by git revision, every downloaded file by
# SHA-256. Downloads come only from github.com and pypi.org, only here (the app itself never
# goes online). No API key is needed.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TP="$ROOT/third_party"
MODELS="$ROOT/models"
PHOTOCRAFT_URL="https://github.com/storytold/photocraft.git"
PHOTOCRAFT_REV="334ae5779ceebbf7681d345b35dd66e79115e352"
CRAFT_FONTS_URL="https://github.com/storytold/craft-fonts.git"
CRAFT_FONTS_REV="8dcdacd5153e64560d109541a47d806f26f048c0"
BUILD=1
[ "${1:-}" = "--no-build" ] && BUILD=0

say() { printf '\n== %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "'$1' is required ($2)"; }

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# fetch <url> <dest> <sha256>: download once, verify, keep.
fetch() {
  local url="$1" dest="$2" want="$3"
  if [ -f "$dest" ] && [ "$(sha256 "$dest")" = "$want" ]; then return 0; fi
  echo "   downloading $(basename "$dest")"
  curl -fL --retry 4 --retry-delay 2 -o "$dest.part" "$url"
  local got; got="$(sha256 "$dest.part")"
  [ "$got" = "$want" ] || { rm -f "$dest.part"; die "checksum mismatch for $url (got $got)"; }
  mv "$dest.part" "$dest"
}

unzip_member() { # <zip> <member> <dest>
  if command -v unzip >/dev/null 2>&1; then unzip -p "$1" "$2" > "$3"
  else python3 -c 'import sys,zipfile; open(sys.argv[3],"wb").write(zipfile.ZipFile(sys.argv[1]).read(sys.argv[2]))' "$1" "$2" "$3"; fi
}

need git "to fetch PhotoCraft"
need curl "to fetch the models"
need tar "to unpack ONNX Runtime"
[ "$BUILD" = 1 ] && need cargo "Rust toolchain from https://rustup.rs (1.95 or newer)"

mkdir -p "$TP" "$MODELS" "$ROOT/.cache"

say "PhotoCraft @ ${PHOTOCRAFT_REV:0:12}"
if [ ! -d "$TP/photocraft/.git" ]; then
  git clone --filter=blob:none "$PHOTOCRAFT_URL" "$TP/photocraft"
fi
git -C "$TP/photocraft" fetch --quiet origin "$PHOTOCRAFT_REV" 2>/dev/null || true
if git -C "$TP/photocraft" apply --reverse --check "$ROOT/patches/photocraft-ai-thumbnail.patch" 2>/dev/null; then
  echo "   AI Thumbnail patch already applied"
else
  git -C "$TP/photocraft" checkout --quiet "$PHOTOCRAFT_REV"
  git -C "$TP/photocraft" apply "$ROOT/patches/photocraft-ai-thumbnail.patch"
  echo "   applied patches/photocraft-ai-thumbnail.patch"
fi

say "craft-fonts (OFL Japanese fonts) @ ${CRAFT_FONTS_REV:0:12}"
if [ ! -d "$TP/craft-fonts/.git" ]; then
  git clone --filter=blob:none "$CRAFT_FONTS_URL" "$TP/craft-fonts"
fi
git -C "$TP/craft-fonts" fetch --quiet origin "$CRAFT_FONTS_REV" 2>/dev/null || true
git -C "$TP/craft-fonts" checkout --quiet "$CRAFT_FONTS_REV"

say "ONNX Runtime 1.28.0 (official build without the telemetry uploader)"
OS="$(uname -s)"; ARCH="$(uname -m)"
case "$OS-$ARCH" in
  Linux-x86_64)  ORT_PKG="onnxruntime-linux-x64-1.28.0";     ORT_SHA="a3e1b79d7bb1bf09696ce675f49e4064e6c81f6202b8225624fff0e93f8d6407"; ORT_LIB="libonnxruntime.so.1.28.0"; ORT_OUT="libonnxruntime.so" ;;
  Linux-aarch64) ORT_PKG="onnxruntime-linux-aarch64-1.28.0"; ORT_SHA="e15ff8b5d85afe6c144d97c6fd432254bf76a219daaf17658087d6ecb3e8f0bb"; ORT_LIB="libonnxruntime.so.1.28.0"; ORT_OUT="libonnxruntime.so" ;;
  Darwin-arm64)  ORT_PKG="onnxruntime-osx-arm64-1.28.0";     ORT_SHA="1268b359718099bde2cedb55787f182a130067bc4f31e8c88478c445b850d3d8"; ORT_LIB="libonnxruntime.1.28.0.dylib"; ORT_OUT="libonnxruntime.dylib" ;;
  *) die "no prebuilt ONNX Runtime 1.28.0 for $OS-$ARCH; put a libonnxruntime in $MODELS or set PLOTMILL_ORT_LIB" ;;
esac
fetch "https://github.com/microsoft/onnxruntime/releases/download/v1.28.0/$ORT_PKG.tgz" "$ROOT/.cache/$ORT_PKG.tgz" "$ORT_SHA"
tar -xzf "$ROOT/.cache/$ORT_PKG.tgz" -C "$ROOT/.cache"
cp "$ROOT/.cache/$ORT_PKG/lib/$ORT_LIB" "$MODELS/$ORT_OUT"
cp "$ROOT/.cache/$ORT_PKG/LICENSE" "$MODELS/LICENSE-onnxruntime.txt"

say "AI models"
# PaddleOCR PP-OCRv6 (Apache-2.0), as shipped in the RapidOCR 3.10.0 wheel on PyPI.
WHEEL="$ROOT/.cache/rapidocr-3.10.0-py3-none-any.whl"
fetch "https://files.pythonhosted.org/packages/59/07/203065283bba0c5e9c10af3602dc25ab3e59ee54dc2d1d22c8faeabf5ed8/rapidocr-3.10.0-py3-none-any.whl" "$WHEEL" \
  "2fc34e26cd0f48514804a94f8832283039c7334076bff60768b1fed9ad068050"
for m in PP-OCRv6_det_small PP-OCRv6_rec_small; do
  unzip_member "$WHEEL" "rapidocr/models/$m.onnx" "$MODELS/$m.onnx"
done
unzip_member "$WHEEL" "rapidocr-3.10.0.dist-info/licenses/MODEL_LICENSES.md" "$MODELS/MODEL_LICENSES-paddleocr.md"
[ "$(sha256 "$MODELS/PP-OCRv6_det_small.onnx")" = "090f04abcd9d9a7498bc4ebf677e4cb9bdce1fe4197ddb7e529f1ef44e1ff94f" ] || die "PP-OCRv6 det checksum"
[ "$(sha256 "$MODELS/PP-OCRv6_rec_small.onnx")" = "6f327246b50388f3c176ae304bd95767ea6dc0c9ae92153ef8cbe210b3c14884" ] || die "PP-OCRv6 rec checksum"
# U²-Net human segmentation (Apache-2.0 weights; ONNX export published by rembg, MIT).
fetch "https://github.com/danielgatis/rembg/releases/download/v0.0.0/u2net_human_seg.onnx" "$MODELS/u2net_human_seg.onnx" \
  "01eb6a29a5c4d8edb30b56adad9bb3a2a0535338e480724a213e0acfd2d1c73c"

if [ "$BUILD" = 1 ]; then
  say "Building PhotoCraft with the AI Thumbnail Bridge (release; the first build takes a while)"
  # shellcheck source=scripts/dev-env.sh
  source "$ROOT/scripts/dev-env.sh"
  (cd "$TP/photocraft" && cargo build --release -p photocraft)
  (cd "$ROOT" && cargo build --release --bin plotmill)
fi

say "Done"
echo "   PhotoCraft:  scripts/run-photocraft.sh   (File › Import › AI Thumbnails… / Window › AI Thumbnail)"
echo "   CLI:         scripts/plotmill.sh convert thumb.png --out out/"
echo "   Check:       scripts/plotmill.sh doctor"
