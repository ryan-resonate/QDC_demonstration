#!/usr/bin/env bash
# Build the qdc-core WASM artifact and emit it to ./pkg/ for the static site.
#
# Usage:
#   ./build.sh              # release build (default)
#   ./build.sh --dev        # faster dev build, larger artifact

set -euo pipefail
cd "$(dirname "$0")"

mode="--release"
for arg in "$@"; do
    if [[ "$arg" == "--dev" || "$arg" == "-Dev" ]]; then
        mode="--dev"
    fi
done

echo "[build.sh] wasm-pack build crates/qdc-core $mode --target web --out-dir ../../pkg"
wasm-pack build crates/qdc-core $mode --target web --out-dir ../../pkg --out-name qdc_core

echo "[build.sh] artifact:"
ls -lh pkg
