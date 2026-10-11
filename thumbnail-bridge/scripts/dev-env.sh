# Source me: shared build settings for the bridge and the patched PhotoCraft checkout.
BRIDGE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_TARGET_DIR="$BRIDGE_ROOT/third_party/photocraft/target"
export CRAFT_FONTS_DIR="$BRIDGE_ROOT/third_party/craft-fonts"
export PLOTMILL_MODELS_DIR="$BRIDGE_ROOT/models"
