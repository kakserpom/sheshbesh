#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WASM_PACK=${WASM_PACK:-wasm-pack}
# Build the AI worker WASM binary; output placed in frontend/public/worker/
"$WASM_PACK" build "$SCRIPT_DIR/ai-worker" \
    --target no-modules \
    --out-dir "$SCRIPT_DIR/public/worker" \
    --out-name sheshbesh_ai_worker \
    --release \
    --no-opt
