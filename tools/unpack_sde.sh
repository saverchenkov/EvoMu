#!/usr/bin/env bash
# tools/unpack_sde.sh — Unpack the bundled golden SDE database archive
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DATA_DIR="$REPO_ROOT/data"
SDE_XZ="$DATA_DIR/evomu_sde.sqlite.xz"
SDE_SQLITE="$DATA_DIR/evomu_sde.sqlite"

if [[ -f "$SDE_SQLITE" && -s "$SDE_SQLITE" ]]; then
    echo "✓ SDE database already unpacked at $SDE_SQLITE ($(du -h "$SDE_SQLITE" | cut -f1))"
    exit 0
fi

if [[ ! -f "$SDE_XZ" ]]; then
    echo "✗ Missing compressed SDE archive at $SDE_XZ" >&2
    exit 1
fi

echo "Unpacking $SDE_XZ (~16 MB -> ~83 MB)..."
if command -v xz >/dev/null 2>&1; then
    xz -dk -f "$SDE_XZ"
elif command -v python3 >/dev/null 2>&1; then
    python3 -c "import lzma, shutil; shutil.copyfileobj(lzma.open('$SDE_XZ'), open('$SDE_SQLITE', 'wb'))"
else
    echo "✗ Neither 'xz' nor 'python3' with lzma support found in PATH." >&2
    exit 1
fi

if [[ -f "$SDE_SQLITE" && -s "$SDE_SQLITE" ]]; then
    echo "✓ Successfully unpacked SDE database at $SDE_SQLITE ($(du -h "$SDE_SQLITE" | cut -f1))"
else
    echo "✗ Failed to unpack $SDE_SQLITE" >&2
    exit 1
fi
