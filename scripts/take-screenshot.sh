#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Grab the Kindle's framebuffer over SSH and turn it into a PNG.
#
#   KINDLE=root@<device-ip> scripts/take-screenshot.sh [NAME]
#
# Requires SSH access to a jailbroken Kindle and python3 with Pillow on the
# host. Output goes to $OUT_DIR (default: docs/screenshots).
set -eu
NAME=${1:-screen}
OUT_DIR="${OUT_DIR:-docs/screenshots}"
: "${KINDLE:?set KINDLE=user@host, e.g. KINDLE=root@192.168.1.50}"
KEY="${KINDLE_KEY:-$HOME/.ssh/id_ed25519}"
mkdir -p "$OUT_DIR"
RAW="/tmp/kindle_fb_$$.raw"
PNG="$OUT_DIR/${NAME}.png"

ssh -o BatchMode=yes -o ConnectTimeout=10 -i "$KEY" "$KINDLE" \
    "dd if=/dev/fb0 bs=1280 count=1680 2>/dev/null" > "$RAW"
python3 aristotle/tools/host/fb2png.py "$RAW" "$PNG" 1280 1264 1680 0
rm -f "$RAW"
echo "Saved screenshot: $PNG"
