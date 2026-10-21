#!/bin/sh
# Capture one window to a PNG (no shadow, no sound). Usage: shot.sh [window-id] <out.png>
# With only <out.png>, captures the first on-screen window whose owner/title contains "EVE".
set -e
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ $# -ge 2 ]; then ID="$1"; OUT="$2"; else OUT="$1"; ID="$(swift "$HERE/winlist.swift" EVE | head -1 | cut -f1)"; fi
[ -n "$OUT" ] || { echo "usage: shot.sh [window-id] <out.png>" >&2; exit 2; }
[ -n "$ID" ] || { echo "no matching window found (is the client running and on screen?)" >&2; exit 1; }
mkdir -p "$(dirname "$OUT")"
screencapture -x -o -l"$ID" "$OUT"
echo "$OUT (window $ID)"
