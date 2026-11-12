#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Grab the Kindle Oasis 2 framebuffer over SSH and write a PNG.
#
#   KINDLE=root@<device-ip> scripts/qa103-shot.sh NAME
#
# Reads a 1264x1680 panel with stride 1280, 8 bpp from /dev/fb0. Requires
# python3 with Pillow on the host (falls back to ffmpeg).
set -eu
NAME=${1:?usage: qa103-shot.sh NAME}
: "${KINDLE:?set KINDLE=user@host, e.g. KINDLE=root@192.168.1.50}"
KEY="${KINDLE_KEY:-$HOME/.ssh/id_ed25519}"
OUT=${OUT:-screenshots}
HERE=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$OUT"
RAW=${TMPDIR:-/tmp}/qa103-$NAME.raw
ssh -o BatchMode=yes -o ConnectTimeout=10 -i "$KEY" "$KINDLE" \
    'dd if=/dev/fb0 bs=1280 count=1680 2>/dev/null' > "$RAW"
if python3 -c 'import PIL' 2>/dev/null; then
    python3 "$HERE/aristotle/tools/host/fb2png.py" "$RAW" "$OUT/$NAME.png" 1280 1264 1680
else
    ffmpeg -loglevel error -y -f rawvideo -pix_fmt gray -s 1280x1680 -i "$RAW" \
        -vf crop=1264:1680:0:0 "$OUT/$NAME.png"
fi
rm -f "$RAW"
echo "$OUT/$NAME.png"
